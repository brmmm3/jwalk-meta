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

    // 移除子目录的执行权限（无法进入）
    std::fs::set_permissions(
        dir.join("accessible/inaccessible"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);

    // 应该有错误（无法读 inaccessible 目录）
    assert!(!r.errs().is_empty(), "expected permission errors");

    // 但仍然能读取可访问的条目
    assert!(r.ents().len() >= 3, "expected >= 3 accessible entries");

    // 恢复权限以便 TempDir 可以清理
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

    // 移除文件读权限
    std::fs::set_permissions(
        dir.join("unreadable.txt"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();

    let wd = WalkDir::new(dir.path())
        .sort(true)
        .parallelism(Parallelism::Serial);
    let r = dir.run_recursive(wd);

    // 文件仍然会被发现（文件名可读），但 metadata 可能失败
    assert!(
        r.ents().len() >= 3,
        "expected >= 3 entries including root"
    );

    // 恢复权限
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

    // 应该有错误
    assert!(
        !r.errs().is_empty(),
        "expected permission errors in parallel mode"
    );

    // 可见文件应该仍然被找到
    let paths: Vec<_> = r.ents().iter().map(|e| e.path().to_path_buf()).collect();
    assert!(
        paths
            .iter()
            .any(|p| p.to_str().unwrap().contains("visible.txt")),
        "visible.txt should be found even with permission errors: {:?}",
        paths
    );

    // 恢复权限
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
    assert!(r.ents().is_empty(), "expected no entries for non-existent path");
}
