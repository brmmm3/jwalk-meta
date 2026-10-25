use std::fs;
use std::mem;

#[cfg(windows)]
fn main() {
    let dir_meta = fs::metadata(r"D:\").unwrap();
    let ft_dir = dir_meta.file_type();
    let file_meta = fs::metadata(r"D:\Project").unwrap();
    let ft_file = file_meta.file_type();
    
    println!("FileType size: {} bytes", mem::size_of::<std::fs::FileType>());
    
    let dir_bytes = unsafe { mem::transmute::<std::fs::FileType, [u8; 2]>(ft_dir) };
    let file_bytes = unsafe { mem::transmute::<std::fs::FileType, [u8; 2]>(ft_file) };
    
    println!("dir   is_dir={} is_file={} is_symlink={}", ft_dir.is_dir(), ft_dir.is_file(), ft_dir.is_symlink());
    println!("dir   raw bytes: {:?} (u16: {})", dir_bytes, u16::from_ne_bytes(dir_bytes));
    println!("file  is_dir={} is_file={} is_symlink={}", ft_file.is_dir(), ft_file.is_file(), ft_file.is_symlink());
    println!("file  raw bytes: {:?} (u16: {})", file_bytes, u16::from_ne_bytes(file_bytes));
    
    let attrs_dir: u32 = 0x10;
    let is_dir = (attrs_dir & 0x10) != 0;
    let is_sym = (attrs_dir & 0x400) != 0;
    let bits: u16 = (is_dir as u16) | ((is_sym as u16) << 8);
    println!("our bits: {} (is_dir={}, is_sym={})", bits, is_dir, is_sym);
    let our_ft: std::fs::FileType = unsafe { mem::transmute(bits) };
    println!("our   is_dir={} is_file={} is_symlink={}", our_ft.is_dir(), our_ft.is_file(), our_ft.is_symlink());
    
    let bits_file: u16 = 0u16;
    let file_ft: std::fs::FileType = unsafe { mem::transmute(bits_file) };
    println!("zeros is_dir={} is_file={} is_symlink={}", file_ft.is_dir(), file_ft.is_file(), file_ft.is_symlink());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("test_ft requires Windows (assumes Windows FileType 2-byte layout)");
}
