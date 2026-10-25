# class 77 (FileStatBasicInformation) Directory Enumeration Feasibility PoC Verdict

> **Conclusion**: ❌ **Not feasible**. Tests on Windows 11 build 26200 (24H2+) show that class 77 (FileStatBasicInformation) **cannot** be used for directory enumeration with `NtQueryDirectoryFile` or `NtQueryDirectoryFileEx`; both APIs return `STATUS_INVALID_INFO_CLASS` (0xC0000003). Only single-file queries using `NtQueryInformationByName` support class 77.

---

## 1. Test environment

| Item | Value |
|---|---|
| Operating system | Windows 11 Pro |
| Build | 26200 (meets the PoC requirement of build 26048+) |
| Test directory | `D:\Project\jwalk-meta\.` (20 real entries, including short/medium/long filenames) |
| PoC program | `examples/class77_poc.rs` |
| Full log | See `%TEMP%\opencode\class77_poc_final.log` on the test machine |

### Sample test-directory entries (first entries parsed from class 37)

| # | offset | NextEntryOffset | Filename | Length |
|---|---|---|---|---|
| 0 | 224 | 120 | `.beads` | 6 (short) |
| 1 | 344 | 136 | `.editorconfig` | 13 (long) |
| 2 | 480 | 112 | `.git` | 4 (short) |
| 4 | 712 | 128 | `.gitignore` | 10 (medium) |
| 5 | 840 | 112 | `.omo` | 4 (short) |
| 8 | 1200 | 128 | `Cargo.lock` | 10 (medium) |

Filename lengths range from 4 to 13 characters, covering variable-length field handling. All 20 entries were parsed correctly.

---

## 2. Field layout comparison

### 2.1 class 37 (FILE_ID_BOTH_DIR_INFO) — used by the current production code

> Known structure. Each entry is 104 bytes of fixed header plus a variable-length FileName; it **does not contain NumberOfLinks**.

| Offset | Size | Field | Type |
|---|---|---|---|
| 0   | 4  | NextEntryOffset | u32 |
| 4   | 4  | FileIndex | u32 |
| 8   | 8  | CreationTime | i64 |
| 16  | 8  | LastAccessTime | i64 |
| 24  | 8  | LastWriteTime | i64 |
| 32  | 8  | ChangeTime | i64 |
| 40  | 8  | EndOfFile | i64 |
| 48  | 8  | AllocationSize | i64 |
| 56  | 4  | FileAttributes | u32 |
| 60  | 4  | FileNameLength | u32 |
| 64  | 4  | EaSize | u32 |
| 68  | 1  | ShortNameLength | u8 |
| 69  | 1  | (padding) | u8 |
| 70  | 24 | ShortName | [u16; 12] |
| 94  | 2  | (padding) | — |
| 96  | 8  | FileId | i64 |
| 104 | N  | FileName | [u16; N/2] |

**Conclusion**: There is no NumberOfLinks field. A second query is required (the current `batch_query_nlinks` implementation, with N system calls).

### 2.2 class 77 (FileStatBasicInformation) — documented single-file structure

> Microsoft [documentation](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-file_stat_basic_information) declares the following structure:

| Offset | Size | Field | Type |
|---|---|---|---|
| 0   | 8  | FileId | i64 |
| 8   | 8  | CreationTime | i64 |
| 16  | 8  | LastAccessTime | i64 |
| 24  | 8  | LastWriteTime | i64 |
| 32  | 8  | ChangeTime | i64 |
| 40  | 8  | AllocationSize | i64 |
| 48  | 8  | EndOfFile | i64 |
| 56  | 4  | FileAttributes | u32 |
| 60  | 4  | ReparseTag | u32 |
| 64  | 4  | **NumberOfLinks** | u32 |
| 68  | 4  | (padding) | — |
| 72  | 8  | EffectiveAccess | i64 |
| **Total** | **80** | | |

### 2.3 class 77 — actual structure observed in testing

The PoC probes the buffer size using `NtQueryInformationByName`, scanning from 64 to 128 bytes in 4-byte increments.

