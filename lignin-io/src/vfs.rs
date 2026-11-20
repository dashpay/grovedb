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
//! # Writers
//!
//! A file is open for writing either through one [`OpenMode::Append`] handle
//! or through any number of [`OpenMode::Write`] handles, never both at once.
//! An append handle is the only writer of its file, which is what makes the
//! offsets it returns right. An open that would break the rule fails with
//! [`ResourceBusy`]. [`OpenMode::Read`] handles are never refused.
//!
//! The rule covers every handle of the file, under any of its names, and for
//! the files of an operating system it covers every process.
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
//! - **Synced content.** A file's bytes and length are durable once
//!   [`VfsFile::sync_data`] or [`VfsFile::sync_all`] returns `Ok` on any handle
//!   of that file. That covers every write, append and length change that
//!   returned before the sync began, unless a sync of the file failed in
//!   between (see below).
//! - **Unsynced content.** After a crash a file is its last synced state with
//!   some of the later changes applied: any subset of them, in any order, and
//!   a write possibly only in part, torn at any byte. Each lands where it was
//!   made, an append at the offset it returned. Where a length survives
//!   without the bytes written below it, those bytes read as zeros. And the
//!   bytes of a write that grew the file can survive without the growth: the
//!   write then ends where the file ended before it.
//! - **Nothing else is damaged.** A byte that no unsynced change touched keeps
//!   its value. This assumes storage that writes a sector whole or not at
//!   all, which is what lets a log be appended to behind bytes that are
//!   already durable.
//! - **Names.** A name that was created, renamed, linked or removed, and a
//!   directory that was created or removed, is durable once [`Vfs::sync_dir`]
//!   returns `Ok` for the directory that holds the name. Syncing a file does
//!   not make its name durable. Syncing a directory does not make a file's
//!   content durable, nor the directory's own name: a file created and synced
//!   in a new directory is gone after a crash, with the directory, unless the
//!   directory that holds the new one was synced as well.
//! - **Unsynced changes of names.** Of the changes of names not yet synced a
//!   crash may keep any subset in which each change comes with the earlier
//!   changes it builds on. A change builds on the earlier unsynced changes of
//!   the names it touches. The removal of a directory also builds on the
//!   changes that emptied it, and a rename on the creation of the directories
//!   its two names are in and of every directory above them. So a crash can
//!   bring back a name that was removed or renamed away, such as a temporary
//!   file, but it cannot keep a file created under a name and lose the rename
//!   that freed that name. Two things follow. A file whose name is durable,
//!   and that is only renamed after that, never removed or replaced, has a
//!   name after every crash. And when every directory and every file has been
//!   synced since its last change, and no sync of them failed, nothing is
//!   unsynced: a crash then loses nothing. A sync may make an earlier change
//!   durable sooner than promised here, when a change it makes durable builds
//!   on it, and never later. This rule, like the atomic rename, assumes a
//!   filesystem that journals its directories. A hard link and the removal of
//!   the old name are two changes, not a rename: a crash that keeps the
//!   removal and loses the link loses the file.
//! - **Rename is atomic, synced or not.** A crash keeps a rename whole or not
//!   at all. The destination name then refers to what it referred to before
//!   the rename or to the renamed file: never to a mix of the two, and never
//!   to nothing if it named a file before. Which of the two is settled only
//!   by [`Vfs::sync_dir`]. This assumes a filesystem that journals its
//!   directories. Sync the file before renaming it, or the name may survive
//!   without the content.
//! - **A failed sync cannot be made good.** When a sync of a file fails, the
//!   changes that were not yet synced are left unknown: the system may have
//!   written them or dropped them, and it will not say which. A later sync
//!   that returns `Ok`, on that handle or on any other, covers only what was
//!   written after the failure. To rely on the earlier changes, write them
//!   again, through another handle or to a new file, and sync that.
//!
//! # Locks
//!
//! A lock is held in a lock file by one guard at a time ([`Vfs::lock`]):
//! in this process, and for the files of an operating system in every
//! other. It is what lets one process at a time into a directory.
//!
//! - **The guard is on the file, not on its name**, as a handle is. When
//!   the lock file is removed, or its directory is removed and made
//!   again, or a copy of the directory is put in its place, the guard is
//!   still held and keeps nobody out of the file that has the name now.
//!   [`VfsLock::is_at`] says whether a path names the guard's file. It
//!   compares the two files, by device and inode number where an operating
//!   system has them, and takes no lock and writes nothing.
//! - **[`Vfs::lock`] makes the lock file if it is missing**, which is a
//!   change of names like any creation. [`Vfs::lock_existing`] makes none:
//!   it fails with [`NotFound`], and a directory that was looked at and is
//!   another one by now is not given a file it did not have.
//! - **A lock is not durable.** After a crash nobody holds it. Taking a
//!   lock that is there, with either call, and dropping a guard change
//!   nothing that a crash keeps or loses. The lock file stays.
//!
//! # Errors
//!
//! Operations return [`std::io::Error`]. These kinds are part of the contract:
//!
//! | Kind | When |
//! | --- | --- |
//! | [`NotFound`] | The path, or a directory leading to it, does not exist. |
//! | [`AlreadyExists`] | [`Create::New`], [`Vfs::hard_link`] or [`Vfs::create_dir`] found the name taken. |
//! | [`ResourceBusy`] | [`Vfs::open`]: the file is open for appending, or is open for writing and this open asks to append. |
//! | [`IsADirectory`] | [`Vfs::open`]: the path is a directory. |
//! | [`NotADirectory`] | [`Vfs::read_dir`] or [`Vfs::sync_dir`]: the path is not a directory. |
//! | [`DirectoryNotEmpty`] | [`Vfs::remove_dir`]: the directory has entries. |
//! | [`WouldBlock`] | [`Vfs::lock`], [`Vfs::lock_existing`]: the lock is held. |
//! | [`UnexpectedEof`] | [`VfsFile::read_exact_at`]: the file ends before the buffer is full. |
//! | [`InvalidInput`] | The handle's [`OpenMode`] does not allow the operation. |
//! | [`StorageFull`], [`QuotaExceeded`] | The device, or a quota on it, has no space left. |
//! | [`CrossesDevices`] | [`Vfs::hard_link`] or [`Vfs::rename`]: the two paths are on different filesystems. |
//!
//! Every other kind is whatever the system reported and may differ between
//! backends.
//!
//! # Failures that stick
//!
//! Two failures leave a handle unable to vouch for its file. The call that
//! fails reports the cause. From then on the handle keeps failing with
//! [`Other`], instead of reporting a success it cannot back:
//!
//! - Once [`VfsFile::sync_data`] or [`VfsFile::sync_all`] fails on a handle,
//!   both fail on that handle from then on. The rule above about a failed
//!   sync holds for every handle of the file, including the ones this does not
//!   stop.
//! - A failed [`VfsFile::append`] is undone: the file keeps the length it had.
//!   If it cannot be undone, every later append on that handle fails.
//!
//! [`NotFound`]: std::io::ErrorKind::NotFound
//! [`AlreadyExists`]: std::io::ErrorKind::AlreadyExists
//! [`ResourceBusy`]: std::io::ErrorKind::ResourceBusy
//! [`IsADirectory`]: std::io::ErrorKind::IsADirectory
//! [`NotADirectory`]: std::io::ErrorKind::NotADirectory
//! [`DirectoryNotEmpty`]: std::io::ErrorKind::DirectoryNotEmpty
//! [`WouldBlock`]: std::io::ErrorKind::WouldBlock
//! [`UnexpectedEof`]: std::io::ErrorKind::UnexpectedEof
//! [`InvalidInput`]: std::io::ErrorKind::InvalidInput
//! [`StorageFull`]: std::io::ErrorKind::StorageFull
//! [`QuotaExceeded`]: std::io::ErrorKind::QuotaExceeded
//! [`CrossesDevices`]: std::io::ErrorKind::CrossesDevices
//! [`Other`]: std::io::ErrorKind::Other

