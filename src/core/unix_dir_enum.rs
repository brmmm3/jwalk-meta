//! Linux getdents64 直调枚举。
//!
//! 替代 `std::fs::read_dir`，单次系统调用获取多条目，并支持流式子目录分发。
//! 仅 Linux 编译：macOS 用 `getdirentries64`（签名不同），其他 Unix 不一定支持。
//!
//! # 核心要点
//!
//! - 64KB thread_local 缓冲区复用，与 Windows `TLS_BUFFER` 模式对齐
//! - `linux_dirent64::d_type` 短路判断 dir/file/symlink，避免 `fstatat`（仅 DT_UNKNOWN fallback）
//! - `fstatat(dirfd, name, AT_SYMLINK_NOFOLLOW)` 替代 `fs::metadata`，省路径解析
//! - 返回 `DirFdGuard` 保留 fd，供后续 `fstatat` 使用

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::path::Path;

// Linux getdents64 返回的目录条目（ABI 布局，不依赖 libc 是否导出 linux_dirent64）。
// 与 linux_dirent64 ABI 严格对应：d_ino(8) | d_off(8) | d_reclen(2) | d_type(1) | d_name[NUL].
#[repr(C)]
struct LinuxDirent64 {
    d_ino: u64,
    d_off: u64,
    d_reclen: u16,
    d_type: u8,
    // d_name 变长 NUL 终止字节串，起始偏移 = 19（由 kernel ABI 固定，不由 Rust 结构计算）
}

/// d_name 在 LinuxDirent64 中的固定偏移（kernel ABI 保证，不随编译器对齐策略变化）。
const DIRENT_NAME_OFFSET: usize = 19;

/// getdents64 缓冲区大小（64KB）。
/// 单次 syscall 在大目录上可返回数百到数千条目，与 Windows TLS_BUFFER 对齐。
const DENTS_BUF_SIZE: usize = 64 * 1024;

// thread_local 缓冲区，避免每次枚举分配 64KB。
thread_local! {
    static TLS_BUFFER: std::cell::RefCell<Vec<u8>> =
        std::cell::RefCell::new(vec![0u8; DENTS_BUF_SIZE]);
}

/// 借用 TLS buffer（不足 64KB 时 resize）。
fn with_tls_buffer<R>(f: impl FnOnce(&mut [u8]) -> R) -> R {
    TLS_BUFFER.with(|cell| {
        let mut buf = cell.borrow_mut();
        if buf.len() < DENTS_BUF_SIZE {
            buf.resize(DENTS_BUF_SIZE, 0);
        }
        f(&mut buf[..DENTS_BUF_SIZE])
    })
}

/// 目录 fd 守卫。Drop 时自动 `close`，确保 fd 不泄漏。
pub struct DirFdGuard(RawFd);

impl DirFdGuard {
    #[inline]
    pub fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl Drop for DirFdGuard {
    fn drop(&mut self) {
        // SAFETY: 构造时 fd >= 0；close 不会 panic，错误也无从恢复。
        unsafe {
            libc::close(self.0);
        }
    }
}

/// 已收集的目录条目（拥有化，可越过 buffer 复用）。
pub struct LinuxDirEntryOwned {
    pub d_type: u8,
    pub name: OsString,
    /// io_uring 批量 STATX 预取结果（NetworkAsync 路径填充）。
    /// LocalSync / 批量失败 / CQE 错误时为 None，调用方走 fstatat fallback。
    #[cfg(all(target_os = "linux", target_env = "gnu", not(feature = "legacy-read-dir")))
    ]
    pub statx: Option<Box<libc::statx>>,
}

/// 打开目录获取 fd（O_RDONLY | O_DIRECTORY | O_CLOEXEC）。
fn open_dir_fd(path: &Path) -> io::Result<RawFd> {
    let cstr = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"))?;
    // SAFETY: cstr 来自合法 CString（NUL 终止）；标志位组合为标准 POSIX。
    let fd = unsafe {
        libc::open(
            cstr.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(fd)
    }
}

/// 调用 getdents64 syscall。
///
/// SAFETY: `fd` 必须为打开的目录 fd；`buf` 必须有 `buf.len()` 字节可写。
unsafe fn syscall_getdents64(fd: RawFd, buf: &mut [u8]) -> io::Result<libc::c_long> {
    let n = libc::syscall(
        libc::SYS_getdents64,
        fd,
        buf.as_mut_ptr() as *mut libc::c_void,
        buf.len() as libc::size_t,
    );
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n)
    }
}

