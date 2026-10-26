//! Direct Linux getdents64 directory enumeration.
//!
//! Replaces `std::fs::read_dir`, fetching multiple entries per syscall and supporting streaming subdirectory dispatch.
//! Compiled only on Linux: macOS uses `getdirentries64` (with a different signature), and other Unix systems may not support it.
//!
//! # Key points
//!
//! - Reuse a 64 KB thread-local buffer, matching the Windows `TLS_BUFFER` pattern.
//! - Use `linux_dirent64::d_type` to identify dirs/files/symlinks without `fstatat` (except for the DT_UNKNOWN fallback).
//! - Use `fstatat(dirfd, name, AT_SYMLINK_NOFOLLOW)` instead of `fs::metadata` to avoid path resolution.
//! - Return a `DirFdGuard` to keep the fd open for subsequent `fstatat` calls.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::path::Path;

// Directory entry returned by Linux getdents64 (ABI layout; does not depend on libc exporting linux_dirent64).
// Matches the linux_dirent64 ABI exactly: d_ino(8) | d_off(8) | d_reclen(2) | d_type(1) | d_name[NUL].
#[repr(C)]
struct LinuxDirent64 {
    d_ino: u64,
    d_off: u64,
    d_reclen: u16,
    d_type: u8,
    // d_name is a variable-length NUL-terminated byte string at offset 19 (fixed by the kernel ABI, not computed from the Rust struct).
}

/// Fixed offset of d_name in LinuxDirent64 (guaranteed by the kernel ABI; independent of compiler alignment).
const DIRENT_NAME_OFFSET: usize = 19;

/// getdents64 buffer size (64 KB).
/// A single syscall can return hundreds to thousands of entries from a large directory, matching the Windows TLS_BUFFER size.
const DENTS_BUF_SIZE: usize = 64 * 1024;

// Thread-local buffer to avoid allocating 64 KB on every enumeration.
thread_local! {
    static TLS_BUFFER: std::cell::RefCell<Vec<u8>> =
        std::cell::RefCell::new(vec![0u8; DENTS_BUF_SIZE]);
}

/// Borrow the TLS buffer (resize if it is smaller than 64 KB).
fn with_tls_buffer<R>(f: impl FnOnce(&mut [u8]) -> R) -> R {
    TLS_BUFFER.with(|cell| {
        let mut buf = cell.borrow_mut();
        if buf.len() < DENTS_BUF_SIZE {
            buf.resize(DENTS_BUF_SIZE, 0);
        }
        f(&mut buf[..DENTS_BUF_SIZE])
    })
}

/// Directory fd guard. Automatically calls `close` on drop to prevent fd leaks.
pub struct DirFdGuard(RawFd);

impl DirFdGuard {
    #[inline]
    pub fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl Drop for DirFdGuard {
    fn drop(&mut self) {
        // SAFETY: fd was >= 0 when constructed; close does not panic, and errors cannot be recovered from here.
        unsafe {
            libc::close(self.0);
        }
    }
}

/// Batched statx results returned by the Linux io_uring path.
///
/// Keeping the results in one contiguous allocation avoids a separate heap allocation for
/// every directory entry. The empty non-GNU variant keeps the directory conversion API
/// available on Linux targets where libc does not expose `statx`.
#[cfg(all(target_env = "gnu", not(feature = "legacy-read-dir")))]
#[derive(Default)]
pub struct LinuxStatxResults {
    pub(crate) results: Vec<Option<libc::statx>>,
}

#[cfg(not(all(target_env = "gnu", not(feature = "legacy-read-dir"))))]
#[derive(Default)]
pub struct LinuxStatxResults;

#[cfg(all(target_env = "gnu", not(feature = "legacy-read-dir")))]
pub struct LinuxStatxResult(pub(crate) Option<libc::statx>);

#[cfg(not(all(target_env = "gnu", not(feature = "legacy-read-dir"))))]
pub struct LinuxStatxResult;

#[cfg(all(target_env = "gnu", not(feature = "legacy-read-dir")))]
impl LinuxStatxResults {
    pub(crate) fn take(&mut self, index: usize) -> LinuxStatxResult {
        LinuxStatxResult(self.results.get_mut(index).and_then(Option::take))
    }
}

#[cfg(not(all(target_env = "gnu", not(feature = "legacy-read-dir"))))]
impl LinuxStatxResults {
    pub(crate) fn take(&mut self, _index: usize) -> LinuxStatxResult {
        LinuxStatxResult
    }
}

/// Collected directory entry (owned, so it can outlive buffer reuse).
pub struct LinuxDirEntryOwned {
    pub d_type: u8,
    pub name: OsString,
}

/// Open a directory and obtain an fd (O_RDONLY | O_DIRECTORY | O_CLOEXEC).
fn open_dir_fd(path: &Path) -> io::Result<RawFd> {
    let cstr = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"))?;
    // SAFETY: cstr is a valid CString (NUL-terminated); the flag combination is standard POSIX.
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

/// Call the getdents64 syscall.
///
/// SAFETY: `fd` must be an open directory fd; `buf` must have `buf.len()` writable bytes.
unsafe fn syscall_getdents64(fd: RawFd, buf: &mut [u8]) -> io::Result<libc::c_long> {
    let n = unsafe {
        libc::syscall(
            libc::SYS_getdents64,
            fd,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len() as libc::size_t,
        )
    };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n)
    }
}

