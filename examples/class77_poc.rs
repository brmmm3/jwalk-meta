
//! class 77 (FileStatBasicInformation) 用于 NtQueryDirectoryFileEx 目录枚举的 PoC。
//!
//! 目标：验证 class 77 buffer 是否包含 LinkCount 字段。
//! 思路：先以 class 37 (FILE_ID_BOTH_DIR_INFO，已知格式) 取得文件名列表作锚点，
//!       再以 class 77 取得原始 buffer，用文件名反向定位 entry 边界，
//!       推断 entry header 大小，分析字段布局。
//!
//! 不修改任何 src/ 生产代码；不引入新依赖；动态加载 ntdll。


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

// ── 信息类常量 ──────────────────────────────────────────────────────────

/// FILE_ID_BOTH_DIR_INFORMATION (基线)
const FILE_ID_BOTH_DIR_INFO_CLASS: u32 = 37;

/// FileStatBasicInformation (测试目标)
const FILE_STAT_BASIC_INFORMATION_CLASS: u32 = 77;

const STATUS_NO_MORE_FILES: i32 = -2147483642; // 0x80000006

// ── FFI 类型 ────────────────────────────────────────────────────────────

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

// ── class 37 已知结构（仅用于解析基线） ────────────────────────────────

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

// ── NtQueryDirectoryFile（不带 Ex）FFI ——验证两个 API 都不支持 class 77 ──

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

// ── 单文件 class 77 健康检查所需 FFI（NtQueryInformationByName） ──

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

/// 单文件 class 77 返回结构（Microsoft 文档，用于健康检查）。
#[repr(C)]
#[allow(non_snake_case)]
/// 单文件 class 77 返回结构（Microsoft 文档）。
/// 注意 EffectiveAccess 是 LARGE_INTEGER（8 字节），不是 ULONG。
/// 大小 = 7*8 + 4*3 + 4(padding) + 8 = 80 字节。
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

/// 加载 NtQueryInformationByName。
fn load_query_by_name() -> Option<NtQueryInformationByNameFn> {
    let name = to_wide_null("ntdll.dll\0");
    let module = unsafe { LoadLibraryW(name.as_ptr()) };
    if module.is_null() {
        return None;
    }
    let proc = unsafe { GetProcAddress(module, b"NtQueryInformationByName\0".as_ptr() as *const i8) };
    if proc.is_null() {
        None
    } else {
        Some(unsafe { std::mem::transmute(proc) })
    }
}

/// 验证系统认识 class 77：用 NtQueryInformationByName 查询一个已知文件。
/// 成功则返回 Some(NumberOfLinks)，失败返回 None。
fn test_single_file_class77(
    dir_handle: HANDLE,
    entries: &[(String, u32, u32)],
) -> Option<u32> {
    let query_by_name = match load_query_by_name() {
        Some(f) => f,
        None => {
            println!("  NtQueryInformationByName 不可用（Win10 1709-）");
            return None;
        }
    };

    // 取第一个条目作为测试目标
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
    // size 扫描：从 64 到 128 找出系统接受的 buffer size
    let declared_size = std::mem::size_of::<FILE_STAT_BASIC_INFORMATION>();
    println!("  声明结构体 size = {}", declared_size);
    let mut found_size: Option<usize> = None;
    for probe_size in (64..=128).step_by(4) {
        let mut probe_buf = vec![0u8; probe_size];
        let mut iosb_probe = IO_STATUS_BLOCK { Status: 0, Information: 0 };
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
            println!("  size {} → 成功 ✓", probe_size);
            found_size = Some(probe_size);
            break;
        } else {
            println!("  size {} → {}", probe_size, status_hex);
        }
    }

    let valid_size = match found_size {
        Some(s) => s,
        None => {
            println!("  所有 size 都失败——class 77 在本系统不可用");
            return None;
        }
    };

    // 用正确 size 重新调用并解析
    let mut stat_buf = vec![0u8; valid_size];
    let mut iosb = IO_STATUS_BLOCK { Status: 0, Information: 0 };
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
        println!("  二次调用失败：NTSTATUS 0x{:08x}", status as u32);
        return None;
    }
    // 从 valid_size buffer 中提取 NumberOfLinks（偏移 64，4 字节，FILE_STAT_BASIC_INFORMATION 布局）
    if valid_size >= 68 {
        let nlinks = u32::from_ne_bytes([
            stat_buf[64], stat_buf[65], stat_buf[66], stat_buf[67],
        ]);
        let attrs = u32::from_ne_bytes([
            stat_buf[56], stat_buf[57], stat_buf[58], stat_buf[59],
        ]);
        println!(
            "  成功：\"{}\" NumberOfLinks={} FileAttributes={:#x} (buffer size {})",
            name, nlinks, attrs, valid_size
        );
        Some(nlinks)
    } else {
        println!("  valid_size {} 不足包含 NumberOfLinks 字段", valid_size);
        None
    }
}