/// 解析 getdents64 写入 buffer 的前 `len` 字节，对每条目调用 `on_entry(d_type, name)`。
///
/// 跳过 "." 和 ".." 由调用方决定；本函数仅做字节级解析。
///
/// SAFETY: `buf[..len]` 必须来自成功的 getdents64 调用，由完整 linux_dirent64 记录组成。
unsafe fn parse_dirents(buf: &[u8], len: usize, mut on_entry: impl FnMut(u8, &OsStr)) {
    let mut offset = 0usize;
    let header_size = std::mem::size_of::<LinuxDirent64>();
    while offset + header_size <= len {
        // SAFETY: offset 后至少有 header_size 字节。LinuxDirent64 为 repr(C)，字段不填充。
        let entry_ptr = buf.as_ptr().add(offset) as *const LinuxDirent64;
        let entry = &*entry_ptr;
        let reclen = entry.d_reclen as usize;
        if reclen == 0 || offset + reclen > len {
            break;
        }

        // d_name 在 header 后固定偏移 DIRENT_NAME_OFFSET（kernel ABI，不随结构布局变化）。
        // SAFETY: getdents64 保证 d_name NUL 终止且整体在 reclen 范围内，strlen 不会越界。
        let name_ptr = buf.as_ptr().add(offset + DIRENT_NAME_OFFSET) as *const libc::c_char;
        let name_len = libc::strlen(name_ptr);
        let name_bytes = std::slice::from_raw_parts(name_ptr as *const u8, name_len);
        let name = OsStr::from_bytes(name_bytes);

        on_entry(entry.d_type, name);

        offset += reclen;
    }
}

/// 非流式枚举：一次性收集所有条目，返回 fd 守卫供后续 `fstatat` 使用。
///
/// 与 `std::fs::read_dir` 行为对等的回调版本，但单次 syscall 批量获取。
pub fn enumerate_dir_unix(
    path: &Path,
    capacity: usize,
) -> io::Result<(DirFdGuard, Vec<LinuxDirEntryOwned>)> {
    let fd = open_dir_fd(path)?;
    let guard = DirFdGuard(fd);
    let mut entries = Vec::with_capacity(capacity);

    let result = with_tls_buffer(|buf| loop {
        let n = unsafe { syscall_getdents64(fd, buf)? };
        if n == 0 {
            break Ok(());
        }
        unsafe {
            parse_dirents(buf, n as usize, |d_type, name| {
                if name.as_bytes() == b"." || name.as_bytes() == b".." {
                    return;
                }
                entries.push(LinuxDirEntryOwned {
                    d_type,
                    name: OsString::from(name),
                    #[cfg(all(target_os = "linux", target_env = "gnu", not(feature = "legacy-read-dir")))
                    ]
                    statx: None,
                });
            });
        }
    });

    result.map(|_| (guard, entries))
}

/// 流式枚举：发现子目录立即调用 `on_subdir` 调度，所有条目（含子目录）也加入返回 Vec。
///
/// 与 Windows 端 `enumerate_dir_streaming` 对齐：
/// 1. 64KB TLS buffer 复用
/// 2. 单次 getdents64 批量查询
/// 3. 对每个子目录条目调用 `on_subdir(&LinuxDirEntryOwned)` 回调
/// 4. 所有条目（文件 + 目录）保留在返回 Vec，供后续 DirEntry 构造
pub fn enumerate_dir_unix_streaming(
    path: &Path,
    capacity: usize,
    mut on_subdir: impl FnMut(&LinuxDirEntryOwned),
) -> io::Result<(DirFdGuard, Vec<LinuxDirEntryOwned>)> {
    let fd = open_dir_fd(path)?;
    let guard = DirFdGuard(fd);
    let mut entries = Vec::with_capacity(capacity);

    let result = with_tls_buffer(|buf| loop {
        let n = unsafe { syscall_getdents64(fd, buf)? };
        if n == 0 {
            break Ok(());
        }
        unsafe {
            parse_dirents(buf, n as usize, |d_type, name| {
                if name.as_bytes() == b"." || name.as_bytes() == b".." {
                    return;
                }
                let owned = LinuxDirEntryOwned {
                    d_type,
                    name: OsString::from(name),
                    #[cfg(all(target_os = "linux", target_env = "gnu", not(feature = "legacy-read-dir")))
                    ]
                    statx: None,
                };
                if d_type == libc::DT_DIR {
                    on_subdir(&owned);
                }
                entries.push(owned);
            });
        }
    });

    result.map(|_| (guard, entries))
}

