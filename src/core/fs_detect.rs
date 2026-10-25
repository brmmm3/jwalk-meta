//! Linux-only filesystem type detection and I/O strategy dispatch.
//!
//! Use `statfs(2)` to inspect the filesystem `f_type` and decide whether to use batched STATX via io_uring.
//! Enable io_uring only for **high-latency** network mounts (SMB/NFS/CIFS); low-latency network mounts
//! and local filesystems use serial fstatat (io_uring's extra overhead can make it slower at low latency).
//!
//! Latency probe: time one `fstatat` on the first network filesystem; use io_uring at >= 1 ms, otherwise LocalSync.
//! Cache policy: a per-worker `HashMap<st_dev, IoStrategy>`. Probe each `st_dev` only once.

#![cfg(target_os = "linux")]

use std::collections::HashMap;
#[cfg(test)]
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::path::Path;

// ── f_type constants (from linux/magic.h; do not depend on libc exporting them) ──────────

/// SMB_SUPER_MAGIC — Linux SMB1 client mount
const SMB_SUPER_MAGIC: i64 = 0x517B;
/// NFS_SUPER_MAGIC — NFSv2/v3/v4
const NFS_SUPER_MAGIC: i64 = 0x6969;
/// CIFS_MAGIC_NUMBER — Linux CIFS/SMB1 client mount
const CIFS_MAGIC_NUMBER: i64 = 0xFF534D42;
/// SMB2_SUPER_MAGIC — Linux SMB2/SMB3 client mount (vers=2.0/2.1/3.0/3.1.1).
const SMB2_SUPER_MAGIC: i64 = 0xFE534D42;
/// EXT4_SUPER_MAGIC (also used by ext2/ext3).
const EXT4_SUPER_MAGIC: i64 = 0xEF53;
/// XFS_SUPER_MAGIC
const XFS_SUPER_MAGIC: i64 = 0x58465342;
/// BTRFS_SUPER_MAGIC
const BTRFS_SUPER_MAGIC: i64 = 0x9123683E;
/// TMPFS_MAGIC — commonly used for /tmp and container overlay upper layers.
const TMPFS_MAGIC: i64 = 0x01021994;
/// OVERLAYFS_SUPER_MAGIC — container environments.
const OVERLAYFS_SUPER_MAGIC: i64 = 0x794C7630;
/// MSDOS_SUPER_MAGIC — FAT12/16/32
const MSDOS_SUPER_MAGIC: i64 = 0x4D44;
/// EXFAT_SUPER_MAGIC
const EXFAT_SUPER_MAGIC: i64 = 0x2011BAB0;

/// Directory metadata retrieval strategy.
///
/// Selection criteria: network mounts use batched STATX via io_uring (saving N round trips); local filesystems use serial fstatat
/// (there is no benefit after an MFT/inode cache hit, and the NTFS MFT lock makes concurrency slower; see rollback 5367f20).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoStrategy {
    /// Serial per-entry `fstatat`. Suitable for local filesystems and low-latency network mounts.
    LocalSync,
    /// Batched STATX via io_uring. Suitable for high-latency network mounts (reap N CQEs with one submit_and_wait).
    NetworkAsync,
    /// FAT family (FAT32/exFAT): hard links are unsupported, `st_nlink` is always 1, so skip the query.
    SkipNlinks,
}

/// Per-worker I/O strategy cache, keyed by `st_dev` (from `stat(2)`).
///
/// Each Rayon worker has its own cache to avoid lock contention. Call statfs only once per device.
#[derive(Debug, Default)]
pub struct IoStrategyCache {
    map: HashMap<u64, IoStrategy>,
}

impl IoStrategyCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the strategy for a cached device (test helper).
    #[cfg(test)]
    pub fn get(&self, dev: u64) -> Option<IoStrategy> {
        self.map.get(&dev).copied()
    }

    /// Insert an entry into the cache (test helper).
    #[cfg(test)]
    pub fn insert(&mut self, dev: u64, strategy: IoStrategy) {
        self.map.insert(dev, strategy);
    }

    /// Test helper retaining the path-based strategy lookup.
    #[cfg(test)]
    pub fn detect(&mut self, path: &Path) -> IoStrategy {
        match stat_dev(path) {
            Ok(dev) => {
                if let Some(s) = self.map.get(&dev).copied() {
                    return s;
                }
                let strategy = statfs_strategy(path).unwrap_or(IoStrategy::LocalSync);
                self.map.insert(dev, strategy);
                strategy
            }
            Err(_) => IoStrategy::LocalSync,
        }
    }

    /// Detect the strategy using an already-open directory descriptor.
    ///
    /// `fstat` avoids resolving the directory path again, and the caller only needs to
    /// invoke this for directories large enough to qualify for batched metadata I/O.
    pub fn detect_for_fd(&mut self, path: &Path, fd: RawFd) -> IoStrategy {
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: fd is the open directory descriptor used by the caller; st is zeroed.
        if unsafe { libc::fstat(fd, &mut st) } < 0 {
            return IoStrategy::LocalSync;
        }
        let dev = st.st_dev;
        if let Some(strategy) = self.map.get(&dev).copied() {
            return strategy;
        }

        let strategy = statfs_strategy(path).unwrap_or(IoStrategy::LocalSync);
        self.map.insert(dev, strategy);
        strategy
    }
}

