# jwalk-meta Ext Mode Performance Optimization Guide

> This document analyzes the Ext mode performance bottleneck (Base 0.34s vs. Ext 3.5s, 936% slower) and presents tiered optimization strategies for jwalk-meta developers.

## Problem overview

### Performance baseline

| Mode | D: drive scan time | Relative performance |
|------|--------------|---------|
| Count(Base) | 0.34s | 1.0x (baseline) |
| Count(Ext) | 3.5s | 0.11x (936% slower) |

**Goal**: Bring Ext mode performance close to Base mode.

### Ext mode data acquisition analysis

When `read_metadata_ext = true`, the `MetaDataExt` collected by jwalk-meta contains four fields:

| Field | Source | Cost |
|------|------|------|
| `file_attributes` | NtQueryDirectoryFileEx (directory enumeration) | ✅ Free |
| `file_index` | NtQueryDirectoryFileEx (directory enumeration) | ✅ Free |
| `number_of_links` | NtQueryInformationByName (per-file query) | 🔴 Expensive |
| `volume_serial_number` | GetVolumeInformationByHandleW (once per directory) | 🟡 Low cost |

**Key issue**: `file_attributes` and `file_index` are obtained for free during directory enumeration, but `number_of_links` and `volume_serial_number` require additional system calls. The current code groups all four fields under the single `read_metadata_ext` switch, so they cannot be controlled independently.

---

## Root-cause analysis

### Core issue: nlink/volserial are coupled to Ext metadata

**Location**: `src/lib.rs:887-905`

```rust
// Current implementation: always query nlink + volserial when read_metadata_ext = true
if read_metadata_ext {
    let fs_type = detect_fs_type(&guard);
    if fs_type.is_fat_family() {
        for entry in entries.iter_mut() {
            entry.number_of_links = Some(1);
        }
    } else {
        batch_query_nlinks(&mut entries, &guard);  // 🔴 Per-file system call
    }
    let vol_serial = query_volume_serial(&guard);
    // ...
}
```

**Impact**:
- All nlink/volserial queries are triggered even when only `file_attributes` and `file_index` are needed.
- `batch_query_nlinks` accounts for ~85% of the additional Ext overhead.
- 147K files → 147K `NtQueryInformationByName` system calls + 147K heap allocations for `Vec<u16>`.

### Secondary issue: per-file UTF-16 buffer allocation

**Location**: `src/core/nt_dir_enum.rs:511`

```rust
for entry in entries.iter_mut() {
    let file_name_wide: Vec<u16> = entry.file_name.encode_wide().collect();
    // ...
}
```

- One `Vec<u16>` allocation per file, resulting in 147K heap allocations.
- This is an internal `batch_query_nlinks` issue and will be eliminated automatically by decoupling nlink.

### Already optimized

| Feature | Location | Optimization status |
|------|------|---------|
| Directory enumeration | `nt_dir_enum.rs:31-33` | ✅ Uses a TLS 64 KB buffer |
| Volume serial number query | `nt_dir_enum.rs:568-589` | ✅ Once per directory |
| Filesystem detection | `nt_dir_enum.rs:621-653` | ✅ Once per directory |
| FAT skip optimization | `lib.rs:890-896` | ✅ Skips nlink queries on FAT32/exFAT |

---

## Tiered optimization strategy

### L0: Decouple nlink/volserial queries (core optimization; recommended first)

**Goal**: Separate `number_of_links` / `volume_serial_number` queries from `read_metadata_ext`.

**The core issue**:

```
Current: read_metadata_ext = true → get file_attrs + file_index + nlink + volserial
Target:  read_metadata_ext = true  → get only file_attrs + file_index (free)
         read_hardlink_info = true → additionally get nlink + volserial (expensive)
```

After decoupling, Ext mode performance when using only `read_metadata_ext` should approach Base mode.

#### Step 1: Add a `read_hardlink_info` option to jwalk-meta

**File to modify**: `src/lib.rs`

```rust
// Add a field to the Options struct (around line 208)
pub struct Options {
    // ... existing fields
    pub read_metadata_ext: bool,
    pub read_hardlink_info: bool,    // New: whether to query number_of_links + volume_serial_number
}

// Default value (around line 236)
read_metadata_ext: false,
read_hardlink_info: false,          // Do not query by default

// Add a builder method (around line 337)
pub fn read_hardlink_info(mut self, read_hardlink_info: bool) -> Self {
    self.options.read_hardlink_info = read_hardlink_info;
    self
}
```

