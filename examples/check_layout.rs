use std::mem::{size_of, offset_of};

#[repr(C)]
#[allow(non_snake_case)]
struct FILE_ID_BOTH_DIR_INFO {
    NextEntryOffset: u32,
    FileIndex: u32,
    CreationTime: i64,
    LastAccessTime: i64,
    LastWriteTime: i64,
    ChangeTime: i64,
    EndOfFile: i64,
    AllocationSize: i64,
    FileAttributes: u32,
    FileNameLength: u32,
    EaSize: u32,
    ShortNameLength: i16,
    ShortName: [u16; 12],
    FileId: i64,
    FileName: [u16; 1],
}

fn main() {
    println!("size_of::<FILE_ID_BOTH_DIR_INFO>(): {}", size_of::<FILE_ID_BOTH_DIR_INFO>());
    println!("offset_of FileName: {}", offset_of!(FILE_ID_BOTH_DIR_INFO, FileName));
    println!("size_of - 2 = {}", size_of::<FILE_ID_BOTH_DIR_INFO>() - 2);
    println!("offset_of FileId: {}", offset_of!(FILE_ID_BOTH_DIR_INFO, FileId));
    println!("offset_of ShortName: {}", offset_of!(FILE_ID_BOTH_DIR_INFO, ShortName));
    println!("offset_of ShortNameLength: {}", offset_of!(FILE_ID_BOTH_DIR_INFO, ShortNameLength));
    
    // Windows C struct layout (for reference)
    // NextEntryOffset: 0 (4)
    // FileIndex: 4 (4)
    // CreationTime: 8 (8)
    // LastAccessTime: 16 (8)
    // LastWriteTime: 24 (8)
    // ChangeTime: 32 (8)
    // EndOfFile: 40 (8)
    // AllocationSize: 48 (8)
    // FileAttributes: 56 (4)
    // FileNameLength: 60 (4)
    // EaSize: 64 (4)
    // ShortNameLength: 68 (1 in C, 2 in Rust)
    // ShortName: 70 (24)
    // FileId: 96 (8)
    // FileName: 104 (2)
    // Total: 106, aligned to 112
}