/// Get `st_dev` using `stat(2)`.
#[cfg(test)]
fn stat_dev(path: &Path) -> io::Result<u64> {
    let cstr = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"))?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: cstr is NUL-terminated; st has been zeroed.
    let rc = unsafe { libc::stat(cstr.as_ptr(), &mut st) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(st.st_dev)
    }
}

/// Get `f_type` with `statfs(2)` and map it to an `IoStrategy`.
///
/// Network filesystems get an additional latency probe: time `fstatat` on the path,
/// use `NetworkAsync` at or above `LATENCY_THRESHOLD`, and otherwise fall back to `LocalSync`.
fn statfs_strategy(path: &Path) -> Option<IoStrategy> {
    let cstr = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut sfs: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: cstr is NUL-terminated; sfs has been zeroed.
    let rc = unsafe { libc::statfs(cstr.as_ptr(), &mut sfs) };
    if rc < 0 {
        return None;
    }
    let preliminary = strategy_from_ftype(sfs.f_type as i64);
    if preliminary != IoStrategy::NetworkAsync {
        return Some(preliminary);
    }
    // Network filesystem: use the latency probe to decide whether to enable io_uring.
    Some(probe_latency(path))
}

/// Infer a strategy from `f_type` (without the latency probe).
fn strategy_from_ftype(f_type: i64) -> IoStrategy {
    match f_type {
        // Network filesystem: candidate for io_uring (final decision is made by the latency probe).
        SMB_SUPER_MAGIC | NFS_SUPER_MAGIC | CIFS_MAGIC_NUMBER | SMB2_SUPER_MAGIC => {
            IoStrategy::NetworkAsync
        }
        // FAT family: skip the nlink query (nlink is always 1).
        MSDOS_SUPER_MAGIC | EXFAT_SUPER_MAGIC => IoStrategy::SkipNlinks,
        // Local filesystem: serial fstatat.
        EXT4_SUPER_MAGIC
        | XFS_SUPER_MAGIC
        | BTRFS_SUPER_MAGIC
        | TMPFS_MAGIC
        | OVERLAYFS_SUPER_MAGIC => IoStrategy::LocalSync,
        // Unknown filesystem: conservatively use LocalSync.
        _ => IoStrategy::LocalSync,
    }
}

/// Latency probe threshold: a single fstatat taking >= 1 ms is considered a high-latency network mount.
///
/// io_uring's advantage comes from pipelining the wait time for N requests:
/// - RTT ~= 0.1 ms (loopback): serial N*0.1 ms ~= io_uring 1*0.1 ms; the difference is negligible, and io_uring overhead can make it slower.
/// - RTT ~= 5 ms (real SMB): serial N*5 ms >> io_uring 1*5 ms, yielding a significant benefit.
///   1 ms is an empirical cutoff; below it, io_uring provides no benefit.
const LATENCY_THRESHOLD: std::time::Duration = std::time::Duration::from_micros(1000);

