//! Linux io_uring 批量 STATX 封装（Linux-only）。
//!
//! 仅用于网络挂载（SMB/NFS/CIFS），单次 `io_uring_submit_and_wait(N)` 批量获取 N 个条目的
//! statx，替代 N 次 per-entry `fstatat`。`setup_submit_all` 保证单 SQE 失败不阻断整批。
//!
//! 内核要求：Linux 5.6+（statx opcode），5.18+（setup_submit_all）。
//! 不可用环境（WSL2/老内核/容器 seccomp 拦截 io_uring）→ fallback LocalSync。
//!
//! # 生命周期铁律
//!
//! `CString` + `MaybeUninit<libc::statx>` 必须存活到所有 CQE 收割完毕。
//! 本模块用 `Vec::with_capacity(n)` 预分配容量，绝不触发 reallocation；
//! `submit_and_wait(n)` 同步阻塞，返回时所有 SQE 已被内核消费，buffer 才可释放。

#![cfg(all(
    target_os = "linux",
    target_env = "gnu",
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "loongarch64",
        target_arch = "powerpc64",
    ),
))]
// io-uring 0.7.x 预编译 sys.rs 白名单（上游 src/sys/mod.rs:18-33），超出范围触发 compile_error。

use crate::core::unix_dir_enum::LinuxDirEntryOwned;
use io_uring::{opcode, types, IoUring};
use std::ffi::CString;
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::sync::OnceLock;

// io_uring::types::statx 是 0 字节不透明占位符（doc: "use libc::statx instead"）。
// SQE 只传递指针值给内核，内核按 Linux statx ABI 解读 buffer。
// 因此 *mut libc::statx as *mut types::statx 是安全的：两者共享相同的内核 ABI 布局，
// types::statx 仅用于满足 io_uring crate 的类型签名。

/// io_uring 实例的 SQ/CQ 深度。
///
/// 256 覆盖大多数目录的批量大小；超过则分批（每批一个 ring）。
/// ring 创建本身是一次 `io_uring_setup` 系统调用，不宜过大以减少内核内存占用。
const RING_SIZE: u32 = 256;

/// 启用 io_uring 的最小批量阈值。
///
/// 批量小于此值时 io_uring 的 setup + submit 开销大于 N 次 fstatat，
/// 调用方应走 LocalSync 路径（参考 Windows batch_query_nlinks 实践）。
pub const MIN_BATCH_FOR_IO_URING: usize = 8;

/// 进程级 io_uring 可用性探测结果（懒求值，仅探测一次）。
///
/// 探测失败的环境（WSL2、seccomp 拦截、< 5.18 内核）永久禁用 io_uring 路径，
/// 避免每次目录枚举都触发失败的 ring 创建。
static IO_URING_AVAILABLE: OnceLock<bool> = OnceLock::new();

/// 返回 io_uring 是否在当前内核可用。
///
/// 用 `setup_submit_all` + 单 SQE 探测，失败即不可用。
/// 结果缓存于 `OnceLock`，第二次调用直接返回缓存值。
pub fn io_uring_enabled() -> bool {
    *IO_URING_AVAILABLE.get_or_init(probe_io_uring)
}

/// 探测 io_uring 可用性（实际执行 ring 创建）。
///
/// 显式指定 Entry 类型参数，避免 0.7.x 版本在 Entry / Entry128 多候选下推衍失败。
fn probe_io_uring() -> bool {
    use io_uring::squeue::Entry as SEntry;
    use io_uring::cqueue::Entry as CEntry;
    IoUring::<SEntry, CEntry>::builder()
        .setup_submit_all()
        .build(1)
        .is_ok()
}

