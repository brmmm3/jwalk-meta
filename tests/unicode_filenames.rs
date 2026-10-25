mod util;
use jwalk_meta::*;
use util::Dir;

#[test]
fn unicode_ascii_mix() {
    let dir = Dir::tmp();
    dir.mkdirp("日本語");
    dir.mkdirp("中文");
    dir.mkdirp("한국어");
    dir.touch("日本語/file.txt");
    dir.touch("中文/文件.txt");
    dir.touch("한국어/파일.txt");

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();
    assert!(
        r.ents().len() >= 7,
        "expected >= 7 entries, got {}",
        r.ents().len()
    );
}

#[test]
fn unicode_emoji_filenames() {
    let dir = Dir::tmp();
    dir.mkdirp("📁folder");
    dir.touch("📁folder/📄doc.txt");
    dir.touch("normal.txt");

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();
    assert!(r.ents().len() >= 4);
}

#[test]
fn unicode_spaces_in_names() {
    let dir = Dir::tmp();
    dir.mkdirp("folder with spaces");
    dir.touch("folder with spaces/file with spaces.txt");
    dir.touch("file with spaces.txt");

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();
    assert!(r.ents().len() >= 4);
}

#[test]
fn unicode_parallel_traversal() {
    let dir = Dir::tmp();
    for name in ["αβγ", "日本", "한글", "العربية", "Привет"] {
        dir.mkdirp(name);
        dir.touch(format!("{}/test.txt", name));
    }
    dir.touch("root.txt");

    let wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::RayonNewPool(4))
        .sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();
    // root + root.txt + 5 dirs + 5 files = 12
    assert_eq!(12, r.ents().len());
}

#[test]
fn unicode_deep_nested() {
    let dir = Dir::tmp();
    dir.mkdirp("日本語/中文/한국어/العربية");
    dir.touch("日本語/中文/한국어/العربية/file.txt");

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();
    assert!(r.ents().len() >= 5);
}