/// 通过 `fstatat(dirfd, name, AT_SYMLINK_NOFOLLOW)` 获取 stat。
///
/// 失败返回 `io::Error`，调用方可降级（仅用 d_type 构造 FileType，metadata 留空）。
pub fn fstatat_metadata(dirfd: RawFd, name: &OsStr) -> io::Result<libc::stat> {
    let cstr = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL byte"))?;
    // SAFETY: cstr NUL 终止；stat 出参 zeroed 初始化（即便 fstatat 失败也安全）。
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            dirfd,
            cstr.as_ptr(),
            &mut stat as *mut libc::stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(stat)
    }
}

/// 通过 `statx(dirfd, name, AT_SYMLINK_NOFOLLOW, STATX_BASIC_STATS|STATX_BTIME)` 同步获取 statx。
///
/// 与 `fstatat_metadata` 的区别：statx 系统调用能拿 birth time (stx_btime)，fstatat 返回
/// 的 POSIX `stat` 结构无 btime 字段。scandir 默认走 LocalSync 路径（fstatat fallback），
/// gnu target 优先用本函数获取 btime，statx 失败才降级 fstatat。
///
/// 仅 gnu target：libc crate 不为 musl 导出 statx/statx_timestamp/STATX_*。
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn statx_metadata(dirfd: RawFd, name: &OsStr) -> io::Result<libc::statx> {
    let cstr = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL byte"))?;
    // SAFETY: cstr NUL 终止；statx 出参 zeroed 初始化（即便 statx 失败也安全）。
    let mut statx: libc::statx = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::statx(
            dirfd,
            cstr.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_BASIC_STATS | libc::STATX_BTIME,
            &mut statx as *mut libc::statx,
        )
    };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(statx)
    }
}

/// 从 `d_type` 构造 `std::fs::FileType`。
///
/// 常见类型（dir/reg/lnk/fifo/sock/blk/chr）直接映射；DT_UNKNOWN 返回 `None`，
/// 调用方需 `fstatat` fallback 取 `st_mode`。
pub fn file_type_from_dtype(d_type: u8) -> Option<std::fs::FileType> {
    let mode = match d_type {
        libc::DT_DIR => libc::S_IFDIR,
        libc::DT_REG => libc::S_IFREG,
        libc::DT_LNK => libc::S_IFLNK,
        libc::DT_FIFO => libc::S_IFIFO,
        libc::DT_SOCK => libc::S_IFSOCK,
        libc::DT_BLK => libc::S_IFBLK,
        libc::DT_CHR => libc::S_IFCHR,
        _ => return None,
    };
    Some(file_type_from_mode(mode))
}

