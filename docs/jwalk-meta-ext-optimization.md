# jwalk-meta Ext 模式性能优化指南

> 本文档分析 Ext 模式性能瓶颈（Base 0.34s vs Ext 3.5s，慢 936%），并提供分级优化策略供 jwalk-meta 开发者参考。

## 问题概述

### 性能基准

| 模式 | D: 盘扫描时间 | 相对性能 |
|------|--------------|---------|
| Count(Base) | 0.34s | 1.0x (基准) |
| Count(Ext) | 3.5s | 0.11x (慢 936%) |

**目标**：将 Ext 模式性能提升至接近 Base 模式。

### Ext 模式数据获取分析

`read_metadata_ext = true` 时，jwalk-meta 获取的 `MetaDataExt` 包含四个字段：

| 字段 | 来源 | 开销 |
|------|------|------|
| `file_attributes` | NtQueryDirectoryFileEx (目录枚举) | ✅ 免费 |
| `file_index` | NtQueryDirectoryFileEx (目录枚举) | ✅ 免费 |
| `number_of_links` | NtQueryInformationByName (逐文件查询) | 🔴 昂贵 |
| `volume_serial_number` | GetVolumeInformationByHandleW (每目录一次) | 🟡 低开销 |

**关键问题**：`file_attributes` 和 `file_index` 从目录枚举中免费获取，但 `number_of_links` 和 `volume_serial_number` 需要额外系统调用。当前代码将这四个字段捆绑在 `read_metadata_ext` 一个开关下，无法单独控制。

---

## 根因分析

### 核心问题：nlink/volserial 与 Ext 元数据捆绑

**位置**：`src/lib.rs:887-905`

```rust
// 当前实现：read_metadata_ext = true 时，无条件查询 nlink + volserial
if read_metadata_ext {
    let fs_type = detect_fs_type(&guard);
    if fs_type.is_fat_family() {
        for entry in entries.iter_mut() {
            entry.number_of_links = Some(1);
        }
    } else {
        batch_query_nlinks(&mut entries, &guard);  // 🔴 逐文件系统调用
    }
    let vol_serial = query_volume_serial(&guard);
    // ...
}
```

**影响**：
- 即使只需要 `file_attributes` + `file_index`，也会触发全部 nlink/volserial 查询
- `batch_query_nlinks` 占 Ext 额外开销的 ~85%
- 147K 文件 → 147K 次 `NtQueryInformationByName` 系统调用 + 147K 次 `Vec<u16>` 堆分配

### 次要问题：UTF-16 缓冲区逐文件分配

**位置**：`src/core/nt_dir_enum.rs:511`

```rust
for entry in entries.iter_mut() {
    let file_name_wide: Vec<u16> = entry.file_name.encode_wide().collect();
    // ...
}
```

- 每个文件分配一次 `Vec<u16>`，147K 次堆分配
- 属于 `batch_query_nlinks` 内部问题，解耦 nlink 后此问题自动消除

### 已优化的部分

| 功能 | 位置 | 优化状态 |
|------|------|---------|
| 目录枚举 | `nt_dir_enum.rs:31-33` | ✅ 使用 TLS 64KB 缓冲区 |
| 卷序列号查询 | `nt_dir_enum.rs:568-589` | ✅ 每目录一次 |
| 文件系统检测 | `nt_dir_enum.rs:621-653` | ✅ 每目录一次 |
| FAT 跳过优化 | `lib.rs:890-896` | ✅ FAT32/exFAT 跳过 nlink 查询 |

---

## 分级优化策略

### L0: 解耦 nlink/volserial 查询（核心优化，推荐优先实施）

**目标**：将 `number_of_links` / `volume_serial_number` 查询从 `read_metadata_ext` 中独立出来

**问题本质**：

```
当前：read_metadata_ext = true → 获取 file_attrs + file_index + nlink + volserial
目标：read_metadata_ext = true  → 仅获取 file_attrs + file_index（免费）
      read_hardlink_info = true → 额外获取 nlink + volserial（昂贵）
```

