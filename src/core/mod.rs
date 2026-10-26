mod dir_entry;
mod dir_entry_iter;
mod error;
mod index_path;
mod metadata;
#[cfg(windows)]
mod nt_dir_enum;
mod ordered;
mod ordered_queue;
mod priority_queue;
mod read_dir;
mod read_dir_iter;
mod read_dir_spec;
mod run_context;
mod weighted;

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
// io-uring 0.7.x prebuilt sys.rs architecture allowlist (upstream src/sys/mod.rs:18-33); other targets trigger compile_error.
pub(crate) mod linux_io_uring;
#[cfg(all(target_os = "linux", not(feature = "legacy-read-dir")))]
pub(crate) mod unix_dir_enum;

use rayon::prelude::*;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use index_path::*;
use ordered::*;
use ordered_queue::*;
use priority_queue::*;
use read_dir_iter::*;
use run_context::RunContext;

pub use dir_entry::DirEntry;
pub use dir_entry_iter::DirEntryIter;
pub use error::Error;
pub(crate) use error::is_transient_error;
pub use metadata::{AncestorIdentity, MetaData, MetaDataExt, get_metadata_ext};
pub use read_dir::ReadDir;
pub(crate) use read_dir_iter::StreamingContext;
pub use read_dir_spec::ReadDirSpec;
pub(crate) use weighted::Weighted;

#[cfg(windows)]
pub(crate) use nt_dir_enum::{
    DirEntryInfo, batch_query_nlinks, detect_fs_type_and_vol_serial, enumerate_dir,
    enumerate_dir_streaming,
};

use crate::{ClientState, Parallelism};