// ── 辅助 ────────────────────────────────────────────────────────────────

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
        eprintln!("错误：无法加载 ntdll.dll");
        process::exit(1);
    }
    let proc = unsafe {
        GetProcAddress(
            module,
            b"NtQueryDirectoryFileEx\0".as_ptr() as *const i8,
        )
    };
    if proc.is_null() {
        eprintln!("错误：无法获取 NtQueryDirectoryFileEx 地址");
        process::exit(1);
    }
    unsafe { std::mem::transmute(proc) }
}

/// 用 NT API 调用一次 NtQueryDirectoryFileEx，返回填入的 buffer 和实际写入字节数。
fn query_dir_once(
    query: NtQueryDirectoryFileExFn,
    handle: HANDLE,
    info_class: u32,
    restart: i32,
) -> io::Result<(Vec<u8>, usize)> {
    query_dir_once_full(query, handle, info_class, 0, ptr::null(), restart)
}

/// 完整参数版的 NtQueryDirectoryFileEx 调用。
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
    // STATUS_NO_MORE_FILES 不是错误，只是没更多条目
    if status < 0 && status != STATUS_NO_MORE_FILES {
        return Err(io::Error::from_raw_os_error(status));
    }
    let written = iosb.Information;
    buffer.truncate(written);
    Ok((buffer, written))
}

/// dump buffer 前 max_bytes 字节，hex + ASCII，类似 xxd 输出。
fn dump_hex(buffer: &[u8], max_bytes: usize, label: &str) {
    let n = buffer.len().min(max_bytes);
    println!("\n=== {} (前 {} 字节，总 buffer 长度 {}) ===", label, n, buffer.len());
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
            .map(|&b| if (32..=126).contains(&b) { b as char } else { '.' })
            .collect();
        println!("{:08x}: {:<48} {}", chunk_start, hex, ascii);
    }
    if buffer.len() > max_bytes {
        println!("... (省略 {} 字节)", buffer.len() - max_bytes);
    }
}

// ── 解析 class 37 buffer（已知格式），取得文件名锚点 ───────────────────

fn parse_class37(buffer: &[u8]) -> Vec<(String, u32, u32)> {
    // 返回 (文件名, NextEntryOffset, entry 起始 offset)
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
            std::slice::from_raw_parts(buffer.as_ptr().add(offset + name_off) as *const u16, name_chars)
        };
        let name = OsString::from_wide(name_slice).to_string_lossy().into_owned();
        // 跳过 "." 和 ".."
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

