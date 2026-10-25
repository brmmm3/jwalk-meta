//! Ext+hardlink 模式 D 盘扫描基准测试
//!
//! 用法: cargo run --example scan_bench_ext -- D:\
//!
//! 对比三种模式:
//!   1. Base (仅目录枚举，无 metadata)
//!   2. Ext (metadata_ext=true, 无 hardlink)
//!   3. Ext+Hardlink (metadata_ext=true + hardlink_info=true)

use std::time::Instant;
use std::io::Write;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| r"D:\".to_string());

    println!("=== jwalk-meta Ext/Hardlink scan benchmark ===");
    println!("Path: {}", path);
    println!();

    // ── Run 1: Base (仅目录枚举) ──────────────────────────────────
    println!("[1/3] Base mode (目录枚举，无 metadata)...");
    let _ = std::io::stdout().flush();
    let (base_entries, base_errors, base_time) = scan_base(&path);
    println!(
        "  -> {} entries, {} errors, {:.3}s",
        base_entries, base_errors, base_time
    );

    // ── Run 2: Ext (metadata_ext=true, 无 hardlink) ───────────────
    println!("[2/3] Ext mode (metadata_ext，无 hardlink)...");
    let _ = std::io::stdout().flush();
    let (ext_entries, ext_errors, ext_time) = scan_ext(&path);
    println!(
        "  -> {} entries, {} errors, {:.3}s",
        ext_entries, ext_errors, ext_time
    );

    // ── Run 3: Ext+Hardlink (metadata_ext + hardlink_info) ────────
    println!("[3/3] Ext+Hardlink mode (metadata_ext + hardlink_info)...");
    let _ = std::io::stdout().flush();
    let (hl_entries, hl_errors, hl_time, hl_with_nlink) = scan_ext_hardlink(&path);
    println!(
        "  -> {} entries, {} errors, {:.3}s (有 nlink: {})",
        hl_entries, hl_errors, hl_time, hl_with_nlink
    );

    // ── 第二轮 (warm cache) ───────────────────────────────────────
    println!();
    println!("=== 第二轮 (warm cache) ===");
    let (_, _, base_time2) = scan_base(&path);
    let (_, _, ext_time2) = scan_ext(&path);
    let (_, _, hl_time2, _) = scan_ext_hardlink(&path);
    println!("  Base:          {:.3}s", base_time2);
    println!("  Ext:           {:.3}s", ext_time2);
    println!("  Ext+Hardlink:  {:.3}s", hl_time2);

    // ── 汇总 ──────────────────────────────────────────────────────
    println!();
    println!("=== 汇总 ===");
    println!("模式              | Cold (s) | Warm (s) | Entries | Ext开销 | HL开销");
    println!("------------------|----------|----------|---------|---------|-------");
    println!(
        "Base              | {:>8.3} | {:>8.3} | {:>7} |    -    |   -",
        base_time, base_time2, base_entries
    );
    println!(
        "Ext               | {:>8.3} | {:>8.3} | {:>7} | {:>+6.3}s |   -",
        ext_time, ext_time2, ext_entries, ext_time - base_time
    );
    println!(
        "Ext+Hardlink      | {:>8.3} | {:>8.3} | {:>7} | {:>+6.3}s | {:>+5.3}s",
        hl_time, hl_time2, hl_entries, hl_time - base_time, hl_time - ext_time
    );
}

fn scan_base(path: &str) -> (usize, usize, f64) {
    let start = Instant::now();
    let mut entries = 0usize;
    let mut errors = 0usize;

    for entry in jwalk_meta::WalkDir::new(path) {
        match entry {
            Ok(_) => entries += 1,
            Err(_) => errors += 1,
        }
    }

    (entries, errors, start.elapsed().as_secs_f64())
}

fn scan_ext(path: &str) -> (usize, usize, f64) {
    let start = Instant::now();
    let mut entries = 0usize;
    let mut errors = 0usize;

    for entry in jwalk_meta::WalkDir::new(path).read_metadata_ext(true) {
        match entry {
            Ok(_) => entries += 1,
            Err(_) => errors += 1,
        }
    }

    (entries, errors, start.elapsed().as_secs_f64())
}

fn scan_ext_hardlink(path: &str) -> (usize, usize, f64, usize) {
    let start = Instant::now();
    let mut entries = 0usize;
    let mut errors = 0usize;
    let mut with_nlink = 0usize;

    for entry in jwalk_meta::WalkDir::new(path)
        .read_metadata_ext(true)
        .read_hardlink_info(true)
    {
        match entry {
            Ok(e) => {
                entries += 1;
                if let Some(ref ext) = e.metadata_ext {
                    let has_nlink = {
                        #[cfg(windows)]
                        { ext.number_of_links.is_some() }
                        #[cfg(unix)]
                        { ext.st_nlink > 0 }
                    };
                    if has_nlink {
                        with_nlink += 1;
                    }
                }
            }
            Err(_) => errors += 1,
        }
    }

    (entries, errors, start.elapsed().as_secs_f64(), with_nlink)
}