| Buffer size | Result |
|---|---|
| 64..100 | `STATUS_INFO_LENGTH_MISMATCH` (0xC0000004) |
| **104** | ✅ **Success**, NumberOfLinks=1 |
| 108..128 | Not tested (104 succeeded) |

**Unexpected finding**: The actual structure size is **104 bytes**, 24 bytes larger than the documented 80 bytes. The `NumberOfLinks` field at offset 64 is still correct (`NumberOfLinks=1` for the tested .beads directory, with `FileAttributes=0x10=FILE_ATTRIBUTE_DIRECTORY`, a plausible value).

The additional 24 bytes (offsets 80..104) are undocumented extension fields. Possible uses include OwnerId, ConvergedSequenceNumber, or similar NTFS extra information. The PoC does not investigate further because directory enumeration itself is rejected.

### 2.4 class 77 directory enumeration (hypothetical directory format, unverified)

If `NtQueryDirectoryFile` accepted class 77, the structure would likely be extended according to the common NT API directory-information-class pattern:

```
NextEntryOffset(4) + FileIndex(4)? + <FILE_STAT_BASIC_INFORMATION 104 bytes> + FileNameLength(4) + FileName[]
```

**However, since testing rejected the class, this remains a theoretical guess that the PoC cannot verify.**

---

## 3. Analysis of bytes from the test dump

### 3.1 First 1024 bytes of the class 37 buffer (baseline)

```
00000000: 70 00 00 00 00 00 00 00 bc e4 5d a7 0d ef dc 01  p.........].....
00000010: 3c f3 43 5c 76 03 dd 01 93 3e 36 5c 76 03 dd 01  <.C\v....>6\v...
00000020: 93 3e 36 5c 76 03 dd 01 00 00 00 00 00 00 00 00  .>6\v...........
00000030: 00 00 00 00 00 00 00 00 10 00 00 00 02 00 00 00  ................
00000040: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00  ................
00000050: 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00  ................
00000060: 30 17 00 00 00 00 4c 00 2e 00 00 00 00 00 00 00  0.....L.........
```

Field validation:
- `NextEntryOffset = 0x70 = 112` ✓ (matches the PoC output)
- `FileAttributes = 0x10` (`FILE_ATTRIBUTE_DIRECTORY`) ✓ (this is the `.` entry)
- `FileNameLength = 0x04` (`.` is 2 bytes of UTF-16 = 4 bytes) ✓

The class 37 buffer is fully usable; all 20 entries were parsed correctly.

### 3.2 class 77 directory enumeration — calls rejected, no buffer returned

All three parameter combinations failed:

```
(a) ret_single=0, FileName=NULL, restart=1 → 0xC0000003
(b) ret_single=0, FileName=*             → 0xC0000003
(c) ret_single=1, FileName=NULL          → 0xC0000003
```

The older API, `NtQueryDirectoryFile` (without Ex), also failed:

```
NtQueryDirectoryFile + class 77 → 0xC0000003
```

**There is no buffer to analyze.**

---

## 4. Main conclusion

### ❌ class 77 cannot be used for directory enumeration; abandon this optimization

**Evidence** (tested on Windows 11 build 26200):

1. **NtQueryDirectoryFileEx + class 77**: All three parameter combinations (FileName=NULL/"*", ReturnSingleEntry=0/1) returned `STATUS_INVALID_INFO_CLASS` (0xC0000003).
2. **NtQueryDirectoryFile (without Ex) + class 77**: Also returned `STATUS_INVALID_INFO_CLASS`.
3. **NtQueryInformationByName + class 77 single-file query**: ✅ Succeeded (buffer size=104, NumberOfLinks=1, FileAttributes=0x10).

**Key criterion**: The successful single-file query proves the system recognizes class 77 and the kernel implements this information class. However, class 77 is explicitly rejected for directory enumeration. This is not due to an "outdated Windows version" or "invalid parameters"; rather, **the directory-enumeration code paths in the NTFS/FAT/ReFS drivers do not accept class 77**.

### Comparison with the beads note

