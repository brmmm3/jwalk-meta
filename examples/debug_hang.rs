use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Z:\\品质部".to_string());

    println!("[DEBUG] Step 1: Creating WalkDir for {:?}", path);
    let walker = jwalk_meta::WalkDir::new(&path);

    println!("[DEBUG] Step 2: Calling into_iter()");
    let start = Instant::now();
    let mut iter = walker.into_iter();

    println!("[DEBUG] Step 3: into_iter() done in {:?}", start.elapsed());

    let counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = counter.clone();
    let start = Instant::now();

    println!("[DEBUG] Step 4: Starting iteration...");

    for _ in 0.. {
        match iter.next() {
            Some(Ok(entry)) => {
                let c = counter_clone.fetch_add(1, Ordering::Relaxed) + 1;
                if c <= 5 || c % 1000 == 0 {
                    println!(
                        "[DEBUG] Entry #{}: {:?} (depth={}) at {:?}",
                        c,
                        entry.path(),
                        entry.depth,
                        start.elapsed()
                    );
                }
            }
            Some(Err(err)) => {
                let c = counter_clone.fetch_add(1, Ordering::Relaxed) + 1;
                if c <= 10 {
                    println!("[DEBUG] Error #{}: {:?}", c, err);
                }
            }
            None => {
                println!(
                    "[DEBUG] Iterator returned None after {} entries in {:?}",
                    counter.load(Ordering::Relaxed),
                    start.elapsed()
                );
                break;
            }
        }

        // Timeout after 30 seconds
        if start.elapsed().as_secs() > 300 {
            println!(
                "[DEBUG] TIMEOUT after {} entries in {:?}",
                counter.load(Ordering::Relaxed),
                start.elapsed()
            );
            break;
        }
    }

    println!(
        "[DEBUG] Total: {} entries, {:?}",
        counter.load(Ordering::Relaxed),
        start.elapsed()
    );
}
