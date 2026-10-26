//! Proof of concept for using class 77 (FileStatBasicInformation) with NtQueryDirectoryFileEx directory enumeration.
//!
//! Goal: verify whether the class 77 buffer contains a LinkCount field.
//! Approach: first use class 37 (FILE_ID_BOTH_DIR_INFO, a known format) to get a filename list as an anchor,
//!           then get the raw buffer with class 77 and use filenames to locate entry boundaries in reverse,
//!           infer the entry header size, and analyze the field layout.
//!
//! Does not modify production code under src/ or add dependencies; dynamically loads ntdll.

use std::env;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::process;
use std::{io, ptr};

use winapi::shared::ntdef::{HANDLE, LPCWSTR};
use winapi::um::fileapi::{CreateFileW, OPEN_EXISTING};
use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
use winapi::um::libloaderapi::{GetProcAddress, LoadLibraryW};
use winapi::um::winbase::FILE_FLAG_BACKUP_SEMANTICS;
use winapi::um::winnt::{
    FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

// ── Information class constants ───────────────────────────────────────

/// FILE_ID_BOTH_DIR_INFORMATION (baseline).
const FILE_ID_BOTH_DIR_INFO_CLASS: u32 = 37;

/// FileStatBasicInformation (test target).
const FILE_STAT_BASIC_INFORMATION_CLASS: u32 = 77;

const STATUS_NO_MORE_FILES: i32 = -2147483642; // 0x80000006

// ── FFI types ──────────────────────────────────────────────────────────

#[repr(C)]
#[allow(non_snake_case)]
struct IO_STATUS_BLOCK {
    Status: i32,
    Information: usize,
}

#[allow(non_snake_case)]
type NtQueryDirectoryFileExFn = unsafe extern "system" fn(
    FileHandle: HANDLE,
    Event: HANDLE,
    ApcRoutine: *mut std::ffi::c_void,
    ApcContext: *mut std::ffi::c_void,
    IoStatusBlock: *mut IO_STATUS_BLOCK,
    FileInformation: *mut u8,
    Length: u32,
    FileInformationClass: u32,
    ReturnSingleEntry: i32,
    FileName: LPCWSTR,
    RestartScan: i32,
) -> i32;

// ── Known class 37 structure (used only to parse the baseline) ─────────

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
    ShortNameLength: u8,
    _pad: [u8; 1],
    ShortName: [u16; 12],
    FileId: i64,
    FileName: [u16; 1],
}

// ── NtQueryDirectoryFile (without Ex) FFI — verify whether both APIs reject class 77 ──

/// UNICODE_STRING for NtQueryDirectoryFile's FileName parameter.
#[repr(C)]
#[allow(non_snake_case)]
struct UNICODE_STRING_NAME {
    Length: u16,
    MaximumLength: u16,
    Buffer: *mut u16,
}

#[allow(non_snake_case)]
type NtQueryDirectoryFileFn = unsafe extern "system" fn(
    FileHandle: HANDLE,
    Event: HANDLE,
    ApcRoutine: *mut std::ffi::c_void,
    ApcContext: *mut std::ffi::c_void,
    IoStatusBlock: *mut IO_STATUS_BLOCK,
    FileInformation: *mut u8,
    Length: u32,
    FileInformationClass: u32,
    ReturnSingleEntry: i32,
    FileName: *mut UNICODE_STRING_NAME,
    RestartScan: i32,
) -> i32;

fn load_query_dir_no_ex() -> Option<NtQueryDirectoryFileFn> {
    let name = to_wide_null("ntdll.dll\0");
    let module = unsafe { LoadLibraryW(name.as_ptr()) };
    if module.is_null() {
        return None;
    }
    let proc = unsafe { GetProcAddress(module, b"NtQueryDirectoryFile\0".as_ptr() as *const i8) };
    if proc.is_null() {
        None
    } else {
        Some(unsafe { std::mem::transmute(proc) })
    }
}

// ── FFI for the single-file class 77 health check (NtQueryInformationByName) ──

