//! Workaround for https://github.com/rust-lang/rust/issues/63010
//! (originally reported as https://github.com/timberio/vector/issues/1480).
//!
//! Most of code is cribbed directly from the Rust stdlib and ported to work with winapi.
//!
//! In stdlib imported code, warnings are allowed.

use std::fs::{self, Permissions};
use std::path::Path;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
use std::sync::Arc;
use std::time::SystemTime;

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
    /// True if DirEntry is a regular file
    pub is_file: bool,
    /// True if DirEntry is a symbolic link
    pub is_symlink: bool,
    /// File size in bytes
    pub size: u64,
    /// Birth time (actual file creation time). Available when filesystem + kernel support it:
    /// Linux gnu + kernel>=4.13 + statx STATX_BTIME mask (CIFS nounix SMB2/3 supported via
    /// Steve French commit 6e70e26dc52b); Windows creation_time. None on fs without btime
    /// (ext3, FAT without mkfs option), musl target (std statx path is gnu-only), or kernel<4.11.
    pub created: Option<SystemTime>,
    /// POSIX change time (Unix) / creation time (Windows). Matches Python `os.stat_result.st_ctime`:
    /// - Unix: POSIX change time (`st_ctime` / `statx.stx_ctime`) — always available.
    /// - Windows: creation time (Windows has no POSIX ctime; Python mirrors this).
    pub changed: Option<SystemTime>,
    /// Last modification time, if available
    pub modified: Option<SystemTime>,
    /// Last access time, if available
    pub accessed: Option<SystemTime>,
    /// File permissions. `None` on Windows NT fast path (NT API doesn't return permissions),
    /// `Some` on all other paths. Always check with `is_some()` before use.
    pub permissions: Option<Permissions>,
}

impl MetaData {
    /// 从 std::fs::Metadata 构造完整的 MetaData。
    #[inline]
    pub fn from_fs_metadata(metadata: &fs::Metadata) -> Self {
        MetaData {
            is_dir: metadata.is_dir(),
            is_file: metadata.is_file(),
            is_symlink: metadata.is_symlink(),
            size: metadata.len(),
            created: created_time_from_metadata(metadata),
            changed: changed_time_from_metadata(metadata),
            modified: metadata.modified().ok(),
            accessed: metadata.accessed().ok(),
            permissions: Some(metadata.permissions()),
        }
    }
}

/// Extracts `MetaData.changed` from a `std::fs::Metadata`.
///
/// Platform-specific implementations are kept in separate cfg-gated functions so the
/// `from_fs_metadata` body stays a single linear field init with no inline cfg branches.
///
/// Note: this is the legacy-read-dir fallback path. The main scan paths (statx/fstatat
/// in lib.rs) populate `changed` directly from `stat.st_ctime` / `statx.stx_ctime`.
#[cfg(unix)]
#[inline]
fn changed_time_from_metadata(_metadata: &fs::Metadata) -> Option<SystemTime> {
    // ponytail: std::os::unix::fs::MetadataExt::ctime() is unstable (rust-lang/rust#63010).
    // Unix stable has no public API to read st_ctime from fs::Metadata without a re-stat.
    // Returns None; main scan paths (statx/fstatat) populate `changed` directly.
    None
}

#[cfg(windows)]
#[inline]
fn changed_time_from_metadata(metadata: &fs::Metadata) -> Option<SystemTime> {
    metadata.created().ok()
}

/// Extracts `MetaData.created` (birth time) from a `std::fs::Metadata`.
///
/// `std::fs::Metadata::created()` is stable across all targets since Rust 1.75
/// (PR rust-lang/rust#67774, 2020). Returns `Err` (-> None) when:
/// - musl target (statx path is gnu-only, musl falls back to stat64 which lacks btime),
/// - kernel < 4.11 (no statx syscall),
/// - filesystem doesn't support btime (ext3, FAT, etc.).
///
/// This is the legacy-read-dir fallback path. The main scan paths (statx/fstatat in lib.rs)
/// populate `created` directly from `statx.stx_btime` (with STATX_BTIME mask check).
#[inline]
fn created_time_from_metadata(metadata: &fs::Metadata) -> Option<SystemTime> {
    metadata.created().ok()
}

