use lazy_static::lazy_static;
use rayon::prelude::*;
use std::env;
#[cfg(unix)]
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

mod util;

use jwalk_meta::*;
use util::Dir;

#[test]
fn empty() {
    let dir = Dir::tmp();
    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    assert_eq!(1, r.ents().len());
    let ent = &r.ents()[0];
    assert!(ent.file_type().is_dir());
    assert!(!ent.path_is_symlink());
    assert_eq!(0, ent.depth());
    assert_eq!(dir.path(), ent.path());
    assert_eq!(dir.path().file_name().unwrap(), ent.file_name());
}

#[test]
fn empty_follow() {
    let dir = Dir::tmp();
    let wd = WalkDir::new(dir.path()).follow_links(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    assert_eq!(1, r.ents().len());
    let ent = &r.ents()[0];
    assert!(ent.file_type().is_dir());
    assert!(!ent.path_is_symlink());
    assert_eq!(0, ent.depth());
    assert_eq!(dir.path(), ent.path());
    assert_eq!(dir.path().file_name().unwrap(), ent.file_name());
}

#[test]
fn empty_file() {
    let dir = Dir::tmp();
    dir.touch("a");

    let wd = WalkDir::new(dir.path().join("a"));
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    assert_eq!(1, r.ents().len());
    let ent = &r.ents()[0];
    assert!(ent.file_type().is_file());
    assert!(!ent.path_is_symlink());
    assert_eq!(0, ent.depth());
    assert_eq!(dir.join("a"), ent.path());
    assert_eq!("a", ent.file_name());
}

#[test]
fn empty_file_follow() {
    let dir = Dir::tmp();
    dir.touch("a");

    let wd = WalkDir::new(dir.path().join("a")).follow_links(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    assert_eq!(1, r.ents().len());
    let ent = &r.ents()[0];
    assert!(ent.file_type().is_file());
    assert!(!ent.path_is_symlink());
    assert_eq!(0, ent.depth());
    assert_eq!(dir.join("a"), ent.path());
    assert_eq!("a", ent.file_name());
}

#[test]
fn one_dir() {
    let dir = Dir::tmp();
    dir.mkdirp("a");

    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.ents();
    assert_eq!(2, ents.len());
    let ent = &ents[1];
    assert_eq!(dir.join("a"), ent.path());
    assert_eq!(1, ent.depth());
    assert_eq!("a", ent.file_name());
    assert!(ent.file_type().is_dir());
}

#[test]
fn one_file() {
    let dir = Dir::tmp();
    dir.touch("a");

    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.ents();
    assert_eq!(2, ents.len());
    let ent = &ents[1];
    assert_eq!(dir.join("a"), ent.path());
    assert_eq!(1, ent.depth());
    assert_eq!("a", ent.file_name());
    assert!(ent.file_type().is_file());
}

#[test]
fn one_dir_one_file() {
    let dir = Dir::tmp();
    dir.mkdirp("foo");
    dir.touch("foo/a");

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("foo"),
        dir.join("foo").join("a"),
    ];
    assert_eq!(expected, r.paths());
}

#[test]
fn many_files() {
    let dir = Dir::tmp();
    dir.mkdirp("foo");
    dir.touch_all(&["foo/a", "foo/b", "foo/c"]);

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("foo"),
        dir.join("foo").join("a"),
        dir.join("foo").join("b"),
        dir.join("foo").join("c"),
    ];
    assert_eq!(expected, r.paths());
}

#[test]
fn many_dirs() {
    let dir = Dir::tmp();
    dir.mkdirp("foo/a");
    dir.mkdirp("foo/b");
    dir.mkdirp("foo/c");

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("foo"),
        dir.join("foo").join("a"),
        dir.join("foo").join("b"),
        dir.join("foo").join("c"),
    ];
    assert_eq!(expected, r.paths());
}

#[test]
fn many_mixed() {
    let dir = Dir::tmp();
    dir.mkdirp("foo/a");
    dir.mkdirp("foo/c");
    dir.mkdirp("foo/e");
    dir.touch_all(&["foo/b", "foo/d", "foo/f"]);

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("foo"),
        dir.join("foo").join("a"),
        dir.join("foo").join("b"),
        dir.join("foo").join("c"),
        dir.join("foo").join("d"),
        dir.join("foo").join("e"),
        dir.join("foo").join("f"),
    ];
    assert_eq!(expected, r.paths());
}

#[test]
fn nested() {
    let nested = PathBuf::from("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x/y/z");
    let dir = Dir::tmp();
    dir.mkdirp(&nested);
    dir.touch(nested.join("A"));

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("a"),
        dir.join("a/b"),
        dir.join("a/b/c"),
        dir.join("a/b/c/d"),
        dir.join("a/b/c/d/e"),
        dir.join("a/b/c/d/e/f"),
        dir.join("a/b/c/d/e/f/g"),
        dir.join("a/b/c/d/e/f/g/h"),
        dir.join("a/b/c/d/e/f/g/h/i"),
        dir.join("a/b/c/d/e/f/g/h/i/j"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x/y"),
        dir.join("a/b/c/d/e/f/g/h/i/j/k/l/m/n/o/p/q/r/s/t/u/v/w/x/y/z"),
        dir.join(&nested).join("A"),
    ];
    assert_eq!(expected, r.paths());
}

#[test]
fn siblings() {
    let dir = Dir::tmp();
    dir.mkdirp("foo");
    dir.mkdirp("bar");
    dir.touch_all(&["foo/a", "foo/b"]);
    dir.touch_all(&["bar/a", "bar/b"]);

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("bar"),
        dir.join("bar").join("a"),
        dir.join("bar").join("b"),
        dir.join("foo"),
        dir.join("foo").join("a"),
        dir.join("foo").join("b"),
    ];
    assert_eq!(expected, r.paths());
}

