use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{self, FileType, Permissions};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(windows)]
use std::os::windows::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::{ClientState, Error, ReadDirSpec, Result};

#[inline]
pub fn get_metadata_ext(metadata: &fs::Metadata) -> MetaDataExt {
    #[cfg(unix)]
    {
        return MetaDataExt {
            st_mode: metadata.mode(),
            st_ino: metadata.ino(),
            st_dev: metadata.dev(),
            st_nlink: metadata.nlink(),
            st_blksize: metadata.blksize(),
            st_blocks: metadata.blocks(),
            st_uid: metadata.uid(),
            st_gid: metadata.gid(),
            st_rdev: metadata.rdev(),
        };
    }
    #[cfg(windows)]
    {
        return MetaDataExt {
            file_attributes: metadata.file_attributes(),
            volume_serial_number: metadata.volume_serial_number(),
            number_of_links: metadata.number_of_links(),
            file_index: metadata.file_index(),
        };
    }
}

#[derive(Debug, Clone)]
pub struct MetaData {
    /// True if DirEntry is a directory
    pub is_dir: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub created: Option<SystemTime>,
    pub modified: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    pub permissions: Option<Permissions>,
}

#[cfg(unix)]
#[derive(Debug, Clone)]
pub struct MetaDataExt {
    pub st_mode: u32,
    pub st_ino: u64,
    pub st_dev: u64,
    pub st_nlink: u64,
    pub st_blksize: u64,
    pub st_blocks: u64,
    pub st_uid: u32,
    pub st_gid: u32,
    pub st_rdev: u64,
}

#[cfg(windows)]
#[derive(Debug, Clone)]
pub struct MetaDataExt {
    pub file_attributes: u32,
    pub volume_serial_number: Option<u32>,
    pub number_of_links: Option<u32>,
    pub file_index: Option<u64>,
}

/// Representation of a file or directory.
///
/// This representation does not wrap a `std::fs::DirEntry`. Instead it copies
/// `file_name`, `file_type`, and optionaly `metadata` out of the underlying
/// `std::fs::DirEntry`. This allows it to quickly drop the underlying file
/// descriptor.
pub struct DirEntry<C: ClientState> {
    /// Depth of this entry relative to the root directory where the walk
    /// started.
    pub depth: usize,
    /// File name of this entry without leading path component.
    pub file_name: OsString,
    /// File type for the file/directory that this entry points at.
    pub file_type: FileType,
    /// Field where clients can store state from within the The
    /// [`process_read_dir`](struct.WalkDirGeneric.html#method.process_read_dir)
    /// callback.
    pub client_state: C::DirEntryState,
    /// Path used by this entry's parent to read this entry.
    pub parent_path: Arc<Path>,
    /// Path that will be used to read child entries. This is automatically set
    /// for directories. The
    /// [`process_read_dir`](struct.WalkDirGeneric.html#method.process_read_dir) callback
    /// may set this field to `None` to skip reading the contents of a
    /// particular directory.
    pub read_children_path: Option<Arc<Path>>,
    /// If `read_children_path` is set and resulting `fs::read_dir` generates an error
    /// then that error is stored here.
    pub read_children_error: Option<Error>,
    /// OS independent metadata
    pub read_metadata: bool,
    pub metadata: Option<MetaData>,
    /// OS dependent extended metadata
    pub read_metadata_ext: bool,
    pub metadata_ext: Option<MetaDataExt>,
    // True if [`follow_links`] is `true` AND was created from a symlink path.
    follow_link: bool,
    // Origins of synlinks followed to get to this entry.
    follow_link_ancestors: Arc<Vec<Arc<Path>>>,
}

impl<C: ClientState> DirEntry<C> {
    pub(crate) fn from_entry(
        depth: usize,
        parent_path: Arc<Path>,
        metadata: Option<MetaData>,
        metadata_ext: Option<MetaDataExt>,
        fs_dir_entry: &fs::DirEntry,
        follow_link_ancestors: Arc<Vec<Arc<Path>>>,
    ) -> Result<Self> {
        let file_type = fs_dir_entry
            .file_type()
            .map_err(|err| Error::from_path(depth, fs_dir_entry.path(), err))?;
        let file_name = fs_dir_entry.file_name();
        let read_children_path: Option<Arc<Path>> = if file_type.is_dir() {
            Some(Arc::from(parent_path.join(&file_name)))
        } else {
            None
        };

        Ok(DirEntry {
            depth,
            file_name,
            file_type,
            parent_path,
            read_children_path,
            read_children_error: None,
            client_state: C::DirEntryState::default(),
            read_metadata: metadata.is_some(),
            metadata,
            read_metadata_ext: metadata_ext.is_some(),
            metadata_ext,
            follow_link: false,
            follow_link_ancestors,
        })
    }