解耦后，仅使用 `read_metadata_ext` 的 Ext 模式性能应接近 Base 模式。

#### 步骤 1：jwalk-meta 添加 `read_hardlink_info` 选项

**修改文件**：`src/lib.rs`

```rust
// 在 Options 结构体中添加字段（约 line 208 附近）
pub struct Options {
    // ... 现有字段
    pub read_metadata_ext: bool,
    pub read_hardlink_info: bool,    // 新增：是否查询 number_of_links + volume_serial_number
}

// 默认值（约 line 236 附近）
read_metadata_ext: false,
read_hardlink_info: false,          // 默认不查询

// 添加 builder 方法（约 line 337 附近）
pub fn read_hardlink_info(mut self, read_hardlink_info: bool) -> Self {
    self.options.read_hardlink_info = read_hardlink_info;
    self
}
```

#### 步骤 2：修改 Ext 信息查询条件

**修改文件**：`src/lib.rs:887-905`

```rust
// 修改前
if read_metadata_ext {
    let fs_type = detect_fs_type(&guard);
    // ... batch_query_nlinks + query_volume_serial
}

// 修改后：nlink/volserial 仅在 read_hardlink_info 时查询
if read_hardlink_info {
    let fs_type = detect_fs_type(&guard);
    if fs_type.is_fat_family() {
        for entry in entries.iter_mut() {
            entry.number_of_links = Some(1);
        }
    } else {
        batch_query_nlinks(&mut entries, &guard);
    }
    let vol_serial = query_volume_serial(&guard);
    if let Some(vs) = vol_serial {
        for entry in entries.iter_mut() {
            entry.volume_serial_number = Some(vs);
        }
    }
}
```

**注意**：`read_metadata_ext` 仍控制 `make_metadata_ext` 是否构造 `MetaDataExt`（包含 `file_attributes` + `file_index`），这两个字段从目录枚举免费获取。

#### 步骤 3：传递 `read_hardlink_info` 到 `read_dir_windows`

**修改文件**：`src/lib.rs`

在 `read_dir_windows` 函数签名和调用处添加 `read_hardlink_info: bool` 参数。

#### 步骤 4：scandir-rs 适配

**修改文件**：`scandir/src/count/worker.rs`

```rust
// 当前实现
WalkDirGeneric::<((), ())>::new(&options.root_path)
    .read_metadata(true)
    .read_metadata_ext(options.return_type == ReturnType::Ext)

// 修改后：仅当需要硬链接检测时才查询 nlink
WalkDirGeneric::<((), ())>::new(&options.root_path)
    .read_metadata(true)
    .read_metadata_ext(options.return_type == ReturnType::Ext)
    .read_hardlink_info(options.return_type == ReturnType::Ext)  // 新增
```

如果后续要添加 `detect_hardlinks` 配置项，可以改为：

```rust
.read_hardlink_info(options.detect_hardlinks && options.return_type == ReturnType::Ext)
```

#### 预期收益

| 场景 | 修改前 | 修改后 |
|------|--------|--------|
| Ext（不需要硬链接检测） | 3.5s | ~0.4s（接近 Base） |
| Ext（需要硬链接检测） | 3.5s | 3.5s（不变） |

- 实现复杂度：低
- 兼容性：完全向后兼容（新增选项，默认 false）
- API 破坏性：无

---

### L1: 复用 UTF-16 缓冲区（L0 之后，优化 nlink 查询本身）

**前提**：L0 已实施，但用户仍需 `read_hardlink_info = true` 时适用

**目标**：消除 `batch_query_nlinks` 中的逐文件堆分配

**修改文件**：`src/core/nt_dir_enum.rs:499-561`

**当前实现**：
```rust
pub fn batch_query_nlinks(entries: &mut [DirEntryInfo], dir_handle: &HandleGuard) {
    for entry in entries.iter_mut() {
        let file_name_wide: Vec<u16> = entry.file_name.encode_wide().collect();  // 🔴 逐文件分配
        // ...
    }
}
```