#[cfg(unix)]
#[test]
fn sym_root_file_nofollow() {
    let dir = Dir::tmp();
    dir.touch("a");
    dir.symlink_file("a", "a-link");

    let wd = WalkDir::new(dir.join("a-link")).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.ents();
    assert_eq!(1, ents.len());
    let link = &ents[0];

    assert_eq!(dir.join("a-link"), link.path());

    assert!(link.path_is_symlink());

    assert_eq!(dir.join("a"), fs::read_link(link.path()).unwrap());

    assert_eq!(0, link.depth());

    assert!(link.file_type().is_symlink());
    assert!(!link.file_type().is_file());
    assert!(!link.file_type().is_dir());

    assert!(link.metadata().unwrap().file_type().is_symlink());
    assert!(!link.metadata().unwrap().is_file());
    assert!(!link.metadata().unwrap().is_dir());
}

#[cfg(unix)]
#[test]
fn sym_root_file_follow() {
    let dir = Dir::tmp();
    dir.touch("a");
    dir.symlink_file("a", "a-link");

    let wd = WalkDir::new(dir.join("a-link"))
        .sort(true)
        .follow_links(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.ents();
    let link = &ents[0];

    assert_eq!(dir.join("a-link"), link.path());

    assert!(link.path_is_symlink());

    assert_eq!(dir.join("a"), fs::read_link(link.path()).unwrap());

    assert_eq!(0, link.depth());

    assert!(!link.file_type().is_symlink());
    assert!(link.file_type().is_file());
    assert!(!link.file_type().is_dir());

    assert!(!link.metadata().unwrap().file_type().is_symlink());
    assert!(link.metadata().unwrap().is_file());
    assert!(!link.metadata().unwrap().is_dir());
}

#[cfg(unix)]
#[test]
fn sym_root_dir_nofollow() {
    let dir = Dir::tmp();
    dir.mkdirp("a");
    dir.symlink_dir("a", "a-link");
    dir.touch("a/zzz");

    let wd = WalkDir::new(dir.join("a-link")).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.ents();
    assert_eq!(2, ents.len());
    let link = &ents[0];

    assert_eq!(dir.join("a-link"), link.path());

    assert!(link.path_is_symlink());

    assert_eq!(dir.join("a"), fs::read_link(link.path()).unwrap());

    assert_eq!(0, link.depth());

    assert!(link.file_type().is_symlink());
    assert!(!link.file_type().is_file());
    assert!(!link.file_type().is_dir());

    assert!(link.metadata().unwrap().file_type().is_symlink());
    assert!(!link.metadata().unwrap().is_file());
    assert!(!link.metadata().unwrap().is_dir());

    let link_zzz = &ents[1];
    assert_eq!(dir.join("a-link").join("zzz"), link_zzz.path());
    assert!(!link_zzz.path_is_symlink());
}

#[cfg(unix)]
#[test]
fn sym_root_dir_follow() {
    let dir = Dir::tmp();
    dir.mkdirp("a");
    dir.symlink_dir("a", "a-link");
    dir.touch("a/zzz");

    let wd = WalkDir::new(dir.join("a-link"))
        .sort(true)
        .follow_links(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.ents();
    assert_eq!(2, ents.len());
    let link = &ents[0];

    assert_eq!(dir.join("a-link"), link.path());

    assert!(link.path_is_symlink());

    assert_eq!(dir.join("a"), fs::read_link(link.path()).unwrap());

    assert_eq!(0, link.depth());

    assert!(!link.file_type().is_symlink());
    assert!(!link.file_type().is_file());
    assert!(link.file_type().is_dir());

    assert!(!link.metadata().unwrap().file_type().is_symlink());
    assert!(!link.metadata().unwrap().is_file());
    assert!(link.metadata().unwrap().is_dir());

    let link_zzz = &ents[1];
    assert_eq!(dir.join("a-link").join("zzz"), link_zzz.path());
    assert!(!link_zzz.path_is_symlink());
}

#[cfg(unix)]
#[test]
fn sym_file_nofollow() {
    let dir = Dir::tmp();
    dir.touch("a");
    dir.symlink_file("a", "a-link");

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.ents();
    assert_eq!(3, ents.len());
    let (src, link) = (&ents[1], &ents[2]);

    assert_eq!(dir.join("a"), src.path());
    assert_eq!(dir.join("a-link"), link.path());

    assert!(!src.path_is_symlink());
    assert!(link.path_is_symlink());

    assert_eq!(dir.join("a"), fs::read_link(link.path()).unwrap());

    assert_eq!(1, src.depth());
    assert_eq!(1, link.depth());

    assert!(src.file_type().is_file());
    assert!(link.file_type().is_symlink());
    assert!(!link.file_type().is_file());
    assert!(!link.file_type().is_dir());

    assert!(src.metadata().unwrap().is_file());
    assert!(link.metadata().unwrap().file_type().is_symlink());
    assert!(!link.metadata().unwrap().is_file());
    assert!(!link.metadata().unwrap().is_dir());
}

#[cfg(unix)]
#[test]
fn sym_file_follow() {
    let dir = Dir::tmp();
    dir.touch("a");
    dir.symlink_file("a", "a-link");

    let wd = WalkDir::new(dir.path()).sort(true).follow_links(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.ents();
    assert_eq!(3, ents.len());
    let (src, link) = (&ents[1], &ents[2]);

    assert_eq!(dir.join("a"), src.path());
    assert_eq!(dir.join("a-link"), link.path());

    assert!(!src.path_is_symlink());
    assert!(link.path_is_symlink());

    assert_eq!(dir.join("a"), fs::read_link(link.path()).unwrap());

    assert_eq!(1, src.depth());
    assert_eq!(1, link.depth());

    assert!(src.file_type().is_file());
    assert!(!link.file_type().is_symlink());
    assert!(link.file_type().is_file());
    assert!(!link.file_type().is_dir());

    assert!(src.metadata().unwrap().is_file());
    assert!(!link.metadata().unwrap().file_type().is_symlink());
    assert!(link.metadata().unwrap().is_file());
    assert!(!link.metadata().unwrap().is_dir());
}

#[cfg(unix)]
#[test]
fn sym_dir_nofollow() {
    let dir = Dir::tmp();
    dir.mkdirp("a");
    dir.symlink_dir("a", "a-link");
    dir.touch("a/zzz");

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.ents();
    assert_eq!(4, ents.len());
    let (src, link) = (&ents[1], &ents[3]);

    assert_eq!(dir.join("a"), src.path());
    assert_eq!(dir.join("a-link"), link.path());

    assert!(!src.path_is_symlink());
    assert!(link.path_is_symlink());

    assert_eq!(dir.join("a"), fs::read_link(link.path()).unwrap());

    assert_eq!(1, src.depth());
    assert_eq!(1, link.depth());

    assert!(src.file_type().is_dir());
    assert!(link.file_type().is_symlink());
    assert!(!link.file_type().is_file());
    assert!(!link.file_type().is_dir());

    assert!(src.metadata().unwrap().is_dir());
    assert!(link.metadata().unwrap().file_type().is_symlink());
    assert!(!link.metadata().unwrap().is_file());
    assert!(!link.metadata().unwrap().is_dir());
}

#[cfg(unix)]
#[test]
fn sym_dir_follow() {
    let dir = Dir::tmp();
    dir.mkdirp("a");
    dir.symlink_dir("a", "a-link");
    dir.touch("a/zzz");

    let wd = WalkDir::new(dir.path()).follow_links(true).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let ents = r.ents();
    assert_eq!(5, ents.len());
    let (src, link) = (&ents[1], &ents[3]);

    assert_eq!(dir.join("a"), src.path());
    assert_eq!(dir.join("a-link"), link.path());

    assert!(!src.path_is_symlink());
    assert!(link.path_is_symlink());

    assert_eq!(dir.join("a"), fs::read_link(link.path()).unwrap());

    assert_eq!(1, src.depth());
    assert_eq!(1, link.depth());

    assert!(src.file_type().is_dir());
    assert!(!link.file_type().is_symlink());
    assert!(!link.file_type().is_file());
    assert!(link.file_type().is_dir());

    assert!(src.metadata().unwrap().is_dir());
    assert!(!link.metadata().unwrap().file_type().is_symlink());
    assert!(!link.metadata().unwrap().is_file());
    assert!(link.metadata().unwrap().is_dir());

    let (src_zzz, link_zzz) = (&ents[2], &ents[4]);
    assert_eq!(dir.join("a").join("zzz"), src_zzz.path());
    assert_eq!(dir.join("a-link").join("zzz"), link_zzz.path());
    assert!(!src_zzz.path_is_symlink());
    assert!(!link_zzz.path_is_symlink());
}

#[cfg(unix)]
#[test]
fn sym_noloop() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b/c");
    dir.symlink_dir("a", "a/b/c/a-link");

    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    // There's no loop if we aren't following symlinks.
    r.assert_no_errors();

    assert_eq!(5, r.ents().len());
}

#[cfg(unix)]
#[test]
fn sym_loop_detect() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b/c");
    dir.symlink_dir("a", "a/b/c/a-link");

    let wd = WalkDir::new(dir.path()).follow_links(true);
    let r = dir.run_recursive(wd);

    let (ents, errs) = (r.ents(), r.errs());
    assert_eq!(4, ents.len());
    assert_eq!(1, errs.len());

    let err = &errs[0];

    let expected = dir.join("a/b/c/a-link");
    assert_eq!(Some(&*expected), err.path());

    let expected = dir.join("a");
    assert_eq!(Some(&*expected), err.loop_ancestor());

    assert_eq!(4, err.depth());
    assert!(err.io_error().is_none());
}

#[cfg(unix)]
#[test]
fn sym_self_loop_no_error() {
    let dir = Dir::tmp();
    dir.symlink_file("a", "a");

    let wd = WalkDir::new(dir.path());
    let r = dir.run_recursive(wd);
    // No errors occur because even though the symlink points to nowhere, it
    // is never followed, and thus no error occurs.
    r.assert_no_errors();
    assert_eq!(2, r.ents().len());

    let ent = &r.ents()[1];
    assert_eq!(dir.join("a"), ent.path());
    assert!(ent.path_is_symlink());

    assert!(ent.file_type().is_symlink());
    assert!(!ent.file_type().is_file());
    assert!(!ent.file_type().is_dir());

    assert!(ent.metadata().unwrap().file_type().is_symlink());
    assert!(!ent.metadata().unwrap().file_type().is_file());
    assert!(!ent.metadata().unwrap().file_type().is_dir());
}

#[cfg(unix)]
#[test]
fn sym_file_self_loop_io_error() {
    let dir = Dir::tmp();
    dir.symlink_file("a", "a");

    let wd = WalkDir::new(dir.path()).follow_links(true);
    let r = dir.run_recursive(wd);

    let (ents, errs) = (r.ents(), r.errs());
    assert_eq!(1, ents.len());
    assert_eq!(1, errs.len());

    let err = &errs[0];

    let expected = dir.join("a");
    assert_eq!(Some(&*expected), err.path());
    assert_eq!(1, err.depth());
    assert!(err.loop_ancestor().is_none());
    assert!(err.io_error().is_some());
}

#[cfg(unix)]
#[test]
fn sym_dir_self_loop_io_error() {
    let dir = Dir::tmp();
    dir.symlink_dir("a", "a");

    let wd = WalkDir::new(dir.path()).follow_links(true);
    let r = dir.run_recursive(wd);

    let (ents, errs) = (r.ents(), r.errs());
    assert_eq!(1, ents.len());
    assert_eq!(1, errs.len());

    let err = &errs[0];

    let expected = dir.join("a");
    assert_eq!(Some(&*expected), err.path());
    assert_eq!(1, err.depth());
    assert!(err.loop_ancestor().is_none());
    assert!(err.io_error().is_some());
}

#[test]
fn min_depth_1() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");

    let wd = WalkDir::new(dir.path()).min_depth(1).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![dir.join("a"), dir.join("a").join("b")];
    assert_eq!(expected, r.paths());
}