use std::{ffi::OsString, fmt, io, path::Path};

/// How a file is opened. Every mode can read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenMode {
    /// Read an existing file.
    Read,
    /// Write at any offset with [`VfsFile::write_at`].
    Write(Create),
    /// Write only at the end of the file with [`VfsFile::append`], which
    /// never overwrites a byte already in the file. While the handle is open
    /// it is the file's only writer: see [Writers](self#writers).
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
    /// Opens the file at `path`, which must not be a directory.
    ///
    /// Fails with [`ResourceBusy`](io::ErrorKind::ResourceBusy) if the file
    /// already has a writer that `mode` cannot share it with: see
    /// [Writers](self#writers).
    ///
    /// A file this creates has a name that is not durable until
    /// [`sync_dir`](Self::sync_dir) on its directory.
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>>;

    /// Gives the file named `from` the name `to` and removes the name `from`.
    /// `from` must name a file, not a directory.
    ///
    /// A file already named `to` is replaced atomically: at every moment, and
    /// after a crash, `to` names one of the two files. If `from` and `to`
    /// already name the same file, nothing changes and both names stay.
    /// Until [`sync_dir`](Self::sync_dir) on the directory of `to`, and on the
    /// directory of `from` if it is a different one, a crash may undo the
    /// rename.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;

    /// Adds `link` as another name for the file named `original`. No bytes are
    /// copied: both names refer to one file from then on.
    ///
    /// Fails with [`AlreadyExists`](io::ErrorKind::AlreadyExists) if `link` is
    /// taken, and with [`CrossesDevices`](io::ErrorKind::CrossesDevices) if the
    /// two paths are on different filesystems.
    ///
    /// The new name is not durable until [`sync_dir`](Self::sync_dir) on its
    /// directory. Linking a file under a new name and removing the old one is
    /// not a [`rename`](Self::rename): a crash may keep the removal and lose
    /// the link, and with it the file.
    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()>;

    /// Removes the name `path`. Other names and open handles of the file keep
    /// working.
    fn remove_file(&self, path: &Path) -> io::Result<()>;

    /// Creates the directory `path`. Its parent must exist.
    ///
    /// Until [`sync_dir`](Self::sync_dir) on the parent, a crash may lose the
    /// new directory with everything in it, whether or not the new directory
    /// itself was synced.
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
    /// guard for the same file is alive. For the files of an operating system
    /// that means in this process or in any other. The lock is released when
    /// the guard is dropped or its process ends.
    ///
    /// The lock file stays in place afterwards, with any content it had.
    /// Removing or renaming it while anything may still lock it breaks the
    /// exclusion. Keep it for locking: whether it can be locked while it is
    /// open for writing, or opened for writing while it is locked, is not
    /// specified.
    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>>;

    /// As [`Self::lock`], for a lock file that is there: no file is made.
    /// Fails with [`NotFound`](io::ErrorKind::NotFound) if `path` names no
    /// file.
    ///
    /// Whoever opens a database that it found in a directory takes the
    /// lock this way, so that a directory which was put in the place of
    /// that one meanwhile, and which is someone else's, is not given a
    /// file it did not have.
    ///
    /// It is one step: no implementation looks for the file and then
    /// calls [`Self::lock`], which would make a file that was removed
    /// between the two. A file layer that is put over another passes the
    /// call on to it.
    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>>;
}

