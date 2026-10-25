mod util;

use jwalk_meta::*;
use std::time::Instant;
use util::Dir;

/// CI 友好的吞吐基准：验证不同并行度的遍历能力。
/// 不是精确基准测试，而是确保性能不退化。
#[test]
fn ci_serial_throughput() {
    let dir = Dir::tmp();
    let file_count = 500;

    dir.mkdirp("bench");
    for i in 0..file_count {
        dir.touch(format!("bench/f_{:04}.txt", i));
    }

    let start = Instant::now();
    let wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::Serial)
        .sort(true);
    let r = dir.run_recursive(wd);
    let elapsed = start.elapsed();

    r.assert_no_errors();
    // root + bench/ + 500 files = 502
    assert_eq!(file_count + 2, r.ents().len());

    // CI 环境下 500 文件遍历应该在 10 秒内完成
    assert!(
        elapsed.as_secs() < 10,
        "serial traversal took {:?}, expected < 10s",
        elapsed
    );
}

#[test]
fn ci_parallel_throughput() {
    let dir = Dir::tmp();
    let file_count = 500;

    dir.mkdirp("bench");
    for i in 0..file_count {
        dir.touch(format!("bench/f_{:04}.txt", i));
    }

    let start = Instant::now();
    let wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::RayonNewPool(4))
        .sort(true);
    let r = dir.run_recursive(wd);
    let elapsed = start.elapsed();

    r.assert_no_errors();
    assert_eq!(file_count + 2, r.ents().len());
    assert!(
        elapsed.as_secs() < 10,
        "parallel traversal took {:?}, expected < 10s",
        elapsed
    );
}

#[test]
fn ci_thread_scalability() {
    let dir = Dir::tmp();
    let file_count = 200;

    dir.mkdirp("bench");
    for i in 0..file_count {
        dir.touch(format!("bench/f_{:04}.txt", i));
    }

    // 测试不同线程数都能正确完成
    for threads in [1, 2, 4, 8] {
        let wd = WalkDir::new(dir.path())
            .parallelism(Parallelism::RayonNewPool(threads))
            .sort(true);
        let r = dir.run_recursive(wd);
        r.assert_no_errors();
        assert_eq!(
            file_count + 2,
            r.ents().len(),
            "thread count {}: expected {} entries, got {}",
            threads,
            file_count + 2,
            r.ents().len()
        );
    }
}

#[test]
fn ci_rayon_pool_reuse() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b/c");
    dir.touch("a/b/c/file.txt");

    // 反复使用 RayonNewPool，验证 TLS 缓存不泄漏
    for _ in 0..20 {
        let wd = WalkDir::new(dir.path())
            .parallelism(Parallelism::RayonNewPool(2))
            .sort(true);
        let r = dir.run_recursive(wd);
        r.assert_no_errors();
    }
}
