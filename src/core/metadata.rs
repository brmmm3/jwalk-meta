//! FIXME: A workaround to fix https://github.com/timberio/vector/issues/1480 resulting from https://github.com/rust-lang/rust/issues/63010
//! Most of code is cribbed directly from the Rust stdlib and ported to work with winapi.
//!
//! In stdlib imported code, warnings are allowed.

use std::fs::{self, Permissions};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
#[cfg(windows)]
use std::ptr;
use std::time::SystemTime;
#[cfg(windows)]
use std::{fs::File, mem::zeroed};
#[cfg(windows)]
use winapi::shared::minwindef::DWORD;
#[cfg(windows)]
use winapi::um::{
    fileapi::GetFileInformationByHandle, fileapi::BY_HANDLE_FILE_INFORMATION,
    ioapiset::DeviceIoControl, winioctl::FSCTL_GET_REPARSE_POINT,
    winnt::FILE_ATTRIBUTE_REPARSE_POINT, winnt::MAXIMUM_REPARSE_DATA_BUFFER_SIZE,
};

#[inline]
pub fn get_metadata_ext(metadata: &fs::Metadata) -> MetaDataExt {
    #[cfg(unix)]
    {
        MetaDataExt {
            st_mode: metadata.mode(),
            st_ino: metadata.ino(),
            st_dev: metadata.dev(),
            st_nlink: metadata.nlink(),
            st_blksize: metadata.blksize(),
            st_blocks: metadata.blocks(),
            st_uid: metadata.uid(),
            st_gid: metadata.gid(),
            st_rdev: metadata.rdev(),
        }
    }
    #[cfg(windows)]
    {
        MetaDataExt {
            file_attributes: metadata.file_attributes(),
            volume_serial_number: metadata.volume_serial_number(),
            number_of_links: metadata.number_of_links(),
            file_index: metadata.file_index(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetaData {
    /// True if DirEntry is a directory
    pub is_dir: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub created: Option<SystemTime>,
    pub modified: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    pub permissions: Option<Permissions>,
}

#[cfg(unix)]
#[derive(Debug, Clone)]
pub struct MetaDataExt {
    pub st_mode: u32,
    pub st_ino: u64,
    pub st_dev: u64,
    pub st_nlink: u64,
    pub st_blksize: u64,
    pub st_blocks: u64,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: u64,
}

#[cfg(windows)]
#[derive(Debug, Clone)]
pub struct MetaDataExt {
    pub file_attributes: u32,
    pub volume_serial_number: Option<u32>,
    pub number_of_links: Option<u32>,
    pub file_index: Option<u64>,
}

#[cfg(windows)]
pub trait FileExt: std::os::windows::io::AsRawHandle {
    // This code is from the Rust stdlib https://github.com/rust-lang/rust/blob/30ddb5a8c1e85916da0acdc665d6a16535a12dd6/src/libstd/sys/windows/fs.rs#L458-L478
    #[allow(unused_assignments, unused_variables)]
    fn reparse_point<'a>(
        &self,
        space: &'a mut [u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE as usize],
    ) -> std::io::Result<(DWORD, &'a REPARSE_DATA_BUFFER)> {
        unsafe {
            let mut bytes = 0;
            cvt({
                DeviceIoControl(
                    self.as_raw_handle(),
                    FSCTL_GET_REPARSE_POINT,
                    ptr::null_mut(),
                    0,
                    space.as_mut_ptr() as *mut _,
                    space.len() as DWORD,
                    &mut bytes,
                    ptr::null_mut(),
                )
            })?;
            Ok((bytes, &*(space.as_ptr() as *const REPARSE_DATA_BUFFER)))
        }
    }
    // This code is from the Rust stdlib https://github.com/rust-lang/rust/blob/30ddb5a8c1e85916da0acdc665d6a16535a12dd6/src/libstd/sys/windows/fs.rs#L326-L351
    #[cfg(windows)]
    #[allow(unused_assignments, unused_variables)]
    fn get_file_info(&self) -> std::io::Result<BY_HANDLE_FILE_INFORMATION> {
        unsafe {
            let mut info: BY_HANDLE_FILE_INFORMATION = zeroed();
            cvt(GetFileInformationByHandle(self.as_raw_handle(), &mut info))?;
            let mut reparse_tag = 0;
            if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                let mut b = [0; MAXIMUM_REPARSE_DATA_BUFFER_SIZE as usize];
                if let Ok((_, buf)) = self.reparse_point(&mut b) {
                    reparse_tag = buf.ReparseTag;
                }
            }
            Ok(info)
        }
    }

    #[cfg(windows)]
    fn volume_serial_number(&self) -> std::io::Result<u64> {
        Ok(0)
    }

    #[cfg(windows)]
    fn portable_ino(&self) -> std::io::Result<u64> {
        Ok(0)
    }
}

#[cfg(windows)]
impl FileExt for File {
    fn volume_serial_number(&self) -> std::io::Result<u64> {
        let info = self.get_file_info()?;
        Ok(info.dwVolumeSerialNumber.into())
    }
    fn portable_ino(&self) -> std::io::Result<u64> {
        let info = self.get_file_info()?;
        Ok(info.nNumberOfLinks.into())
    }
}

// This code is from the Rust stdlib https://github.com/rust-lang/rust/blob/a916ac22b9f7f1f0f7aba0a41a789b3ecd765018/src/libstd/sys/windows/c.rs#L380-L386
#[cfg(windows)]
#[allow(non_snake_case, non_camel_case_types)]
pub struct REPARSE_DATA_BUFFER {
    pub ReparseTag: libc::c_uint,
    pub ReparseDataLength: libc::c_ushort,
    pub Reserved: libc::c_ushort,
    pub rest: (),
}

// This code is from the Rust stdlib  https://github.com/rust-lang/rust/blob/30ddb5a8c1e85916da0acdc665d6a16535a12dd6/src/libstd/sys/hermit/mod.rs#L141-L143
#[cfg(windows)]
pub fn cvt(result: i32) -> std::io::Result<usize> {
    if result < 0 {
        Err(std::io::Error::from_raw_os_error(-result))
    } else {
        Ok(result as usize)
    }
}
