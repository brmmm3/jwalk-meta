mod util;

#[cfg(windows)]
use jwalk_meta::*;
#[cfg(windows)]
use util::Dir;

/// Windows symlink tests require administrator privileges or Developer Mode.
/// Regular users can run these tests with `cargo test -- --ignored`.
#[cfg(windows)]
#[test]
#[ignore] // Requires SeCreateSymbolicLinkPrivilege (administrator/Developer Mode).
fn windows_symlink_dir_nofollow() {
    let dir = Dir::tmp();
    dir.mkdirp("target");
    dir.touch("target/file.txt");
    dir.symlink_dir("target", "link");

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    assert!(
        r.ents().len() >= 3,
        "expected >= 3 entries, got {}",
        r.ents().len()
    );

    let link_ent = r.ents().iter().find(|e| e.file_name() == "link");
    if let Some(link) = link_ent {
        assert!(link.path_is_symlink(), "link should be a symlink");
    }
}

#[cfg(windows)]
#[test]
#[ignore]
fn windows_symlink_dir_follow() {
    let dir = Dir::tmp();
    dir.mkdirp("target");
    dir.touch("target/file.txt");
    dir.symlink_dir("target", "link");

    let wd = WalkDir::new(dir.path()).follow_links(true).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    assert!(
        r.ents().len() >= 4,
        "expected >= 4 entries, got {}",
        r.ents().len()
    );
}

#[cfg(windows)]
#[test]
#[ignore]
fn windows_symlink_file() {
    let dir = Dir::tmp();
    dir.touch("original.txt");
    dir.symlink_file("original.txt", "link.txt");

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    assert_eq!(3, r.ents().len());

    let link_ent = r
        .ents()
        .iter()
        .find(|e| e.file_name() == "link.txt")
        .unwrap();
    assert!(link_ent.path_is_symlink());
}

#[cfg(windows)]
#[test]
#[ignore]
fn windows_symlink_loop_detect() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");
    dir.symlink_dir("a", "a/b/loop");

    let wd = WalkDir::new(dir.path()).follow_links(true);
    let r = dir.run_recursive(wd);

    assert!(!r.errs().is_empty(), "expected loop detection error");
}

#[cfg(windows)]
#[test]
#[ignore]
fn windows_symlink_parallel() {
    let dir = Dir::tmp();
    dir.mkdirp("real/sub1");
    dir.mkdirp("real/sub2");
    dir.touch("real/sub1/file1.txt");
    dir.touch("real/sub2/file2.txt");
    dir.symlink_dir("real", "link_to_real");

    let wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::RayonNewPool(2))
        .sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    assert!(
        r.ents().len() >= 5,
        "expected >= 5 entries, got {}",
        r.ents().len()
    );
}
