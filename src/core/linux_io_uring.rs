//! Linux-only wrapper for batched STATX via io_uring.
//!
//! Used only for network mounts (SMB/NFS/CIFS). A single `io_uring_submit_and_wait(N)` fetches
//! statx for N entries, replacing N per-entry `fstatat` calls. `setup_submit_all` ensures one failed SQE does not block the batch.
//!
//! Kernel requirements: Linux 5.6+ (statx opcode), 5.18+ (setup_submit_all).
//! If unavailable (WSL2, older kernels, or container seccomp blocking io_uring), fall back to LocalSync.
//!
//! # Lifetime requirements
//!
//! `CString` and `MaybeUninit<libc::statx>` must remain alive until all CQEs have been reaped.
//! This module preallocates with `Vec::with_capacity(n)` to prevent reallocation;
//! `submit_and_wait(n)` blocks synchronously, and buffers can be released only after it returns and the kernel has consumed all SQEs.

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
// io-uring 0.7.x prebuilt sys.rs architecture allowlist (upstream src/sys/mod.rs:18-33); other targets trigger compile_error.

use crate::core::unix_dir_enum::{LinuxDirEntryOwned, LinuxStatxResults};
use io_uring::{IoUring, opcode, types};
use std::ffi::CString;
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::sync::OnceLock;

// io_uring::types::statx is a zero-sized opaque placeholder (docs: "use libc::statx instead").
// The SQE passes only a pointer to the kernel, which interprets the buffer according to the Linux statx ABI.
// Therefore, casting *mut libc::statx to *mut types::statx is safe: both use the same kernel ABI layout,
// and types::statx is used only to satisfy the io_uring crate's type signature.

/// SQ/CQ depth of an io_uring instance.
///
/// 256 covers the batch size of most directories; larger batches are split (one ring per batch).
/// Ring creation itself requires an `io_uring_setup` syscall, so avoid larger rings to reduce kernel memory use.
const RING_SIZE: u32 = 256;

/// Minimum batch size for enabling io_uring.
///
/// Below this batch size, io_uring setup and submission cost more than N fstatat calls,
/// so callers should use the LocalSync path (based on the Windows batch_query_nlinks experience).
pub const MIN_BATCH_FOR_IO_URING: usize = 8;

/// Process-wide io_uring availability result (lazy evaluation; probed only once).
///
/// Permanently disable io_uring on environments where probing fails (WSL2, seccomp blocking, or kernels older than 5.18),
/// avoiding a failed ring creation on every directory enumeration.
static IO_URING_AVAILABLE: OnceLock<bool> = OnceLock::new();

/// Return whether io_uring is available on the current kernel.
///
/// Probe with `setup_submit_all` and a single SQE; failure means it is unavailable.
/// The result is cached in `OnceLock`, so subsequent calls return it directly.
pub fn io_uring_enabled() -> bool {
    *IO_URING_AVAILABLE.get_or_init(probe_io_uring)
}

/// Probe io_uring availability by actually creating a ring.
///
/// Specify the Entry type parameters explicitly to avoid inference failures with the multiple Entry / Entry128 candidates in version 0.7.x.
fn probe_io_uring() -> bool {
    use io_uring::cqueue::Entry as CEntry;
    use io_uring::squeue::Entry as SEntry;
    IoUring::<SEntry, CEntry>::builder()
        .setup_submit_all()
        .build(1)
        .is_ok()
}

/// Run batched STATX via io_uring.
///
/// Submit one Statx SQE for each entry in `entries` with a valid name, then reap all CQEs with one submit_and_wait.
/// Successful entries (`cqe.result() == 0`) are written to their corresponding result slots;
/// failed entries (including skipped NUL bytes, SQE submission failures, and CQE result < 0) remain `None`,
/// so the caller can fall back to d_type inference and per-entry statx/fstatat.
///
/// # Algorithm
///
/// 1. Split into batches of `RING_SIZE` (create a new ring per batch to avoid long-lived ring state management).
/// 2. For each batch, create N CStrings (holding the paths) and N zeroed `MaybeUninit<statx>` buffers.
/// 3. Push one Statx SQE per valid name, with `user_data` set to its local index in the batch.
/// 4. `submit_and_wait(submitted)` blocks until all requests finish.
/// 5. Iterate over the CQEs: for result==0, call `assume_init` and write the result to entries.
///
/// # Error handling
///
/// Ring creation failure -> return `Err`; callers should fall back to LocalSync (do not call this function).
/// submit_and_wait failure -> return `Err`; already-written results are retained, and incomplete entries remain None.
///
/// # Safety
///
/// - `Vec::with_capacity(n)` ensures statx_bufs is not reallocated.
/// - CStrings remain in the cstrings Vec until after submit_and_wait returns.
/// - Call `assume_init` only when CQE result==0 (the kernel has written a complete statx).
pub fn batch_statx_via_io_uring(
    entries: &mut [LinuxDirEntryOwned],
    dirfd: RawFd,
    results: &mut LinuxStatxResults,
) -> io::Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    results.results.clear();
    results.results.resize_with(entries.len(), || None);

    // Process in batches of at most RING_SIZE entries.
    for chunk_start in (0..entries.len()).step_by(RING_SIZE as usize) {
        let chunk_end = (chunk_start + RING_SIZE as usize).min(entries.len());
        let chunk = &mut entries[chunk_start..chunk_end];
        // A failed batch does not affect completed batches, but record the error so the caller knows the result is incomplete.
        batch_statx_chunk(chunk, chunk_start, dirfd, results)?;
    }
    Ok(())
}