/// 从 st_mode 构造 FileType（用于 fstatat fallback 与 d_type → FileType 映射）。
///
/// `std::fs::FileType` 在 Linux 上内部布局为 `{ mode: u32 }`（mode_t）但 stdlib 不暴露构造器。
/// 不同 Linux 发行版 / 不同 Rust 版本上 FileType 可能有额外 padding（实测 Ubuntu 24.04 是 8 bytes）。
/// 用字节 buffer + ptr::read 避免 transmute 的 size 等价要求：把 mode 写入 buffer 前 N 字节，
/// 后续 padding 为零，不影响 FileType::is_dir/is_file 等 mode 位判断。
pub fn file_type_from_mode(mode: libc::mode_t) -> std::fs::FileType {
    const FT_SIZE: usize = std::mem::size_of::<std::fs::FileType>();
    const MODE_SIZE: usize = std::mem::size_of::<libc::mode_t>();
    // 编译期保证 buffer 能容下 mode_t
    const _: () = assert!(FT_SIZE >= MODE_SIZE);
    let mut buf = [0u8; FT_SIZE];
    buf[..MODE_SIZE].copy_from_slice(&mode.to_ne_bytes());
    // SAFETY: buf 已零初始化，前 MODE_SIZE 字节来自合法 mode_t；
    // FileType 是 POD 类型，ptr::read 不产生 UB。
    unsafe { std::ptr::read(buf.as_ptr() as *const std::fs::FileType) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一条 linux_dirent64 字节流写入 buffer（8 字节对齐）。
    /// 布局：d_ino(8) | d_off(8) | d_reclen(2) | d_type(1) | d_name(NUL, padded)
    fn make_dirent_bytes(d_type: u8, name: &str) -> Vec<u8> {
        let name_bytes = name.as_bytes();
        let name_with_nul_len = name_bytes.len() + 1;
        // 头部固定 19 字节（d_ino 8 + d_off 8 + d_reclen 2 + d_type 1）。
        let header_len = 19;
        let total_len = header_len + name_with_nul_len;
        // linux_dirent64 8 字节对齐（与 u64 字段对齐）。
        let padded_len = (total_len + 7) & !7;

        let mut buf = vec![0u8; padded_len];
        buf[0..8].copy_from_slice(&1234u64.to_ne_bytes()); // d_ino
        buf[8..16].copy_from_slice(&5678u64.to_ne_bytes()); // d_off
        buf[16..18].copy_from_slice(&(padded_len as u16).to_ne_bytes()); // d_reclen
        buf[18] = d_type; // d_type
        buf[19..19 + name_bytes.len()].copy_from_slice(name_bytes);
        buf[19 + name_bytes.len()] = 0; // NUL 终止
        buf
    }

    #[test]
    fn test_parse_dirents_single_regular() {
        let buf = make_dirent_bytes(libc::DT_REG, "hello.txt");
        let mut found = Vec::new();
        unsafe {
            parse_dirents(&buf, buf.len(), |dt, name| {
                found.push((dt, name.to_string_lossy().into_owned()));
            });
        }
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, libc::DT_REG);
        assert_eq!(found[0].1, "hello.txt");
    }

    #[test]
    fn test_parse_dirents_multiple_mixed_types() {
        let mut buf = Vec::new();
        buf.extend(make_dirent_bytes(libc::DT_DIR, "subdir"));
        buf.extend(make_dirent_bytes(libc::DT_REG, "file1.txt"));
        buf.extend(make_dirent_bytes(libc::DT_LNK, "link"));

        let mut found = Vec::new();
        unsafe {
            parse_dirents(&buf, buf.len(), |dt, name| {
                found.push((dt, name.to_string_lossy().into_owned()));
            });
        }
        assert_eq!(found.len(), 3);
        assert_eq!(found[0], (libc::DT_DIR, "subdir".to_string()));
        assert_eq!(found[1], (libc::DT_REG, "file1.txt".to_string()));
        assert_eq!(found[2], (libc::DT_LNK, "link".to_string()));
    }

    #[test]
    fn test_parse_dirents_dot_entries_not_filtered() {
        // parse_dirents 自身不跳过 "."/".."，由调用方决定。
        let mut buf = Vec::new();
        buf.extend(make_dirent_bytes(libc::DT_DIR, "."));
        buf.extend(make_dirent_bytes(libc::DT_DIR, ".."));
        buf.extend(make_dirent_bytes(libc::DT_REG, "real.txt"));

        let mut all = Vec::new();
        unsafe {
            parse_dirents(&buf, buf.len(), |dt, name| {
                all.push((dt, name.to_string_lossy().into_owned()));
            });
        }
        assert_eq!(all.len(), 3);

        let filtered: Vec<_> = all
            .into_iter()
            .filter(|(_, n)| n != "." && n != "..")
            .collect();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].1, "real.txt");
    }

    #[test]
    fn test_parse_dirents_truncation_safe() {
        // 不完整记录（reclen 越界）必须停止解析而非越界读取。
        let mut buf = make_dirent_bytes(libc::DT_REG, "truncated.txt");
        // 截断最后 5 字节，让 reclen 越界
        let trunc_len = buf.len() - 5;
        buf.truncate(trunc_len);
        // 截断后 header 都不全，parse_dirents 不应触发任何回调或 panic。
        let mut found = 0;
        unsafe {
            parse_dirents(&buf, buf.len(), |_, _| {
                found += 1;
            });
        }
        assert_eq!(found, 0);
    }

    #[test]
    fn test_file_type_from_dtype_common_types() {
        assert!(file_type_from_dtype(libc::DT_DIR)
            .unwrap()
            .is_dir());
        assert!(file_type_from_dtype(libc::DT_REG)
            .unwrap()
            .is_file());
        assert!(file_type_from_dtype(libc::DT_LNK)
            .unwrap()
            .is_symlink());
        assert!(file_type_from_dtype(libc::DT_UNKNOWN).is_none());
    }

    #[test]
    fn test_file_type_from_mode_safety_assertion() {
        // 验证 FileType 内部布局假设（同 Windows 端 file_type_from_attrs）。
        // 若 stdlib 未来改变 FileType 布局，此处 panic 强迫重新评估 transmute。
        assert_eq!(
            std::mem::size_of::<std::fs::FileType>(),
            std::mem::size_of::<libc::mode_t>(),
        );
        assert!(file_type_from_mode(libc::S_IFDIR).is_dir());
        assert!(file_type_from_mode(libc::S_IFREG).is_file());
        assert!(file_type_from_mode(libc::S_IFLNK).is_symlink());
    }

    #[test]
    fn test_dirent_layout_offsets() {
        // 校验本模块定义的 LinuxDirent64 头部字段偏移（ABI 套图 linux_dirent64，不依赖 libc）。
        // d_name 在 offset 19，但 Rust 结构中不含 d_name 字段，这里仅校验头部 4 字段。
        assert_eq!(std::mem::offset_of!(LinuxDirent64, d_ino), 0);
        assert_eq!(std::mem::offset_of!(LinuxDirent64, d_off), 8);
        assert_eq!(std::mem::offset_of!(LinuxDirent64, d_reclen), 16);
        assert_eq!(std::mem::offset_of!(LinuxDirent64, d_type), 18);
        assert_eq!(DIRENT_NAME_OFFSET, 19);
    }

    /// 流式 schedule 序号正确性：模拟一次 getdents64 返回 3 个子目录 + 2 个文件，
    /// 验证 on_subdir 仅对子目录触发、返回 Vec 包含全部 5 条。
    #[test]
    fn test_streaming_dispatches_only_subdirs() {
        let mut buf = Vec::new();
        buf.extend(make_dirent_bytes(libc::DT_DIR, "d1"));
        buf.extend(make_dirent_bytes(libc::DT_REG, "f1"));
        buf.extend(make_dirent_bytes(libc::DT_DIR, "d2"));
        buf.extend(make_dirent_bytes(libc::DT_REG, "f2"));
        buf.extend(make_dirent_bytes(libc::DT_DIR, "d3"));

        // 直接驱动 parse_dirents + on_subdir 逻辑（绕过文件系统）
        let mut subdir_names = Vec::new();
        let mut all_entries = Vec::new();
        unsafe {
            parse_dirents(&buf, buf.len(), |d_type, name| {
                if name.as_bytes() == b"." || name.as_bytes() == b".." {
                    return;
                }
                let owned = LinuxDirEntryOwned {
                    d_type,
                    name: OsString::from(name),
                    #[cfg(all(target_os = "linux", target_env = "gnu", not(feature = "legacy-read-dir")))
                    ]
                    statx: None,
                };
                if d_type == libc::DT_DIR {
                    subdir_names.push(owned.name.clone());
                }
                all_entries.push(owned);
            });
        }

        assert_eq!(subdir_names, vec!["d1", "d2", "d3"]);
        assert_eq!(all_entries.len(), 5);
    }

    /// 端到端校验：在临时目录上跑 enumerate_dir_unix_streaming。
    #[test]
    fn test_enumerate_dir_unix_streaming_on_tmpdir() {
        let tmp = std::env::temp_dir().join(format!(
            "jwalk_meta_unix_enum_test_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        // 清理保护
        let _guard = scopeguard::guard(tmp.clone(), |p| {
            let _ = std::fs::remove_dir_all(p);
        });

        std::fs::create_dir(tmp.join("sub1")).unwrap();
        std::fs::create_dir(tmp.join("sub2")).unwrap();
        std::fs::write(tmp.join("file1.txt"), b"x").unwrap();
        std::fs::write(tmp.join("file2.txt"), b"x").unwrap();

        let mut subdir_count = 0usize;
        let (guard, entries) =
            enumerate_dir_unix_streaming(&tmp, 16, |_| subdir_count += 1).unwrap();
        drop(guard);

        // 至少 4 条目（2 subdir + 2 file，可能有别的进程临时文件，故 ≥）
        assert!(entries.len() >= 4, "entries len = {}", entries.len());
        assert!(subdir_count >= 2, "subdir_count = {}", subdir_count);

        // 所有条目类型合法
        for e in &entries {
            assert!(!e.name.is_empty());
        }
    }
}

// scopeguard 没在 Cargo.toml，内联一个最小 Drop guard（仅测试用）
#[cfg(test)]
mod scopeguard {
    pub struct Guard<T, F: FnMut(T)> {
        value: Option<T>,
        cleanup: Option<F>,
    }
    pub fn guard<T, F: FnMut(T)>(value: T, cleanup: F) -> Guard<T, F> {
        Guard {
            value: Some(value),
            cleanup: Some(cleanup),
        }
    }
    impl<T, F: FnMut(T)> Drop for Guard<T, F> {
        fn drop(&mut self) {
            if let (Some(value), Some(mut cleanup)) = (self.value.take(), self.cleanup.take()) {
                cleanup(value);
            }
        }
    }
}
