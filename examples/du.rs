use std::env;
use std::time::Instant;

extern crate jwalk;

use jwalk::WalkDirGeneric;

fn main() {
    let path = env::args().skip(1).next().unwrap_or("./".to_owned());
    let mut total: u64 = 0;

    let start = Instant::now();
    for dir_entry_result in WalkDirGeneric::<((), Option<u64>)>::new(&path)
        .skip_hidden(false)
        .read_metadata(true)
        .process_read_dir(|_, _, _, dir_entry_results| {
            dir_entry_results.iter_mut().for_each(|dir_entry_result| {
                if let Ok(dir_entry) = dir_entry_result {
                    if let Some(ref metadata) = dir_entry.metadata {
                        dir_entry.client_state = Some(metadata.size);
                    }
                }
            })
        })
    {
        match dir_entry_result {
            Ok(dir_entry) => {
                if let Some(len) = &dir_entry.client_state {
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