#### Step 2: Change the Ext information-query condition

**File to modify**: `src/lib.rs:887-905`

```rust
// Before
if read_metadata_ext {
    let fs_type = detect_fs_type(&guard);
    // ... batch_query_nlinks + query_volume_serial
}

// After: query nlink/volserial only when read_hardlink_info is enabled
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

**Note**: `read_metadata_ext` will still control whether `make_metadata_ext` constructs `MetaDataExt` (containing `file_attributes` + `file_index`); these two fields are obtained for free during directory enumeration.

#### Step 3: Pass `read_hardlink_info` to `read_dir_windows`

**File to modify**: `src/lib.rs`

Add a `read_hardlink_info: bool` parameter to the `read_dir_windows` function signature and its call sites.

#### Step 4: Adapt scandir-rs

**File to modify**: `scandir/src/count/worker.rs`

```rust
// Current implementation
WalkDirGeneric::<((), ())>::new(&options.root_path)
    .read_metadata(true)
    .read_metadata_ext(options.return_type == ReturnType::Ext)

// After: query nlink only when hard-link detection is needed
WalkDirGeneric::<((), ())>::new(&options.root_path)
    .read_metadata(true)
    .read_metadata_ext(options.return_type == ReturnType::Ext)
    .read_hardlink_info(options.return_type == ReturnType::Ext)  // New
```

If a `detect_hardlinks` configuration option is added later, this could be changed to:

```rust
.read_hardlink_info(options.detect_hardlinks && options.return_type == ReturnType::Ext)
```

#### Expected benefits

| Scenario | Before | After |
|------|--------|--------|
| Ext (hard-link detection not needed) | 3.5s | ~0.4s (close to Base) |
| Ext (hard-link detection needed) | 3.5s | 3.5s (unchanged) |

- Implementation complexity: low
- Compatibility: fully backward-compatible (new option defaults to false)
- Breaking API changes: none

---

### L1: Reuse the UTF-16 buffer (after L0, to optimize nlink queries themselves)

**Prerequisite**: L0 has been implemented, but this applies when users still need `read_hardlink_info = true`.

**Goal**: Eliminate per-file heap allocations in `batch_query_nlinks`.

**File to modify**: `src/core/nt_dir_enum.rs:499-561`

**Current implementation**:
```rust
pub fn batch_query_nlinks(entries: &mut [DirEntryInfo], dir_handle: &HandleGuard) {
    for entry in entries.iter_mut() {
        let file_name_wide: Vec<u16> = entry.file_name.encode_wide().collect();  // 🔴 Allocate once per file
        // ...
    }
}
```

**Optimization**: Use a thread-local buffer (the project already has `TLS_BUFFER` as a precedent).

```rust
// Add near the top of the file (around line 31, beside the existing TLS_BUFFER)
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

            // ... Leave the remaining OBJECT_ATTRIBUTES + NtQueryInformationByName code unchanged
        }
    });
}
```

**Expected benefits**:
- Eliminate 147K heap allocations.
- Improve performance by about 10–20% when `read_hardlink_info = true`.
- Implementation complexity: low.

---

### L2: FileStatBasicInformation (Windows 11 24H2+, long-term optimization)

**Prerequisite**: L0 + L1 have been implemented; this is a further optimization for users who need `read_hardlink_info = true`.

**Goal**: Retrieve NumberOfLinks + VolumeSerialNumber directly during directory enumeration, eliminating per-file queries.

**Background**:
- Windows 11 24H2 (build 26100+) introduced `FileStatBasicInformation` (class 77).
- This information class directly includes `NumberOfLinks` and `VolumeSerialNumber` in the data returned by `NtQueryDirectoryFileEx`.
- No additional `batch_query_nlinks` + `query_volume_serial` calls are needed.

**File to modify**: `src/core/nt_dir_enum.rs`

#### Step 1: Define the new struct

```rust
/// FileStatBasicInformation (class 77) - Win11 24H2+
/// Returns NumberOfLinks and VolumeSerialNumber directly during directory enumeration
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
    NumberOfLinks: u32,           // ✅ Contains the hard-link count
    EffectiveAccess: u32,
    VolumeSerialNumber: u32,      // ✅ Contains the volume serial number
    Reserved: u32,
}

