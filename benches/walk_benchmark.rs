#![allow(dead_code)]
#![allow(unused_imports)]

use std::cmp;
use std::fs::{create_dir_all, File, Metadata};
use std::io::{self, ErrorKind, Read};
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use flate2::read::GzDecoder;
use ignore::WalkBuilder;
use num_cpus;
use rayon::prelude::*;
use tar::Archive;
use walkdir;

use jwalk_meta::{Error, Parallelism, WalkDir, WalkDirGeneric};

#[cfg(unix)]
fn linux_kernel_archive() -> PathBuf {
    PathBuf::from("~/Rust/_Data/benches/linux-5.9.tar.gz")
}

#[cfg(windows)]
fn linux_kernel_archive() -> PathBuf {
    PathBuf::from("C:/Workspace/benches/linux-5.9.tar.gz")
}

#[cfg(unix)]
fn linux_dir() -> PathBuf {
    PathBuf::from("~/Rust/_Data/benches/linux-5.9")
}

#[cfg(windows)]
fn linux_dir() -> PathBuf {
    PathBuf::from("C:/Workspace/benches/linux-5.9")
}

fn download_linux_kernel() -> Result<(), reqwest::Error> {
    println!("Downloading linux-5.9.tar.gz...");
    let mut client = reqwest::blocking::Client::builder();
    if let Ok(proxy) = std::env::var("HTTP_PROXY") {
        client = client.proxy(reqwest::Proxy::https(proxy)?);
    }
    let client = client.build()?;
    let mut resp = client
        .get("https://cdn.kernel.org/pub/linux/kernel/v5.x/linux-5.9.tar.gz")
        .send()?;
    let path = linux_kernel_archive();
    let parent = path.parent().unwrap();
    if !parent.exists() {
        println!("Create {:?}", parent);
        create_dir_all(parent).unwrap();
    }
    let path = linux_kernel_archive();
    println!("Write archive to {:?}", path);
    let mut out = File::create(&path).expect("failed to create file");
    io::copy(&mut resp, &mut out).expect("failed to copy content");
    Ok(())
}

fn download_and_unpack_linux_kernel() -> Result<(), io::Error> {
    let linux_kernel_archive = linux_kernel_archive();
    if !linux_kernel_archive.exists() {
        // Download linux kernel
        download_linux_kernel().map_err(|e| io::Error::new(ErrorKind::Other, e.to_string()))?;
    }
    let linux_dir = linux_dir();
    if !linux_dir.exists() {
        let root = linux_dir.parent().unwrap();
        println!("Extracting linux-5.9.tar.gz to {:?}...", root);
        let tar_gz = File::open(&linux_kernel_archive)?;
        let tar = GzDecoder::new(tar_gz);
        let mut archive = Archive::new(tar);
        // Unpack only files. Ignore symlinks, etc. Needed for Windows :-(
        for file in archive.entries().unwrap() {
            let mut file = file?;
            if file.header().entry_type().is_file() {
                let path = root.join(file.path().unwrap());
                let parent = path.parent().unwrap();
                if !parent.exists() {
                    create_dir_all(parent)?;
                }
                file.unpack(path)?;
            }
        }
    }
    Ok(())
}

