//! The file operations LigninDB is built on.
//!
//! [`Vfs`] works on names: it opens files and changes directories. [`VfsFile`]
//! is one open file. Every backend behaves as described here, and the
//! conformance suite in this crate checks it.
//!
//! # Names and files
//!
//! A directory maps names to files, and several names may refer to one file
//! (hard links). A handle refers to the file, not to the name it was opened
//! by: it keeps working after that name is renamed or removed. A file's bytes
//! are freed when its last name and its last handle are gone.
//!
//! Paths are passed to the backend as they are. Build them by joining file
//! names onto a root directory.
//!
//! # Threads
//!
//! Every method takes `&self` and may be called from any thread. Reads may run
//! at the same time as writes; a read sees every write whose call returned
//! before the read began, through any handle of the file. A read that overlaps
//! a write still in progress may see any mix of old and new bytes.
//!
//! # Durability
//!
//! A crash is a power loss or a kernel failure. A process that exits or is
//! killed loses nothing it wrote. After a crash only this is certain:
//!
//! - **Content.** A file's bytes and length are durable once
//!   [`VfsFile::sync_data`] or [`VfsFile::sync_all`] returns `Ok` on any handle
//!   of that file. That covers every write, append and length change that
//!   returned before the sync began. Of the changes not yet synced, a crash may
//!   keep any subset, in any order, and any of them torn.
//! - **Names.** A name that was created, renamed, linked or removed, and a
//!   directory that was created or removed, is durable once [`Vfs::sync_dir`]
//!   returns `Ok` for the directory that holds the name. Syncing a file does
//!   not make its name durable, and syncing a directory does not make a file's
//!   content durable.
//! - **Rename is atomic.** After a crash the destination name refers to the
//!   file it referred to before the rename or to the renamed file, never to
//!   nothing and never to a mix. Sync the file before renaming it, or the name
//!   may survive without the content.
//!
//! # Errors
//!
//! Operations return [`std::io::Error`]. These kinds are part of the contract:
//!
//! | Kind | When |
//! | --- | --- |
//! | [`NotFound`] | The path, or a directory leading to it, does not exist. |
//! | [`AlreadyExists`] | [`Create::New`], [`Vfs::hard_link`] or [`Vfs::create_dir`] found the name taken. |
//! | [`WouldBlock`] | [`Vfs::lock`]: the lock is held. |
//! | [`UnexpectedEof`] | [`VfsFile::read_exact_at`]: the file ends before the buffer is full. |
//! | [`InvalidInput`] | The handle's [`OpenMode`] does not allow the operation. |
//! | [`DirectoryNotEmpty`] | [`Vfs::remove_dir`]: the directory has entries. |
//! | [`NotADirectory`] | [`Vfs::read_dir`] or [`Vfs::sync_dir`]: the path is not a directory. |
//! | [`StorageFull`] | The device has no space left. |
//! | [`CrossesDevices`] | [`Vfs::hard_link`] or [`Vfs::rename`]: the two paths are on different filesystems. |
//!
//! Any other kind is a failure of the device or the system.
//!
//! # Failures that stick
//!
//! Two failures leave a handle unable to vouch for its file, and the handle
//! then keeps failing instead of reporting a success it cannot back:
//!
//! - After a sync fails, the system may have dropped the unsynced bytes, so a
//!   later sync that succeeds proves nothing about them. Once
//!   [`VfsFile::sync_data`] or [`VfsFile::sync_all`] fails on a handle, both
//!   fail on that handle from then on.
//! - A failed [`VfsFile::append`] is undone: the file keeps the length it had.
//!   If it cannot be undone, every later append on that handle fails.
//!
//! [`NotFound`]: std::io::ErrorKind::NotFound
//! [`AlreadyExists`]: std::io::ErrorKind::AlreadyExists
//! [`WouldBlock`]: std::io::ErrorKind::WouldBlock
//! [`UnexpectedEof`]: std::io::ErrorKind::UnexpectedEof
//! [`InvalidInput`]: std::io::ErrorKind::InvalidInput
//! [`DirectoryNotEmpty`]: std::io::ErrorKind::DirectoryNotEmpty
//! [`NotADirectory`]: std::io::ErrorKind::NotADirectory
//! [`StorageFull`]: std::io::ErrorKind::StorageFull
//! [`CrossesDevices`]: std::io::ErrorKind::CrossesDevices

use std::{ffi::OsString, fmt, io, path::Path};

/// How a file is opened. Every mode can read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    /// Read an existing file.
    Read,
    /// Write at any offset with [`VfsFile::write_at`].
    Write(Create),
    /// Write only at the end of the file with [`VfsFile::append`]. Bytes
    /// already in the file cannot be overwritten through this handle.
    ///
    /// While the handle is open nothing else may write to the file or change
    /// its length, or the offsets [`VfsFile::append`] returns are wrong.
    Append(Create),
}