#[repr(C)]
#[allow(non_snake_case)]
struct UNICODE_STRING {
    Length: u16,
    MaximumLength: u16,
    Buffer: *mut u16,
}

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

const OBJ_CASE_INSENSITIVE: u32 = 0x40;

#[allow(non_snake_case)]
type NtQueryInformationByNameFn = unsafe extern "system" fn(
    ObjectAttributes: *mut OBJECT_ATTRIBUTES,
    IoStatusBlock: *mut IO_STATUS_BLOCK,
    FileInformation: *mut std::ffi::c_void,
    Length: u32,
    FileInformationClass: u32,
) -> i32;

/// Single-file class 77 return structure (Microsoft documentation; used for the health check).
#[repr(C)]
#[allow(non_snake_case)]
/// Single-file class 77 return structure (Microsoft documentation).
/// Note: EffectiveAccess is a LARGE_INTEGER (8 bytes), not a ULONG.
/// Size = 7*8 + 4*3 + 4 (padding) + 8 = 80 bytes.
#[repr(C)]
#[allow(non_snake_case)]
struct FILE_STAT_BASIC_INFORMATION {
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
    EffectiveAccess: i64,
}

/// Load NtQueryInformationByName.
fn load_query_by_name() -> Option<NtQueryInformationByNameFn> {
    let name = to_wide_null("ntdll.dll\0");
    let module = unsafe { LoadLibraryW(name.as_ptr()) };
    if module.is_null() {
        return None;
    }
    let proc =
        unsafe { GetProcAddress(module, b"NtQueryInformationByName\0".as_ptr() as *const i8) };
    if proc.is_null() {
        None
    } else {
        Some(unsafe { std::mem::transmute(proc) })
    }
}

/// Verify that the system recognizes class 77 by querying a known file with NtQueryInformationByName.
/// Return Some(NumberOfLinks) on success, or None on failure.
fn test_single_file_class77(dir_handle: HANDLE, entries: &[(String, u32, u32)]) -> Option<u32> {
    let query_by_name = match load_query_by_name() {
        Some(f) => f,
        None => {
            println!(
                "  NtQueryInformationByName is unavailable (Windows 10 version 1709 or earlier)"
            );
            return None;
        }
    };

    // Use the first entry as the test target.
    let (name, _, _) = entries.first()?;
    let mut wide: Vec<u16> = name.encode_utf16().collect();
    if wide.is_empty() {
        return None;
    }
    let byte_len = (wide.len() * 2) as u16;
    let mut unicode_str = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        Buffer: wide.as_mut_ptr(),
    };
    let mut obj_attrs = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: dir_handle,
        ObjectName: &mut unicode_str,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: ptr::null_mut(),
        SecurityQualityOfService: ptr::null_mut(),
    };
    // Scan sizes from 64 to 128 to find a buffer size accepted by the system.
    let declared_size = std::mem::size_of::<FILE_STAT_BASIC_INFORMATION>();
    println!("  Declared structure size = {}", declared_size);
    let mut found_size: Option<usize> = None;
    for probe_size in (64..=128).step_by(4) {
        let mut probe_buf = vec![0u8; probe_size];
        let mut iosb_probe = IO_STATUS_BLOCK {
            Status: 0,
            Information: 0,
        };
        let status_probe = unsafe {
            query_by_name(
                &mut obj_attrs,
                &mut iosb_probe,
                probe_buf.as_mut_ptr() as *mut std::ffi::c_void,
                probe_buf.len() as u32,
                FILE_STAT_BASIC_INFORMATION_CLASS,
            )
        };
        let status_hex = format!("0x{:08x}", status_probe as u32);
        if status_probe == 0 {
            println!("  size {} → success ✓", probe_size);
            found_size = Some(probe_size);
            break;
        } else {
            println!("  size {} → {}", probe_size, status_hex);
        }
    }

    let valid_size = match found_size {
        Some(s) => s,
        None => {
            println!("  All sizes failed — class 77 is unavailable on this system");
            return None;
        }
    };

    // Call again with the correct size and parse the result.
    let mut stat_buf = vec![0u8; valid_size];
    let mut iosb = IO_STATUS_BLOCK {
        Status: 0,
        Information: 0,
    };
    let status = unsafe {
        query_by_name(
            &mut obj_attrs,
            &mut iosb,
            stat_buf.as_mut_ptr() as *mut std::ffi::c_void,
            stat_buf.len() as u32,
            FILE_STAT_BASIC_INFORMATION_CLASS,
        )
    };
    if status != 0 {
        println!("  Second call failed: NTSTATUS 0x{:08x}", status as u32);
        return None;
    }
    // Extract NumberOfLinks from the valid_size buffer (offset 64, 4 bytes, per the FILE_STAT_BASIC_INFORMATION layout).
    if valid_size >= 68 {
        let nlinks = u32::from_ne_bytes([stat_buf[64], stat_buf[65], stat_buf[66], stat_buf[67]]);
        let attrs = u32::from_ne_bytes([stat_buf[56], stat_buf[57], stat_buf[58], stat_buf[59]]);
        println!(
            "  Success: \"{}\" NumberOfLinks={} FileAttributes={:#x} (buffer size {})",
            name, nlinks, attrs, valid_size
        );
        Some(nlinks)
    } else {
        println!(
            "  valid_size {} is too small to contain the NumberOfLinks field",
            valid_size
        );
        None
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn path_to_widestring(path: &Path) -> Vec<u16> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    wide
}

