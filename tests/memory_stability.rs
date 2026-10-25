mod util;
use jwalk_meta::*;
use util::Dir;

/// Run traversal repeatedly to verify that memory does not accumulate and cause OOM or a panic.
/// This is not a precise memory measurement; it checks that repeated traversals do not crash.
#[test]
fn repeated_traversal_no_crash() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b/c");
    dir.mkdirp("d/e/f");
    for i in 0..20 {
        dir.touch(format!("a/b/c/file{}.txt", i));
        dir.touch(format!("d/e/f/file{}.txt", i));
    }

    // Run 100 traversals to verify stability.
    for iteration in 0..100 {
        let wd = WalkDir::new(dir.path())
            .parallelism(Parallelism::RayonNewPool(4))
            .sort(true);
        let r = dir.run_recursive(wd);
        r.assert_no_errors();
        // Each traversal should find the same number of entries.
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
