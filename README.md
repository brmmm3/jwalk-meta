# jwalk-meta

高性能并行目录遍历库，专为大规模文件系统扫描优化。

基于 [jwalk](https://github.com/Byron/jwalk) fork，在保留原有并行遍历能力的基础上，新增 Windows NT Native API 枚举、流式子目录分发和优先淹没调度算法。

## 特性

- **rayon 并行遍历** — 目录级并行，自动利用所有 CPU 核心
- **NT Native API 枚举** (Windows) — 使用 `NtQueryDirectoryFileEx` + 64KB 批量缓冲区替代 `FindFirstFile/FindNextFile`，单次系统调用获取数百条目，大幅减少用户态-内核态切换
- **流式子目录分发** — 枚举巨型目录时，每批 NtQuery 发现子目录立即推入调度队列，不等整个目录枚举完成。消除百万级目录的启动延迟
- **优先淹没算法** — `weight = parent_weight + subdir_count`，子目录越多的分支获得越高调度优先级，自动将更多线程分配给"大水管"子树
- **有序流式输出** — DFS 前序保证父目录先于子目录产出，OrderedMatcher 跟踪遍历游标
- **元数据扩展** — 可选收集文件属性、时间戳、大小等元数据，避免二次 `stat` 调用

## 性能

实测环境：Windows 11，SMB 网络共享 (`Z:\测试`)


| 指标     | 数值            |
| -------- | --------------- |
| 总条目数 | 1,981,338       |
| 目录数   | 412,935         |
| 文件数   | 1,568,403       |
| 总耗时   | 377 秒          |
| 吞吐量   | 5,255 entries/s |
| 首秒产出 | 15,007 entries  |
| 错误数   | 0               |

对比优化前：同目录扫描首 265 秒仅有 271 条产出（卡在单个巨型目录的阻塞枚举上）。

## 安装

```toml
[dependencies]
jwalk-meta = "1.0"
```

需要 Rust nightly 工具链。

## 使用

### 基本用法

```rust
use jwalk_meta::WalkDir;

for entry in WalkDir::new("foo").sort(true) {
    println!("{}", entry?.path().display());
}
```

### 带元数据收集

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

## 架构

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
              │  优先淹没：子目录越多 → 权重越高 → 越多线程处理
              │
         BinaryHeap (max-heap) 调度
```

### 核心组件


| 组件                      | 文件                | 职责                                  |
| ------------------------- | ------------------- | ------------------------------------- |
| `Weighted<T>`             | `weighted.rs`       | 带权重的调度单元，BinaryHeap max-heap |
| `PriorityQueue`           | `priority_queue.rs` | 线程安全的优先级队列 + channel        |
| `StreamingContext`        | `read_dir_iter.rs`  | 流式分发上下文，携带 parent_weight    |
| `ReadDirIter`             | `read_dir_iter.rs`  | 并行遍历迭代器，流式/常规双模式       |
| `enumerate_dir_streaming` | `nt_dir_enum.rs`    | NT Native API 流式枚举（Windows）     |
| `OrderedQueue`            | `ordered_queue.rs`  | 有序输出队列，Strict/Relaxed 排序     |

## 致谢

- [jwalk](https://github.com/Byron/jwalk) — 原始并行遍历框架
- [walkdir](https://crates.io/crates/walkdir) — 流式迭代器 API 设计
- [ignore](https://crates.io/crates/ignore) — 并行遍历思路

## 许可证

MIT