/// Probe latency on a network mount by timing one fstatat.
///
/// High latency (>= LATENCY_THRESHOLD) -> NetworkAsync (io_uring is beneficial).
/// Low latency (< LATENCY_THRESHOLD) -> LocalSync (io_uring would be slower).
fn probe_latency(path: &Path) -> IoStrategy {
    let cstr = match std::ffi::CString::new(path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return IoStrategy::LocalSync,
    };
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let start = std::time::Instant::now();
    // SAFETY: cstr is NUL-terminated; st has been zeroed; AT_SYMLINK_NOFOLLOW = 0x100.
    let rc = unsafe {
        libc::fstatat(
            -1i32 as libc::c_int, // AT_FDCWD
            cstr.as_ptr(),
            &mut st,
            0x100, // AT_SYMLINK_NOFOLLOW
        )
    };
    let elapsed = start.elapsed();
    if rc < 0 {
        // fstatat failed (e.g. path does not exist): conservatively use LocalSync.
        return IoStrategy::LocalSync;
    }
    if elapsed >= LATENCY_THRESHOLD {
        IoStrategy::NetworkAsync
    } else {
        IoStrategy::LocalSync
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `strategy_from_ftype` returns only a candidate strategy (without latency probing); network filesystems return NetworkAsync.
    /// The final strategy is determined by the latency probe in `statfs_strategy`.
    #[test]
    fn test_strategy_from_ftype_known_network() {
        assert_eq!(
            strategy_from_ftype(NFS_SUPER_MAGIC),
            IoStrategy::NetworkAsync
        );
        assert_eq!(
            strategy_from_ftype(CIFS_MAGIC_NUMBER),
            IoStrategy::NetworkAsync
        );
        assert_eq!(
            strategy_from_ftype(SMB2_SUPER_MAGIC),
            IoStrategy::NetworkAsync
        );
    }

    #[test]
    fn test_strategy_from_ftype_known_local() {
        assert_eq!(strategy_from_ftype(EXT4_SUPER_MAGIC), IoStrategy::LocalSync);
        assert_eq!(strategy_from_ftype(XFS_SUPER_MAGIC), IoStrategy::LocalSync);
        assert_eq!(
            strategy_from_ftype(BTRFS_SUPER_MAGIC),
            IoStrategy::LocalSync
        );
        assert_eq!(strategy_from_ftype(TMPFS_MAGIC), IoStrategy::LocalSync);
        assert_eq!(
            strategy_from_ftype(OVERLAYFS_SUPER_MAGIC),
            IoStrategy::LocalSync
        );
    }

    #[test]
    fn test_strategy_from_ftype_fat_family() {
        assert_eq!(
            strategy_from_ftype(MSDOS_SUPER_MAGIC),
            IoStrategy::SkipNlinks
        );
        assert_eq!(
            strategy_from_ftype(EXFAT_SUPER_MAGIC),
            IoStrategy::SkipNlinks
        );
    }

    #[test]
    fn test_strategy_from_ftype_unknown_conservative_local() {
        // Unknown filesystem magic (not listed in magic.h).
        assert_eq!(strategy_from_ftype(0x12345678), IoStrategy::LocalSync);
        assert_eq!(strategy_from_ftype(0), IoStrategy::LocalSync);
        assert_eq!(strategy_from_ftype(-1), IoStrategy::LocalSync);
    }

    /// On CI, /tmp is usually ext4 or tmpfs/overlay, so it should use LocalSync.
    /// This test covers the real syscall paths in stat_dev/statfs_strategy.
    #[test]
    fn test_detect_tmpdir_returns_local_sync() {
        let tmp = std::env::temp_dir();
        let mut cache = IoStrategyCache::new();
        let strategy = cache.detect(&tmp);
        // CI should not return NetworkAsync unless SMB was mounted manually; this test makes no such assumption.
        assert_ne!(
            strategy,
            IoStrategy::NetworkAsync,
            "temp dir should not be network-mounted on CI"
        );
    }

    /// A second detect on the same directory should hit the cache (no repeated statfs call).
    /// Verify that cache.get(st_dev) returns the same result as detect.
    #[test]
    fn test_detect_caches_by_st_dev() {
        let tmp = std::env::temp_dir();
        let mut cache = IoStrategyCache::new();
        let s1 = cache.detect(&tmp);
        // A subdirectory on the same filesystem has the same st_dev, so this should hit the cache.
        let dev = stat_dev(&tmp).unwrap();
        assert_eq!(cache.get(dev), Some(s1));
        let s2 = cache.detect(&tmp);
        assert_eq!(s1, s2);
    }

    #[test]
    fn test_detect_for_fd_uses_cached_device_strategy() {
        let tmp = std::env::temp_dir();
        let fd = std::fs::File::open(&tmp).unwrap();
        let dev = stat_dev(&tmp).unwrap();
        let mut cache = IoStrategyCache::new();
        cache.insert(dev, IoStrategy::SkipNlinks);
        assert_eq!(
            cache.detect_for_fd(
                Path::new("/path/that/does/not/exist"),
                std::os::unix::io::AsRawFd::as_raw_fd(&fd)
            ),
            IoStrategy::SkipNlinks
        );
    }

    /// A nonexistent path falls back to LocalSync without panicking.
    #[test]
    fn test_detect_nonexistent_path_falls_back() {
        let mut cache = IoStrategyCache::new();
        let strategy = cache.detect(Path::new("/this/path/should/not/exist/9mf"));
        assert_eq!(strategy, IoStrategy::LocalSync);
    }

    /// A path containing a NUL byte falls back to LocalSync without panicking.
    #[test]
    fn test_detect_path_with_nul_byte_falls_back() {
        let mut cache = IoStrategyCache::new();
        // CString::new rejects embedded NUL bytes.
        let path = Path::new("/tmp/foo\0bar");
        let strategy = cache.detect(path);
        assert_eq!(strategy, IoStrategy::LocalSync);
    }

    /// probe_latency should return LocalSync for a local path (latency < 1 ms).
    #[test]
    fn test_probe_latency_local_returns_local_sync() {
        let tmp = std::env::temp_dir();
        // Local path latency should be far below 1 ms.
        assert_eq!(probe_latency(&tmp), IoStrategy::LocalSync);
    }
}
