All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## 0.9.1

### Changed

- Make from_path public.

## 0.9.0

### Changed

- Rename project to jwalk-meta.

## 0.8.4

Merge changes from original jwalk repo.

### New Features

 - re-export `rayon` in the crate root.
   This makes creating a `ThreadPool` easier as it doesn't force us to
   maintain our own `rayon` dependency.
 - `Parallelism::RayonExistingPool::busy_timeout` is now optional.
   That way we can indicate that no waiting should be done as we know the
   given threadpool has enough resources.

## 0.8.3

Fix dependencies problems.
Fix several typos.

## 0.8.2

Update dependencies.

## 0.8.1

Fix Windows issues.

## 0.8.0

Fix clippy findings and make some methods of DirEntry public.

## 0.7.1

Do not crash on file permission error.

## 0.7.0

Added argument read_metadata und read_metadata_ext to method new.
Extended DirEntry struct with optional MetaData and MetaDataExt.

## 0.6.0

Added depth and path being read to params to ProcessReadDirFunction callback.

Allow setting initial root_read_dir_state (ReadDirState) instead of always
getting ::default() value.

## 0.5.0

First major change is that API and behavior are now closer to [`walkdir`] and
jwalk now runs the majority of `walkdir`s tests.

Second major change is the walk can now be parameterized with a client state
type. This state can be manipulated from the `process_read_dir` callback and
then is passed down when reading descendens with the `process_read_dir`
callback.

Part of this second change is that `preload_metadata` option is removed. That
means `DirEntry.metadata()` is never a cached value. Instead you want to read
metadata you should do it in the `process_entries` callback and store whatever
values you need as `client_state`. See this [benchmark] as an example.

[benchmark]: https://github.com/jessegrosjean/jwalk/blob/master/benches/walk_benchmark.rs#L45
