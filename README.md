# jwalk-meta

High-performance parallel directory traversal library optimized for large-scale filesystem scans.

Forked from [jwalk](https://github.com/Byron/jwalk). It preserves the original parallel traversal capabilities and adds Windows NT Native API enumeration, streaming subdirectory dispatch, and priority-flood scheduling.

## Features

- **rayon parallel traversal** — Directory-level parallelism automatically uses all CPU cores.
- **NT Native API enumeration** (Windows) — Uses `NtQueryDirectoryFileEx` with a 64 KB batch buffer instead of `FindFirstFile/FindNextFile`. Each system call retrieves hundreds of entries, greatly reducing user/kernel transitions.
- **Streaming subdirectory dispatch** — While enumerating a huge directory, subdirectories discovered in each NtQuery batch are immediately added to the scheduling queue instead of waiting for the entire directory to finish. This eliminates startup latency when traversing directories with millions of entries.
- **Priority-flood scheduling** — `weight = parent_weight + subdir_count`. Branches with more subdirectories receive higher scheduling priority, automatically assigning more threads to the largest subtrees.
- **Ordered streaming output** — DFS preorder ensures parent directories are emitted before their children; `OrderedMatcher` tracks the traversal cursor.
- **Extended metadata** — Optionally collects file attributes, timestamps, sizes, and other metadata to avoid additional `stat` calls.

## Performance

Measured on Windows 11 using an SMB network share (`Z:\测试`).


| Metric       | Value           |
| -------- | --------------- |
| Total entries | 1,981,338       |
| Directories   | 412,935         |
| Files         | 1,568,403       |
| Total time    | 377 seconds     |
| Throughput    | 5,255 entries/s |
| First-second output | 15,007 entries |
| Errors        | 0               |

Before optimization, the first 265 seconds of scanning the same directory produced only 271 entries (blocked while enumerating a single huge directory).

## Installation

```toml
[dependencies]
jwalk-meta = "1.0"
```

The stable Rust toolchain is supported.

## Usage

### Basic usage

```rust
use jwalk_meta::WalkDir;

for entry in WalkDir::new("foo").sort(true) {
    println!("{}", entry?.path().display());
}
```

### Collecting metadata

```rust
use jwalk_meta::WalkDir;

for entry in WalkDir::new("foo")
    .read_metadata(true)
{
    let entry = entry?;
    if let Some(metadata) = &entry.metadata {
        println!("{} ({} bytes)", entry.path().display(), metadata.size);
    }
}
```

## Architecture

```
                    ┌─────────────┐
                    │   Root Dir  │ weight = usize::MAX >> 1
                    └──────┬──────┘
                           │
              ┌────────────┼────────────┐
              ▼            ▼            ▼
        ┌──────────┐ ┌──────────┐ ┌──────────┐
        │ Dir A    │ │ Dir B    │ │ Dir C    │
        │ children │ │ children │ │ children │
        │ = 150    │ │ = 2      │ │ = 50     │
        │ weight   │ │ weight   │ │ weight   │
        │ = MAX    │ │ = MAX    │ │ = MAX    │
        │ +150     │ │ +2       │ │ +50      │
        └──────────┘ └──────────┘ └──────────┘
              ▲
              │  Priority flood: more subdirectories → higher weight → more threads assigned
              │
         BinaryHeap (max-heap) scheduling
```

### Core components


| Component                 | File                | Responsibility                       |
| ------------------------- | ------------------- | ------------------------------------- |
| `Weighted<T>`             | `weighted.rs`       | Weighted scheduling item for a BinaryHeap max-heap |
| `PriorityQueue`           | `priority_queue.rs` | Thread-safe priority queue and channel |
| `StreamingContext`        | `read_dir_iter.rs`  | Streaming dispatch context carrying `parent_weight` |
| `ReadDirIter`             | `read_dir_iter.rs`  | Parallel traversal iterator with streaming and regular modes |
| `enumerate_dir_streaming` | `nt_dir_enum.rs`    | NT Native API streaming enumeration (Windows) |
| `OrderedQueue`            | `ordered_queue.rs`  | Ordered output queue with Strict/Relaxed ordering |

## Acknowledgments

- [jwalk](https://github.com/Byron/jwalk) — Original parallel traversal framework
- [walkdir](https://crates.io/crates/walkdir) — Streaming iterator API design
- [ignore](https://crates.io/crates/ignore) — Parallel traversal ideas

## License

Licensed under either of

- [MIT](LICENSE)
- [Apache License, Version 2.0](LICENSE-APACHE)

at your option.
