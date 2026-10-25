use std::time::Instant;

fn main() {
    let path = std::env::args().nth(1).expect("Usage: scan_bench <path>");
    
    println!("=== jwalk-meta scan benchmark ===");
    println!("Path: {}", path);
    println!();

    // Warm up (first run - may include compilation overhead)
    println!("Running scan...");
    use std::io::Write;
    let _ = std::io::stdout().flush();
    
    let start = Instant::now();
    let mut total_entries = 0usize;
    let mut total_dirs = 0usize;
    let mut total_files = 0usize;
    let mut total_errors = 0usize;

    let mut last_log = Instant::now();
    let log_interval = std::time::Duration::from_secs(1);
    for entry in jwalk_meta::WalkDir::new(&path) {
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
            let rate = total_entries as f64 / elapsed_so_far.as_secs_f64();
            println!(
                "  [{:.1}s] entries: {} (dirs: {}, files: {}, errors: {}) | {:.0} entries/s",
                elapsed_so_far.as_secs_f64(),
                total_entries, total_dirs, total_files, total_errors, rate,
            );
            // flush stdout for immediate output
            use std::io::Write;
            let _ = std::io::stdout().flush();
            last_log = now;
        }
    }
    
    let elapsed = start.elapsed();
    
    println!("Scan complete!");
    println!();
    println!("=== Results ===");
    println!("Total entries: {}", total_entries);
    println!("Directories:   {}", total_dirs);
    println!("Files:         {}", total_files);
    println!("Errors:        {}", total_errors);
    println!("Time:          {:.3}s", elapsed.as_secs_f64());
    println!("Rate:          {:.0} entries/s", total_entries as f64 / elapsed.as_secs_f64());
    
    // Run again for comparison (caches warm)
    println!();
    println!("=== Second run (warm) ===");
    let start2 = Instant::now();
    let mut entries2 = 0usize;
    for entry in jwalk_meta::WalkDir::new(&path) {
        let _ = entry;
        entries2 += 1;
    }
    let elapsed2 = start2.elapsed();
    println!("Entries: {}", entries2);
    println!("Time:    {:.3}s", elapsed2.as_secs_f64());
    println!("Rate:    {:.0} entries/s", entries2 as f64 / elapsed2.as_secs_f64());
    
    // std::fs comparison (skip for large/network paths)
    if elapsed.as_secs() < 30 {
        println!();
        println!("=== std::fs walk (sequential, for comparison) ===");
        let start3 = Instant::now();
        let mut entries3 = 0usize;
        let mut errors3 = 0usize;
        fn walk_std(path: &std::path::Path, entries: &mut usize, errors: &mut usize) {
            if let Ok(read_dir) = std::fs::read_dir(path) {
                for entry in read_dir {
                    match entry {
                        Ok(e) => {
                            *entries += 1;
                            let p = e.path();
                            if p.is_dir() {
                                walk_std(&p, entries, errors);
                            }
                        }
                        Err(_) => {
                            *errors += 1;
                        }
                    }
                }
            }
        }
        walk_std(std::path::Path::new(&path), &mut entries3, &mut errors3);
        let elapsed3 = start3.elapsed();
        println!("Entries: {}", entries3);
        println!("Errors:  {}", errors3);
        println!("Time:    {:.3}s", elapsed3.as_secs_f64());
        println!("Rate:    {:.0} entries/s", entries3 as f64 / elapsed3.as_secs_f64());
        println!();
        println!("=== Summary ===");
        println!("jwalk (cold):       {:.3}s", elapsed.as_secs_f64());
        println!("jwalk (warm):       {:.3}s", elapsed2.as_secs_f64());
        println!("std::fs (seq):      {:.3}s", elapsed3.as_secs_f64());
        let speedup = elapsed3.as_secs_f64() / elapsed2.as_secs_f64();
        println!("Parallel speedup:   {:.2}x", speedup);
    } else {
        println!();
        println!("=== Summary ===");
        println!("jwalk (cold):       {:.3}s", elapsed.as_secs_f64());
        println!("jwalk (warm):       {:.3}s", elapsed2.as_secs_f64());
        println!("(std::fs skipped: path too large)");
    }
}
