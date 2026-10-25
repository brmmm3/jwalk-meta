mod util;
use jwalk_meta::*;
use util::Dir;

/// 运行多次遍历，验证不会累积内存导致 OOM 或 panic。
/// 这不是精确的内存测量，而是验证反复遍历不会崩溃。
#[test]
fn repeated_traversal_no_crash() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b/c");
    dir.mkdirp("d/e/f");
    for i in 0..20 {
        dir.touch(format!("a/b/c/file{}.txt", i));
        dir.touch(format!("d/e/f/file{}.txt", i));
    }

    // 运行 100 次遍历，验证稳定性
    for iteration in 0..100 {
        let wd = WalkDir::new(dir.path())
            .parallelism(Parallelism::RayonNewPool(4))
            .sort(true);
        let r = dir.run_recursive(wd);
        r.assert_no_errors();
        // 每次遍历应该找到相同数量的条目
        assert!(
            r.ents().len() > 10,
            "iteration {}: expected > 10 entries, got {}",
            iteration,
            r.ents().len()
        );
    }
}

#[test]
fn repeated_traversal_serial_no_crash() {
    let dir = Dir::tmp();
    dir.mkdirp("x/y/z");
    dir.touch("x/y/z/file.txt");

    for _ in 0..50 {
        let wd = WalkDir::new(dir.path())
            .parallelism(Parallelism::Serial)
            .sort(true);
        let r = dir.run_recursive(wd);
        r.assert_no_errors();
        assert!(r.ents().len() >= 5);
    }
}

#[test]
fn alternating_parallelism_no_crash() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");
    dir.touch("a/b/file.txt");

    for i in 0..50 {
        let parallelism = if i % 2 == 0 {
            Parallelism::Serial
        } else {
            Parallelism::RayonNewPool(2)
        };
        let wd = WalkDir::new(dir.path())
            .parallelism(parallelism)
            .sort(true);
        let r = dir.run_recursive(wd);
        r.assert_no_errors();
    }
}