/// One open file.
///
/// See the [module documentation](self) for the contract.
#[allow(clippy::len_without_is_empty)]
pub trait VfsFile: fmt::Debug + Send + Sync {
    /// Reads from `offset` until `buf` is full or the file ends, and returns
    /// the number of bytes read. A count below `buf.len()` means the file ends
    /// there; reading at or past the end gives 0.
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
    ///
    /// A crash can still keep part of an append that was not synced.
    fn append(&self, buf: &[u8]) -> io::Result<u64>;

    /// Makes the file's bytes and length durable.
    ///
    /// Once this or [`sync_all`](Self::sync_all) fails on a handle, both keep
    /// failing on it.
    fn sync_data(&self) -> io::Result<()>;

    /// Makes the file's bytes, length and every other attribute durable.
    ///
    /// Once this or [`sync_data`](Self::sync_data) fails on a handle, both
    /// keep failing on it.
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
pub trait VfsLock: fmt::Debug + Send + Sync {
    /// Whether the file this guard is on is the file that `path` names
    /// now. It is not once the lock file was removed, or another file was
    /// put under its name: the directory was removed and made again, or a
    /// copy of it took its place. A path that names nothing, or whose
    /// directory is gone, is answered with `false`.
    ///
    /// The guard stays as it is, and nothing is written. Whoever is about
    /// to write into the directory of a lock it has held for long asks
    /// this first: the guard keeps others out of the file it is on, and
    /// says nothing of the path.
    ///
    /// `path` is read as the [`Vfs`] that gave the guard reads paths. A
    /// file layer that is put over another and gives paths another
    /// meaning gives guards that read them so too.
    fn is_at(&self, path: &Path) -> io::Result<bool>;
}