/// Parse the first `len` bytes written to buffer by getdents64 and call `on_entry(d_type, name)` for each entry.
///
/// The caller decides whether to skip "." and ".."; this function only parses bytes.
///
/// SAFETY: `buf[..len]` must come from a successful getdents64 call and consist of complete linux_dirent64 records.
unsafe fn parse_dirents(buf: &[u8], len: usize, mut on_entry: impl FnMut(u8, &OsStr)) {
    let mut offset = 0usize;
    let header_size = std::mem::size_of::<LinuxDirent64>();
    while offset + header_size <= len {
        // SAFETY: At least header_size bytes remain after offset. LinuxDirent64 is repr(C) and its fields have no padding.
        let entry_ptr = unsafe { buf.as_ptr().add(offset) as *const LinuxDirent64 };
        let entry = unsafe { &*entry_ptr };
        let reclen = entry.d_reclen as usize;
        if reclen == 0 || offset + reclen > len {
            break;
        }

        // d_name starts at the fixed DIRENT_NAME_OFFSET after the header (kernel ABI, independent of struct layout).
        // SAFETY: getdents64 guarantees d_name is NUL-terminated and within reclen, so strlen cannot go out of bounds.
        let name_ptr =
            unsafe { buf.as_ptr().add(offset + DIRENT_NAME_OFFSET) as *const libc::c_char };
        let name_len = unsafe { libc::strlen(name_ptr) };
        let name_bytes = unsafe { std::slice::from_raw_parts(name_ptr as *const u8, name_len) };
        let name = OsStr::from_bytes(name_bytes);

        on_entry(entry.d_type, name);

        offset += reclen;
    }
}

/// Non-streaming enumeration: collect all entries at once and return an fd guard for subsequent `fstatat` calls.
///
/// Callback-based version with behavior equivalent to `std::fs::read_dir`, but using a single syscall.
pub fn enumerate_dir_unix(
    path: &Path,
    capacity: usize,
) -> io::Result<(DirFdGuard, Vec<LinuxDirEntryOwned>)> {
    let fd = open_dir_fd(path)?;
    let guard = DirFdGuard(fd);
    let mut entries = Vec::with_capacity(capacity);

    let result = with_tls_buffer(|buf| {
        loop {
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
                    });
                });
            }
        }
    });

    result.map(|_| (guard, entries))
}

/// Streaming enumeration: call `on_subdir` immediately when a subdirectory is found, and also add every entry (including subdirectories) to the returned Vec.
///
/// Matches the Windows `enumerate_dir_streaming` implementation:
/// 1. Reuse a 64 KB TLS buffer.
/// 2. Fetch entries in a single getdents64 call.
/// 3. Call `on_subdir(&LinuxDirEntryOwned)` for each subdirectory entry.
/// 4. Retain all entries (files and directories) in the returned Vec for subsequent DirEntry construction.
pub fn enumerate_dir_unix_streaming(
    path: &Path,
    capacity: usize,
    mut on_subdir: impl FnMut(&LinuxDirEntryOwned),
) -> io::Result<(DirFdGuard, Vec<LinuxDirEntryOwned>)> {
    let fd = open_dir_fd(path)?;
    let guard = DirFdGuard(fd);
    let mut entries = Vec::with_capacity(capacity);

    let result = with_tls_buffer(|buf| {
        loop {
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
                    };

                    // DT_UNKNOWN is common on some network and synthetic filesystems. Resolve it
                    // here so streamed scheduling counts every real directory in the same order
                    // that ReadDir will later expose its child specs.
                    if entry_is_directory(fd, d_type, &owned.name) {
                        on_subdir(&owned);
                    }
                    entries.push(owned);
                });
            }
        }
    });

    result.map(|_| (guard, entries))
}

