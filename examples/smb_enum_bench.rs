//! SMB 单目录枚举 benchmark。
//!
//! 使用 WalkDir(max_depth=1) 测试单目录枚举性能，
//! 内部调用 enumerate_dir，eprintln 会输出检测到的缓冲区大小。
//!
//! Usage: cargo +nightly run --example smb_enum_bench -- <path>

use std::time::Instant;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"Z:\品质部".to_string());

    println!("=== SMB 单目录枚举 benchmark ===");
    println!("Path: {}", path);
    println!();

    // 第一次运行 (cold)
    println!("--- Cold run (max_depth=1) ---");
    let start = Instant::now();
    let mut total = 0usize;
    let mut files = 0usize;
    let mut dirs = 0usize;
    let mut errors = 0usize;
    for entry in jwalk_meta::WalkDir::new(&path).max_depth(1) {
        match entry {
            Ok(e) => {
                total += 1;
                if e.file_type().is_dir() {
                    dirs += 1;
                } else {
                    files += 1;
                }
            }
            Err(_) => errors += 1,
        }
    }
    let elapsed = start.elapsed();
    println!(
        "Total: {} (files: {}, dirs: {}, errors: {})",
        total, files, dirs, errors
    );
    println!("Time:  {:.3}s", elapsed.as_secs_f64());
    println!(
        "Rate:  {:.0} entries/s",
        total as f64 / elapsed.as_secs_f64().max(0.001)
    );

    // 第二次运行 (warm)
    println!();
    println!("--- Warm run (max_depth=1) ---");
    let start2 = Instant::now();
    let mut total2 = 0usize;
    for entry in jwalk_meta::WalkDir::new(&path).max_depth(1) {
        let _ = entry;
        total2 += 1;
    }
    let elapsed2 = start2.elapsed();
    println!("Total: {}", total2);
    println!("Time:  {:.3}s", elapsed2.as_secs_f64());
    println!(
        "Rate:  {:.0} entries/s",
        total2 as f64 / elapsed2.as_secs_f64().max(0.001)
    );

    println!();
    println!("=== Summary ===");
    println!("Cold: {:.3}s", elapsed.as_secs_f64());
    println!("Warm: {:.3}s", elapsed2.as_secs_f64());
}