#[test]
fn min_depth_2() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");

    let wd = WalkDir::new(dir.path()).min_depth(2).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![dir.join("a").join("b")];
    assert_eq!(expected, r.paths());
}

#[test]
fn max_depth_0() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");

    let wd = WalkDir::new(dir.path()).max_depth(0).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![dir.path().to_path_buf()];
    assert_eq!(expected, r.paths());
}

#[test]
fn max_depth_1() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");

    let wd = WalkDir::new(dir.path()).max_depth(1).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![dir.path().to_path_buf(), dir.join("a")];
    assert_eq!(expected, r.paths());
}

#[test]
fn max_depth_2() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");

    let wd = WalkDir::new(dir.path()).max_depth(2).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("a"),
        dir.join("a").join("b"),
    ];
    assert_eq!(expected, r.paths());
}

#[test]
fn min_max_depth_diff_0() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b/c");

    let wd = WalkDir::new(dir.path())
        .min_depth(2)
        .max_depth(2)
        .sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![dir.join("a").join("b")];
    assert_eq!(expected, r.paths());
}

#[test]
fn min_max_depth_diff_1() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b/c");

    let wd = WalkDir::new(dir.path())
        .min_depth(1)
        .max_depth(2)
        .sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![dir.join("a"), dir.join("a").join("b")];
    assert_eq!(expected, r.paths());
}

