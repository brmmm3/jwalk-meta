jwalk-meta
==========

Filesystem walk.

- Performed in parallel using rayon
- Entries streamed in sorted order
- Custom sort/filter/skip/state

This is a fork of [https://github.com/Byron/jwalk](https://github.com/Byron/jwalk). This project adds optional collecting metadata to improve performance if metadata is needed later.

[![Build Status](https://travis-ci.org/brmmm3/jwalk-meta.svg?branch=master)](https://travis-ci.org/brmmm3/jwalk-meta)
[![Latest version](http://meritbadge.herokuapp.com/jwalk-meta)](https://crates.io/crates/jwalk-meta)

### Usage

Add this to your `Cargo.toml`:

```toml
[dependencies]
jwalk-meta = "0.9"
```

Lean More: [docs.rs/jwalk-meta](https://docs.rs/jwalk-meta)

### Example

Recursively iterate over the "foo" directory sorting by name:

```rust
use jwalk_meta::{WalkDir};

for entry in WalkDir::new("foo").sort(true) {
  println!("{}", entry?.path().display());
}
```

### Inspiration

This crate is inspired by both [`walkdir`](https://crates.io/crates/walkdir) and
[`ignore`](https://crates.io/crates/ignore). It attempts to combine the
parallelism of `ignore` with `walkdir`'s streaming iterator API. Some code and
comments are copied directly from `walkdir`.

### Why use this crate?

This crate is particularly good when you want streamed sorted results. In my
tests it's about 4x `walkdir` speed for sorted results with metadata. Also this
crate's `process_read_dir` callback allows you to arbitrarily
sort/filter/skip/state entries before they are yielded.

### Why not use this crate?

Directory traversal is already pretty fast. If you don't need this crate's speed
then `walkdir` provides a smaller and more tested single threaded
implementation.

This crates parallelism happens at the directory level. It will help when
walking deep file systems with many directories. It wont help when reading a
single directory with many files.

### Benchmarks

[Benchmarks](https://github.com/brmmm3/jwalk-meta/blob/master/benches/benchmarks.md)
comparing this crate with `walkdir` and `ignore`.