    // Only used for root and when following links.
    pub(crate) fn from_path(
        depth: usize,
        path: &Path,
        read_metadata: bool,
        read_metadata_ext: bool,
        follow_link: bool,
        follow_link_ancestors: Arc<Vec<Arc<Path>>>,
    ) -> Result<Self> {
        let metadata = if follow_link {
            fs::metadata(&path).map_err(|err| Error::from_path(depth, path.to_owned(), err))?
        } else {
            fs::symlink_metadata(&path)
                .map_err(|err| Error::from_path(depth, path.to_owned(), err))?
        };

        let root_name = path.file_name().unwrap_or_else(|| path.as_os_str());

        let read_children_path: Option<Arc<Path>> = if metadata.file_type().is_dir() {
            Some(Arc::from(path))
        } else {
            None
        };

        let entry_metadata;
        let entry_metadata_ext;
        if read_metadata {
            entry_metadata = Some(MetaData {
                is_dir: metadata.is_dir(),
                is_file: metadata.is_file(),
                is_symlink: metadata.is_symlink(),
                size: metadata.len(),
                created: metadata.created().map_or(None, |x| Some(x)),
                modified: metadata.modified().map_or(None, |x| Some(x)),
                accessed: metadata.accessed().map_or(None, |x| Some(x)),
                permissions: Some(metadata.permissions()),
            });
            if read_metadata_ext {
                entry_metadata_ext = Some(get_metadata_ext(&metadata));
            } else {
                entry_metadata_ext = None;
            }
        } else {
            entry_metadata = None;
            entry_metadata_ext = None;
        }

        Ok(DirEntry {
            depth,
            file_name: root_name.to_owned(),
            file_type: metadata.file_type(),
            parent_path: Arc::from(path.parent().map(Path::to_path_buf).unwrap_or_default()),
            read_children_path,
            read_children_error: None,
            client_state: C::DirEntryState::default(),
            read_metadata,
            metadata: entry_metadata,
            read_metadata_ext,
            metadata_ext: entry_metadata_ext,
            follow_link,
            follow_link_ancestors,
        })
    }

    /// Return the file type for the file that this entry points to.
    ///
    /// If this is a symbolic link and [`follow_links`] is `true`, then this
    /// returns the type of the target.
    ///
    /// This never makes any system calls.
    ///
    /// [`follow_links`]: struct.WalkDir.html#method.follow_links
    pub fn file_type(&self) -> fs::FileType {
        self.file_type
    }

    /// Return the file name of this entry.
    ///
    /// If this entry has no file name (e.g., `/`), then the full path is
    /// returned.
    pub fn file_name(&self) -> &OsStr {
        &self.file_name
    }

    /// Returns the depth at which this entry was created relative to the root.
    ///
    /// The smallest depth is `0` and always corresponds to the path given
    /// to the `new` function on `WalkDir`. Its direct descendents have depth
    /// `1`, and their descendents have depth `2`, and so on.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Path to the file/directory represented by this entry.
    ///
    /// The path is created by joining `parent_path` with `file_name`.
    pub fn path(&self) -> PathBuf {
        self.parent_path.join(&self.file_name)
    }

    /// Returns `true` if and only if this entry was created from a symbolic
    /// link. This is unaffected by the [`follow_links`] setting.
    ///
    /// When `true`, the value returned by the [`path`] method is a
    /// symbolic link name. To get the full target path, you must call
    /// [`std::fs::read_link(entry.path())`].
    ///
    /// [`path`]: struct.DirEntry.html#method.path
    /// [`follow_links`]: struct.WalkDir.html#method.follow_links
    /// [`std::fs::read_link(entry.path())`]: https://doc.rust-lang.org/stable/std/fs/fn.read_link.html
    pub fn path_is_symlink(&self) -> bool {
        self.file_type.is_symlink() || self.follow_link
    }

    /// Return the metadata for the file that this entry points to.
    ///
    /// This will follow symbolic links if and only if the [`WalkDir`] value
    /// has [`follow_links`] enabled.
    ///
    /// # Platform behavior
    ///
    /// This always calls [`std::fs::symlink_metadata`].
    ///
    /// If this entry is a symbolic link and [`follow_links`] is enabled, then
    /// [`std::fs::metadata`] is called instead.
    ///
    /// # Errors
    ///
    /// Similar to [`std::fs::metadata`], returns errors for path values that
    /// the program does not have permissions to access or if the path does not
    /// exist.
    ///
    /// [`WalkDir`]: struct.WalkDir.html
    /// [`follow_links`]: struct.WalkDir.html#method.follow_links
    /// [`std::fs::metadata`]: https://doc.rust-lang.org/std/fs/fn.metadata.html
    /// [`std::fs::symlink_metadata`]: https://doc.rust-lang.org/stable/std/fs/fn.symlink_metadata.html
    pub fn metadata(&self) -> Result<fs::Metadata> {
        if self.follow_link {
            fs::metadata(&self.path())
        } else {
            fs::symlink_metadata(&self.path())
        }
        .map_err(|err| Error::from_entry(self, err))
    }

    /// Reference to the path of the directory containing this entry.
    pub fn parent_path(&self) -> &Path {
        &self.parent_path
    }

    pub(crate) fn read_children_spec(
        &self,
        client_read_state: C::ReadDirState,
    ) -> Option<ReadDirSpec<C>> {
        if let Some(read_children_path) = self.read_children_path.as_ref() {
            Some(ReadDirSpec {
                depth: self.depth,
                client_read_state,
                path: read_children_path.clone(),
                follow_link_ancestors: self.follow_link_ancestors.clone(),
            })
        } else {
            None
        }
    }

    pub(crate) fn follow_symlink(&self) -> Result<Self> {
        let path = self.path();
        let origins = self.follow_link_ancestors.clone();
        let dir_entry = DirEntry::from_path(
            self.depth,
            &path,
            self.read_metadata,
            self.read_metadata_ext,
            true,
            origins,
        )?;

        if dir_entry.file_type.is_dir() {
            let target = std::fs::read_link(&path).unwrap();
            for ancestor in self.follow_link_ancestors.iter().rev() {
                if target.as_path() == ancestor.as_ref() {
                    return Err(Error::from_loop(
                        self.depth,
                        ancestor.as_ref(),
                        path.as_ref(),
                    ));
                }
            }
        }

        Ok(dir_entry)
    }
}

impl<C: ClientState> fmt::Debug for DirEntry<C> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "DirEntry({:?})", self.path())
    }
}
