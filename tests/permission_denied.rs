mod util;

use jwalk_meta::*;
use util::Dir;

#[cfg(unix)]
#[test]
#[ignore = "environment-sensitive: CI runners don't enforce 0o000 permissions"]
fn permission_denied_dir() {
    use std::os::unix::fs::PermissionsExt;

    let dir = Dir::tmp();
    dir.mkdirp("accessible/inaccessible");
    dir.touch("accessible/file.txt");
    dir.touch("accessible/inaccessible/secret.txt");

    // Remove execute permission from the subdirectory (cannot enter it).
    std::fs::set_permissions(
        dir.join("accessible/inaccessible"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);

    // There should be an error (the inaccessible directory cannot be read).
    assert!(!r.errs().is_empty(), "expected permission errors");

    // Accessible entries should still be readable.
    assert!(r.ents().len() >= 3, "expected >= 3 accessible entries");

    // Restore permissions so TempDir can clean up.
    std::fs::set_permissions(
        dir.join("accessible/inaccessible"),
        std::fs::Permissions::from_mode(0o755),
    )
    .ok();
}

#[cfg(unix)]
#[test]
#[ignore = "environment-sensitive: CI runners don't enforce 0o000 permissions"]
fn permission_denied_file() {
    use std::os::unix::fs::PermissionsExt;

    let dir = Dir::tmp();
    dir.touch("readable.txt");
    dir.touch("unreadable.txt");

    // Remove read permission from the file.
    std::fs::set_permissions(
        dir.join("unreadable.txt"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();

    let wd = WalkDir::new(dir.path())
        .sort(true)
        .parallelism(Parallelism::Serial);
    let r = dir.run_recursive(wd);

    // The file should still be found (its name is readable), but metadata retrieval may fail.
    assert!(r.ents().len() >= 3, "expected >= 3 entries including root");

    // Restore permissions.
    std::fs::set_permissions(
        dir.join("unreadable.txt"),
        std::fs::Permissions::from_mode(0o644),
    )
    .ok();
}

#[cfg(unix)]
#[test]
#[ignore = "environment-sensitive: CI runners don't enforce 0o000 permissions"]
fn permission_denied_parallel() {
    use std::os::unix::fs::PermissionsExt;

    let dir = Dir::tmp();
    dir.mkdirp("a/blocked");
    dir.mkdirp("c");
    dir.touch("a/blocked/secret.txt");
    dir.touch("c/visible.txt");

    std::fs::set_permissions(
        dir.join("a/blocked"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();

    let wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::RayonNewPool(2))
        .sort(true);
    let r = dir.run_recursive(wd);

    // There should be an error.
    assert!(
        !r.errs().is_empty(),
        "expected permission errors in parallel mode"
    );

    // Visible files should still be found.
    let paths: Vec<_> = r.ents().iter().map(|e| e.path().to_path_buf()).collect();
    assert!(
        paths
            .iter()
            .any(|p| p.to_str().unwrap().contains("visible.txt")),
        "visible.txt should be found even with permission errors: {:?}",
        paths
    );

    // Restore permissions.
    std::fs::set_permissions(
        dir.join("a/blocked"),
        std::fs::Permissions::from_mode(0o755),
    )
    .ok();
}

#[test]
fn non_existent_path_error() {
    let dir = Dir::tmp();
    let wd = WalkDir::new(dir.path().join("does_not_exist"));
    let r = dir.run_recursive(wd);

    assert!(!r.errs().is_empty(), "expected error for non-existent path");
    assert!(
        r.ents().is_empty(),
        "expected no entries for non-existent path"
    );
}