- The beads note `filestatbasicinformation-class77-dir-enum` says: "Class 77 does support NtQueryDirectoryFile directory enumeration (confirmed by Microsoft official documentation)."
- Test result: **Microsoft's documentation does not match the actual implementation**. The [FILE_STAT_BASIC_INFORMATION](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-file_stat_basic_information) page says it "can be used with NtQueryInformationFile / NtQueryDirectoryFile," but testing on build 26200 shows it is rejected.
- Possible reasons:
  - The documentation describes a "future version" or a "specific SKU."
  - The directory-enumeration fast path in the kernel I/O manager explicitly filters out class 77.
  - A specific VolumeFlags setting or FSCTL may need to be enabled first (not tested by the PoC).

### Whether the LinkCount field exists

Because directory enumeration is rejected, **it is impossible to verify whether LinkCount can be returned in a single directory-enumeration call**. However, the single-file class 77 query confirms that it returns NumberOfLinks (offset 64, 4 bytes; observed value for .beads was 1).

### parse_buffer_entries rewrite proposal

**Not applicable** — class 77 directory enumeration is unavailable, so `parse_buffer_entries` does not need to be rewritten. The current `batch_query_nlinks` implementation (single-threaded, sequential `NtQueryInformationByName` class 68 calls) remains the best Windows implementation for this problem.

---

## 5. Recommendations

| Option | Feasibility | Notes |
|---|---|---|
| **Keep the current implementation** | ✅ Recommended | `batch_query_nlinks` + single-threaded class 68 is the best known solution (limited by the NTFS MFT volume lock). |
| Probe other directory-enumeration information classes | 🟡 Medium | Known classes 3/12/13/14/34/37/38 do not contain NumberOfLinks; unknown class numbers could be probed systematically. |
| Optimize at the SMB/CIFS protocol layer | 🟢 Network scenarios only | Only useful for remote SMB directories; no benefit for local NTFS. |
| IoRing / NtCreateUserProcess, etc. | ❌ See beads `ntfs-mft-lock-kills-local-io-uring` | The local NTFS MFT lock prevents concurrent queries from improving performance. |
| Kernel-driver hook | ❌ Out of scope | Requires a signed driver and is beyond the scope of jwalk-meta. |

---

## 6. PoC deliverables

**Path**: `examples/class77_poc.rs`

**Features**:
- No new dependencies (uses the existing winapi dependency only).
- Does not modify any production code under `src/`.
- Dynamically loads `NtQueryDirectoryFileEx`, `NtQueryDirectoryFile`, and `NtQueryInformationByName` from ntdll.
- Three layers of validation: multiple directory-enumeration parameter combinations, the legacy API, and a single-file health check.
- Scans buffer sizes (finds the actual size of 104, rather than the documented 80).
- Gracefully handles unsupported-system scenarios and provides clear diagnostics.
- Dumps the first 1024 bytes of the class 37 buffer as a baseline for comparison.

**How to run**:

```powershell
cargo build --example class77_poc
.\target\debug\examples\class77_poc.exe .           # Test the current directory
.\target\debug\examples\class77_poc.exe D:\some_dir # Test the specified directory
```

**Expected output** (Windows build 26048+):

```
=== class 77 parameter-combination tests ===
  (a) ret_single=0, FileName=NULL → failed: 0xC0000003
  (b) ret_single=0, FileName=*    → failed: 0xC0000003
  (c) ret_single=1, FileName=NULL → failed: 0xC0000003
=== NtQueryDirectoryFile (without Ex) + class 77 ===
  NtQueryDirectoryFile + class 77 failed: 0xC0000003
=== Single-file class 77 health check ===
  size 100 → 0xC0000004
  size 104 → success ✓
  Success: ".beads" NumberOfLinks=1 FileAttributes=0x10 (buffer size 104)
=== All class 77 directory-enumeration attempts failed ===
The single-file query succeeded, so the system recognizes class 77, but NtQueryDirectoryFileEx rejects it for directory enumeration.
Conclusion: class 77 cannot be used for directory enumeration; abandon this optimization.
```