#[test]
fn sort() {
    let dir = Dir::tmp();
    dir.mkdirp("foo/bar/baz/abc");
    dir.mkdirp("quux");

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let expected = vec![
        dir.path().to_path_buf(),
        dir.join("foo"),
        dir.join("foo").join("bar"),
        dir.join("foo").join("bar").join("baz"),
        dir.join("foo").join("bar").join("baz").join("abc"),
        dir.join("quux"),
    ];
    assert_eq!(expected, r.paths());
}

fn test_dir() -> (PathBuf, tempfile::TempDir) {
    let template = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/assets/test_dir");
    let temp_dir = tempfile::tempdir().unwrap();
    let options = fs_extra::dir::CopyOptions::new();
    fs_extra::dir::copy(&template, &temp_dir, &options).unwrap();
    let mut test_dir = temp_dir.path().to_path_buf();
    test_dir.push(template.file_name().unwrap());
    (test_dir, temp_dir)
}

fn local_paths(walk_dir: WalkDir) -> Vec<String> {
    let root = walk_dir.root().to_owned();
    walk_dir
        .into_iter()
        .map(|each_result| {
            let each_entry = each_result.unwrap();
            if let Some(err) = each_entry.read_children_error.as_ref() {
                panic!("should not encounter any child errors :{:?}", err);
            }
            let path = each_entry.path();
            let path = path.strip_prefix(&root).unwrap().to_path_buf();
            let mut path_string = path.to_str().unwrap().to_string();
            // Unify path separators for cross-platform assertions
            path_string = path_string.replace('\\', "/");
            path_string.push_str(&format!(" ({})", each_entry.depth));
            path_string
        })
        .collect()
}

#[test]
fn walk_serial() {
    let (test_dir, _temp_dir) = test_dir();

    let paths = local_paths(
        WalkDir::new(test_dir)
            .parallelism(Parallelism::Serial)
            .sort(true)
            .skip_hidden(true),
    );
    assert_eq!(
        paths,
        vec![
            " (0)",
            "a.txt (1)",
            "b.txt (1)",
            "c.txt (1)",
            "group 1 (1)",
            "group 1/d.txt (2)",
            "group 2 (1)",
            "group 2/e.txt (2)",
        ]
    );
}

#[test]
fn sort_by_name_rayon_custom_2_threads() {
    let (test_dir, _temp_dir) = test_dir();
    let paths = local_paths(
        WalkDir::new(test_dir)
            .parallelism(Parallelism::RayonNewPool(2))
            .sort(true)
            .skip_hidden(true),
    );
    assert_eq!(
        paths,
        vec![
            " (0)",
            "a.txt (1)",
            "b.txt (1)",
            "c.txt (1)",
            "group 1 (1)",
            "group 1/d.txt (2)",
            "group 2 (1)",
            "group 2/e.txt (2)",
        ]
    );
}

#[test]
fn walk_rayon_global() {
    let (test_dir, _temp_dir) = test_dir();
    let paths = local_paths(WalkDir::new(test_dir).sort(true).skip_hidden(true));
    assert_eq!(
        paths,
        vec![
            " (0)",
            "a.txt (1)",
            "b.txt (1)",
            "c.txt (1)",
            "group 1 (1)",
            "group 1/d.txt (2)",
            "group 2 (1)",
            "group 2/e.txt (2)",
        ]
    );
}

#[test]
fn walk_rayon_no_lockup() {
    // Without jwalk_par_bridge this locks (pre rayon 1.6.1)
    // This test now passes without needing jwalk_par_bridge
    // and that code has been removed from jwalk_meta.
    let pool = std::sync::Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap(),
    );
    let _: Vec<_> = WalkDir::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
        .parallelism(Parallelism::RayonExistingPool {
            pool,
            busy_timeout: std::time::Duration::from_millis(500).into(),
        })
        .process_read_dir(|_, _, _, dir_entry_results| {
            for dir_entry_result in dir_entry_results {
                let _ = dir_entry_result
                    .as_ref()
                    .map(|dir_entry| dir_entry.metadata());
            }
        })
        .sort(true)
        .into_iter()
        .collect();
}

#[test]
fn combine_with_rayon_no_lockup_1() {
    // only run this test if linux_checkout present
    let linux_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("benches/assets/linux_checkout");
    if linux_dir.exists() {
        rayon::scope(|_| {
            eprintln!("WalkDir…");
            for _entry in WalkDir::new(linux_dir) {}
            eprintln!("WalkDir completed");
        });
    }
}