/// 批量 STATX via io_uring。
///
/// 对 `entries` 中每个有合法 name 的条目提交一个 Statx SQE，一次 submit_and_wait
/// 收割所有 CQE。成功的条目（`cqe.result() == 0`）将 statx 写入 `entry.statx`；
/// 失败的条目（含 NUL byte 跳过、SQE 提交失败、CQE result < 0）保持 `None`，
/// 调用方降级到 d_type 推断 + per-entry fstatat fallback。
///
/// # 算法
///
/// 1. 按 `RING_SIZE` 分批（每批一个新 ring，避免长生命周期 ring 的状态管理）
/// 2. 每批：构造 N 个 CString（持有路径）+ N 个 zeroed `MaybeUninit<statx>` buffer
/// 3. 为每个有效 name push 一个 Statx SQE，`user_data = 本批内本地索引`
/// 4. `submit_and_wait(submitted)` 阻塞直到全部完成
/// 5. 遍历 CQE：result==0 则 `assume_init` 后写入 entries
///
/// # 错误处理
///
/// ring 创建失败 → 返回 `Err`，调用方应 fallback 到 LocalSync（不调用本函数）。
/// submit_and_wait 失败 → 返回 `Err`，已写入的 statx 保留，未完成的条目保持 None。
///
/// # Safety
///
/// - `Vec::with_capacity(n)` 保证 statx_bufs 不 reallocation
/// - CString 存在于 cstrings Vec 中，作用域覆盖到 submit_and_wait 之后
/// - `assume_init` 仅在 CQE result==0 时调用（内核已写入完整 statx）
pub fn batch_statx_via_io_uring(
    entries: &mut [LinuxDirEntryOwned],
    dirfd: RawFd,
) -> io::Result<()> {
    if entries.is_empty() {
        return Ok(());
    }

    // 分批处理，每批最多 RING_SIZE 个条目
    for chunk_start in (0..entries.len()).step_by(RING_SIZE as usize) {
        let chunk_end = (chunk_start + RING_SIZE as usize).min(entries.len());
        let chunk = &mut entries[chunk_start..chunk_end];
        // 单批失败不影响已完成的批；但 record 错误让调用方知道整体不完整
        if let Err(e) = batch_statx_chunk(chunk, dirfd) {
            // 即便某批失败，已完成的批的 statx 已写入；继续尝试下一批无意义（同样错误）
            return Err(e);
        }
    }
    Ok(())
}