struct HandleGuard(HANDLE);
impl Drop for HandleGuard {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

fn open_dir(path: &Path) -> io::Result<HandleGuard> {
    let wide = path_to_widestring(path);
    let h = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        Ok(HandleGuard(h))
    }
}

fn load_ntdll_query_dir() -> NtQueryDirectoryFileExFn {
    let name = to_wide_null("ntdll.dll\0");
    let module = unsafe { LoadLibraryW(name.as_ptr()) };
    if module.is_null() {
        eprintln!("Error: failed to load ntdll.dll");
        process::exit(1);
    }
    let proc = unsafe { GetProcAddress(module, b"NtQueryDirectoryFileEx\0".as_ptr() as *const i8) };
    if proc.is_null() {
        eprintln!("Error: failed to get the address of NtQueryDirectoryFileEx");
        process::exit(1);
    }
    unsafe { std::mem::transmute(proc) }
}

/// Call NtQueryDirectoryFileEx once using the NT API and return the filled buffer and number of bytes written.
fn query_dir_once(
    query: NtQueryDirectoryFileExFn,
    handle: HANDLE,
    info_class: u32,
    restart: i32,
) -> io::Result<(Vec<u8>, usize)> {
    query_dir_once_full(query, handle, info_class, 0, ptr::null(), restart)
}

/// Call NtQueryDirectoryFileEx with the full parameter set.
fn query_dir_once_full(
    query: NtQueryDirectoryFileExFn,
    handle: HANDLE,
    info_class: u32,
    return_single: i32,
    file_name: LPCWSTR,
    restart: i32,
) -> io::Result<(Vec<u8>, usize)> {
    let mut buffer = vec![0u8; 64 * 1024];
    let mut iosb = IO_STATUS_BLOCK {
        Status: 0,
        Information: 0,
    };
    let status = unsafe {
        query(
            handle,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut iosb,
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            info_class,
            return_single,
            file_name,
            restart,
        )
    };
    // STATUS_NO_MORE_FILES is not an error; it only indicates there are no more entries.
    if status < 0 && status != STATUS_NO_MORE_FILES {
        return Err(io::Error::from_raw_os_error(status));
    }
    let written = iosb.Information;
    buffer.truncate(written);
    Ok((buffer, written))
}

