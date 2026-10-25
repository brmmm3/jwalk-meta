mod util;

use jwalk_meta::*;
use std::time::Instant;
use util::Dir;

#[test]
fn stress_1000_files_serial() {
    let dir = Dir::tmp();
    let count = 1000;

    // 创建扁平目录结构
    dir.mkdirp("files");
    for i in 0..count {
        dir.touch(format!("files/file_{:04}.txt", i));
    }
    dir.touch("root.txt");

    let start = Instant::now();
    let wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::Serial)
        .sort(true);
    let r = dir.run_recursive(wd);
    let elapsed = start.elapsed();

    r.assert_no_errors();
    // root + "files" dir + root.txt + count files = count + 3
    let expected = count + 3;
    assert_eq!(
        expected,
        r.ents().len(),
        "expected {} entries, got {} (elapsed: {:?})",
        expected,
        r.ents().len(),
        elapsed
    );
}

#[test]
fn stress_1000_files_parallel() {
    let dir = Dir::tmp();
    let count = 1000;

    dir.mkdirp("files");
    for i in 0..count {
        dir.touch(format!("files/file_{:04}.txt", i));
    }
    dir.touch("root.txt");

    let start = Instant::now();
    let wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::RayonNewPool(4))
        .sort(true);
    let r = dir.run_recursive(wd);
    let elapsed = start.elapsed();

    r.assert_no_errors();
    let expected = count + 3;
    assert_eq!(
        expected,
        r.ents().len(),
        "expected {} entries, got {} (elapsed: {:?})",
        expected,
        r.ents().len(),
        elapsed
    );
}

#[test]
fn stress_1000_files_nested() {
    let dir = Dir::tmp();
    let dirs_per_level = 10;
    let depth = 3;

    // 创建 10 x 10 x 10 = 1000 个目录，每个含 1 个文件
    fn create_nested(
        dir: &Dir,
        prefix: &str,
        current_depth: usize,
        max_depth: usize,
        dirs_per_level: usize,
    ) {
        if current_depth >= max_depth {
            dir.touch(format!("{}/file.txt", prefix));
            return;
        }
        for i in 0..dirs_per_level {
            let subdir = format!("{}/d{}", prefix, i);
            dir.mkdirp(&subdir);
            create_nested(dir, &subdir, current_depth + 1, max_depth, dirs_per_level);
        }
    }

    create_nested(&dir, "root", 0, depth, dirs_per_level);

    let wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::RayonNewPool(4))
        .sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    // root tmpdir + "root" subdir + 10 + 100 + 1000 dirs + 1000 files = 2112
    let expected = 1 + 1 + 10 + 100 + 1000 + 1000;
    assert_eq!(
        expected,
        r.ents().len(),
        "expected {} entries, got {}",
        expected,
        r.ents().len()
    );
}

#[test]
fn stress_serial_parallel_same_count() {
    let dir = Dir::tmp();
    let count = 500;

    dir.mkdirp("files");
    for i in 0..count {
        dir.touch(format!("files/file_{:04}.txt", i));
    }

    let serial_wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::Serial)
        .sort(true);
    let serial_r = dir.run_recursive(serial_wd);

    let par_wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::RayonNewPool(4))
        .sort(true);
    let par_r = dir.run_recursive(par_wd);

    assert_eq!(
        serial_r.ents().len(),
        par_r.ents().len(),
        "serial {} != parallel {}",
        serial_r.ents().len(),
        par_r.ents().len()
    );
    serial_r.assert_no_errors();
    par_r.assert_no_errors();
}

/// 压力测试：手动运行 `cargo test -- --ignored`
#[test]
#[ignore]
fn stress_100k_files() {
    let dir = Dir::tmp();
    // 200 dirs x 500 files = 100K files
    let subdir_count = 200;
    let files_per_dir = 500;

    for i in 0..subdir_count {
        let subdir = format!("d{:04}", i);
        dir.mkdirp(&subdir);
        for j in 0..files_per_dir {
            dir.touch(format!("{}/f{:03}.txt", subdir, j));
        }
    }

    let start = Instant::now();
    let wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::RayonNewPool(4))
        .sort(false);
    let r = dir.run_recursive(wd);
    let elapsed = start.elapsed();

    let expected = 1 + subdir_count + (subdir_count * files_per_dir);
    assert_eq!(
        expected,
        r.ents().len(),
        "expected {} entries, got {}",
        expected,
        r.ents().len()
    );
    r.assert_no_errors();

    eprintln!("100K files traversed in {:?}", elapsed);
}
