//! 文件系统类型检测与 I/O 策略派发（Linux-only）。
//!
//! 用 `statfs(2)` 检测路径所在文件系统的 `f_type`，决定是否启用 io_uring 批量 STATX。
//! 仅对**高延迟**网络挂载（SMB/NFS/CIFS）启用 io_uring；低延迟网络挂载和本地文件系统
//! 走串行 fstatat（io_uring 在低延迟场景有额外开销，反而更慢）。
//!
//! 延迟探测：首次遇到网络 FS 时做一次 `fstatat` 计时，≥ 1ms 走 io_uring，< 1ms 走 LocalSync。
//! 缓存策略：per-worker `HashMap<st_dev, IoStrategy>`。同一 `st_dev` 只探测一次。

#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

// ── f_type 常量（来自 linux/magic.h，避免依赖 libc 是否导出这些常量）──────────

/// SMB_SUPER_MAGIC — Linux SMB1 client mount
const SMB_SUPER_MAGIC: i64 = 0x517B;
/// NFS_SUPER_MAGIC — NFSv2/v3/v4
const NFS_SUPER_MAGIC: i64 = 0x6969;
/// CIFS_MAGIC_NUMBER — Linux CIFS/SMB1 client mount
const CIFS_MAGIC_NUMBER: i64 = 0xFF534D42;
/// SMB2_SUPER_MAGIC — Linux SMB2/SMB3 client mount（vers=2.0/2.1/3.0/3.1.1）
const SMB2_SUPER_MAGIC: i64 = 0xFE534D42;
/// EXT4_SUPER_MAGIC（也为 ext2/ext3）
const EXT4_SUPER_MAGIC: i64 = 0xEF53;
/// XFS_SUPER_MAGIC
const XFS_SUPER_MAGIC: i64 = 0x58465342;
/// BTRFS_SUPER_MAGIC
const BTRFS_SUPER_MAGIC: i64 = 0x9123683E;
/// TMPFS_MAGIC — /tmp、容器 overlay 上层常用
const TMPFS_MAGIC: i64 = 0x01021994;
/// OVERLAYFS_SUPER_MAGIC — 容器场景
const OVERLAYFS_SUPER_MAGIC: i64 = 0x794C7630;
/// MSDOS_SUPER_MAGIC — FAT12/16/32
const MSDOS_SUPER_MAGIC: i64 = 0x4D44;
/// EXFAT_SUPER_MAGIC
const EXFAT_SUPER_MAGIC: i64 = 0x2011BAB0;

/// 目录元数据获取策略。
///
/// 选择标准：网络挂载走 io_uring 批量 STATX（节省 N 次 RTT）；本地 FS 走串行 fstatat
///（MFT/inode 缓存命中后无收益，且 NTFS MFT 锁让并发变慢——见历史回档 5367f20）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoStrategy {
    /// 串行 per-entry `fstatat`。适用于本地 FS 和低延迟网络挂载。
    LocalSync,
    /// io_uring 批量 STATX。适用于高延迟网络挂载（单次 submit_and_wait 收割 N 个 CQE）。
    NetworkAsync,
    /// FAT 系列（FAT32/exFAT）：不支持硬链接，`st_nlink` 恒为 1，跳过查询。
    SkipNlinks,
}

/// Per-worker I/O 策略缓存。Keyed by `st_dev`（来自 `stat(2)`）。
///
/// rayon 线程池中每个 worker 持有独立缓存，避免锁竞争。同一设备只 statfs 一次。
#[derive(Debug, Default)]
pub struct IoStrategyCache {
    map: HashMap<u64, IoStrategy>,
}