/// 预计算的祖先 metadata identity，用于符号链接循环检测。
///
/// 在添加 ancestor 时一次性计算并存储，避免 follow_symlink 中
/// 每次循环检测都需要 O(depth) 次 fs::metadata 系统调用。
/// 循环检测降为 O(depth) 内存比较。
#[cfg(unix)]
#[derive(Debug, Clone)]
pub struct AncestorIdentity {
    /// 祖先路径（用于错误消息）
    pub path: Arc<Path>,
    /// 设备 ID (st_dev)
    pub dev: u64,
    /// inode 号 (st_ino)
    pub ino: u64,
}

#[cfg(unix)]
impl AncestorIdentity {
    /// 从路径的 symlink_metadata 计算身份标识。
    ///
    /// 如果获取 metadata 失败，返回 None（保守策略：跳过此 ancestor 的循环检测）。
    pub fn from_path(path: Arc<Path>) -> Option<Self> {
        let meta = fs::symlink_metadata(path.as_ref()).ok()?;
        Some(AncestorIdentity {
            path,
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }
}

#[cfg(windows)]
#[derive(Debug, Clone)]
pub struct AncestorIdentity {
    /// 祖先路径（用于错误消息）
    pub path: Arc<Path>,
    /// 卷序列号
    pub volume_serial_number: Option<u32>,
    /// 文件索引
    pub file_index: Option<u64>,
}

#[cfg(windows)]
impl AncestorIdentity {
    /// 从路径的 symlink_metadata 计算身份标识。
    ///
    /// 如果获取 metadata 失败，返回 None（保守策略：跳过此 ancestor 的循环检测）。
    pub fn from_path(path: Arc<Path>) -> Option<Self> {
        let meta = fs::symlink_metadata(path.as_ref()).ok()?;
        let ext = get_metadata_ext(&meta);
        Some(AncestorIdentity {
            path,
            volume_serial_number: ext.volume_serial_number,
            file_index: ext.file_index,
        })
    }
}

/// Unix-specific file metadata, corresponding to POSIX `stat` fields.
#[cfg(unix)]
#[derive(Debug, Clone)]
pub struct MetaDataExt {
    /// File mode (permissions + file type bits), corresponds to `st_mode`
    pub st_mode: u32,
    /// Inode number, corresponds to `st_ino`
    pub st_ino: u64,
    /// Device ID, corresponds to `st_dev`
    pub st_dev: u64,
    /// Number of hard links, corresponds to `st_nlink`
    pub st_nlink: u64,
    /// Block size for filesystem I/O, corresponds to `st_blksize`
    pub st_blksize: u64,
    /// Number of 512-byte blocks allocated, corresponds to `st_blocks`
    pub st_blocks: u64,
    /// User ID of owner, corresponds to `st_uid`
    pub st_uid: u32,
    /// Group ID of owner, corresponds to `st_gid`
    pub st_gid: u32,
    /// Device ID for special file, corresponds to `st_rdev`
    pub st_rdev: u64,
}

/// Windows-specific file metadata, corresponding to `BY_HANDLE_FILE_INFORMATION`.
#[cfg(windows)]
#[derive(Debug, Clone)]
pub struct MetaDataExt {
    /// File attributes (e.g. `FILE_ATTRIBUTE_DIRECTORY` = `0x10`)
    pub file_attributes: u32,
    /// Volume serial number, may be `None` if not available
    pub volume_serial_number: Option<u32>,
    /// Number of links to this file, may be `None` if not available
    pub number_of_links: Option<u32>,
    /// File index (inode equivalent on NTFS/ReFS), may be `None` if not available
    pub file_index: Option<u64>,
}


