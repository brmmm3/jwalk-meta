//! Linux directory enumeration with a directory fd for relative metadata queries.
//!
//! `std::fs::read_dir` supplies `FileType` without relying on its private layout.
//! On Linux it uses directory-entry type information without a stat for known types.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::path::Path;

pub struct DirFdGuard(RawFd);

impl DirFdGuard {
    pub fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

impl Drop for DirFdGuard {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.0);
        }
    }
}

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

pub struct LinuxDirEntryOwned {
    pub name: OsString,
    pub file_type: std::fs::FileType,
}

fn open_dir_fd(path: &Path) -> io::Result<RawFd> {
    let cstr = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"))?;
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

pub fn enumerate_dir_unix(
    path: &Path,
    capacity: usize,
) -> io::Result<(DirFdGuard, Vec<LinuxDirEntryOwned>)> {
    enumerate_dir_unix_streaming(path, capacity, |_| {})
}

pub fn enumerate_dir_unix_streaming(
    path: &Path,
    capacity: usize,
    mut on_subdir: impl FnMut(&LinuxDirEntryOwned),
) -> io::Result<(DirFdGuard, Vec<LinuxDirEntryOwned>)> {
    let guard = DirFdGuard(open_dir_fd(path)?);
    let mut entries = Vec::with_capacity(capacity);
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let owned = LinuxDirEntryOwned {
            name: entry.file_name(),
            file_type: entry.file_type()?,
        };
        if owned.file_type.is_dir() {
            on_subdir(&owned);
        }
        entries.push(owned);
    }
    Ok((guard, entries))
}

pub fn fstatat_metadata(dirfd: RawFd, name: &OsStr) -> io::Result<libc::stat> {
    let cstr = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL byte"))?;
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::fstatat(dirfd, cstr.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(stat)
    }
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub fn statx_metadata(dirfd: RawFd, name: &OsStr) -> io::Result<libc::statx> {
    let cstr = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL byte"))?;
    let mut statx: libc::statx = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::statx(
            dirfd,
            cstr.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_BASIC_STATS | libc::STATX_BTIME,
            &mut statx,
        )
    };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(statx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_reports_real_types_and_only_directories() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("file"), b"x").unwrap();
        std::os::unix::fs::symlink("sub", tmp.path().join("link")).unwrap();

        let mut streamed = Vec::new();
        let (_guard, entries) =
            enumerate_dir_unix_streaming(tmp.path(), 0, |entry| streamed.push(entry.name.clone()))
                .unwrap();
        assert_eq!(streamed, [OsString::from("sub")]);
        assert!(
            entries
                .iter()
                .find(|e| e.name == "sub")
                .unwrap()
                .file_type
                .is_dir()
        );
        assert!(
            entries
                .iter()
                .find(|e| e.name == "file")
                .unwrap()
                .file_type
                .is_file()
        );
        assert!(
            entries
                .iter()
                .find(|e| e.name == "link")
                .unwrap()
                .file_type
                .is_symlink()
        );
    }

    #[test]
    fn relative_metadata_reports_invalid_names_and_descriptors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("file"), b"content").unwrap();
        let (guard, _) = enumerate_dir_unix(dir.path(), 0).unwrap();
        let stat = fstatat_metadata(guard.as_raw_fd(), OsStr::new("file")).unwrap();
        assert_eq!(stat.st_size, 7);
        assert!(fstatat_metadata(guard.as_raw_fd(), OsStr::new("missing")).is_err());
        assert_eq!(
            fstatat_metadata(guard.as_raw_fd(), OsStr::from_bytes(b"bad\0name"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        #[cfg(target_env = "gnu")]
        {
            assert!(statx_metadata(guard.as_raw_fd(), OsStr::new("file")).is_ok());
            assert!(statx_metadata(-1, OsStr::new("file")).is_err());
            assert_eq!(
                statx_metadata(guard.as_raw_fd(), OsStr::from_bytes(b"bad\0name"))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }
}