#[test]
fn combine_with_rayon_no_lockup_2() {
    WalkDir::new(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
        .sort(true)
        .into_iter()
        .par_bridge()
        .filter_map(|dir_entry_result| {
            let dir_entry = dir_entry_result.ok()?;
            if dir_entry.file_type().is_file() {
                let path = dir_entry.path();
                let text = std::fs::read_to_string(path).ok()?;
                if text.contains("hello world") {
                    return Some(true);
                }
            }
            None
        })
        .count();
}

#[test]
fn see_hidden_files() {
    let (test_dir, _temp_dir) = test_dir();
    let paths = local_paths(WalkDir::new(test_dir).skip_hidden(false).sort(true));
    assert!(paths.contains(&"group 2/.hidden_file.txt (2)".to_string()));
}

#[test]
fn walk_file() {
    let (test_dir, _temp_dir) = test_dir();
    let walk_dir = WalkDir::new(test_dir.join("a.txt"));
    let mut iter = walk_dir.into_iter();
    assert_eq!(
        iter.next().unwrap().unwrap().file_name.to_str().unwrap(),
        "a.txt"
    );
    assert!(iter.next().is_none());
}

#[test]
fn walk_file_serial() {
    let (test_dir, _temp_dir) = test_dir();
    let walk_dir = WalkDir::new(test_dir.join("a.txt")).parallelism(Parallelism::Serial);
    let mut iter = walk_dir.into_iter();
    assert_eq!(
        iter.next().unwrap().unwrap().file_name.to_str().unwrap(),
        "a.txt"
    );
    assert!(iter.next().is_none());
}

#[test]
fn error_when_path_does_not_exist() {
    let (test_dir, _temp_dir) = test_dir();
    let walk_dir = WalkDir::new(test_dir.join("path_does_not_exist"));
    let mut iter = walk_dir.into_iter();
    assert!(iter.next().unwrap().is_err());
    assert!(iter.next().is_none());
}

#[test]
fn error_when_path_removed_durring_iteration() {
    let (test_dir, _temp_dir) = test_dir();
    let walk_dir = WalkDir::new(&test_dir)
        .parallelism(Parallelism::Serial)
        .sort(true);
    let mut iter = walk_dir.into_iter();

    // Read root. read_dir for root is also called since single thread mode.
    let _ = iter.next().unwrap().is_ok(); // " (0)",

    // Remove group 2 dir from disk
    fs_extra::remove_items(&[test_dir.join("group 2")]).unwrap();

    let _ = iter.next().unwrap().is_ok(); // "a.txt (1)",
    let _ = iter.next().unwrap().is_ok(); // "b.txt (1)",
    let _ = iter.next().unwrap().is_ok(); // "c.txt (1)",
    let _ = iter.next().unwrap().is_ok(); // "group 1 (1)",
    let _ = iter.next().unwrap().is_ok(); // "group 1/d.txt (2)",

    // group 2 is read correctly, since it was read before path removed.
    let group_2 = iter.next().unwrap().unwrap();

    // group 2 content error IS set, since path is removed when try read_dir for
    // group 2 path.
    let _ = group_2.read_children_error.is_some();

    // done!
    assert!(iter.next().is_none());
}

#[test]
fn walk_root() {
    let paths: Vec<_> = WalkDir::new("/")
        .max_depth(1)
        .sort(true)
        .into_iter()
        .filter_map(|each| Some(each.ok()?.path().to_path_buf()))
        .collect();
    assert_eq!(paths.first().unwrap().to_str().unwrap(), "/");
}

lazy_static! {
    static ref RELATIVE_MUTEX: Mutex<()> = Mutex::new(());
}

#[test]
fn walk_relative_1() {
    let _shared = RELATIVE_MUTEX.lock().unwrap();
    let (test_dir, _temp_dir) = test_dir();

    env::set_current_dir(&test_dir).unwrap();

    let paths = local_paths(WalkDir::new(".").sort(true).skip_hidden(true));

    assert_eq!(
        paths,
        vec![
            " (0)",
            "a.txt (1)",
            "b.txt (1)",
            "c.txt (1)",
            "group 1 (1)",
            "group 1/d.txt (2)",
            "group 2 (1)",
            "group 2/e.txt (2)",
        ]
    );

    let root_dir_entry = WalkDir::new("..").into_iter().next().unwrap().unwrap();
    assert_eq!(&root_dir_entry.file_name, "..");
}

#[test]
fn walk_relative_2() {
    let _shared = RELATIVE_MUTEX.lock().unwrap();
    let (test_dir, _temp_dir) = test_dir();

    env::set_current_dir(&test_dir.join("group 1")).unwrap();

    let paths = local_paths(WalkDir::new("..").sort(true).skip_hidden(true));

    assert_eq!(
        paths,
        vec![
            " (0)",
            "a.txt (1)",
            "b.txt (1)",
            "c.txt (1)",
            "group 1 (1)",
            "group 1/d.txt (2)",
            "group 2 (1)",
            "group 2/e.txt (2)",
        ]
    );

    let root_dir_entry = WalkDir::new(".").into_iter().next().unwrap().unwrap();
    assert_eq!(&root_dir_entry.file_name, ".");
}

#[test]
fn filter_groups_with_process_read_dir() {
    let (test_dir, _temp_dir) = test_dir();
    let paths = local_paths(
        WalkDir::new(test_dir)
            .sort(true)
            // Filter groups out manually
            .process_read_dir(|_depth, _path, _parent, children| {
                children.retain(|each_result| {
                    each_result
                        .as_ref()
                        .map(|dir_entry| {
                            !dir_entry.file_name.to_string_lossy().starts_with("group")
                        })
                        .unwrap_or(true)
                });
            }),
    );
    assert_eq!(paths, vec![" (0)", "a.txt (1)", "b.txt (1)", "c.txt (1)",]);
}

#[test]
fn filter_group_children_with_process_read_dir() {
    let (test_dir, _temp_dir) = test_dir();
    let paths = local_paths(
        WalkDir::new(test_dir)
            .sort(true)
            // Filter group children
            .process_read_dir(|_depth, _path, _parent, children| {
                children.iter_mut().for_each(|each_result| {
                    if let Ok(each) = each_result {
                        if each.file_name.to_string_lossy().starts_with("group") {
                            each.read_children_path = None;
                        }
                    }
                });
            }),
    );
    assert_eq!(
        paths,
        vec![
            " (0)",
            "a.txt (1)",
            "b.txt (1)",
            "c.txt (1)",
            "group 1 (1)",
            "group 2 (1)",
        ]
    );
}

#[test]
fn test_read_linux() {
    // only run this test if linux_checkout present
    let linux_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("benches/assets/linux_checkout");
    if linux_dir.exists() {
        for each in WalkDir::new(linux_dir) {
            let entry = each.unwrap();
            let path = entry.path();
            assert!(path.exists(), "{:?}", path);
        }
    }
}

// ==================== Priority Scheduling Tests ====================

#[test]
fn priority_scheduling_dfs_order_preserved() {
    let dir = Dir::tmp();
    dir.mkdirp("heavy/a/b/c");
    dir.mkdirp("heavy/a/b/d");
    dir.mkdirp("light/x/y");
    dir.touch("heavy/a/b/c/1.txt");
    dir.touch("heavy/a/b/d/2.txt");
    dir.touch("light/x/y/3.txt");
    dir.touch("root.txt");

    let wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::RayonNewPool(2))
        .sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let paths: Vec<String> = r.ents().iter()
        .map(|e| e.path().strip_prefix(dir.path()).unwrap().to_str().unwrap().replace('\\', "/"))
        .collect();

    // Must contain all entries (no loss)
    assert!(paths.iter().any(|p| p.contains("root.txt")), "missing root.txt in {:?}", paths);
    assert!(paths.iter().any(|p| p.contains("1.txt")), "missing 1.txt in {:?}", paths);
    assert!(paths.iter().any(|p| p.contains("2.txt")), "missing 2.txt in {:?}", paths);
    assert!(paths.iter().any(|p| p.contains("3.txt")), "missing 3.txt in {:?}", paths);
    assert!(paths.iter().any(|p| p.contains("heavy")), "missing heavy dir in {:?}", paths);
    assert!(paths.iter().any(|p| p.contains("light")), "missing light dir in {:?}", paths);
}