/// Get stat using `fstatat(dirfd, name, AT_SYMLINK_NOFOLLOW)`.
///
/// Return `io::Error` on failure; the caller can fall back to constructing FileType from d_type and leaving metadata empty.
pub fn fstatat_metadata(dirfd: RawFd, name: &OsStr) -> io::Result<libc::stat> {
    let cstr = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL byte"))?;
    // SAFETY: cstr is NUL-terminated; stat is zero-initialized (safe even if fstatat fails).
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

/// Determine whether a getdents entry is a directory, resolving `DT_UNKNOWN` when necessary.
fn entry_is_directory(dirfd: RawFd, d_type: u8, name: &OsStr) -> bool {
    if d_type == libc::DT_DIR {
        return true;
    }
    d_type == libc::DT_UNKNOWN
        && fstatat_metadata(dirfd, name)
            .is_ok_and(|stat| (stat.st_mode & libc::S_IFMT) == libc::S_IFDIR)
}

/// Synchronously get statx using `statx(dirfd, name, AT_SYMLINK_NOFOLLOW, STATX_BASIC_STATS|STATX_BTIME)`.
///
/// Unlike `fstatat_metadata`, the statx syscall can retrieve birth time (stx_btime); the POSIX `stat` structure returned by fstatat
/// has no btime field. scandir defaults to the LocalSync path (fstatat fallback); GNU targets use this function to get btime
/// and fall back to fstatat only if statx fails.
///
/// GNU targets only: the libc crate does not export statx/statx_timestamp/STATX_* for musl.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn statx_metadata(dirfd: RawFd, name: &OsStr) -> io::Result<libc::statx> {
    let cstr = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL byte"))?;
    // SAFETY: cstr is NUL-terminated; statx output is zero-initialized (safe even if statx fails).
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

/// Construct `std::fs::FileType` from `d_type`.
///
/// Common types (dir/reg/lnk/fifo/sock/blk/chr) are mapped directly; DT_UNKNOWN returns `None`,
/// and the caller must fall back to `fstatat` to get `st_mode`.
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

/// Construct FileType from st_mode (for the fstatat fallback and d_type -> FileType mapping).
///
/// On Linux, `std::fs::FileType` is internally laid out as `{ mode: u32 }` (mode_t), but stdlib exposes no constructor.
/// FileType may have additional padding on different Linux distributions or Rust versions (measured as 8 bytes on Ubuntu 24.04).
/// Use a byte buffer and ptr::read to avoid transmute's equal-size requirement: write mode to the first N bytes of the buffer,
/// and leave the remaining padding zeroed; this does not affect mode-bit checks such as FileType::is_dir/is_file.
pub fn file_type_from_mode(mode: libc::mode_t) -> std::fs::FileType {
    const FT_SIZE: usize = std::mem::size_of::<std::fs::FileType>();
    const MODE_SIZE: usize = std::mem::size_of::<libc::mode_t>();
    // Compile-time check that the buffer can hold mode_t.
    const _: () = assert!(FT_SIZE >= MODE_SIZE);
    let mut buf = [0u8; FT_SIZE];
    buf[..MODE_SIZE].copy_from_slice(&mode.to_ne_bytes());
    // SAFETY: buf is zero-initialized, and the first MODE_SIZE bytes come from a valid mode_t;
    // FileType is a POD type, so ptr::read does not cause UB.
    unsafe { std::ptr::read(buf.as_ptr() as *const std::fs::FileType) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct one linux_dirent64 byte record in the buffer (8-byte aligned).
    /// Layout: d_ino(8) | d_off(8) | d_reclen(2) | d_type(1) | d_name(NUL, padded).
    fn make_dirent_bytes(d_type: u8, name: &str) -> Vec<u8> {
        let name_bytes = name.as_bytes();
        let name_with_nul_len = name_bytes.len() + 1;
        // Header is fixed at 19 bytes (d_ino 8 + d_off 8 + d_reclen 2 + d_type 1).
        let header_len = 19;
        let total_len = header_len + name_with_nul_len;
        // Align linux_dirent64 to 8 bytes (matching its u64 fields).
        let padded_len = (total_len + 7) & !7;

        let mut buf = vec![0u8; padded_len];
        buf[0..8].copy_from_slice(&1234u64.to_ne_bytes()); // d_ino
        buf[8..16].copy_from_slice(&5678u64.to_ne_bytes()); // d_off
        buf[16..18].copy_from_slice(&(padded_len as u16).to_ne_bytes()); // d_reclen
        buf[18] = d_type; // d_type
        buf[19..19 + name_bytes.len()].copy_from_slice(name_bytes);
        buf[19 + name_bytes.len()] = 0; // NUL terminator.
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
        // parse_dirents itself does not skip "."/".."; that is up to the caller.
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
        // Stop parsing on an incomplete record (reclen out of bounds) instead of reading out of bounds.
        let mut buf = make_dirent_bytes(libc::DT_REG, "truncated.txt");
        // Truncate the final 5 bytes to make reclen exceed the buffer.
        let trunc_len = buf.len() - 5;
        buf.truncate(trunc_len);
        // The truncated record no longer has a complete header; parse_dirents should not invoke callbacks or panic.
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
        assert!(file_type_from_dtype(libc::DT_DIR).unwrap().is_dir());
        assert!(file_type_from_dtype(libc::DT_REG).unwrap().is_file());
        assert!(file_type_from_dtype(libc::DT_LNK).unwrap().is_symlink());
        assert!(file_type_from_dtype(libc::DT_UNKNOWN).is_none());
    }

    #[test]
    fn test_file_type_from_mode_safety_assertion() {
        // Verify the FileType internal-layout assumption (as in the Windows file_type_from_attrs implementation).
        // If stdlib changes FileType's layout in the future, this panic forces a review of the transmute.
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
        // Verify header-field offsets for this module's LinuxDirent64 definition (models linux_dirent64 ABI without depending on libc).
        // d_name is at offset 19, but the Rust struct does not contain a d_name field, so only the four header fields are checked here.
        assert_eq!(std::mem::offset_of!(LinuxDirent64, d_ino), 0);
        assert_eq!(std::mem::offset_of!(LinuxDirent64, d_off), 8);
        assert_eq!(std::mem::offset_of!(LinuxDirent64, d_reclen), 16);
        assert_eq!(std::mem::offset_of!(LinuxDirent64, d_type), 18);
        assert_eq!(DIRENT_NAME_OFFSET, 19);
    }

    /// Verify streaming schedule indices: simulate one getdents64 call returning three subdirectories and two files,
    /// and check that on_subdir runs only for subdirectories while the returned Vec contains all five entries.
    #[test]
    fn test_streaming_dispatches_only_subdirs() {
        let mut buf = Vec::new();
        buf.extend(make_dirent_bytes(libc::DT_DIR, "d1"));
        buf.extend(make_dirent_bytes(libc::DT_REG, "f1"));
        buf.extend(make_dirent_bytes(libc::DT_DIR, "d2"));
        buf.extend(make_dirent_bytes(libc::DT_REG, "f2"));
        buf.extend(make_dirent_bytes(libc::DT_DIR, "d3"));

        // Drive the parse_dirents + on_subdir logic directly (bypassing the filesystem).
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

    /// End-to-end check: run enumerate_dir_unix_streaming on a temporary directory.
    #[test]
    fn test_enumerate_dir_unix_streaming_on_tmpdir() {
        let tmp =
            std::env::temp_dir().join(format!("jwalk_meta_unix_enum_test_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // Cleanup guard.
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

        // At least four entries (two subdirectories + two files; other processes may create temporary files, hence >=).
        assert!(entries.len() >= 4, "entries len = {}", entries.len());
        assert!(subdir_count >= 2, "subdir_count = {}", subdir_count);

        // All entry names should be valid.
        for e in &entries {
            assert!(!e.name.is_empty());
        }
    }

    #[test]
    fn test_unknown_dtype_directory_is_streamable() {
        let tmp = std::env::temp_dir().join(format!(
            "jwalk_meta_unknown_dtype_test_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(tmp.join("subdir")).unwrap();
        std::fs::write(tmp.join("file"), b"x").unwrap();
        let _guard = scopeguard::guard(tmp.clone(), |path| {
            let _ = std::fs::remove_dir_all(path);
        });

        let cstr = std::ffi::CString::new(tmp.as_os_str().as_bytes()).unwrap();
        let fd = unsafe {
            libc::open(
                cstr.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        assert!(fd >= 0);
        assert!(entry_is_directory(
            fd,
            libc::DT_UNKNOWN,
            OsStr::new("subdir")
        ));
        assert!(!entry_is_directory(
            fd,
            libc::DT_UNKNOWN,
            OsStr::new("file")
        ));
        unsafe { libc::close(fd) };
    }
}

// scopeguard is not in Cargo.toml, so define a minimal Drop guard inline (for tests only).
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
