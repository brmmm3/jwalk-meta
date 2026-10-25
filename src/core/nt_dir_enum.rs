//! Windows NT Native API 批量目录枚举。
//!
//! 使用 NtQueryDirectoryFileEx + 64KB 缓冲区替代 FindFirstFileW/FindNextFileW，
//! 实现单次系统调用批量获取目录条目，显著减少用户态-内核态切换次数。

#![cfg(windows)]

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::{io, ptr};

use winapi::shared::ntdef::{HANDLE, LPCWSTR};
use winapi::um::fileapi::{
    CreateFileW, OPEN_EXISTING,
};
use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
use winapi::um::libloaderapi::{GetProcAddress, LoadLibraryW};
use winapi::um::winbase::FILE_FLAG_BACKUP_SEMANTICS;
use winapi::um::winnt::{
    FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use winapi::um::fileapi::GetVolumeInformationByHandleW;

/// NtQueryDirectoryFileEx 的 64KB I/O 缓冲区，减少系统调用次数。
/// 本地磁盘 64KB 已足够（单次系统调用微秒级延迟）。
/// SMB/网络路径可考虑更大值以减少网络往返，但需权衡 TLS 内存占用。
const BUFFER_SIZE: usize = 64 * 1024;

// 线程本地缓冲区，在 rayon worker 线程间复用 64KB 堆分配。
thread_local! {
    static TLS_BUFFER: std::cell::RefCell<Option<Vec<u8>>> = std::cell::RefCell::new(None);
}

/// FILE_ID_BOTH_DIR_INFO 的文件信息类编号。
const FILE_ID_BOTH_DIR_INFO_CLASS: u32 = 37;

/// NtQueryDirectoryFileEx 返回 "没有更多文件" 的状态码。
const STATUS_NO_MORE_FILES: i32 = -2147483642; // 0x80000006

/// FILE_ATTRIBUTE_DIRECTORY flag value.
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

// ── FFI 结构体 ──────────────────────────────────────────────────────────

/// NtQueryDirectoryFileEx 返回的目录条目信息结构。
/// 对应 Windows FILE_ID_BOTH_DIR_INFO。
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

/// NT I/O 状态块，NtQueryDirectoryFileEx 的输出参数。
#[repr(C)]
#[allow(non_snake_case)]
struct IO_STATUS_BLOCK {
    Status: i32,
    Information: usize,
}

/// NT UNICODE_STRING，用于 OBJECT_ATTRIBUTES 中的文件名。
/// 注意：Length 和 MaximumLength 以字节为单位（不含 null terminator）。
#[repr(C)]
#[allow(non_snake_case)]
struct UNICODE_STRING {
    Length: u16,
    MaximumLength: u16,
    Buffer: *mut u16,
}

/// NT OBJECT_ATTRIBUTES，用于 NtQueryInformationByName。
/// RootDirectory 为已打开的目录句柄，ObjectName 为相对文件名。
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

/// NtQueryInformationByName 返回的文件统计信息 (class 68)。
/// Win10 1709+ 可用。包含 FileId + NumberOfLinks + 时间戳 + 文件属性，
/// 无需打开文件句柄即可查询。
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

/// FILE_STAT_INFORMATION 的信息类编号 (class 68)。
/// Win10 1709+ 可用。
const FILE_STAT_INFORMATION_CLASS: u32 = 68;

/// OBJECT_ATTRIBUTES 的 OBJ_CASE_INSENSITIVE 标志。
const OBJ_CASE_INSENSITIVE: u32 = 0x40;

// ── 动态加载 ntdll ─────────────────────────────────────────────────────

/// NtQueryDirectoryFileEx 函数签名。
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

/// RtlNtStatusToDosError 函数签名，用于将 NTSTATUS 转换为 Win32 错误码。
type RtlNtStatusToDosErrorFn = unsafe extern "system" fn(status: i32) -> u32;

/// NtQueryInformationByName 函数签名 (Win10 1709+)。
/// 使用 RootDirectory + FileName 查询文件信息，无需打开文件句柄。
#[allow(non_snake_case)]
type NtQueryInformationByNameFn = unsafe extern "system" fn(
    ObjectAttributes: *mut OBJECT_ATTRIBUTES,
    IoStatusBlock: *mut IO_STATUS_BLOCK,
    FileInformation: *mut std::ffi::c_void,
    Length: u32,
    FileInformationClass: u32,
) -> i32;

/// 从 ntdll.dll 动态加载的函数指针集合。
struct NtDllFuncs {
    query_dir: NtQueryDirectoryFileExFn,
    status_to_win32: RtlNtStatusToDosErrorFn,
    /// NtQueryInformationByName (Win10 1709+)。Option 因为老版本 Windows 不存在。
    query_by_name: Option<NtQueryInformationByNameFn>,
}

/// 懒加载 ntdll 函数指针。失败时 panic（首次调用时初始化，通常在用户首次调用 into_iter() 时触发）。
///
/// 设计决策：panic 是有意的 fail-fast——ntdll.dll 是 Windows 进程级核心 DLL，
/// 加载失败意味着系统安装损坏或安全软件干预，进程无法继续正常运行。
/// 改用 Result 传播只会把不可避免的崩溃推迟到调用方，无实际收益。
///
/// LoadLibraryW 增加了 ntdll.dll 的引用计数但未调用 FreeLibrary。
/// 这是有意为之——ntdll.dll 永远不会被卸载，函数指针需要在整个进程生命周期内保持有效。
fn ntdll_funcs() -> &'static NtDllFuncs {
    use std::sync::OnceLock;
    static FUNCS: OnceLock<NtDllFuncs> = OnceLock::new();
    FUNCS.get_or_init(|| unsafe {
        // SAFETY:
        // 1. LoadLibraryW("ntdll.dll") 安全——ntdll.dll 是 Windows 系统核心 DLL，始终存在
        // 2. GetProcAddress 获取的函数指针签名由上面的 type alias 保证与 NT API 文档一致
        // 3. transmute 将 raw 指针转为类型化函数指针，类型由 NtQueryDirectoryFileExFn
        //    和 RtlNtStatusToDosErrorFn 的签名定义保证正确
        let ntdll_name = to_wide_null("ntdll.dll\0");
        let module = LoadLibraryW(ntdll_name.as_ptr());
        if module.is_null() {
            panic!("nt_dir_enum: 无法加载 ntdll.dll");
        }

        let query_dir_ptr = GetProcAddress(module, b"NtQueryDirectoryFileEx\0".as_ptr() as *const i8);
        let status_to_win32_ptr = GetProcAddress(module, b"RtlNtStatusToDosError\0".as_ptr() as *const i8);

        if query_dir_ptr.is_null() || status_to_win32_ptr.is_null() {
            panic!("nt_dir_enum: 无法获取 ntdll 函数地址");
        }

        // NtQueryInformationByName (Win10 1709+)：可选加载，老版本 Windows 不存在
        let query_by_name_ptr = GetProcAddress(module, b"NtQueryInformationByName\0".as_ptr() as *const i8);
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

// ── RAII 句柄守卫 ──────────────────────────────────────────────────────

/// RAII 守卫，Drop 时自动 CloseHandle。用于 CreateFileW 返回的句柄。
pub(crate) struct HandleGuard(HANDLE);

impl HandleGuard {
    /// 返回内部句柄，用于批量查询等操作。
    pub(crate) fn handle(&self) -> HANDLE {
        self.0
    }
}

impl Drop for HandleGuard {
    fn drop(&mut self) {
        // SAFETY: HandleGuard 仅包装 CreateFileW 返回的有效句柄，
        // Drop 时调用 CloseHandle 是安全的。
        unsafe {
            CloseHandle(self.0);
        }
    }
}

// ── 公共结构体 ─────────────────────────────────────────────────────────

/// NtQueryDirectoryFileEx 返回的目录条目信息。
#[derive(Debug)]
pub struct DirEntryInfo {
    pub file_name: OsString,
    pub file_attributes: u32,
    pub file_size: u64,
    pub creation_time: i64,
    pub last_write_time: i64,
    pub last_access_time: i64,
    pub file_id: u64,
    /// 硬链接数。NT API 批量枚举不提供此字段，
    /// 需通过 NtQueryInformationByName + FileStatInformation (class 68) 查询。
    /// Win10 1709+ 可用；老版本 Windows 或查询失败时为 None。
    pub number_of_links: Option<u32>,
    /// 卷序列号。通过 GetVolumeInformationByHandleW 从目录句柄获取一次。
    /// 所有条目共享同一值（同一目录内的文件属于同一卷）。
    pub volume_serial_number: Option<u32>,
}

// ── 辅助函数 ───────────────────────────────────────────────────────────

/// 将字节字符串转为 null-terminated UTF-16 Vec（用于 LoadLibraryW/GetProcAddress）。
fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 将 OS 路径转为 null-terminated UTF-16 Vec（用于 CreateFileW 等宽字符 API）。
fn path_to_widestring(path: &Path) -> Vec<u16> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    wide
}

/// 将 NTSTATUS 转换为 io::Error。
fn ntstatus_to_io_error(status: i32, funcs: &NtDllFuncs) -> io::Error {
    // SAFETY: RtlNtStatusToDosError 是稳定的 Windows API，输入任意 i32 均安全。
    let win32_err = unsafe { (funcs.status_to_win32)(status) };
    io::Error::from_raw_os_error(win32_err as i32)
}

// ── 核心枚举函数 ───────────────────────────────────────────────────────

/// 打开目录句柄，返回 RAII 守卫。
///
/// SAFETY: CreateFileW 参数合法，FILE_FLAG_BACKUP_SEMANTICS 允许打开目录。
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

/// 解析单次 NtQueryDirectoryFileEx 返回的 buffer，提取目录条目。
///
/// 对每个非 "."/".." 条目调用 `on_entry` 回调。
///
/// SAFETY: 调用者必须保证 buffer 包含有效的 NT API 返回数据。
fn parse_buffer_entries(
    buffer: &[u8],
    mut on_entry: impl FnMut(&FILE_ID_BOTH_DIR_INFO, OsString),
) {
    let mut offset: usize = 0;
    let buf_len = buffer.len();
    loop {
        // 边界检查：确保能读取完整的 entry 头部（含变长 FileName 的第一个元素）
        // 内核返回的数据通常是可信的，此检查防御内存损坏或异常 NextEntryOffset
        if offset + std::mem::size_of::<FILE_ID_BOTH_DIR_INFO>() > buf_len {
            break;
        }

        // SAFETY: 边界检查已保证 offset..offset+size_of 在 buffer 内。
        // FILE_ID_BOTH_DIR_INFO may appear at unaligned offsets (NT only guarantees
        // 4-byte alignment, but the struct has u64 fields requiring 8-byte alignment).
        let entry_ptr = unsafe { buffer.as_ptr().add(offset) };
        let entry = unsafe { std::ptr::read_unaligned(entry_ptr as *const FILE_ID_BOTH_DIR_INFO) };

        let name_len = entry.FileNameLength as usize;
        let name_chars = name_len / 2;

        // 边界检查：确保 FileName 变长数据完全在 buffer 内
        let name_start_offset = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
        if offset + name_start_offset + name_len > buf_len {
            break;
        }

        // Raw UTF-16 比较 "." 和 ".."，避免 to_string_lossy 堆分配
        // FileName 是 C 变长数组 ([u16; 1])，必须用指针访问避免编译器越界检查
        let first_char = entry.FileName[0];
        let is_dot = name_chars == 1 && first_char == b'.' as u16;
        let is_dotdot = name_chars == 2
            && first_char == b'.' as u16
            // SAFETY: name_chars == 2 guarantees at least 2 u16 elements in FileName
            && unsafe { *entry.FileName.as_ptr().add(1) } == b'.' as u16;

        if !is_dot && !is_dotdot {
            // SAFETY: 边界检查已保证 FileName 的 name_len 字节在 buffer 内
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

/// 核心枚举循环：NtQueryDirectoryFileEx 批量查询 + buffer 解析。
///
/// `on_entry` 对每个非 "."/".." 条目调用，接收解析出的原始 entry 和文件名。
/// 调用者负责构造 `DirEntryInfo` 并决定是否收集/分发。
fn enumerate_dir_core(
    guard: &HandleGuard,
    funcs: &NtDllFuncs,
    on_entry: impl FnMut(&FILE_ID_BOTH_DIR_INFO, OsString),
) -> io::Result<()> {
    let mut buffer = TLS_BUFFER.with(|b| {
        b.borrow_mut().take().unwrap_or_else(|| vec![0u8; BUFFER_SIZE])
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

        // SAFETY: handle 是有效的目录句柄，buffer 大小充足，
        // FILE_ID_BOTH_DIR_INFO_CLASS 是合法的信息类。
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

        // 安全：iosb.Information 包含 buffer 中有效数据的字节数
        let bytes_returned = iosb.Information;
        if bytes_returned == 0 {
            break;
        }

        parse_buffer_entries(buffer, &mut on_entry);
    }

    Ok(())
}

/// 使用 NtQueryDirectoryFileEx 批量枚举目录中的所有文件和子目录。
///
/// 返回除 "." 和 ".." 外的所有条目，以及保持打开的目录句柄。
/// 目录句柄用于后续的 ext info 批量查询（NtQueryInformationByName），
/// 句柄在 HandleGuard drop 时自动关闭。
/// 使用 64KB 缓冲区减少系统调用次数，并预分配 Vec 以减少堆重分配。
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

/// 使用 NtQueryDirectoryFileEx 批量枚举，并对每个子目录调用回调实现流式分发。
///
/// 子目录通过回调立即分发（无需等待完整枚举），所有条目（文件和子目录）
/// 仍然收集到返回的 Vec 中，不遗漏任何条目。
/// 返回保持打开的目录句柄，用于后续 ext info 批量查询。
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

// ── Ext info 批量查询 ────────────────────────────────────────────────

/// 使用 NtQueryInformationByName + FileStatInformation (class 68) 查询
/// 目录条目的 NumberOfLinks。
///
/// 利用已打开的目录句柄 + 相对文件名查询，无需逐文件打开句柄，
/// 消除 symlink_metadata 的路径解析开销。
///
/// 单线程遍历：NTFS 内核对同卷 MFT 查询有卷级互斥锁，多线程并发查同一卷
/// 没有加速效果，反而有线程调度开销（实测 rayon par_iter_mut 8 线程 5.1s >
/// 单线程 iter_mut 3.5s）。
///
/// 线程安全：
/// - HANDLE 是内核对象引用，多线程并发读操作（NtQueryInformationByName）
///   是安全的——内核内部维护引用计数，CloseHandle 由 HandleGuard Drop 保证
///   在所有查询完成后才执行。
/// - 每次查询独立构造 UNICODE_STRING + OBJECT_ATTRIBUTES + stat_info_buf，
///   无共享可变状态，无竞锁风险。
///
/// Win10 1709+ 可用。老版本 Windows 或查询失败时 number_of_links 保持 None。
///
/// `query_file_nlinks`: 是否对普通文件也查询 NumberOfLinks。
/// 设为 false 时仅对目录条目（FILE_ATTRIBUTE_DIRECTORY）查询，
/// 文件条目 number_of_links 保持 None，减少约 50% 查询量。
pub fn batch_query_nlinks(
    entries: &mut [DirEntryInfo],
    dir_handle: &HandleGuard,
    query_file_nlinks: bool,
) {
    let funcs = ntdll_funcs();
    let query_by_name = match funcs.query_by_name {
        Some(f) => f,
        None => return, // NtQueryInformationByName 不可用（老 Windows），保持 None
    };

    // 单线程遍历：NTFS 内核对同卷 MFT 查询有卷级互斥锁，
    // 多线程并发查同一卷没有加速效果，反而有线程调度开销。
    // 参见：batch_query_nlinks 性能分析（rayon par_iter_mut 8线程 5.1s > 单线程 3.5s）
    let root_handle_raw = dir_handle.handle() as isize;
    // UTF-16 文件名缓冲区，跨条目复用避免 per-entry 堆分配。
    // 260 容量覆盖 MAX_PATH，文件名超过时 Vec 自动扩容。
    let mut wide_buf: Vec<u16> = Vec::with_capacity(260);
    entries.iter_mut().for_each(|entry| {
        // 条件过滤：仅目录条目需要查询 NumberOfLinks（除非 query_file_nlinks=true）
        if !query_file_nlinks && (entry.file_attributes & FILE_ATTRIBUTE_DIRECTORY) == 0 {
            return; // 非目录条目，跳过查询
        }

        wide_buf.clear();
        wide_buf.extend(entry.file_name.encode_wide());
        if wide_buf.is_empty() {
            return;
        }

        // 每次迭代独立的 FILE_STAT_INFORMATION 缓冲区（栈上 72 字节）
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
        // 1. obj_attrs 由 root_handle + 相对文件名构成，NtQueryInformationByName
        //    使用 RootDirectory + ObjectName 定位文件，无需打开文件句柄
        // 2. stat_info_buf 大小 = size_of::<FILE_STAT_INFORMATION>() = 72 字节
        // 3. FILE_STAT_INFORMATION_CLASS = 68 是合法的信息类（Win10 1709+）
        // 4. unicode_str.Buffer 指向 wide_buf，在本函数作用域内有效
        // 5. root_handle 是内核句柄，单线程读操作安全
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
            // SAFETY: NtQueryInformationByName 成功时，缓冲区包含有效的 FILE_STAT_INFORMATION
            let stat_info = unsafe {
                std::ptr::read_unaligned(stat_info_buf.as_ptr() as *const FILE_STAT_INFORMATION)
            };
            entry.number_of_links = Some(stat_info.NumberOfLinks);
        }
        // 查询失败时保持 number_of_links = None
    });
}

/// 一次 GetVolumeInformationByHandleW 调用同时获取文件系统类型和卷序列号。
///
/// 合并了原先的 detect_fs_type + query_volume_serial 两次调用，
/// 消除冗余的 GetVolumeInformationByHandleW 系统调用。
/// 失败时返回 (FsType::Unknown, None)。
pub fn detect_fs_type_and_vol_serial(dir_handle: &HandleGuard) -> (FsType, Option<u32>) {
    let mut fs_name_buf = [0u16; 16]; // "NTFS"=4, "FAT32"=5, "exFAT"=5, 加 null 足够
    let mut volume_serial: u32 = 0;
    let success = unsafe {
        GetVolumeInformationByHandleW(
            dir_handle.handle(),
            ptr::null_mut(),               // lpVolumeNameBuffer
            0,                             // nVolumeNameLength
            &mut volume_serial,            // lpVolumeSerialNumber
            ptr::null_mut(),               // lpMaximumComponentLength
            ptr::null_mut(),               // lpFileSystemFlags
            fs_name_buf.as_mut_ptr(),      // lpFileSystemNameBuffer
            fs_name_buf.len() as u32,      // nFileSystemNameLength
        )
    };

    if success == 0 {
        return (FsType::Unknown, None);
    }

    // 找到 null terminator 的位置
    let len = fs_name_buf.iter().position(|&c| c == 0).unwrap_or(fs_name_buf.len());
    let fs_name = OsString::from_wide(&fs_name_buf[..len]);
    // ASCII 大写比较（Windows 文件系统名称始终为 ASCII 大写）
    let fs_type = match fs_name.to_string_lossy().as_ref() {
        "NTFS" => FsType::Ntfs,
        "ReFS" => FsType::Refs,
        "FAT32" => FsType::Fat32,
        "exFAT" => FsType::ExFat,
        "FAT" => FsType::Fat32,   // FAT12/FAT16 也无硬链接
        "FAT16" => FsType::Fat32, // 同上
        _ => FsType::Unknown,
    };

    (fs_type, Some(volume_serial))
}

// ── 文件系统类型检测 ────────────────────────────────────────────────────

/// Windows 文件系统类型，用于决定 number_of_links 查询策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsType {
    /// NTFS — 支持硬链接，需要逐文件查询 NumberOfLinks
    Ntfs,
    /// ReFS — 支持硬链接，需要逐文件查询 NumberOfLinks
    Refs,
    /// FAT32 — 不支持硬链接，NumberOfLinks 恒为 1
    Fat32,
    /// exFAT — 不支持硬链接，NumberOfLinks 恒为 1
    ExFat,
    /// 未知文件系统 — 保守策略，逐文件查询
    Unknown,
}

impl FsType {
    /// FAT 系列（FAT32/exFAT）不支持硬链接，NumberOfLinks 恒为 1。
    /// 可以安全跳过 batch_query_nlinks，直接设 nlink=1。
    #[inline]
    pub fn is_fat_family(self) -> bool {
        matches!(self, FsType::Fat32 | FsType::ExFat)
    }
}