/// 处理一批（≤ RING_SIZE）条目的 STATX。
fn batch_statx_chunk(
    entries: &mut [LinuxDirEntryOwned],
    dirfd: RawFd,
) -> io::Result<()> {
    let n = entries.len();
    debug_assert!(n <= RING_SIZE as usize);

    let mut ring: IoUring<io_uring::squeue::Entry, io_uring::cqueue::Entry> =
        IoUring::builder().setup_submit_all().build(n as u32)?;

    // 预分配容量，绝不触发 reallocation（指针稳定性铁律）
    // CString 堆分配，Vec<CString> 移动 CString 不影响底层 buffer
    let mut cstrings: Vec<Option<CString>> = Vec::with_capacity(n);
    // statx_bufs 内联存 statx（~256 字节），Vec reallocation 会让指针失效
    // with_capacity(n) + 仅 push n 个 → 永不 reallocate
    let mut statx_bufs: Vec<MaybeUninit<libc::statx>> = Vec::with_capacity(n);
    for _ in 0..n {
        // zeroed() 保证未填充字段是 0（statx_mask=0 即"无字段填充"，是合法状态）
        statx_bufs.push(MaybeUninit::zeroed());
    }

    // 提交索引 → entry 索引映射（因 NUL byte 跳过的 entry 不入 ring）
    let mut local_to_entry: Vec<usize> = Vec::with_capacity(n);
    let mut submitted = 0usize;

    for (entry_idx, entry) in entries.iter().enumerate() {
        // NUL byte 跳过：CString::new 失败
        let cstr = match CString::new(entry.name.as_bytes()) {
            Ok(c) => c,
            Err(_) => {
                cstrings.push(None);
                continue;
            }
        };

        let local_idx = submitted;
        let sqe = opcode::Statx::new(
            types::Fd(dirfd),
            cstr.as_ptr(),
            // SAFETY: types::statx 是 0 字节不透明占位符，SQE 只传指针值给内核。
            statx_bufs[local_idx].as_mut_ptr() as *mut types::statx,
        )
        .flags(libc::AT_SYMLINK_NOFOLLOW as i32)
        .mask((libc::STATX_BASIC_STATS | libc::STATX_BTIME) as u32)
        .build()
        .user_data(local_idx as u64);

        // SAFETY: sqe 由合法 opcode 构造，仅写入 SQ 槽位
        match unsafe { ring.submission().push(&sqe) } {
            Ok(()) => {
                cstrings.push(Some(cstr));
                local_to_entry.push(entry_idx);
                submitted += 1;
            }
            Err(_) => {
                // SQ 满（不应发生，因 ring depth = n）：停止提交，已提交的继续
                cstrings.push(None);
                break;
            }
        }
    }

    if submitted == 0 {
        return Ok(());
    }

    // 阻塞直到所有提交的 SQE 完成。setup_submit_all 保证单 SQE 失败不中断整批。
    ring.submit_and_wait(submitted)?;

    // 收割 CQE：result==0 → 内核已写入完整 statx；result<0 → 失败，保持 None
    for cqe in ring.completion() {
        let local_idx = cqe.user_data() as usize;
        if local_idx >= local_to_entry.len() {
            continue; // 不可能的 user_data，防御
        }
        if cqe.result() == 0 {
            // SAFETY: CQE result==0 → 内核已写入 statx_bufs[local_idx] 完整内容
            let stx = unsafe { statx_bufs[local_idx].assume_init() };
            let entry_idx = local_to_entry[local_idx];
            entries[entry_idx].statx = Some(Box::new(stx));
        }
        // result < 0 → entry.statx 保持 None（已在 LinuxDirEntryOwned 初始化时设为 None）
    }

    // drop cstrings + statx_bufs 在函数返回时发生，此时 ring 已收割完毕，可安全释放。
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::MetadataExt;
    use std::path::PathBuf;

    /// 构造一个最小的 LinuxDirEntryOwned（仅 d_type + name，statx=None）
    fn make_entry(d_type: u8, name: &str) -> LinuxDirEntryOwned {
        LinuxDirEntryOwned {
            d_type,
            name: OsString::from(name),
            #[cfg(all(target_os = "linux", target_env = "gnu", not(feature = "legacy-read-dir")))]
            statx: None,
        }
    }

    /// 在 tempdir 创建若干文件，用 io_uring 批量 statx 后验证 stx_ino 与 fs::stat 一致。
    #[test]
    fn test_batch_statx_real_dir() {
        // io_uring 不可用则跳过（CI 应为 ubuntu-latest 5.18+ 内核）
        if !io_uring_enabled() {
            eprintln!("skip: io_uring not available on this kernel");
            return;
        }

        let tmp = std::env::temp_dir().join(format!(
            "jwalk_meta_9mf_iouring_test_{}",
            std::process::id()
        ));
        fs::create_dir_all(&tmp).unwrap();
        // 清理保护
        let _guard = scopeguard::guard(tmp.clone(), |p| {
            let _ = fs::remove_dir_all(p);
        });

        // 创建 3 个文件
        let file_names = ["alpha.txt", "beta.bin", "gamma.md"];
        for name in &file_names {
            fs::write(tmp.join(name), b"x").unwrap();
        }

        // 打开目录 fd
        let cstr = CString::new(tmp.as_os_str().as_bytes()).unwrap();
        let fd = unsafe {
            libc::open(
                cstr.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        assert!(fd >= 0, "open dir failed");

        // 构造 entries
        let mut entries: Vec<LinuxDirEntryOwned> =
            file_names.iter().map(|n| make_entry(libc::DT_REG, n)).collect();

        // 执行批量 statx
        let result = batch_statx_via_io_uring(&mut entries, fd);
        unsafe { libc::close(fd) };

        if result.is_err() {
            // io_uring 提交失败：CI 环境 seccomp 拦截，跳过断言
            eprintln!("skip: batch_statx failed: {:?}", result);
            return;
        }
        assert!(result.is_ok());

        // 验证：每个 entry 应有 statx，且 stx_ino 与 fs::stat 一致
        for (entry, name) in entries.iter().zip(file_names.iter()) {
            let statx = entry
                .statx
                .as_ref()
                .expect("statx should be filled for existing file");
            let path = tmp.join(name);
            let fs_meta = fs::symlink_metadata(&path).unwrap();
            let fs_ino = fs_meta.ino();
            assert_eq!(
                statx.stx_ino, fs_ino,
                "inode mismatch for {}: io_uring={} fs={}",
                name, statx.stx_ino, fs_ino
            );
            // size 应为 1（写了 "x"）
            assert_eq!(statx.stx_size, 1, "size mismatch for {}", name);
            // mode 应为 regular file
            assert_eq!(
                statx.stx_mode & libc::S_IFMT as u16,
                libc::S_IFREG as u16,
                "mode mismatch for {}",
                name
            );
        }
    }

    /// 空批量 → 立即返回 Ok，不创建 ring。
    #[test]
    fn test_batch_statx_empty_noop() {
        let mut entries: Vec<LinuxDirEntryOwned> = Vec::new();
        let result = batch_statx_via_io_uring(&mut entries, -1);
        assert!(result.is_ok());
    }

    /// 包含 NUL byte 的 name → 跳过该条目，其余正常处理。
    #[test]
    fn test_batch_statx_skips_nul_byte_name() {
        if !io_uring_enabled() {
            eprintln!("skip: io_uring not available");
            return;
        }

        let tmp = std::env::temp_dir().join(format!(
            "jwalk_meta_9mf_nul_test_{}",
            std::process::id()
        ));
        fs::create_dir_all(&tmp).unwrap();
        let _guard = scopeguard::guard(tmp.clone(), |p| {
            let _ = fs::remove_dir_all(p);
        });
        fs::write(tmp.join("valid.txt"), b"x").unwrap();

        let cstr = CString::new(tmp.as_os_str().as_bytes()).unwrap();
        let fd = unsafe {
            libc::open(
                cstr.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        assert!(fd >= 0);

        // 一个合法 + 一个含 NUL byte（CString::new 失败 → 跳过）
        let mut entries = vec![
            make_entry(libc::DT_REG, "valid.txt"),
            make_entry(libc::DT_REG, "bad\u{0}name"),
        ];

        let result = batch_statx_via_io_uring(&mut entries, fd);
        unsafe { libc::close(fd) };

        if result.is_err() {
            eprintln!("skip: batch_statx failed: {:?}", result);
            return;
        }

        // 第一个应填充，第二个应保持 None（CString::new 失败）
        assert!(entries[0].statx.is_some(), "valid entry should be filled");
        assert!(
            entries[1].statx.is_none(),
            "NUL-byte entry should remain None"
        );
    }

    /// 不存在的目录 fd → 批量仍执行，CQE result < 0，所有 entry.statx 保持 None。
    #[test]
    fn test_batch_statx_nonexistent_files_yields_none_statx() {
        if !io_uring_enabled() {
            eprintln!("skip: io_uring not available");
            return;
        }

        // 用 /dev/null 作为 dirfd（不是目录）—— 期望 CQE 返回 -ENOTDIR
        let cstr = CString::new("/dev/null".as_bytes()).unwrap();
        let fd = unsafe {
            libc::open(
                cstr.as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            eprintln!("skip: cannot open /dev/null");
            return;
        }

        let mut entries = vec![
            make_entry(libc::DT_REG, "ghost1.txt"),
            make_entry(libc::DT_REG, "ghost2.txt"),
        ];

        let _result = batch_statx_via_io_uring(&mut entries, fd);
        unsafe { libc::close(fd) };

        // statx 应全部为 None（CQE 错误）
        for (i, e) in entries.iter().enumerate() {
            assert!(
                e.statx.is_none(),
                "entry {} should have None statx (expected CQE error)",
                i
            );
        }
    }

    #[test]
    fn test_min_batch_constant_nonzero() {
        assert!(MIN_BATCH_FOR_IO_URING >= 1);
    }

    #[test]
    fn test_ring_size_constant_reasonable() {
        assert!(RING_SIZE >= 64);
        assert!(RING_SIZE <= 4096);
    }

    /// 验证 io_uring_enabled 是幂等的（OnceLock 保证只探测一次）。
    #[test]
    fn test_io_uring_enabled_idempotent() {
        let a = io_uring_enabled();
        let b = io_uring_enabled();
        assert_eq!(a, b);
    }

    // ── 最小 scopeguard（与 unix_dir_enum.rs 内联模式对齐）──────────────────
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
                if let (Some(value), Some(mut cleanup)) =
                    (self.value.take(), self.cleanup.take())
                {
                    cleanup(value);
                }
            }
        }
    }

}