/// Run STATX for one batch of at most RING_SIZE entries.
fn batch_statx_chunk(
    entries: &mut [LinuxDirEntryOwned],
    chunk_start: usize,
    dirfd: RawFd,
    results: &mut LinuxStatxResults,
) -> io::Result<()> {
    let n = entries.len();
    debug_assert!(n <= RING_SIZE as usize);

    let mut ring: IoUring<io_uring::squeue::Entry, io_uring::cqueue::Entry> =
        IoUring::builder().setup_submit_all().build(n as u32)?;

    // Preallocate capacity and never reallocate (pointer stability is essential).
    // CString uses a heap allocation, so moving a CString in Vec<CString> does not move its underlying buffer.
    let mut cstrings: Vec<Option<CString>> = Vec::with_capacity(n);
    // statx_bufs stores statx inline (~256 bytes); Vec reallocation would invalidate pointers.
    // with_capacity(n) followed by at most n pushes guarantees no reallocation.
    let mut statx_bufs: Vec<MaybeUninit<libc::statx>> = Vec::with_capacity(n);
    for _ in 0..n {
        // zeroed() ensures unfilled fields are 0 (statx_mask=0, meaning "no fields filled", is valid).
        statx_bufs.push(MaybeUninit::zeroed());
    }

    // Map submission indices to entry indices (entries skipped due to NUL bytes are not submitted).
    let mut local_to_entry: Vec<usize> = Vec::with_capacity(n);
    let mut submitted = 0usize;

    for (entry_idx, entry) in entries.iter().enumerate() {
        // Skip NUL bytes: CString::new fails.
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
            // SAFETY: types::statx is a zero-sized opaque placeholder; the SQE passes only a pointer to the kernel.
            statx_bufs[local_idx].as_mut_ptr() as *mut types::statx,
        )
        .flags(libc::AT_SYMLINK_NOFOLLOW)
        .mask(libc::STATX_BASIC_STATS | libc::STATX_BTIME)
        .build()
        .user_data(local_idx as u64);

        // SAFETY: sqe was built with a valid opcode and is written only to an SQ slot.
        match unsafe { ring.submission().push(&sqe) } {
            Ok(()) => {
                cstrings.push(Some(cstr));
                local_to_entry.push(entry_idx);
                submitted += 1;
            }
            Err(_) => {
                // SQ is full (unexpected because ring depth = n): stop submitting, then continue with submitted requests.
                cstrings.push(None);
                break;
            }
        }
    }

    if submitted == 0 {
        return Ok(());
    }

    // Block until all submitted SQEs complete. setup_submit_all ensures one failed SQE does not interrupt the batch.
    ring.submit_and_wait(submitted)?;

    // Reap CQEs: result==0 means the kernel wrote a complete statx; result<0 means failure, so leave None.
    for cqe in ring.completion() {
        let local_idx = cqe.user_data() as usize;
        if local_idx >= local_to_entry.len() {
            continue; // Defensive handling for an unexpected user_data value.
        }
        if cqe.result() == 0 {
            // SAFETY: CQE result==0 means the kernel wrote a complete value to statx_bufs[local_idx].
            let stx = unsafe { statx_bufs[local_idx].assume_init() };
            let entry_idx = local_to_entry[local_idx];
            results.results[chunk_start + entry_idx] = Some(stx);
        }
        // result < 0: leave the result slot as None so the caller uses the fstatat fallback.
    }

    // cstrings and statx_bufs are dropped when this function returns; all CQEs have been reaped, so they can be safely released.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::MetadataExt;

    /// Construct a minimal LinuxDirEntryOwned (d_type and name only; statx=None).
    fn make_entry(d_type: u8, name: &str) -> LinuxDirEntryOwned {
        LinuxDirEntryOwned {
            d_type,
            name: OsString::from(name),
        }
    }

    /// Create several files in a tempdir, batch-stat them with io_uring, and verify stx_ino matches fs::stat.
    #[test]
    fn test_batch_statx_real_dir() {
        // Skip if io_uring is unavailable (CI should use an ubuntu-latest kernel >= 5.18).
        if !io_uring_enabled() {
            eprintln!("skip: io_uring not available on this kernel");
            return;
        }

        let tmp = std::env::temp_dir().join(format!(
            "jwalk_meta_9mf_iouring_test_{}",
            std::process::id()
        ));
        fs::create_dir_all(&tmp).unwrap();
        // Cleanup guard.
        let _guard = scopeguard::guard(tmp.clone(), |p| {
            let _ = fs::remove_dir_all(p);
        });

        // Create three files.
        let file_names = ["alpha.txt", "beta.bin", "gamma.md"];
        for name in &file_names {
            fs::write(tmp.join(name), b"x").unwrap();
        }

        // Open the directory fd.
        let cstr = CString::new(tmp.as_os_str().as_bytes()).unwrap();
        let fd = unsafe {
            libc::open(
                cstr.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        assert!(fd >= 0, "open dir failed");

        // Construct entries.
        let mut entries: Vec<LinuxDirEntryOwned> = file_names
            .iter()
            .map(|n| make_entry(libc::DT_REG, n))
            .collect();

        // Run batched statx.
        let mut statx_results = LinuxStatxResults::default();
        let result = batch_statx_via_io_uring(&mut entries, fd, &mut statx_results);
        unsafe { libc::close(fd) };

        if result.is_err() {
            // io_uring submission failed (seccomp may block it in CI); skip the assertions.
            eprintln!("skip: batch_statx failed: {:?}", result);
            return;
        }
        assert!(result.is_ok());

        // Verify that each entry has statx and that stx_ino matches fs::stat.
        for (name, statx) in file_names.iter().zip(statx_results.results.iter()) {
            let statx = statx
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
            // Size should be 1 (the file contains "x").
            assert_eq!(statx.stx_size, 1, "size mismatch for {}", name);
            // Mode should indicate a regular file.
            assert_eq!(
                statx.stx_mode & libc::S_IFMT as u16,
                libc::S_IFREG as u16,
                "mode mismatch for {}",
                name
            );
        }
    }

    /// An empty batch returns Ok immediately without creating a ring.
    #[test]
    fn test_batch_statx_empty_noop() {
        let mut entries: Vec<LinuxDirEntryOwned> = Vec::new();
        let mut statx_results = LinuxStatxResults::default();
        let result = batch_statx_via_io_uring(&mut entries, -1, &mut statx_results);
        assert!(result.is_ok());
    }

    /// An entry with a NUL byte in its name is skipped; the remaining entries are processed normally.
    #[test]
    fn test_batch_statx_skips_nul_byte_name() {
        if !io_uring_enabled() {
            eprintln!("skip: io_uring not available");
            return;
        }

        let tmp =
            std::env::temp_dir().join(format!("jwalk_meta_9mf_nul_test_{}", std::process::id()));
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

        // One valid name and one containing a NUL byte (CString::new fails, so it is skipped).
        let mut entries = vec![
            make_entry(libc::DT_REG, "valid.txt"),
            make_entry(libc::DT_REG, "bad\u{0}name"),
        ];

        let mut statx_results = LinuxStatxResults::default();
        let result = batch_statx_via_io_uring(&mut entries, fd, &mut statx_results);
        unsafe { libc::close(fd) };

        if result.is_err() {
            eprintln!("skip: batch_statx failed: {:?}", result);
            return;
        }

        // The first result should be filled; the second should remain None (CString::new failed).
        assert!(
            statx_results.results[0].is_some(),
            "valid entry should be filled"
        );
        assert!(
            statx_results.results[1].is_none(),
            "NUL-byte entry should remain None"
        );
    }

    /// A non-directory fd produces a CQE error, leaving result slots as None.
    #[test]
    fn test_batch_statx_nonexistent_files_yields_none_statx() {
        if !io_uring_enabled() {
            eprintln!("skip: io_uring not available");
            return;
        }

        // Use /dev/null as dirfd (it is not a directory); expect the CQE to return -ENOTDIR.
        let cstr = CString::new("/dev/null".as_bytes()).unwrap();
        let fd = unsafe { libc::open(cstr.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            eprintln!("skip: cannot open /dev/null");
            return;
        }

        let mut entries = vec![
            make_entry(libc::DT_REG, "ghost1.txt"),
            make_entry(libc::DT_REG, "ghost2.txt"),
        ];

        let mut statx_results = LinuxStatxResults::default();
        let _result = batch_statx_via_io_uring(&mut entries, fd, &mut statx_results);
        unsafe { libc::close(fd) };

        // All statx results should be None (CQE errors).
        for i in 0..entries.len() {
            assert!(
                statx_results.results[i].is_none(),
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

    /// Verify that io_uring_enabled is idempotent (OnceLock ensures it is probed only once).
    #[test]
    fn test_io_uring_enabled_idempotent() {
        let a = io_uring_enabled();
        let b = io_uring_enabled();
        assert_eq!(a, b);
    }

    // ── Minimal scopeguard (matches the inline pattern in unix_dir_enum.rs) ──────────────────
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
}