/// Dump the first max_bytes of the buffer as hex + ASCII, similar to xxd output.
fn dump_hex(buffer: &[u8], max_bytes: usize, label: &str) {
    let n = buffer.len().min(max_bytes);
    println!(
        "\n=== {} (first {} bytes, total buffer length {}) ===",
        label,
        n,
        buffer.len()
    );
    for chunk_start in (0..n).step_by(16) {
        let chunk_end = (chunk_start + 16).min(n);
        let chunk = &buffer[chunk_start..chunk_end];
        let hex: String = chunk
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect::<Vec<_>>()
            .join(" ");
        let ascii: String = chunk
            .iter()
            .map(|&b| {
                if (32..=126).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        println!("{:08x}: {:<48} {}", chunk_start, hex, ascii);
    }
    if buffer.len() > max_bytes {
        println!("... ({} bytes omitted)", buffer.len() - max_bytes);
    }
}

// ── Parse the class 37 buffer (known format) to obtain filename anchors ─

fn parse_class37(buffer: &[u8]) -> Vec<(String, u32, u32)> {
    // Return (filename, NextEntryOffset, entry start offset).
    let mut out = Vec::new();
    let mut offset: usize = 0;
    let header_size = std::mem::size_of::<FILE_ID_BOTH_DIR_INFO>();
    let name_off = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
    while offset + header_size <= buffer.len() {
        let entry_ptr = unsafe { buffer.as_ptr().add(offset) };
        let entry = unsafe { std::ptr::read_unaligned(entry_ptr as *const FILE_ID_BOTH_DIR_INFO) };
        let name_len = entry.FileNameLength as usize;
        let name_chars = name_len / 2;
        if offset + name_off + name_len > buffer.len() {
            break;
        }
        let name_slice = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr().add(offset + name_off) as *const u16,
                name_chars,
            )
        };
        let name = OsString::from_wide(name_slice)
            .to_string_lossy()
            .into_owned();
        // Skip "." and "..".
        if name != "." && name != ".." {
            out.push((name, entry.NextEntryOffset, offset as u32));
        }
        if entry.NextEntryOffset == 0 {
            break;
        }
        offset += entry.NextEntryOffset as usize;
    }
    out
}

/// Search the buffer for a UTF-16-encoded filename and return all matching offsets.
fn find_utf16_occurrences(buffer: &[u8], needle: &str) -> Vec<usize> {
    let encoded: Vec<u16> = needle.encode_utf16().collect();
    if encoded.is_empty() {
        return Vec::new();
    }
    let needle_bytes: Vec<u8> = encoded.iter().flat_map(|&w| w.to_ne_bytes()).collect();
    let mut hits = Vec::new();
    if buffer.len() < needle_bytes.len() {
        return hits;
    }
    for i in 0..=(buffer.len() - needle_bytes.len()) {
        if &buffer[i..i + needle_bytes.len()] == needle_bytes.as_slice() {
            hits.push(i);
        }
    }
    hits
}

/// Read entries assuming the supplied position is the entry start (NextEntryOffset is assumed to be at offset 0),
/// then attempt linked-list traversal. Return each inferred entry start.
fn walk_next_offset_chain(buffer: &[u8]) -> Vec<usize> {
    let mut offsets = vec![0usize];
    let mut cur = 0usize;
    let max_iter = 1024;
    for _ in 0..max_iter {
        if cur + 4 > buffer.len() {
            break;
        }
        let next = u32::from_ne_bytes(buffer[cur..cur + 4].try_into().unwrap()) as usize;
        if next == 0 {
            break;
        }
        cur += next;
        if cur >= buffer.len() {
            break;
        }
        offsets.push(cur);
    }
    offsets
}

