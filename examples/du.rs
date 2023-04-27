use std::time::Instant;

extern crate jwalk_meta;

use jwalk_meta::{Parallelism, WalkDirGeneric};
use std::env;

fn main() {
    let path = env::args().skip(1).next().unwrap_or("./".to_owned());
    let mut total: u64 = 0;

    let start = Instant::now();
    for dir_entry_result in WalkDirGeneric::<((), Option<u64>)>::new(&path)
        .skip_hidden(false)
        .parallelism(Parallelism::RayonNewPool(4))
        .process_read_dir(|_, _, _, dir_entry_results| {
            dir_entry_results.iter_mut().for_each(|dir_entry_result| {
                if let Ok(dir_entry) = dir_entry_result {
                    if !dir_entry.file_type.is_dir() {
                        dir_entry.client_state =
                            Some(dir_entry.metadata().map(|m| m.len()).unwrap_or_default());
                    }
                }
            })
        })
    {
        match dir_entry_result {
            Ok(dir_entry) => {
                if let Some(len) = &dir_entry.client_state {
                    eprintln!("counting {:?}", dir_entry.path());
                    total += len;
                }
            }
            Err(error) => {
                println!("Read dir_entry error: {}", error);
            }
        }
    }
    let dt = start.elapsed().as_secs_f64();

    println!("path: {} total bytes: {} after {} seconds", path, total, dt);
}