fn walk_benches(c: &mut Criterion) {
    download_and_unpack_linux_kernel().unwrap();

    c.bench_function("rayon (unsorted, n threads)", |b| {
        b.iter(|| black_box(rayon_recursive_descent(linux_dir(), None, false)))
    });

    c.bench_function("rayon (unsorted, metadata, n threads)", |b| {
        b.iter(|| black_box(rayon_recursive_descent(linux_dir(), None, true)))
    });

    c.bench_function("jwalk (unsorted, n threads)", |b| {
        b.iter(|| for _ in WalkDir::new(linux_dir()) {})
    });

    c.bench_function("jwalk (sorted, n threads)", |b| {
        b.iter(|| for _ in WalkDir::new(linux_dir()).sort(true) {})
    });

    c.bench_function("jwalk (sorted, metadata, n threads)", |b| {
        b.iter(|| {
            for _ in WalkDirGeneric::<((), Option<Result<Metadata, Error>>)>::new(linux_dir())
                .sort(true)
                .process_read_dir(|_, _, _, dir_entry_results| {
                    dir_entry_results.iter_mut().for_each(|dir_entry_result| {
                        if let Ok(dir_entry) = dir_entry_result {
                            dir_entry.client_state = Some(dir_entry.metadata());
                        }
                    })
                })
            {}
        })
    });

    c.bench_function("jwalk (sorted, n threads, first 100)", |b| {
        b.iter(
            || {
                for _ in WalkDir::new(linux_dir()).sort(true).into_iter().take(100) {}
            },
        )
    });

    c.bench_function("jwalk (unsorted, 2 threads)", |b| {
        b.iter(
            || {
                for _ in WalkDir::new(linux_dir()).parallelism(Parallelism::RayonNewPool(2)) {}
            },
        )
    });

    c.bench_function("jwalk (unsorted, 1 thread)", |b| {
        b.iter(
            || {
                for _ in WalkDir::new(linux_dir()).parallelism(Parallelism::Serial) {}
            },
        )
    });

    c.bench_function("jwalk (sorted, 1 thread)", |b| {
        b.iter(|| {
            for _ in WalkDir::new(linux_dir())
                .sort(true)
                .parallelism(Parallelism::Serial)
            {}
        })
    });

    c.bench_function("jwalk (sorted, metadata, 1 thread)", |b| {
        b.iter(|| {
            for _ in WalkDirGeneric::<((), Option<Result<Metadata, Error>>)>::new(linux_dir())
                .sort(true)
                .parallelism(Parallelism::Serial)
                .process_read_dir(|_, _, _, dir_entry_results| {
                    dir_entry_results.iter_mut().for_each(|dir_entry_result| {
                        if let Ok(dir_entry) = dir_entry_result {
                            dir_entry.client_state = Some(dir_entry.metadata());
                        }
                    })
                })
            {}
        })
    });

    c.bench_function("ignore (unsorted, n threads)", move |b| {
        b.iter(|| {
            WalkBuilder::new(linux_dir())
                .hidden(false)
                .standard_filters(false)
                .threads(cmp::min(12, num_cpus::get()))
                .build_parallel()
                .run(move || Box::new(move |_| ignore::WalkState::Continue));
        })
    });

    c.bench_function("ignore (sorted, n threads)", move |b| {
        b.iter(|| {
            let (tx, rx) = mpsc::channel();
            WalkBuilder::new(linux_dir())
                .hidden(false)
                .standard_filters(false)
                .threads(cmp::min(12, num_cpus::get()))
                .build_parallel()
                .run(move || {
                    let tx = tx.clone();
                    Box::new(move |dir_entry_result| {
                        if let Ok(dir_entry) = dir_entry_result {
                            tx.send(dir_entry.file_name().to_owned()).unwrap();
                        }
                        ignore::WalkState::Continue
                    })
                });
            let mut metadatas: Vec<_> = rx.into_iter().collect();
            metadatas.sort_by(|a, b| a.len().cmp(&b.len()))
        })
    });

    c.bench_function("ignore (sorted, metadata, n threads)", move |b| {
        b.iter(|| {
            let (tx, rx) = mpsc::channel();
            WalkBuilder::new(linux_dir())
                .hidden(false)
                .standard_filters(false)
                .threads(cmp::min(12, num_cpus::get()))
                .build_parallel()
                .run(move || {
                    let tx = tx.clone();
                    Box::new(move |dir_entry_result| {
                        if let Ok(dir_entry) = dir_entry_result {
                            let _ = dir_entry.metadata();
                            tx.send(dir_entry.file_name().to_owned()).unwrap();
                        }
                        ignore::WalkState::Continue
                    })
                });
            let mut metadatas: Vec<_> = rx.into_iter().collect();
            metadatas.sort_by(|a, b| a.len().cmp(&b.len()))
        })
    });

    c.bench_function("ignore (unsorted, 2 threads)", move |b| {
        b.iter(|| {
            WalkBuilder::new(linux_dir())
                .hidden(false)
                .standard_filters(false)
                .threads(cmp::min(2, num_cpus::get()))
                .build_parallel()
                .run(move || Box::new(move |_| ignore::WalkState::Continue));
        })
    });

    c.bench_function("walkdir (unsorted, 1 thread)", move |b| {
        b.iter(|| for _ in walkdir::WalkDir::new(linux_dir()) {})
    });

    c.bench_function("walkdir (sorted, 1 thread)", move |b| {
        b.iter(|| {
            for _ in
                walkdir::WalkDir::new(linux_dir()).sort_by(|a, b| a.file_name().cmp(b.file_name()))
            {
            }
        })
    });

    c.bench_function("walkdir (sorted, metadata, 1 thread)", move |b| {
        b.iter(|| {
            for each in
                walkdir::WalkDir::new(linux_dir()).sort_by(|a, b| a.file_name().cmp(b.file_name()))
            {
                let _ = each.unwrap().metadata();
            }
        })
    });
}

fn rayon_recursive_descent(
    root: impl AsRef<Path>,
    file_type: Option<std::fs::FileType>,
    get_file_metadata: bool,
) {
    let root = root.as_ref();
    let (_metadata, is_dir) = file_type
        .map(|ft| {
            (
                if !ft.is_dir() && get_file_metadata {
                    std::fs::symlink_metadata(root).ok()
                } else {
                    None
                },
                ft.is_dir(),
            )
        })
        .or_else(|| {
            std::fs::symlink_metadata(root)
                .map(|m| {
                    let is_dir = m.file_type().is_dir();
                    (Some(m), is_dir)
                })
                .ok()
        })
        .unwrap_or((None, false));

    if is_dir {
        std::fs::read_dir(root)
            .map(|iter| {
                iter.filter_map(Result::ok)
                    .collect::<Vec<_>>()
                    .into_par_iter()
                    .map(|entry| {
                        rayon_recursive_descent(
                            entry.path(),
                            entry.file_type().ok(),
                            get_file_metadata,
                        )
                    })
                    .for_each(|_| {})
            })
            .unwrap_or_default()
    };
}

criterion_group! {
  name = benches;
  config = Criterion::default().sample_size(10);
  targets = walk_benches
}

criterion_main!(benches);