#[cfg(windows)]
fn main() {
    let args: Vec<String> = env::args().collect();
    let dir = if args.len() >= 2 {
        Path::new(&args[1])
    } else {
        Path::new(".")
    };
    println!("Test directory: {}", dir.display());

    let query = load_ntdll_query_dir();
    let guard = match open_dir(dir) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Failed to open directory: {}", e);
            process::exit(1);
        }
    };

    // ── Step 1: obtain a baseline using class 37 ────────────────────
    let (buf37, _) = match query_dir_once(query, guard.0, FILE_ID_BOTH_DIR_INFO_CLASS, 1) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("class 37 call failed: {}", e);
            process::exit(1);
        }
    };

    let class37_entries = parse_class37(&buf37);
    println!(
        "\nclass 37 (FILE_ID_BOTH_DIR_INFO) parsed {} entries (excluding . / ..):",
        class37_entries.len()
    );
    for (i, (name, nxt, off)) in class37_entries.iter().take(10).enumerate() {
        println!(
            "  [{}] offset={} NextEntryOffset={} name=\"{}\"",
            i, off, nxt, name
        );
    }
    if class37_entries.len() < 3 {
        eprintln!(
            "Warning: too few entries in the test directory ({}); rerun in a directory containing at least 5 files",
            class37_entries.len()
        );
    }

    // Dump the first 1024 bytes of class 37.
    dump_hex(&buf37, 1024, "class 37 buffer");

    // ── Step 2: try class 77 (FileStatBasicInformation) ────────────
    // Reopen the directory to restart enumeration (avoiding a conflict with the class 37 scan cursor).
    drop(guard);
    let guard2 = match open_dir(dir) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Failed to reopen directory: {}", e);
            process::exit(1);
        }
    };

    // Try class 77 with several parameter combinations:
    //   (a) ReturnSingleEntry=0, FileName=NULL, RestartScan=1
    //   (b) ReturnSingleEntry=0, FileName="*", RestartScan=1
    //   (c) ReturnSingleEntry=1, FileName=NULL, RestartScan=1
    let star_wildcard = to_wide_null("*");
    let mut buf77: Option<(Vec<u8>, usize)> = None;
    println!("\n=== class 77 (FileStatBasicInformation) parameter-combination tests ===");
    for (label, ret_single, fname, restart) in [
        (
            "(a) ret_single=0, FileName=NULL, restart=1",
            0i32,
            ptr::null::<u16>() as LPCWSTR,
            1i32,
        ),
        (
            "(b) ret_single=0, FileName=*",
            0i32,
            star_wildcard.as_ptr() as LPCWSTR,
            1i32,
        ),
        (
            "(c) ret_single=1, FileName=NULL",
            1i32,
            ptr::null::<u16>() as LPCWSTR,
            1i32,
        ),
    ] {
        match query_dir_once_full(
            query,
            guard2.0,
            FILE_STAT_BASIC_INFORMATION_CLASS,
            ret_single,
            fname,
            restart,
        ) {
            Ok((b, n)) => {
                println!("  {}→ success, wrote {} bytes", label, n);
                if buf77.is_none() {
                    buf77 = Some((b, n));
                }
            }
            Err(e) => {
                println!(
                    "  {}→ failed: NTSTATUS {} (raw OS error {})",
                    label,
                    e,
                    e.raw_os_error().unwrap_or(0)
                );
            }
        }
    }

    // Additional check: does NtQueryDirectoryFile (without Ex) support class 77?
    println!("\n=== Additional check: NtQueryDirectoryFile (without Ex) + class 77 ===");
    if let Some(query_dir_no_ex) = load_query_dir_no_ex() {
        let star_name = to_wide_null("*");
        let mut ustr = UNICODE_STRING_NAME {
            Length: (star_name.len() - 1) as u16 * 2,
            MaximumLength: (star_name.len() - 1) as u16 * 2,
            Buffer: star_name.as_ptr() as *mut u16,
        };
        let mut probe_buf = vec![0u8; 64 * 1024];
        let mut iosb = IO_STATUS_BLOCK {
            Status: 0,
            Information: 0,
        };
        let status = unsafe {
            query_dir_no_ex(
                guard2.0,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut iosb,
                probe_buf.as_mut_ptr(),
                probe_buf.len() as u32,
                FILE_STAT_BASIC_INFORMATION_CLASS,
                0,
                &mut ustr,
                1,
            )
        };
        if status == 0 {
            println!(
                "  NtQueryDirectoryFile + class 77 succeeded, wrote {} bytes",
                iosb.Information
            );
        } else {
            println!(
                "  NtQueryDirectoryFile + class 77 failed: 0x{:08x}",
                status as u32
            );
        }
    } else {
        println!("  NtQueryDirectoryFile could not be loaded");
    }

    // Single-file class 77 health check (verify that the system recognizes this info class).
    println!("\n=== Health check: single-file class 77 (NtQueryInformationByName) ===");
    let single_file_class77_ok = test_single_file_class77(guard2.0, &class37_entries);

    let (buf77, written77) = match buf77 {
        Some(r) => r,
        None => {
            println!("\n=== All class 77 directory-enumeration attempts failed ===");
            if single_file_class77_ok.is_some() {
                println!(
                    "The single-file query succeeded, so the system recognizes class 77, but NtQueryDirectoryFileEx rejects it for directory enumeration."
                );
                println!(
                    "Conclusion: class 77 cannot be used for directory enumeration; abandon this optimization."
                );
            } else {
                println!(
                    "The single-file query also failed — the system may not recognize class 77, possibly because Windows is too old."
                );
            }
            println!(
                "Tip: see the PoC startup information for the Windows build number (PowerShell: `[Environment]::OSVersion.Version.Build`)"
            );
            return;
        }
    };

    println!(
        "\n=== class 77 (FileStatBasicInformation) call succeeded, wrote {} bytes ===",
        written77
    );

    // Dump the first 2048 bytes of class 77 for field analysis.
    dump_hex(&buf77, 2048, "class 77 buffer");

    // ── Step 3: linked-list traversal (assuming NextEntryOffset is at the start of each entry) ─
    let chain = walk_next_offset_chain(&buf77);
    println!(
        "\nclass 77 NextEntryOffset chain: inferred entry starts: {:?}",
        chain
    );

    // ── Step 4: use known filenames as anchors to locate entry headers in reverse ───────────
    println!("\n=== Searching the class 77 buffer for anchors using class 37 filenames ===");
    let mut anchors = Vec::new();
    for (name, _, _) in &class37_entries {
        let hits = find_utf16_occurrences(&buf77, name);
        if hits.is_empty() {
            println!("  \"{}\": not found", name);
            continue;
        }
        let pos = hits[0];
        // Confirm that this falls within an entry in the chain.
        let entry_start = chain
            .iter()
            .filter(|&&c| c <= pos)
            .copied()
            .last()
            .unwrap_or(0);
        let header_size = pos - entry_start;
        println!(
            "  \"{}\": name_at={}, entry_start={}, header_size={}",
            name, pos, entry_start, header_size
        );
        anchors.push((name.clone(), pos, entry_start, header_size));
    }

    // ── Step 5: analyze the entry header field layout ──────────────
    if !anchors.is_empty() {
        // Estimate the header size using the most common header_size among all anchors.
        use std::collections::HashMap;
        let mut size_count: HashMap<usize, usize> = HashMap::new();
        for (_, _, _, hs) in &anchors {
            *size_count.entry(*hs).or_insert(0) += 1;
        }
        let common_header = size_count
            .iter()
            .max_by_key(|(_, &c)| c)
            .map(|(&s, _)| s)
            .unwrap_or(0);
        println!(
            "\nInferred class 77 entry-header size (including NextEntryOffset, excluding FileName): {} bytes",
            common_header
        );

        // Dump the first entry's header bytes for detailed analysis and search for key fields.
        if let Some((name, pos, entry_start, _)) = anchors.first() {
            let entry_start = *entry_start;
            let header_end = *pos;
            let baseline_name = name.clone();
            println!(
                "\n=== Detailed analysis of the first entry header (\"{}\", offset {}..{}) ===",
                baseline_name, entry_start, header_end
            );
            let header = &buf77[entry_start..header_end];
            for (i, chunk) in header.chunks(8).enumerate().take(16) {
                let off = entry_start + i * 8;
                let hex: String = chunk
                    .iter()
                    .map(|b| format!("{:02x}", b))
                    .collect::<Vec<_>>()
                    .join(" ");
                let as_u32_lo = if chunk.len() >= 4 {
                    u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])
                } else {
                    0
                };
                let as_u32_hi = if chunk.len() >= 8 {
                    u32::from_ne_bytes([chunk[4], chunk[5], chunk[6], chunk[7]])
                } else {
                    0
                };
                let as_i64 = if chunk.len() >= 8 {
                    i64::from_ne_bytes([
                        chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6],
                        chunk[7],
                    ])
                } else {
                    0
                };
                println!(
                    "  +{:04x}: {:<24} | u32 lo={:>12} hi={:>12} | i64={:>20}",
                    off - entry_start,
                    hex,
                    as_u32_lo,
                    as_u32_hi,
                    as_i64
                );
            }

            // Key-field search: use the timestamp of a same-named class 37 entry to find a match in the class 77 header.
            // Single-file FILE_STAT_BASIC_INFORMATION layout (Microsoft documentation):
            //   FileId(0) CreationTime(8) LastAccessTime(16) LastWriteTime(24) ChangeTime(32)
            //   AllocationSize(40) EndOfFile(48) FileAttributes(56,4) ReparseTag(60,4)
            //   NumberOfLinks(64,4) EffectiveAccess(68,8) = 72 bytes
            // Directory enumeration adds NextEntryOffset + FileNameLength + FileName[] to the header.
            println!("\n=== Searching for the LinkCount field ===");
            if let Some((_, _, c37_off)) =
                class37_entries.iter().find(|(n, _, _)| *n == baseline_name)
            {
                let c37_ptr = unsafe { buf37.as_ptr().add(*c37_off as usize) };
                let c37_entry =
                    unsafe { std::ptr::read_unaligned(c37_ptr as *const FILE_ID_BOTH_DIR_INFO) };
                println!(
                    "  Baseline reference \"{}\": LastWriteTime={:#x}, EndOfFile={:#x}, FileAttributes={:#x}",
                    baseline_name,
                    c37_entry.LastWriteTime as u64,
                    c37_entry.EndOfFile as u64,
                    c37_entry.FileAttributes
                );
                println!(
                    "  Searching the class 77 header for the LastWriteTime byte sequence ({}, 8 bytes)",
                    c37_entry.LastWriteTime
                );
                let ts_bytes = c37_entry.LastWriteTime.to_ne_bytes();
                let header_slice = &buf77[entry_start..(entry_start + common_header)];
                if let Some(idx_in_header) = header_slice
                    .windows(8)
                    .position(|w| w == ts_bytes.as_slice())
                {
                    let abs_off = entry_start + idx_in_header;
                    println!(
                        "  → Found LastWriteTime bytes at entry_start + {} (absolute offset {})",
                        idx_in_header, abs_off
                    );
                    // Based on the FILE_STAT_BASIC_INFORMATION layout: NumberOfLinks offset relative to LastWriteTime = 64 - 24 = 40 bytes.
                    println!(
                        "  → According to the FILE_STAT_BASIC_INFORMATION layout, NumberOfLinks should be 40 bytes after LastWriteTime"
                    );
                    let nlinks_off = idx_in_header + 40;
                    if entry_start + nlinks_off + 4 <= buf77.len() {
                        let nlinks = u32::from_ne_bytes([
                            buf77[entry_start + nlinks_off],
                            buf77[entry_start + nlinks_off + 1],
                            buf77[entry_start + nlinks_off + 2],
                            buf77[entry_start + nlinks_off + 3],
                        ]);
                        println!(
                            "  → Candidate NumberOfLinks field (offset +{}, 4 bytes) = {}",
                            nlinks_off, nlinks
                        );
                    }
                } else {
                    println!("  → LastWriteTime byte sequence not found in the header");
                }
            }
        }
    } else {
        println!("\nWarning: no known filename anchors found in the class 77 buffer");
    }

    // ── Step 6: save analysis data to a file for reference in the verdict document ────────
    write_verdict_supported(&buf37, &buf77, &class37_entries, &anchors);
}

fn write_verdict_supported(
    _buf37: &[u8],
    _buf77: &[u8],
    _class37_entries: &[(String, u32, u32)],
    _anchors: &[(String, usize, usize, usize)],
) {
    let _ = std::fs::create_dir_all("docs");
}

#[cfg(not(windows))]
fn main() {
    eprintln!("class77_poc requires Windows (uses NtQueryDirectoryFileEx + ntdll.dll)");
}