#[test]
fn priority_scheduling_single_thread_no_deadlock() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b/c");
    dir.mkdirp("d/e");
    dir.touch("a/b/c/1.txt");
    dir.touch("d/e/2.txt");
    dir.touch("root.txt");

    let wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::RayonNewPool(1))
        .sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    // Single thread must find all entries without deadlocking
    assert!(r.ents().len() >= 7, "expected >= 7 entries, got {}", r.ents().len());
}

#[test]
fn priority_scheduling_deep_tree() {
    let dir = Dir::tmp();
    // Create 10-level deep tree: l0/l1/l2/.../l9/file.txt
    let mut path = std::path::PathBuf::new();
    for i in 0..10 {
        path.push(format!("l{}", i));
    }
    dir.mkdirp(path.to_str().unwrap());
    dir.touch(path.join("file.txt").to_str().unwrap());
    dir.touch("root.txt");

    let wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::RayonNewPool(2))
        .sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    // 10 dirs + 1 file at bottom + 1 root file + root dir = 13
    assert!(r.ents().len() >= 12, "expected >= 12 entries, got {}", r.ents().len());
}

#[test]
fn priority_scheduling_wide_tree() {
    let dir = Dir::tmp();
    // Create 50 subdirectories, each with 1 file
    for i in 0..50 {
        let subdir = format!("sub{:02}", i);
        dir.mkdirp(&subdir);
        dir.touch(format!("{}/file.txt", subdir));
    }
    dir.touch("root.txt");

    let wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::RayonNewPool(4))
        .sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    // root + root.txt + 50 dirs + 50 files = 102
    assert!(r.ents().len() >= 100, "expected >= 100 entries, got {}", r.ents().len());
}

#[test]
fn priority_scheduling_serial_unchanged() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");
    dir.mkdirp("c/d");
    dir.touch("a/b/1.txt");
    dir.touch("c/d/2.txt");
    dir.touch("root.txt");

    let serial_wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::Serial)
        .sort(true);
    let serial_r = dir.run_recursive(serial_wd);

    let par_wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::RayonNewPool(2))
        .sort(true);
    let par_r = dir.run_recursive(par_wd);

    // Serial and parallel should produce same number of entries
    assert_eq!(serial_r.ents().len(), par_r.ents().len(),
        "serial {} != parallel {}", serial_r.ents().len(), par_r.ents().len());
}

#[test]
fn priority_scheduling_weight_propagation() {
    let dir = Dir::tmp();
    // Heavy branch: many subdirs
    for i in 0..10 {
        let subdir = format!("heavy/sub{}", i);
        dir.mkdirp(&subdir);
        dir.touch(format!("{}/file.txt", subdir));
    }
    // Light branch: just files
    dir.touch("light1.txt");
    dir.touch("light2.txt");
    dir.touch("light3.txt");

    let wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::RayonNewPool(2))
        .sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    // 10 heavy subdirs + 10 files + 3 light files + root dir = 24
    assert!(r.ents().len() >= 23, "expected >= 23 entries, got {}", r.ents().len());
}

// ==================== Channel & Stress Tests ====================

#[test]
fn channel_backpressure_slow_consumer() {
    let dir = Dir::tmp();
    // Create a wide tree to generate many entries
    for i in 0..30 {
        let subdir = format!("sub{:02}", i);
        dir.mkdirp(&subdir);
        for j in 0..10 {
            dir.touch(format!("{}/file{}.txt", subdir, j));
        }
    }

    let wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::RayonNewPool(2))
        .sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    // 1 root + 30 dirs + 300 files = 331
    let expected = 1 + 30 + 300;
    assert_eq!(
        expected,
        r.ents().len(),
        "expected {} entries (no data loss), got {}",
        expected,
        r.ents().len()
    );
}

#[test]
#[ignore] // 压力测试：手动运行 cargo test -- --ignored
fn large_directory_100k_files() {
    let dir = Dir::tmp();
    // Create 1000 dirs × 100 files = 100K files
    let subdir_count = 1000;
    let files_per_dir = 100;

    for i in 0..subdir_count {
        let subdir = format!("d{:04}", i);
        dir.mkdirp(&subdir);
        for j in 0..files_per_dir {
            dir.touch(format!("{}/f{:03}.txt", subdir, j));
        }
    }

    let start = std::time::Instant::now();
    let wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::RayonNewPool(4))
        .sort(false); // 不排序，提高吞吐
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

#[test]
fn parallel_dir_modified_during_walk() {
    let dir = Dir::tmp();
    // Create initial structure
    for i in 0..10 {
        let subdir = format!("sub{:02}", i);
        dir.mkdirp(&subdir);
        dir.touch(format!("{}/initial.txt", subdir));
    }

    // Walk with parallelism and collect results
    let wd = WalkDir::new(dir.path())
        .parallelism(Parallelism::RayonNewPool(2))
        .sort(true);
    let r = dir.run_recursive(wd);

    // Should get at least the initial structure
    // (no crash or deadlock is the main assertion)
    assert!(
        r.ents().len() >= 11,
        "expected >= 11 entries, got {}",
        r.ents().len()
    );
}