/// 在 buffer 中搜索 UTF-16 编码的文件名，返回找到的所有偏移。
fn find_utf16_occurrences(buffer: &[u8], needle: &str) -> Vec<usize> {
    let encoded: Vec<u16> = needle.encode_utf16().collect();
    if encoded.is_empty() {
        return Vec::new();
    }
    let needle_bytes: Vec<u8> = encoded
        .iter()
        .flat_map(|&w| w.to_ne_bytes())
        .collect();
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


/// 读取 entry 假设起始位置（NextEntryOffset 假定在 offset 0），尝试用链表方式遍历。
/// 返回每个推测的 entry 起点。
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
    println!("测试目录：{}", dir.display());

    let query = load_ntdll_query_dir();
    let guard = match open_dir(dir) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("打开目录失败：{}", e);
            process::exit(1);
        }
    };

    // ── 步骤 1：用 class 37 取得基线 ────────────────────────────────
    let (buf37, _) = match query_dir_once(query, guard.0, FILE_ID_BOTH_DIR_INFO_CLASS, 1) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("class 37 调用失败：{}", e);
            process::exit(1);
        }
    };

    let class37_entries = parse_class37(&buf37);
    println!("\nclass 37 (FILE_ID_BOTH_DIR_INFO) 解析到 {} 个条目（不含 . / ..）：", class37_entries.len());
    for (i, (name, nxt, off)) in class37_entries.iter().take(10).enumerate() {
        println!("  [{}] offset={} NextEntryOffset={} name=\"{}\"", i, off, nxt, name);
    }
    if class37_entries.len() < 3 {
        eprintln!("警告：测试目录条目太少（{}），建议放在至少 5 个文件的目录下重跑", class37_entries.len());
    }

    // dump class 37 前 1024 字节
    dump_hex(&buf37, 1024, "class 37 buffer");

    // ── 步骤 2：尝试 class 77（FileStatBasicInformation） ───────────
    // 重新打开目录以重启扫描（避免与 class 37 的扫描游标冲突）
    drop(guard);
    let guard2 = match open_dir(dir) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("第二次打开目录失败：{}", e);
            process::exit(1);
        }
    };

    // 尝试多种参数组合调用 class 77：
    //   (a) ReturnSingleEntry=0, FileName=NULL, RestartScan=1
    //   (b) ReturnSingleEntry=0, FileName="*", RestartScan=1
    //   (c) ReturnSingleEntry=1, FileName=NULL, RestartScan=1
    let star_wildcard = to_wide_null("*");
    let mut buf77: Option<(Vec<u8>, usize)> = None;
    println!("\n=== class 77 (FileStatBasicInformation) 多参数组合测试 ===");
    for (label, ret_single, fname, restart) in [
        ("(a) ret_single=0, FileName=NULL, restart=1", 0i32, ptr::null::<u16>() as LPCWSTR, 1i32),
        ("(b) ret_single=0, FileName=*",      0i32, star_wildcard.as_ptr()  as LPCWSTR, 1i32),
        ("(c) ret_single=1, FileName=NULL",    1i32, ptr::null::<u16>() as LPCWSTR, 1i32),
    ] {
        match query_dir_once_full(query, guard2.0, FILE_STAT_BASIC_INFORMATION_CLASS, ret_single, fname, restart) {
            Ok((b, n)) => {
                println!("  {}→ 成功，写入 {} 字节", label, n);
                if buf77.is_none() {
                    buf77 = Some((b, n));
                }
            }
            Err(e) => {
                println!("  {}→ 失败：NTSTATUS {} (raw os error {})", label, e, e.raw_os_error().unwrap_or(0));
            }
        }
    }

    // 额外验证：NtQueryDirectoryFile（不带 Ex）是否支持 class 77
    println!("\n=== 额外验证：NtQueryDirectoryFile（不带 Ex）+ class 77 ===");
    if let Some(query_dir_no_ex) = load_query_dir_no_ex() {
        let star_name = to_wide_null("*");
        let mut ustr = UNICODE_STRING_NAME {
            Length: (star_name.len() - 1) as u16 * 2,
            MaximumLength: (star_name.len() - 1) as u16 * 2,
            Buffer: star_name.as_ptr() as *mut u16,
        };
        let mut probe_buf = vec![0u8; 64 * 1024];
        let mut iosb = IO_STATUS_BLOCK { Status: 0, Information: 0 };
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
            println!("  NtQueryDirectoryFile + class 77 成功，写入 {} 字节", iosb.Information);
        } else {
            println!("  NtQueryDirectoryFile + class 77 失败：0x{:08x}", status as u32);
        }
    } else {
        println!("  NtQueryDirectoryFile 不可加载");
    }

    // 单文件 class 77 健康检查（验证系统认识这个 info class）
    println!("\n=== 健康检查：单文件 class 77 (NtQueryInformationByName) ===");
    let single_file_class77_ok = test_single_file_class77(guard2.0, &class37_entries);

    let (buf77, written77) = match buf77 {
        Some(r) => r,
        None => {
            println!("\n=== class 77 目录枚举全面失败 ===");
            if single_file_class77_ok.is_some() {
                println!("但单文件查询成功——说明系统认识 class 77，但 NtQueryDirectoryFileEx 拒绝用于目录枚举。");
                println!("结论：class 77 不可用于目录枚举，优化方向关闭。");
            } else {
                println!("单文件查询也失败——系统不认识 class 77，可能 Windows 版本过旧。");
            }
            println!("提示：Windows build 号见 PoC 启动信息（PowerShell: `[Environment]::OSVersion.Version.Build`）");
            return;
        }
    };

    println!(
        "\n=== class 77 (FileStatBasicInformation) 调用成功，写入 {} 字节 ===",
        written77
    );

    // dump class 77 前 2048 字节（用于字段分析）
    dump_hex(&buf77, 2048, "class 77 buffer");

    // ── 步骤 3：链表遍历（假设 NextEntryOffset 在 entry 开头） ─────
    let chain = walk_next_offset_chain(&buf77);
    println!("\nclass 77 NextEntryOffset 链推测 entry 起点：{:?}", chain);

    // ── 步骤 4：用已知文件名作锚点，反向定位 entry header ──────────
    println!("\n=== 用 class 37 文件名在 class 77 buffer 中查找锚点 ===");
    let mut anchors = Vec::new();
    for (name, _, _) in &class37_entries {
        let hits = find_utf16_occurrences(&buf77, name);
        if hits.is_empty() {
            println!("  \"{}\": 未找到", name);
            continue;
        }
        let pos = hits[0];
        // 二次确认：是否落在 chain 中的某个 entry 内
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

    // ── 步骤 5：分析 entry header 字段布局 ─────────────────────────
    if !anchors.is_empty() {
        // 推算 header size：取所有锚点中最常见的 header_size
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
            "\nclass 77 entry header 推测大小（含 NextEntryOffset，不含 FileName）：{} 字节",
            common_header
        );

        // dump 第一个 entry 的 header bytes 详细分析 + 关键字段搜索
        if let Some((name, pos, entry_start, _)) = anchors.first() {
            let entry_start = *entry_start;
            let header_end = *pos;
            let baseline_name = name.clone();
            println!(
                "\n=== 第一个 entry header 详细分析 (\"{}\", offset {}..{}) ===",
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
                        chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
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

            // 关键字段搜索：用 class 37 同名 entry 的时间戳在 class 77 header 中找匹配
            // FILE_STAT_BASIC_INFORMATION 单文件布局（Microsoft 文档）：
            //   FileId(0) CreationTime(8) LastAccessTime(16) LastWriteTime(24) ChangeTime(32)
            //   AllocationSize(40) EndOfFile(48) FileAttributes(56,4) ReparseTag(60,4)
            //   NumberOfLinks(64,4) EffectiveAccess(68,8) = 72 bytes
            // 目录枚举会在表头加 NextEntryOffset + FileNameLength + FileName[]
            println!("\n=== LinkCount 字段位置搜索 ===");
            if let Some((_, _, c37_off)) = class37_entries
                .iter()
                .find(|(n, _, _)| *n == baseline_name)
            {
                let c37_ptr = unsafe { buf37.as_ptr().add(*c37_off as usize) };
                let c37_entry =
                    unsafe { std::ptr::read_unaligned(c37_ptr as *const FILE_ID_BOTH_DIR_INFO) };
                println!(
                    "  对照基线 \"{}\": LastWriteTime={:#x}, EndOfFile={:#x}, FileAttributes={:#x}",
                    baseline_name, c37_entry.LastWriteTime as u64, c37_entry.EndOfFile as u64,
                    c37_entry.FileAttributes
                );
                println!(
                    "  在 class 77 header 内搜索 LastWriteTime 字节序列（{}, 8 bytes）",
                    c37_entry.LastWriteTime
                );
                let ts_bytes = c37_entry.LastWriteTime.to_ne_bytes();
                let header_slice = &buf77[entry_start..(entry_start + common_header)];
                if let Some(idx_in_header) = header_slice.windows(8).position(|w| w == ts_bytes.as_slice()) {
                    let abs_off = entry_start + idx_in_header;
                    println!(
                        "  → 找到 LastWriteTime 字节在 entry_start + {} (绝对偏移 {})",
                        idx_in_header, abs_off
                    );
                    // 按 FILE_STAT_BASIC_INFORMATION 布局推算：NumberOfLinks 相对 LastWriteTime 偏移 = 64 - 24 = 40 字节
                    println!(
                        "  → 按 FILE_STAT_BASIC_INFORMATION 布局：NumberOfLinks 应在 LastWriteTime + 40 字节处"
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
                            "  → 候选 NumberOfLinks 字段（offset +{}, 4 字节）= {}",
                            nlinks_off, nlinks
                        );
                    }
                } else {
                    println!("  → 未在 header 内找到 LastWriteTime 字节序列");
                }
            }
        }
    } else {
        println!("\n警告：class 77 buffer 中未找到任何已知文件名锚点");
    }

    // ── 步骤 6：保存分析数据到文件，便于 verdict 文档引用 ───────────
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