impl IoStrategyCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// 已缓存的设备直接返回（测试用）。
    #[cfg(test)]
    pub fn get(&self, dev: u64) -> Option<IoStrategy> {
        self.map.get(&dev).copied()
    }

    /// 插入缓存（测试用）。
    #[cfg(test)]
    pub fn insert(&mut self, dev: u64, strategy: IoStrategy) {
        self.map.insert(dev, strategy);
    }

    /// 检测路径所在文件系统的 I/O 策略。结果按 `st_dev` 缓存。
    ///
    /// 流程：`stat(path)` → `st_dev` → cache lookup → 命中返回；
    /// 未命中则 `statfs(path)` → `f_type` → 策略 → 写缓存。
    ///
    /// 任何 syscall 失败（路径不存在 / 权限不足）保守返回 `LocalSync`。
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
}

/// 用 `stat(2)` 取 `st_dev`。
fn stat_dev(path: &Path) -> io::Result<u64> {
    let cstr = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"))?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: cstr NUL 终止；st 已 zeroed。
    let rc = unsafe { libc::stat(cstr.as_ptr(), &mut st) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(st.st_dev)
    }
}

/// 用 `statfs(2)` 取 `f_type` 并映射到 `IoStrategy`。
///
/// 网络 FS 会额外做一次延迟探测：对路径做 `fstatat` 计时，
/// ≥ `LATENCY_THRESHOLD` 走 `NetworkAsync`，否则降级为 `LocalSync`。
fn statfs_strategy(path: &Path) -> Option<IoStrategy> {
    let cstr = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut sfs: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: cstr NUL 终止；sfs 已 zeroed。
    let rc = unsafe { libc::statfs(cstr.as_ptr(), &mut sfs) };
    if rc < 0 {
        return None;
    }
    let preliminary = strategy_from_ftype(sfs.f_type as i64);
    if preliminary != IoStrategy::NetworkAsync {
        return Some(preliminary);
    }
    // 网络 FS → 探测延迟决定是否启用 io_uring
    Some(probe_latency(path))
}

/// 由 `f_type` 推断策略（不含延迟探测）。
fn strategy_from_ftype(f_type: i64) -> IoStrategy {
    match f_type {
        // 网络 FS → 候选 io_uring（最终由延迟探测决定）
        SMB_SUPER_MAGIC | NFS_SUPER_MAGIC | CIFS_MAGIC_NUMBER | SMB2_SUPER_MAGIC => IoStrategy::NetworkAsync,
        // FAT 系列 → 跳过 nlink 查询（nlink 恒为 1）
        MSDOS_SUPER_MAGIC | EXFAT_SUPER_MAGIC => IoStrategy::SkipNlinks,
        // 本地 FS → 串行 fstatat
        EXT4_SUPER_MAGIC
        | XFS_SUPER_MAGIC
        | BTRFS_SUPER_MAGIC
        | TMPFS_MAGIC
        | OVERLAYFS_SUPER_MAGIC => IoStrategy::LocalSync,
        // 未知 FS → 保守 LocalSync
        _ => IoStrategy::LocalSync,
    }
}

/// 延迟探测阈值：单次 fstatat ≥ 1ms 视为高延迟网络。
///
/// io_uring 的优势来自流水线化 N 个请求的等待时间：
/// - RTT ≈ 0.1ms（loopback）：串行 N×0.1ms ≈ io_uring 1×0.1ms，差距可忽略，io_uring 额外开销反而更慢
/// - RTT ≈ 5ms（真实 SMB）：串行 N×5ms >> io_uring 1×5ms，收益显著
/// 1ms 是经验分界线：低于此阈值 io_uring 无收益。
const LATENCY_THRESHOLD: std::time::Duration = std::time::Duration::from_micros(1000);

