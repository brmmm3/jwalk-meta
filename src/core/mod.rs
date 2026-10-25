mod dir_entry;
mod dir_entry_iter;
mod metadata;
mod error;
mod index_path;
mod ordered;
mod ordered_queue;
mod read_dir;
mod read_dir_iter;
mod read_dir_spec;
mod run_context;
mod priority_queue;
mod weighted;
#[cfg(windows)]
mod nt_dir_enum;

#[cfg(all(target_os = "linux", not(feature = "legacy-read-dir"))
)]
pub(crate) mod unix_dir_enum;
#[cfg(target_os = "linux")]
pub(crate) mod fs_detect;
#[cfg(all(
    target_os = "linux",
    target_env = "gnu",
    not(feature = "legacy-read-dir"),
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "riscv64",
        target_arch = "loongarch64",
        target_arch = "powerpc64",
    ),
))]
// io-uring 0.7.x 预编译 sys.rs 白名单（上游 src/sys/mod.rs:18-33），超出范围触发 compile_error。
pub(crate) mod linux_io_uring;

use rayon::prelude::*;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use index_path::*;
use ordered::*;
use ordered_queue::*;
use priority_queue::*;
use read_dir_iter::*;
use run_context::RunContext;

pub use dir_entry::DirEntry;
pub use dir_entry_iter::DirEntryIter;
pub use error::Error;
pub use read_dir::ReadDir;
pub use read_dir_spec::ReadDirSpec;
pub use metadata::{get_metadata_ext, MetaData, MetaDataExt, AncestorIdentity};
pub(crate) use weighted::Weighted;
pub(crate) use read_dir_iter::StreamingContext;
pub(crate) use error::is_transient_error;

#[cfg(windows)]
pub(crate) use nt_dir_enum::{
    enumerate_dir, enumerate_dir_streaming, DirEntryInfo, batch_query_nlinks,
    detect_fs_type_and_vol_serial,
};

use crate::{ClientState, Parallelism};