**优化方案**：使用线程本地缓冲区（项目已有 `TLS_BUFFER` 先例）

```rust
// 在文件顶部添加（约 line 31 附近，已有 TLS_BUFFER 旁边）
thread_local! {
    static TLS_FILENAME_BUF: std::cell::RefCell<Vec<u16>> =
        std::cell::RefCell::new(Vec::with_capacity(260));
}

pub fn batch_query_nlinks(entries: &mut [DirEntryInfo], dir_handle: &HandleGuard) {
    let funcs = ntdll_funcs();
    let query_by_name = match funcs.query_by_name {
        Some(f) => f,
        None => return,
    };

    let mut stat_info_buf = [0u8; std::mem::size_of::<FILE_STAT_INFORMATION>()];

    TLS_FILENAME_BUF.with(|buf_cell| {
        let mut filename_buf = buf_cell.borrow_mut();

        for entry in entries.iter_mut() {
            filename_buf.clear();
            filename_buf.extend(entry.file_name.encode_wide());

            if filename_buf.is_empty() {
                continue;
            }

            let byte_len = (filename_buf.len() * 2) as u16;
            let mut unicode_str = UNICODE_STRING {
                Length: byte_len,
                MaximumLength: byte_len,
                Buffer: filename_buf.as_ptr() as *mut u16,
            };

            // ... 其余 OBJECT_ATTRIBUTES + NtQueryInformationByName 代码不变
        }
    });
}
```

**预期收益**：
- 消除 147K 次堆分配
- `read_hardlink_info = true` 时性能提升约 10-20%
- 实现复杂度：低

---

### L2: FileStatBasicInformation（Win11 24H2+，长期优化）

**前提**：L0 + L1 已实施，用户需要 `read_hardlink_info = true` 时进一步优化

**目标**：在目录枚举时直接获取 NumberOfLinks + VolumeSerialNumber，消除逐文件查询

**背景**：
- Windows 11 24H2 (build 26100+) 引入 `FileStatBasicInformation` (class 77)
- 该信息类在 `NtQueryDirectoryFileEx` 返回数据中直接包含 `NumberOfLinks` 和 `VolumeSerialNumber`
- 无需额外的 `batch_query_nlinks` + `query_volume_serial` 调用

**修改文件**：`src/core/nt_dir_enum.rs`

#### 步骤 1：定义新结构体

```rust
/// FileStatBasicInformation (class 77) - Win11 24H2+
/// 在目录枚举时直接返回 NumberOfLinks 和 VolumeSerialNumber
#[repr(C)]
#[allow(non_snake_case)]
struct FILE_STAT_BASIC_INFORMATION {
    FileId: i64,
    CreationTime: i64,
    LastAccessTime: i64,
    LastWriteTime: i64,
    ChangeTime: i64,
    AllocationSize: i64,
    EndOfFile: i64,
    FileAttributes: u32,
    ReparseTag: u32,
    NumberOfLinks: u32,           // ✅ 包含硬链接数
    EffectiveAccess: u32,
    VolumeSerialNumber: u32,      // ✅ 包含卷序列号
    Reserved: u32,
}

const FILE_STAT_BASIC_INFORMATION_CLASS: u32 = 77;
```

#### 步骤 2：运行时检测 + 回退逻辑

```rust
/// 检测 FileStatBasicInformation (class 77) 是否可用
fn try_file_stat_basic_info() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static AVAILABLE: AtomicBool = AtomicBool::new(true);
    static CHECKED: AtomicBool = AtomicBool::new(false);

    if CHECKED.load(Ordering::Relaxed) {
        return AVAILABLE.load(Ordering::Relaxed);
    }

    // 尝试调用 NtQueryDirectoryFileEx with class 77
    // 如果返回 STATUS_INVALID_INFO_CLASS (0xC0000003)，则标记不可用
    let result = unsafe {
        // ... 实际调用
    };

    let available = result != STATUS_INVALID_INFO_CLASS;
    AVAILABLE.store(available, Ordering::Relaxed);
    CHECKED.store(true, Ordering::Relaxed);
    available
}
```