/// 对网络挂载路径做一次 fstatat 探测延迟。
///
/// 高延迟（≥ LATENCY_THRESHOLD）→ NetworkAsync（io_uring 有收益）
/// 低延迟（< LATENCY_THRESHOLD）→ LocalSync（io_uring 反而更慢）
fn probe_latency(path: &Path) -> IoStrategy {
    let cstr = match std::ffi::CString::new(path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return IoStrategy::LocalSync,
    };
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let start = std::time::Instant::now();
    // SAFETY: cstr NUL 终止；st 已 zeroed；AT_SYMLINK_NOFOLLOW = 0x100。
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
        // fstatat 失败（路径不存在等）→ 保守 LocalSync
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

    /// `strategy_from_ftype` 只返回候选策略（不含延迟探测），网络 FS 返回 NetworkAsync。
    /// 最终策略由 `statfs_strategy` 中的延迟探测决定。
    #[test]
    fn test_strategy_from_ftype_known_network() {
        assert_eq!(strategy_from_ftype(NFS_SUPER_MAGIC), IoStrategy::NetworkAsync);
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
        assert_eq!(strategy_from_ftype(BTRFS_SUPER_MAGIC), IoStrategy::LocalSync);
        assert_eq!(strategy_from_ftype(TMPFS_MAGIC), IoStrategy::LocalSync);
        assert_eq!(
            strategy_from_ftype(OVERLAYFS_SUPER_MAGIC),
            IoStrategy::LocalSync
        );
    }

    #[test]
    fn test_strategy_from_ftype_fat_family() {
        assert_eq!(strategy_from_ftype(MSDOS_SUPER_MAGIC), IoStrategy::SkipNlinks);
        assert_eq!(strategy_from_ftype(EXFAT_SUPER_MAGIC), IoStrategy::SkipNlinks);
    }

    #[test]
    fn test_strategy_from_ftype_unknown_conservative_local() {
        // 未知 FS 号（不在 magic.h 中）
        assert_eq!(strategy_from_ftype(0x12345678), IoStrategy::LocalSync);
        assert_eq!(strategy_from_ftype(0), IoStrategy::LocalSync);
        assert_eq!(strategy_from_ftype(-1), IoStrategy::LocalSync);
    }

    /// CI 上 /tmp 通常为 ext4 或 tmpfs/overlay → LocalSync。
    /// 此测试覆盖 stat_dev/statfs_strategy 真实 syscall 路径。
    #[test]
    fn test_detect_tmpdir_returns_local_sync() {
        let tmp = std::env::temp_dir();
        let mut cache = IoStrategyCache::new();
        let strategy = cache.detect(&tmp);
        // CI 上不会是 NetworkAsync（除非人为挂载 SMB，本测试不假设）
        assert_ne!(
            strategy,
            IoStrategy::NetworkAsync,
            "temp dir should not be network-mounted on CI"
        );
    }

    /// 同目录第二次 detect 应命中缓存（不重复 statfs）。
    /// 验证方式：cache.get(st_dev) 应返回与 detect 相同的结果。
    #[test]
    fn test_detect_caches_by_st_dev() {
        let tmp = std::env::temp_dir();
        let mut cache = IoStrategyCache::new();
        let s1 = cache.detect(&tmp);
        // 同目录下创建子目录，st_dev 相同 → 缓存命中
        let dev = stat_dev(&tmp).unwrap();
        assert_eq!(cache.get(dev), Some(s1));
        let s2 = cache.detect(&tmp);
        assert_eq!(s1, s2);
    }

    /// 不存在路径 → 保守 LocalSync（不 panic）。
    #[test]
    fn test_detect_nonexistent_path_falls_back() {
        let mut cache = IoStrategyCache::new();
        let strategy = cache.detect(Path::new("/this/path/should/not/exist/9mf"));
        assert_eq!(strategy, IoStrategy::LocalSync);
    }

    /// NUL 字节路径 → 保守 LocalSync（不 panic）。
    #[test]
    fn test_detect_path_with_nul_byte_falls_back() {
        let mut cache = IoStrategyCache::new();
        // CString::new 会拒绝内嵌 NUL
        let path = Path::new("/tmp/foo\0bar");
        let strategy = cache.detect(path);
        assert_eq!(strategy, IoStrategy::LocalSync);
    }

    /// probe_latency 对本地路径应返回 LocalSync（延迟 < 1ms）。
    #[test]
    fn test_probe_latency_local_returns_local_sync() {
        let tmp = std::env::temp_dir();
        // 本地路径延迟应远低于 1ms
        assert_eq!(probe_latency(&tmp), IoStrategy::LocalSync);
    }
}