/// Whether opening a file may create it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Create {
    /// The file must exist.
    Never,
    /// A missing file is created empty. An existing file is opened as it is.
    IfMissing,
    /// The file must not exist, and is created empty.
    New,
}

/// What a directory entry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// A file.
    File,
    /// A directory.
    Dir,
    /// Anything else, such as a symbolic link. Links are not followed.
    Other,
}

/// One entry of a directory, as [`Vfs::read_dir`] lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    /// The entry's name inside its directory.
    pub name: OsString,
    /// What the entry is.
    pub kind: EntryKind,
}

/// A file system: the operations on names.
///
/// See the [module documentation](self) for the contract.
pub trait Vfs: fmt::Debug + Send + Sync {
    /// Opens the file at `path`.
    ///
    /// A file this creates has a name that is not durable until
    /// [`sync_dir`](Self::sync_dir) on its directory.
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>>;

    /// Gives the file named `from` the name `to` and removes the name `from`.
    ///
    /// A file already named `to` is replaced atomically: at every moment, and
    /// after a crash, `to` names one of the two files. Durable after
    /// [`sync_dir`](Self::sync_dir) on the directory of `to`, and on the
    /// directory of `from` if it is a different one.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// Adds `link` as another name for the file named `original`. No bytes are
    /// copied: both names refer to one file from then on.
    ///
    /// Fails with [`AlreadyExists`](io::ErrorKind::AlreadyExists) if `link` is
    /// taken, and with [`CrossesDevices`](io::ErrorKind::CrossesDevices) if the
    /// two paths are on different filesystems.
    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()>;

    /// Removes the name `path`. Other names and open handles of the file keep
    /// working.
    fn remove_file(&self, path: &Path) -> io::Result<()>;

    /// Creates the directory `path`. Its parent must exist.
    fn create_dir(&self, path: &Path) -> io::Result<()>;

    /// Removes the directory `path`, which must be empty.
    fn remove_dir(&self, path: &Path) -> io::Result<()>;

    /// Lists the directory `path`, sorted by name.
    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>>;

    /// Makes every change of names in the directory `path` durable.
    fn sync_dir(&self, path: &Path) -> io::Result<()>;

    /// Takes the exclusive lock kept in the file at `path`, which is created
    /// if it is missing. Locking `<database directory>/LOCK` is what allows
    /// one process at a time into a database directory.
    ///
    /// Fails with [`WouldBlock`](io::ErrorKind::WouldBlock) while another
    /// guard for the same file is alive, in this process or in any other. The
    /// lock is released when the guard is dropped or its process ends.
    ///
    /// The lock file stays in place afterwards. Removing or renaming it while
    /// anything may still lock it breaks the exclusion.
    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>>;
}

/// One open file.
///
/// See the [module documentation](self) for the contract.
#[allow(clippy::len_without_is_empty)]
pub trait VfsFile: fmt::Debug + Send + Sync {
    /// Reads from `offset` until `buf` is full or the file ends, and returns
    /// the number of bytes read. A count below `buf.len()` means the file ends
    /// there.
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize>;

    /// Fills `buf` from `offset`. Fails with
    /// [`UnexpectedEof`](io::ErrorKind::UnexpectedEof) if the file ends first;
    /// `buf` is then unspecified.
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        if self.read_at(buf, offset)? == buf.len() {
            Ok(())
        } else {
            Err(io::ErrorKind::UnexpectedEof.into())
        }
    }

    /// Writes all of `buf` at `offset`. The file grows if the write ends past
    /// its length, and a gap this leaves reads as zeros.
    ///
    /// Needs [`OpenMode::Write`]. An empty `buf` changes nothing. If the write
    /// fails, any part of the range may have been written.
    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()>;

    /// Writes all of `buf` at the end of the file and returns the offset it
    /// starts at. Appends through one handle do not interleave: each lands
    /// whole at the offset it returns.
    ///
    /// Needs [`OpenMode::Append`]. If it fails, the file keeps the length it
    /// had and the next append starts there; if the file could not be put
    /// back, every later append on this handle fails.
    fn append(&self, buf: &[u8]) -> io::Result<u64>;

    /// Makes the file's bytes and length durable.
    fn sync_data(&self) -> io::Result<()>;

    /// Makes the file's bytes, length and every other attribute durable.
    fn sync_all(&self) -> io::Result<()>;

    /// The file's length in bytes.
    fn len(&self) -> io::Result<u64>;

    /// Sets the file's length. Shrinking drops the bytes past `len`; growing
    /// adds zeros. On an [`OpenMode::Append`] handle the next append starts at
    /// `len`.
    ///
    /// Needs [`OpenMode::Write`] or [`OpenMode::Append`].
    fn set_len(&self, len: u64) -> io::Result<()>;
}

/// A held lock from [`Vfs::lock`]. Dropping it releases the lock.
pub trait VfsLock: fmt::Debug + Send + Sync {}