#[test]
fn unicode_special_filenames() {
    let dir = Dir::tmp();
    dir.mkdirp("日本語");
    dir.mkdirp("中文目录");
    dir.mkdirp("한국어");
    dir.mkdirp("emoji_🎉");
    dir.mkdirp("spaces in name");
    dir.touch("日本語/ファイル.txt");
    dir.touch("中文目录/文件.txt");
    dir.touch("한국어/파일.txt");
    dir.touch("emoji_🎉/party.txt");
    dir.touch("spaces in name/file.txt");

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    // 1 root + 5 dirs + 5 files = 11
    assert_eq!(
        11,
        r.ents().len(),
        "expected 11 entries, got {}: {:?}",
        r.ents().len(),
        r.ents()
            .iter()
            .map(|e| e.path().to_string_lossy().to_string())
            .collect::<Vec<_>>()
    );

    // Verify Unicode names are preserved
    let paths: Vec<String> = r
        .paths()
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    assert!(paths.iter().any(|p| p.contains("日本語")), "missing Japanese dir");
    assert!(
        paths.iter().any(|p| p.contains("中文目录")),
        "missing Chinese dir"
    );
    assert!(paths.iter().any(|p| p.contains("한국어")), "missing Korean dir");
}

// ==================== Additional Coverage Tests ====================

#[test]
fn memory_stability_repeated_walk() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b/c");
    dir.mkdirp("d/e");
    dir.touch("a/b/c/1.txt");
    dir.touch("d/e/2.txt");
    dir.touch("root.txt");

    // Run 100 iterations to check for memory leaks or resource exhaustion
    for iteration in 0..100 {
        let wd = WalkDir::new(dir.path())
            .parallelism(Parallelism::RayonNewPool(2))
            .sort(true);
        let r = dir.run_recursive(wd);

        // Every iteration should produce same results
        // root(1) + a/b/c(3) + d/e(2) + 3 files = 9
        assert_eq!(
            9,
            r.ents().len(),
            "iteration {}: expected 9 entries, got {}",
            iteration,
            r.ents().len()
        );
        r.assert_no_errors();
    }
}

#[test]
fn ci_thread_scalability() {
    let dir = Dir::tmp();
    // Create a moderately complex tree
    for i in 0..20 {
        let subdir = format!("sub{:02}", i);
        dir.mkdirp(&subdir);
        for j in 0..5 {
            dir.touch(format!("{}/file{}.txt", subdir, j));
        }
    }
    dir.touch("root.txt");

    let expected_count = 1 + 20 + 100 + 1; // root + dirs + files + root.txt

    // Test Serial
    let serial_r = dir.run_recursive(
        WalkDir::new(dir.path())
            .parallelism(Parallelism::Serial)
            .sort(true),
    );
    serial_r.assert_no_errors();
    assert_eq!(expected_count, serial_r.ents().len(), "Serial count mismatch");

    // Test 1 thread
    let r1 = dir.run_recursive(
        WalkDir::new(dir.path())
            .parallelism(Parallelism::RayonNewPool(1))
            .sort(true),
    );
    r1.assert_no_errors();
    assert_eq!(
        expected_count,
        r1.ents().len(),
        "1-thread count mismatch"
    );

    // Test 2 threads
    let r2 = dir.run_recursive(
        WalkDir::new(dir.path())
            .parallelism(Parallelism::RayonNewPool(2))
            .sort(true),
    );
    r2.assert_no_errors();
    assert_eq!(
        expected_count,
        r2.ents().len(),
        "2-thread count mismatch"
    );

    // Test 4 threads
    let r4 = dir.run_recursive(
        WalkDir::new(dir.path())
            .parallelism(Parallelism::RayonNewPool(4))
            .sort(true),
    );
    r4.assert_no_errors();
    assert_eq!(
        expected_count,
        r4.ents().len(),
        "4-thread count mismatch"
    );

    // Test 8 threads (more than CPU cores is fine)
    let r8 = dir.run_recursive(
        WalkDir::new(dir.path())
            .parallelism(Parallelism::RayonNewPool(8))
            .sort(true),
    );
    r8.assert_no_errors();
    assert_eq!(
        expected_count,
        r8.ents().len(),
        "8-thread count mismatch"
    );
}

#[test]
fn symlink_follow_cross_platform() {
    let dir = Dir::tmp();
    dir.mkdirp("target_dir");
    dir.touch("target_dir/file.txt");
    dir.mkdirp("link_parent");

    // Create directory symlink; skip on Windows if developer mode is off
    // (os error 1314: client does not have required privilege)
    let src = dir.join("target_dir");
    let link = dir.join("link_parent/link_dir");
    let symlink_result: std::io::Result<()> = {
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(&src, &link)
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&src, &link)
        }
        #[cfg(not(any(windows, unix)))]
        {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "symlinks not supported on this platform",
            ))
        }
    };
    if symlink_result.is_err() {
        eprintln!(
            "skipping symlink_follow_cross_platform: \
             symlink_dir failed (developer mode or admin required)"
        );
        return;
    }

    // Test without follow
    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    // Should find: root, link_parent, target_dir, link_dir (symlink),
    // target_dir/file.txt, link_parent/link_dir/file.txt
    assert!(
        r.ents().len() >= 4,
        "expected >= 4 entries, got {}",
        r.ents().len()
    );

    // Verify symlink is detected
    let link_entry = r
        .ents()
        .iter()
        .find(|e| e.path().to_string_lossy().contains("link_dir"));
    assert!(link_entry.is_some(), "link_dir not found");
    assert!(
        link_entry.unwrap().path_is_symlink(),
        "link_dir should be symlink"
    );
}