#### 步骤 3：在 `enumerate_dir` 中条件使用 class 77

仅当 `read_hardlink_info = true` 时尝试 class 77，否则使用 class 37。

**预期收益**：
- Win11 24H2+ 上 `read_hardlink_info = true` 时消除所有逐文件 nlink 查询
- 性能提升 50-80%，接近 Base 模式
- 实现复杂度：中
- 兼容性：需要回退逻辑（class 77 仅 Win11 24H2+ 可用）

---

## 实施优先级

| 优先级 | 优化项 | 预期收益 | 实现难度 | 适用场景 |
|--------|--------|---------|---------|---------|
| 🔴 P0 | L0: 解耦 nlink/volserial | **Ext 接近 Base** | 低 | 所有场景 |
| 🟡 P1 | L1: UTF-16 缓冲区复用 | 10-20% | 低 | `read_hardlink_info=true` 时 |
| 🟢 P2 | L2: FileStatBasicInformation | 50-80% | 中 | Win11 24H2+ 且需 nlink |

**推荐实施顺序**：
1. **L0** — 解耦后 Ext 模式默认不再查询 nlink，性能问题基本解决
2. **L1** — 对仍需 nlink 的用户优化缓冲区分配
3. **L2** — 对 Win11 24H2+ 用户彻底消除逐文件 nlink 系统调用

---

## 测试验证

### 基准测试命令

```bash
# 在 scandir-rs 项目根目录执行
cargo build --release

# Base 模式基准
./target/release/scandir count D:\ --return-type base

# Ext 模式基准（当前：含 nlink 查询）
./target/release/scandir count D:\ --return-type ext

# Ext 模式基准（L0 后：不含 nlink 查询）
# 需确认 scandir-rs 适配后的 API
```

### 预期验证指标

| 指标 | 优化前 | L0 后 (无 nlink) | L0+L2 后 (Win11+) |
|------|--------|-----------------|-------------------|
| Ext 扫描时间 | 3.5s | ~0.4s | ~0.4s |
| Ext/Base 比值 | 10.3x | ~1.2x | ~1.2x |
| 逐文件系统调用 | 147K | 0 | 0 |
| 堆分配次数 | 147K | 0 | 0 |

---

## 相关文件清单

### jwalk-meta 需修改的文件

| 文件 | L0 修改 | L1 修改 | L2 修改 |
|------|---------|---------|---------|
| `src/lib.rs` | 添加 `read_hardlink_info` 选项 + 修改查询条件 | — | 调用路径 |
| `src/core/nt_dir_enum.rs` | — | TLS 缓冲区复用 | class 77 支持 |
| `src/core/dir_entry.rs` | — | — | — |

### scandir-rs 需修改的文件

| 文件 | L0 修改 |
|------|---------|
| `scandir/src/count/worker.rs` | 传递 `read_hardlink_info` |
| `scandir/src/count/mod.rs` | 可选：添加 `detect_hardlinks` 配置 |

---

## 参考资料

### Windows NT API 文档

- [NtQueryDirectoryFileEx](https://docs.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntquerydirectoryfileex)
- [NtQueryInformationByName](https://docs.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntqueryinformationbyname)
- [FILE_INFO_CLASS](https://docs.microsoft.com/en-us/windows/win32/api/winternl/ne-winternl-file_info_class)

### 相关文档

- jwalk-meta API 迁移：`docs/jwalk-meta-migration.md`
- jwalk-meta win32_fallback_path 修复：commit 6d9b016

---

**文档版本**：2.0  
**最后更新**：2026-06-03  
**变更说明**：v2.0 修正根因分析 — Ext 模式瓶颈的核心原因是 nlink/volserial 与 read_metadata_ext 捆绑，而非 UTF-16 缓冲区分配。将解耦 nlink/volserial 调整为 P0 优化。
