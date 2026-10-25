//! Unix 目录扫描示例。
//!
//! 在 Linux 上自动启用 getdents64 直调 + 流式子目录分发；macOS 走 fs::read_dir fallback。
//!
//! 用法：
//! ```sh
//! cargo run --release --example scan_unix -- <path>
//! ```

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let metadata = args.iter().any(|a| a == "--metadata" || a == "-m");
    let path = args.iter().skip(1).find(|a| !a.starts_with('-')).cloned().unwrap_or_else(|| ".".to_string());

    println!("=== jwalk-meta Unix scan ===");
    println!("Path: {}", path);
    println!("Metadata: {}", if metadata { "yes" } else { "no" });

    #[cfg(not(unix))]
    {
        println!("Backend: this example targets Unix; on Windows use scan_bench.rs instead");
        return;
    }

    #[cfg(all(target_os = "linux", not(feature = "legacy-read-dir")))]
    {
        let io_uring_status = if jwalk_meta::linux_io_uring_available() {
            "io_uring batch STATX"
        } else {
            "fstatat (io_uring unavailable)"
        };
        println!("Backend: Linux getdents64 + {} (streaming subdirs enabled)", io_uring_status);
    }

    #[cfg(all(target_os = "linux", feature = "legacy-read-dir"))]
    println!("Backend: Linux std::fs::read_dir (legacy-read-dir feature ENABLED, baseline benchmark)");

    #[cfg(all(unix, not(target_os = "linux")))]
    println!("Backend: std::fs::read_dir (getdents64 not available on this platform)");


    #[cfg(unix)]
    run_scan(&path, metadata);
}

#[cfg(unix)]
fn run_scan(path: &str, metadata: bool) {
    use std::io::Write;
    use std::time::Instant;

    println!();
    let _ = std::io::stdout().flush();

    let start = Instant::now();
    let mut total_entries = 0usize;
    let mut total_dirs = 0usize;
    let mut total_files = 0usize;
    let mut total_errors = 0usize;

    let mut last_log = Instant::now();
    let log_interval = std::time::Duration::from_secs(1);

    let mut walker = jwalk_meta::WalkDir::new(path);
    if metadata {
        walker = walker.read_metadata(true);
    }

    for entry in walker {
        match entry {
            Ok(e) => {
                total_entries += 1;
                if e.file_type().is_dir() {
                    total_dirs += 1;
                } else {
                    total_files += 1;
                }
            }
            Err(ee) => {
                total_errors += 1;
                if total_errors <= 10 {
                    println!("  ERROR: {:?}", ee);
                }
            }
        }
        let now = Instant::now();
        if now.duration_since(last_log) >= log_interval {
            let elapsed_so_far = now.duration_since(start);
            let rate = total_entries as f64 / elapsed_so_far.as_secs_f64().max(1e-9);
            println!(
                "  [{:.1}s] entries: {} (dirs: {}, files: {}, errors: {}) | {:.0} entries/s",
                elapsed_so_far.as_secs_f64(),
                total_entries,
                total_dirs,
                total_files,
                total_errors,
                rate,
            );
            let _ = std::io::stdout().flush();
            last_log = now;
        }
    }

    let elapsed = start.elapsed();
    println!();
    println!("=== Results ===");
    println!("Total entries: {}", total_entries);
    println!("Directories:   {}", total_dirs);
    println!("Files:         {}", total_files);
    println!("Errors:        {}", total_errors);
    println!("Time:          {:.3}s", elapsed.as_secs_f64());
    println!(
        "Rate:          {:.0} entries/s",
        total_entries as f64 / elapsed.as_secs_f64().max(1e-9)
    );
}
