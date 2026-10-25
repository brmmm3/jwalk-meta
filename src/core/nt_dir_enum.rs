//! Batched directory enumeration using the Windows NT Native API.
//!
//! Use NtQueryDirectoryFileEx with a 64 KB buffer instead of FindFirstFileW/FindNextFileW,
//! fetching directory entries in batches with one syscall and significantly reducing user/kernel transitions.

#![cfg(windows)]

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::{io, ptr};

use winapi::shared::ntdef::{HANDLE, LPCWSTR};
use winapi::um::fileapi::GetVolumeInformationByHandleW;
use winapi::um::fileapi::{CreateFileW, OPEN_EXISTING};
use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
use winapi::um::libloaderapi::{GetProcAddress, LoadLibraryW};
use winapi::um::winbase::FILE_FLAG_BACKUP_SEMANTICS;
use winapi::um::winnt::{
    FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

/// 64 KB I/O buffer for NtQueryDirectoryFileEx to reduce syscall count.
/// 64 KB is sufficient for local disks (microsecond-scale latency per syscall).
/// A larger buffer may reduce network round trips for SMB/network paths, but increases TLS memory use.
const BUFFER_SIZE: usize = 64 * 1024;

// Thread-local buffer that reuses a 64 KB heap allocation across Rayon worker threads.
thread_local! {
    static TLS_BUFFER: std::cell::RefCell<Option<Vec<u8>>> = const { std::cell::RefCell::new(None) };
}

/// File information class number for FILE_ID_BOTH_DIR_INFO.
const FILE_ID_BOTH_DIR_INFO_CLASS: u32 = 37;

/// Status code returned by NtQueryDirectoryFileEx when there are no more files.
const STATUS_NO_MORE_FILES: i32 = -2147483642; // 0x80000006

/// FILE_ATTRIBUTE_DIRECTORY flag value.
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

// ── FFI structures ──────────────────────────────────────────────────────

/// Directory entry information structure returned by NtQueryDirectoryFileEx.
/// Corresponds to the Windows FILE_ID_BOTH_DIR_INFO structure.
#[repr(C)]
#[allow(non_snake_case)]
struct FILE_ID_BOTH_DIR_INFO {
    NextEntryOffset: u32,
    FileIndex: u32,
    CreationTime: i64,
    LastAccessTime: i64,
    LastWriteTime: i64,
    ChangeTime: i64,
    EndOfFile: i64,
    AllocationSize: i64,
    FileAttributes: u32,
    FileNameLength: u32,
    EaSize: u32,
    ShortNameLength: i8,
    ShortName: [u16; 12],
    FileId: i64,
    FileName: [u16; 1],
}

/// NT I/O status block, an output parameter for NtQueryDirectoryFileEx.
#[repr(C)]
#[allow(non_snake_case)]
struct IO_STATUS_BLOCK {
    Status: i32,
    Information: usize,
}

/// NT UNICODE_STRING for the filename in OBJECT_ATTRIBUTES.
/// Note: Length and MaximumLength are in bytes and exclude the null terminator.
#[repr(C)]
#[allow(non_snake_case)]
struct UNICODE_STRING {
    Length: u16,
    MaximumLength: u16,
    Buffer: *mut u16,
}

/// NT OBJECT_ATTRIBUTES for NtQueryInformationByName.
/// RootDirectory is an open directory handle; ObjectName is a relative filename.
#[repr(C)]
#[allow(non_snake_case)]
struct OBJECT_ATTRIBUTES {
    Length: u32,
    RootDirectory: HANDLE,
    ObjectName: *mut UNICODE_STRING,
    Attributes: u32,
    SecurityDescriptor: *mut std::ffi::c_void,
    SecurityQualityOfService: *mut std::ffi::c_void,
}

/// File statistics returned by NtQueryInformationByName (class 68).
/// Available on Windows 10 1709+. Includes FileId, NumberOfLinks, timestamps, and file attributes,
/// and can be queried without opening a file handle.
#[repr(C)]
#[allow(non_snake_case)]
struct FILE_STAT_INFORMATION {
    FileId: i64,
    CreationTime: i64,
    LastAccessTime: i64,
    LastWriteTime: i64,
    ChangeTime: i64,
    AllocationSize: i64,
    EndOfFile: i64,
    FileAttributes: u32,
    ReparseTag: u32,
    NumberOfLinks: u32,
    EffectiveAccess: u32,
}

/// Information class number for FILE_STAT_INFORMATION (class 68).
/// Available on Windows 10 1709+.
const FILE_STAT_INFORMATION_CLASS: u32 = 68;

/// OBJ_CASE_INSENSITIVE flag for OBJECT_ATTRIBUTES.
const OBJ_CASE_INSENSITIVE: u32 = 0x40;

// ── Dynamically load ntdll ──────────────────────────────────────────────

/// Function signature for NtQueryDirectoryFileEx.
#[allow(non_snake_case)]
type NtQueryDirectoryFileExFn = unsafe extern "system" fn(
    FileHandle: HANDLE,
    Event: HANDLE,
    ApcRoutine: Option<unsafe extern "system" fn(...)>,
    ApcContext: *mut std::ffi::c_void,
    IoStatusBlock: *mut IO_STATUS_BLOCK,
    FileInformation: *mut u8,
    Length: u32,
    FileInformationClass: u32,
    ReturnSingleEntry: i32,
    FileName: LPCWSTR,
    RestartScan: i32,
) -> i32;

/// Function signature for RtlNtStatusToDosError, which converts NTSTATUS to a Win32 error code.
type RtlNtStatusToDosErrorFn = unsafe extern "system" fn(status: i32) -> u32;

/// Function signature for NtQueryInformationByName (Windows 10 1709+).
/// Queries file information using RootDirectory and FileName without opening a file handle.
#[allow(non_snake_case)]
type NtQueryInformationByNameFn = unsafe extern "system" fn(
    ObjectAttributes: *mut OBJECT_ATTRIBUTES,
    IoStatusBlock: *mut IO_STATUS_BLOCK,
    FileInformation: *mut std::ffi::c_void,
    Length: u32,
    FileInformationClass: u32,
) -> i32;

/// Collection of function pointers dynamically loaded from ntdll.dll.
struct NtDllFuncs {
    query_dir: NtQueryDirectoryFileExFn,
    status_to_win32: RtlNtStatusToDosErrorFn,
    /// NtQueryInformationByName (Windows 10 1709+). Optional because it is absent on older Windows versions.
    query_by_name: Option<NtQueryInformationByNameFn>,
}

/// Lazily load ntdll function pointers. Panics on failure (initialized on first call, usually when the user first calls into_iter()).
///
/// Design decision: panicking is intentional fail-fast behavior. ntdll.dll is a core, process-wide Windows DLL;
/// failure to load it indicates a damaged system installation or security software interference, and the process cannot operate normally.
/// Propagating a Result would only defer the inevitable failure to the caller without any benefit.
///
/// LoadLibraryW increments ntdll.dll's reference count; FreeLibrary is intentionally not called.
/// ntdll.dll is never unloaded, and the function pointers must remain valid for the process lifetime.
fn ntdll_funcs() -> &'static NtDllFuncs {
    use std::sync::OnceLock;
    static FUNCS: OnceLock<NtDllFuncs> = OnceLock::new();
    FUNCS.get_or_init(|| unsafe {
        // SAFETY:
        // 1. LoadLibraryW("ntdll.dll") is safe: ntdll.dll is a core Windows system DLL and is always present.
        // 2. The type aliases above ensure that GetProcAddress function pointer signatures match the NT API documentation.
        // 3. transmute converts raw pointers to typed function pointers; their types are guaranteed by the signatures
        //    of NtQueryDirectoryFileExFn and RtlNtStatusToDosErrorFn.
        let ntdll_name = to_wide_null("ntdll.dll\0");
        let module = LoadLibraryW(ntdll_name.as_ptr());
        if module.is_null() {
            panic!("nt_dir_enum: failed to load ntdll.dll");
        }

        let query_dir_ptr = GetProcAddress(module, c"NtQueryDirectoryFileEx".as_ptr());
        let status_to_win32_ptr = GetProcAddress(module, c"RtlNtStatusToDosError".as_ptr());

        if query_dir_ptr.is_null() || status_to_win32_ptr.is_null() {
            panic!("nt_dir_enum: failed to get ntdll function addresses");
        }

        // NtQueryInformationByName (Windows 10 1709+): load optionally; it is absent on older Windows versions.
        let query_by_name_ptr = GetProcAddress(module, c"NtQueryInformationByName".as_ptr());
        let query_by_name = if query_by_name_ptr.is_null() {
            None
        } else {
            Some(std::mem::transmute(query_by_name_ptr))
        };

        NtDllFuncs {
            query_dir: std::mem::transmute(query_dir_ptr),
            status_to_win32: std::mem::transmute(status_to_win32_ptr),
            query_by_name,
        }
    })
}