const FILE_STAT_BASIC_INFORMATION_CLASS: u32 = 77;
```

#### Step 2: Runtime detection and fallback logic

```rust
/// Detect whether FileStatBasicInformation (class 77) is available
fn try_file_stat_basic_info() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static AVAILABLE: AtomicBool = AtomicBool::new(true);
    static CHECKED: AtomicBool = AtomicBool::new(false);

    if CHECKED.load(Ordering::Relaxed) {
        return AVAILABLE.load(Ordering::Relaxed);
    }

    // Try calling NtQueryDirectoryFileEx with class 77.
    // If it returns STATUS_INVALID_INFO_CLASS (0xC0000003), mark it as unavailable.
    let result = unsafe {
        // ... Actual call
    };

    let available = result != STATUS_INVALID_INFO_CLASS;
    AVAILABLE.store(available, Ordering::Relaxed);
    CHECKED.store(true, Ordering::Relaxed);
    available
}
```

#### Step 3: Use class 77 conditionally in `enumerate_dir`

Try class 77 only when `read_hardlink_info = true`; otherwise, use class 37.

**Expected benefits**:
- Eliminate all per-file nlink queries when `read_hardlink_info = true` on Windows 11 24H2+.
- Improve performance by 50–80%, approaching Base mode.
- Implementation complexity: medium.
- Compatibility: requires fallback logic (class 77 is available only on Windows 11 24H2+).

---

## Implementation priorities

| Priority | Optimization | Expected benefit | Difficulty | Use case |
|--------|--------|---------|---------|---------|
| 🔴 P0 | L0: Decouple nlink/volserial | **Ext approaches Base** | Low | All scenarios |
| 🟡 P1 | L1: Reuse UTF-16 buffer | 10–20% | Low | When `read_hardlink_info=true` |
| 🟢 P2 | L2: FileStatBasicInformation | 50–80% | Medium | Windows 11 24H2+ with nlink needed |

**Recommended implementation order**:
1. **L0** — After decoupling, Ext mode no longer queries nlink by default, largely resolving the performance issue.
2. **L1** — Optimize buffer allocation for users who still need nlink.
3. **L2** — Completely eliminate per-file nlink system calls for Windows 11 24H2+ users.

---

## Testing and validation

### Benchmark commands

```bash
# Run from the scandir-rs project root
cargo build --release

# Base mode benchmark
./target/release/scandir count D:\ --return-type base

# Ext mode benchmark (current: includes nlink queries)
./target/release/scandir count D:\ --return-type ext

# Ext mode benchmark (after L0: excludes nlink queries)
# Confirm the API after adapting scandir-rs
```

### Expected validation metrics

| Metric | Before optimization | After L0 (no nlink) | After L0+L2 (Windows 11+) |
|------|--------|-----------------|-------------------|
| Ext scan time | 3.5s | ~0.4s | ~0.4s |
| Ext/Base ratio | 10.3x | ~1.2x | ~1.2x |
| Per-file system calls | 147K | 0 | 0 |
| Heap allocations | 147K | 0 | 0 |

---

## Related files

### Files to modify in jwalk-meta

| File | L0 changes | L1 changes | L2 changes |
|------|---------|---------|---------|
| `src/lib.rs` | Add `read_hardlink_info` option + change query condition | — | Call path |
| `src/core/nt_dir_enum.rs` | — | Reuse TLS buffer | class 77 support |
| `src/core/dir_entry.rs` | — | — | — |

### Files to modify in scandir-rs

| File | L0 changes |
|------|---------|
| `scandir/src/count/worker.rs` | Pass `read_hardlink_info` |
| `scandir/src/count/mod.rs` | Optional: add `detect_hardlinks` configuration |

---

## References

### Windows NT API documentation

- [NtQueryDirectoryFileEx](https://docs.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntquerydirectoryfileex)
- [NtQueryInformationByName](https://docs.microsoft.com/en-us/windows/win32/api/winternl/nf-winternl-ntqueryinformationbyname)
- [FILE_INFO_CLASS](https://docs.microsoft.com/en-us/windows/win32/api/winternl/ne-winternl-file_info_class)

### Related documents

- jwalk-meta API migration: `docs/jwalk-meta-migration.md`
- jwalk-meta `win32_fallback_path` fix: commit 6d9b016

---

**Document version**: 2.0
**Last updated**: 2026-06-03
**Change summary**: v2.0 corrects the root-cause analysis — the primary Ext mode bottleneck is coupling nlink/volserial with `read_metadata_ext`, not UTF-16 buffer allocation. Decoupling nlink/volserial is now the P0 optimization.