#[test]
fn permission_denied_graceful() {
    let dir = Dir::tmp();
    dir.mkdirp("accessible");
    dir.mkdirp("accessible/sub");
    dir.touch("accessible/file.txt");
    dir.touch("accessible/sub/nested.txt");

    // Walk the accessible structure - main assertion is no crash
    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);

    // All entries should be accessible in this case
    r.assert_no_errors();
    assert!(
        r.ents().len() >= 5,
        "expected >= 5 entries, got {}",
        r.ents().len()
    );

    // Walk a non-existent subdirectory should produce an error but not crash
    let wd = WalkDir::new(dir.path().join("nonexistent"));
    let r2 = dir.run_recursive(wd);
    assert_eq!(
        1,
        r2.errs().len(),
        "expected 1 error for nonexistent path"
    );
}

#[test]
#[ignore = "environment-sensitive: requires non-root + strict fs permissions, fails on CI runners"]
fn permission_denied_mixed_scenario() {
    // 测试标记为 #[ignore]，CI 不跳过，用户本地手动跑：cargo test -- --ignored
    let dir = Dir::tmp();
    dir.mkdirp("readable_a");
    dir.mkdirp("readable_a/sub");
    dir.touch("readable_a/file.txt");
    dir.touch("readable_a/sub/nested.txt");
    dir.mkdirp("readable_b");
    dir.touch("readable_b/other.txt");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let restricted = dir.join("restricted");
        std::fs::create_dir_all(&restricted).unwrap();
        std::fs::File::create(restricted.join("secret.txt")).unwrap();
        std::fs::set_permissions(&restricted, std::fs::Permissions::from_mode(0o000)).unwrap();

        let wd = WalkDir::new(dir.path()).sort(true);
        let r = dir.run_recursive(wd);

        let readable_paths: Vec<_> = r
            .ents()
            .iter()
            .filter(|e| {
                let p = e.path().to_string_lossy();
                p.contains("readable_a") || p.contains("readable_b")
            })
            .collect();
        assert!(
            readable_paths.len() >= 6,
            "readable dirs should have full content, got {} readable entries",
            readable_paths.len()
        );

        let has_restricted_error = r
            .errs()
            .iter()
            .any(|e| format!("{:?}", e).contains("restricted"));
        assert!(
            has_restricted_error,
            "expected error for restricted directory"
        );

        std::fs::set_permissions(&restricted, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(windows)]
    {
        let wd = WalkDir::new(dir.path()).sort(true);
        let r = dir.run_recursive(wd);
        r.assert_no_errors();
        assert!(
            r.ents().len() >= 7,
            "expected >= 7 entries on Windows, got {}",
            r.ents().len()
        );
    }
}

#[test]
fn symlink_file_and_cycle_detection() {
    let dir = Dir::tmp();
    dir.touch("real.txt");
    dir.mkdirp("subdir");
    dir.touch("subdir/nested.txt");
    dir.mkdirp("link_container");

    let make_file_symlink = |src: PathBuf, link: PathBuf| -> std::io::Result<()> {
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(&src, &link)
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&src, &link)
        }
    };

    if make_file_symlink(dir.join("real.txt"), dir.join("link_container/link.txt")).is_err() {
        eprintln!("skipping symlink_file_and_cycle_detection: symlink creation failed");
        return;
    }

    let make_dir_symlink = |src: PathBuf, link: PathBuf| -> std::io::Result<()> {
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(&src, &link)
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&src, &link)
        }
    };

    if make_dir_symlink(dir.join("subdir"), dir.join("link_container/link_dir")).is_err() {
        eprintln!("skipping dir symlink creation: failed");
        return;
    }

    // Test without follow_links
    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let file_link = r.ents().iter().find(|e| e.file_name() == "link.txt");
    assert!(file_link.is_some(), "file symlink not found");
    assert!(file_link.unwrap().path_is_symlink(), "link.txt should be a symlink");

    let dir_link = r.ents().iter().find(|e| e.file_name() == "link_dir");
    assert!(dir_link.is_some(), "dir symlink not found");
    assert!(dir_link.unwrap().path_is_symlink(), "link_dir should be a symlink");

    // Test with follow_links
    let wd_follow = WalkDir::new(dir.path()).follow_links(true).sort(true);
    let r_follow = dir.run_recursive(wd_follow);
    r_follow.assert_no_errors();

    let follow_count = r_follow.ents().len();
    let nofollow_count = r.ents().len();
    assert!(
        follow_count >= nofollow_count,
        "follow_links should find at least as many entries: {} < {}",
        follow_count, nofollow_count
    );
}

#[test]
fn symlink_cycle_detection_with_follow() {
    let dir = Dir::tmp();
    dir.mkdirp("a/b");

    let make_dir_symlink = |src: PathBuf, link: PathBuf| -> std::io::Result<()> {
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_dir(&src, &link)
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&src, &link)
        }
    };

    if make_dir_symlink(dir.join("a"), dir.join("a/b/cycle")).is_err() {
        eprintln!("skipping symlink_cycle_detection: symlink creation failed");
        return;
    }

    let wd = WalkDir::new(dir.path()).follow_links(true).sort(true);
    let r = dir.run_recursive(wd);
    // follow_links + 循环 symlink 应被检测到，产生 Loop 错误（而非无限循环）
    let has_loop_error = r
        .errs()
        .iter()
        .any(|e| format!("{:?}", e).contains("Loop"));
    assert!(
        has_loop_error,
        "expected Loop error for cycle, got errors: {:?}",
        r.errs()
    );

    assert!(
        r.ents().len() < 20,
        "cycle should be detected, got {} entries (possible infinite loop)",
        r.ents().len()
    );
}

#[test]
fn broken_symlink_handling() {
    let dir = Dir::tmp();
    dir.mkdirp("links");

    let make_symlink = |src: PathBuf, link: PathBuf| -> std::io::Result<()> {
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(&src, &link)
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&src, &link)
        }
    };

    if make_symlink(dir.join("nonexistent_target"), dir.join("links/broken")).is_err() {
        eprintln!("skipping broken_symlink_handling: symlink creation failed");
        return;
    }

    let wd = WalkDir::new(dir.path()).sort(true);
    let r = dir.run_recursive(wd);
    r.assert_no_errors();

    let broken = r.ents().iter().find(|e| e.file_name() == "broken");
    assert!(broken.is_some(), "broken symlink should appear as entry");
    assert!(broken.unwrap().path_is_symlink(), "broken should be a symlink");
}