// ── RAII handle guard ──────────────────────────────────────────────────

/// RAII guard that automatically calls CloseHandle on drop. Used for handles returned by CreateFileW.
pub(crate) struct HandleGuard(HANDLE);

impl HandleGuard {
    /// Return the wrapped handle for batch queries and other operations.
    pub(crate) fn handle(&self) -> HANDLE {
        self.0
    }
}

impl Drop for HandleGuard {
    fn drop(&mut self) {
        // SAFETY: HandleGuard wraps only valid handles returned by CreateFileW,
        // so calling CloseHandle on drop is safe.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

// ── Public structures ──────────────────────────────────────────────────

/// Directory entry information returned by NtQueryDirectoryFileEx.
#[derive(Debug)]
pub struct DirEntryInfo {
    pub file_name: OsString,
    pub file_attributes: u32,
    pub file_size: u64,
    pub creation_time: i64,
    pub last_write_time: i64,
    pub last_access_time: i64,
    pub file_id: u64,
    /// Hard-link count. NT API batch enumeration does not provide this field;
    /// query it with NtQueryInformationByName + FileStatInformation (class 68).
    /// Available on Windows 10 1709+; None on older Windows versions or if the query fails.
    pub number_of_links: Option<u32>,
    /// Volume serial number. Retrieved once from the directory handle using GetVolumeInformationByHandleW.
    /// Shared by all entries (files in a directory belong to the same volume).
    pub volume_serial_number: Option<u32>,
}

// ── Helper functions ──────────────────────────────────────────────────

/// Convert a byte string to a null-terminated UTF-16 Vec (for LoadLibraryW/GetProcAddress).
fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Convert an OS path to a null-terminated UTF-16 Vec (for wide-character APIs such as CreateFileW).
fn path_to_widestring(path: &Path) -> Vec<u16> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    wide
}

/// Convert NTSTATUS to io::Error.
fn ntstatus_to_io_error(status: i32, funcs: &NtDllFuncs) -> io::Error {
    // SAFETY: RtlNtStatusToDosError is a stable Windows API; any i32 input is safe.
    let win32_err = unsafe { (funcs.status_to_win32)(status) };
    io::Error::from_raw_os_error(win32_err as i32)
}

// ── Core enumeration functions ────────────────────────────────────────

/// Open a directory handle and return an RAII guard.
///
/// SAFETY: CreateFileW arguments are valid; FILE_FLAG_BACKUP_SEMANTICS allows directories to be opened.
fn open_dir_handle(path: &Path) -> io::Result<HandleGuard> {
    let wide_path = path_to_widestring(path);
    let handle = unsafe {
        CreateFileW(
            wide_path.as_ptr(),
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }

    Ok(HandleGuard(handle))
}

/// Parse the buffer returned by one NtQueryDirectoryFileEx call and extract directory entries.
///
/// Call the `on_entry` callback for each entry other than "." and "..".
///
/// SAFETY: The caller must ensure that buffer contains valid data returned by the NT API.
fn parse_buffer_entries(buffer: &[u8], mut on_entry: impl FnMut(&FILE_ID_BOTH_DIR_INFO, OsString)) {
    let mut offset: usize = 0;
    let buf_len = buffer.len();
    loop {
        // Bounds check: ensure the complete entry header can be read (including the first element of the variable-length FileName).
        // Kernel data is generally trusted; this check guards against memory corruption or an invalid NextEntryOffset.
        if offset + std::mem::size_of::<FILE_ID_BOTH_DIR_INFO>() > buf_len {
            break;
        }

        // SAFETY: The bounds check ensures offset..offset+size_of is within buffer.
        // FILE_ID_BOTH_DIR_INFO may appear at unaligned offsets (NT only guarantees
        // 4-byte alignment, but the struct has u64 fields requiring 8-byte alignment).
        let entry_ptr = unsafe { buffer.as_ptr().add(offset) };
        let entry = unsafe { std::ptr::read_unaligned(entry_ptr as *const FILE_ID_BOTH_DIR_INFO) };

        let name_len = entry.FileNameLength as usize;
        let name_chars = name_len / 2;

        // Bounds check: ensure all variable-length FileName data is within buffer.
        let name_start_offset = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
        if offset + name_start_offset + name_len > buf_len {
            break;
        }

        // Compare "." and ".." as raw UTF-16 to avoid a to_string_lossy heap allocation.
        // FileName is a C variable-length array ([u16; 1]); access it through a pointer to avoid compiler bounds checks.
        let first_char = entry.FileName[0];
        let is_dot = name_chars == 1 && first_char == b'.' as u16;
        let is_dotdot = name_chars == 2
            && first_char == b'.' as u16
            // SAFETY: name_chars == 2 guarantees at least 2 u16 elements in FileName
            && unsafe { *entry.FileName.as_ptr().add(1) } == b'.' as u16;

        if !is_dot && !is_dotdot {
            // SAFETY: The bounds check ensures name_len bytes of FileName are within buffer.
            let name_slice = unsafe {
                let name_start = entry_ptr.add(name_start_offset);
                std::slice::from_raw_parts(name_start as *const u16, name_chars)
            };
            let file_name = OsString::from_wide(name_slice);

            on_entry(&entry, file_name);
        }

        let next_offset = entry.NextEntryOffset;
        if next_offset == 0 {
            break;
        }
        offset += next_offset as usize;
    }
}

/// Core enumeration loop: batch queries with NtQueryDirectoryFileEx and parse the returned buffer.
///
/// Call `on_entry` for each entry other than "."/"..", passing the parsed raw entry and filename.
/// The caller constructs `DirEntryInfo` and decides whether to collect or dispatch it.
fn enumerate_dir_core(
    guard: &HandleGuard,
    funcs: &NtDllFuncs,
    on_entry: impl FnMut(&FILE_ID_BOTH_DIR_INFO, OsString),
) -> io::Result<()> {
    let mut buffer = TLS_BUFFER.with(|b| {
        b.borrow_mut()
            .take()
            .unwrap_or_else(|| vec![0u8; BUFFER_SIZE])
    });

    let result = enumerate_dir_core_inner(guard, funcs, &mut buffer, on_entry);

    let _ = TLS_BUFFER.try_with(|b| {
        *b.borrow_mut() = Some(buffer);
    });

    result
}

fn enumerate_dir_core_inner(
    guard: &HandleGuard,
    funcs: &NtDllFuncs,
    buffer: &mut [u8],
    mut on_entry: impl FnMut(&FILE_ID_BOTH_DIR_INFO, OsString),
) -> io::Result<()> {
    let mut restart_scan: i32 = 1;

    loop {
        let mut iosb = IO_STATUS_BLOCK {
            Status: 0,
            Information: 0,
        };

        // SAFETY: handle is a valid directory handle, buffer is large enough,
        // and FILE_ID_BOTH_DIR_INFO_CLASS is a valid information class.
        let status = unsafe {
            (funcs.query_dir)(
                guard.0,
                ptr::null_mut(),
                None,
                ptr::null_mut(),
                &mut iosb,
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                FILE_ID_BOTH_DIR_INFO_CLASS,
                0, // ReturnSingleEntry = FALSE
                ptr::null(),
                restart_scan,
            )
        };

        restart_scan = 0;

        if status != 0 {
            if status == STATUS_NO_MORE_FILES {
                break;
            }
            return Err(ntstatus_to_io_error(status, funcs));
        }

        // SAFETY: iosb.Information contains the number of valid data bytes in buffer.
        let bytes_returned = iosb.Information;
        if bytes_returned == 0 {
            break;
        }

        parse_buffer_entries(buffer, &mut on_entry);
    }

    Ok(())
}

/// Use NtQueryDirectoryFileEx to batch-enumerate all files and subdirectories in a directory.
///
/// Return all entries except "." and "..", along with the still-open directory handle.
/// The directory handle is used for subsequent batch ext-info queries (NtQueryInformationByName)
/// and is automatically closed when HandleGuard is dropped.
/// Use a 64 KB buffer to reduce syscall count and preallocate the Vec to reduce heap reallocations.
pub fn enumerate_dir(path: &Path, capacity: usize) -> io::Result<(HandleGuard, Vec<DirEntryInfo>)> {
    let funcs = ntdll_funcs();
    let guard = open_dir_handle(path)?;
    let mut result = Vec::with_capacity(capacity);

    enumerate_dir_core(&guard, funcs, |entry, file_name| {
        result.push(DirEntryInfo {
            file_name,
            file_attributes: entry.FileAttributes,
            file_size: entry.EndOfFile as u64,
            creation_time: entry.CreationTime,
            last_write_time: entry.LastWriteTime,
            last_access_time: entry.LastAccessTime,
            file_id: entry.FileId as u64,
            number_of_links: None,
            volume_serial_number: None,
        });
    })?;

    Ok((guard, result))
}

/// Use NtQueryDirectoryFileEx to batch-enumerate and invoke the callback for each subdirectory for streaming dispatch.
///
/// Subdirectories are dispatched immediately through the callback (without waiting for enumeration to finish), while all entries (files and subdirectories)
/// are still collected in the returned Vec, so no entries are omitted.
/// Return the still-open directory handle for subsequent batch ext-info queries.
pub fn enumerate_dir_streaming(
    path: &Path,
    capacity: usize,
    mut on_subdir: impl FnMut(&DirEntryInfo),
) -> io::Result<(HandleGuard, Vec<DirEntryInfo>)> {
    let funcs = ntdll_funcs();
    let guard = open_dir_handle(path)?;
    let mut result = Vec::with_capacity(capacity);

    enumerate_dir_core(&guard, funcs, |entry, file_name| {
        let info = DirEntryInfo {
            file_name,
            file_attributes: entry.FileAttributes,
            file_size: entry.EndOfFile as u64,
            creation_time: entry.CreationTime,
            last_write_time: entry.LastWriteTime,
            last_access_time: entry.LastAccessTime,
            file_id: entry.FileId as u64,
            number_of_links: None,
            volume_serial_number: None,
        };

        if info.file_attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            on_subdir(&info);
        }

        result.push(info);
    })?;

    Ok((guard, result))
}

// ── Batch ext-info queries ───────────────────────────────────────────

/// Query NumberOfLinks for directory entries using NtQueryInformationByName + FileStatInformation (class 68).
///
/// Query using the open directory handle and relative filename, without opening a handle for each file,
/// eliminating symlink_metadata path-resolution overhead.
///
/// Iterate on a single thread: the NTFS kernel uses a volume-wide mutex for MFT queries on the same volume, so concurrent queries
/// do not speed things up and instead add thread-scheduling overhead (measured: rayon par_iter_mut with 8 threads took 5.1s >
/// single-threaded iter_mut at 3.5s).
///
/// Thread safety:
/// - HANDLE is a kernel object reference; concurrent read operations (NtQueryInformationByName) are safe—the kernel maintains the reference count,
///   and HandleGuard ensures CloseHandle is called only after all queries complete.
/// - Each query constructs its own UNICODE_STRING, OBJECT_ATTRIBUTES, and stat_info_buf,
///   with no shared mutable state or lock contention.
///
/// Available on Windows 10 1709+. number_of_links remains None on older Windows versions or if the query fails.
///
/// `query_file_nlinks`: whether to query NumberOfLinks for regular files as well.
/// When false, query only directory entries (FILE_ATTRIBUTE_DIRECTORY);
/// regular-file number_of_links remains None, reducing the query count by about 50%.
pub fn batch_query_nlinks(
    entries: &mut [DirEntryInfo],
    dir_handle: &HandleGuard,
    query_file_nlinks: bool,
) {
    let funcs = ntdll_funcs();
    let query_by_name = match funcs.query_by_name {
        Some(f) => f,
        None => return, // NtQueryInformationByName is unavailable (older Windows); leave None.
    };

    // Iterate on a single thread: the NTFS kernel uses a volume-wide mutex for same-volume MFT queries,
    // so concurrent queries do not speed things up and instead add thread-scheduling overhead.
    // See the batch_query_nlinks performance analysis (rayon par_iter_mut with 8 threads: 5.1s > single-threaded: 3.5s).
    let root_handle_raw = dir_handle.handle() as isize;
    // Reuse the UTF-16 filename buffer across entries to avoid per-entry heap allocations.
    // Capacity 260 covers MAX_PATH; Vec grows automatically for longer filenames.
    let mut wide_buf: Vec<u16> = Vec::with_capacity(260);
    entries.iter_mut().for_each(|entry| {
        // Filter: query NumberOfLinks only for directory entries unless query_file_nlinks=true.
        if !query_file_nlinks && (entry.file_attributes & FILE_ATTRIBUTE_DIRECTORY) == 0 {
            return; // Not a directory entry; skip the query.
        }

        wide_buf.clear();
        wide_buf.extend(entry.file_name.encode_wide());
        if wide_buf.is_empty() {
            return;
        }

        // Separate FILE_STAT_INFORMATION buffer for each iteration (72 bytes on the stack).
        let mut stat_info_buf = [0u8; std::mem::size_of::<FILE_STAT_INFORMATION>()];

        let byte_len = (wide_buf.len() * 2) as u16;
        let mut unicode_str = UNICODE_STRING {
            Length: byte_len,
            MaximumLength: byte_len,
            Buffer: wide_buf.as_ptr() as *mut u16,
        };

        let mut obj_attrs = OBJECT_ATTRIBUTES {
            Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: root_handle_raw as HANDLE,
            ObjectName: &mut unicode_str,
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: ptr::null_mut(),
            SecurityQualityOfService: ptr::null_mut(),
        };

        let mut iosb = IO_STATUS_BLOCK {
            Status: 0,
            Information: 0,
        };

        // SAFETY:
        // 1. obj_attrs is built from root_handle and a relative filename; NtQueryInformationByName
        //    locates the file using RootDirectory and ObjectName, without opening a file handle.
        // 2. stat_info_buf size = size_of::<FILE_STAT_INFORMATION>() = 72 bytes.
        // 3. FILE_STAT_INFORMATION_CLASS = 68 is a valid information class (Windows 10 1709+).
        // 4. unicode_str.Buffer points to wide_buf, which remains valid for this function's scope.
        // 5. root_handle is a kernel handle; single-threaded read access is safe.
        let status = unsafe {
            query_by_name(
                &mut obj_attrs,
                &mut iosb,
                stat_info_buf.as_mut_ptr() as *mut std::ffi::c_void,
                stat_info_buf.len() as u32,
                FILE_STAT_INFORMATION_CLASS,
            )
        };

        if status == 0 {
            // SAFETY: On success, NtQueryInformationByName fills the buffer with a valid FILE_STAT_INFORMATION.
            let stat_info = unsafe {
                std::ptr::read_unaligned(stat_info_buf.as_ptr() as *const FILE_STAT_INFORMATION)
            };
            entry.number_of_links = Some(stat_info.NumberOfLinks);
        }
        // Leave number_of_links as None if the query fails.
    });
}

/// Get the filesystem type and volume serial number in one GetVolumeInformationByHandleW call.
///
/// Combines the previous detect_fs_type and query_volume_serial calls,
/// eliminating a redundant GetVolumeInformationByHandleW syscall.
/// Returns (FsType::Unknown, None) on failure.
pub fn detect_fs_type_and_vol_serial(dir_handle: &HandleGuard) -> (FsType, Option<u32>) {
    let mut fs_name_buf = [0u16; 16]; // Enough for "NTFS"=4, "FAT32"=5, or "exFAT"=5, plus the null terminator.
    let mut volume_serial: u32 = 0;
    let success = unsafe {
        GetVolumeInformationByHandleW(
            dir_handle.handle(),
            ptr::null_mut(),          // lpVolumeNameBuffer
            0,                        // nVolumeNameLength
            &mut volume_serial,       // lpVolumeSerialNumber
            ptr::null_mut(),          // lpMaximumComponentLength
            ptr::null_mut(),          // lpFileSystemFlags
            fs_name_buf.as_mut_ptr(), // lpFileSystemNameBuffer
            fs_name_buf.len() as u32, // nFileSystemNameLength
        )
    };

    if success == 0 {
        return (FsType::Unknown, None);
    }

    // Find the null terminator.
    let len = fs_name_buf
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(fs_name_buf.len());
    let fs_name = OsString::from_wide(&fs_name_buf[..len]);
    // Compare as ASCII (Windows filesystem names are always ASCII uppercase).
    let fs_type = match fs_name.to_string_lossy().as_ref() {
        "NTFS" => FsType::Ntfs,
        "ReFS" => FsType::Refs,
        "FAT32" => FsType::Fat32,
        "exFAT" => FsType::ExFat,
        "FAT" => FsType::Fat32, // FAT12/FAT16 also do not support hard links.
        "FAT16" => FsType::Fat32, // Same as above.
        _ => FsType::Unknown,
    };

    (fs_type, Some(volume_serial))
}

// ── Filesystem type detection ──────────────────────────────────────────

/// Windows filesystem type, used to determine the number_of_links query strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsType {
    /// NTFS — supports hard links; query NumberOfLinks for each file.
    Ntfs,
    /// ReFS — supports hard links; query NumberOfLinks for each file.
    Refs,
    /// FAT32 — does not support hard links; NumberOfLinks is always 1.
    Fat32,
    /// exFAT — does not support hard links; NumberOfLinks is always 1.
    ExFat,
    /// Unknown filesystem — conservatively query each file.
    Unknown,
}

impl FsType {
    /// The FAT family (FAT32/exFAT) does not support hard links; NumberOfLinks is always 1.
    /// It is safe to skip batch_query_nlinks and set nlink=1 directly.
    #[inline]
    pub fn is_fat_family(self) -> bool {
        matches!(self, FsType::Fat32 | FsType::ExFat)
    }
}
