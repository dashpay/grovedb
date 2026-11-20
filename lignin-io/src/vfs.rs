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
//! by: it keeps working after that name is renamed or removed, and after a
//! directory above it is renamed. A file's bytes are freed when its last
//! name and its last handle are gone.
//!
//! Paths are passed to the backend as they are. Build them by joining file
//! names onto a root directory.
//!
//! A name is its bytes, and two names are one when their bytes are the same.
//! Some file systems take two spellings for one name: APFS, as macOS formats
//! it unless told otherwise, does not tell upper case from lower. What an
//! operation does with two such spellings is not specified here. Give each
//! name one spelling.
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
//!   directory that was created, renamed or removed, is durable once
//!   [`Vfs::sync_dir`] returns `Ok` for the directory that holds the name, and
//!   a rename once that is so for the directories of both its names. Syncing
//!   a file does not make its name durable. Syncing a directory does not make
//!   a file's content durable, nor the directory's own name: a file created
//!   and synced in a new directory is gone after a crash, with the directory,
//!   unless the directory that holds the new one was synced as well.
//! - **Unsynced changes of names.** Of the changes of names not yet synced a
//!   crash may keep any subset in which each change comes with the earlier
//!   changes it builds on. A change builds on the earlier unsynced changes of
//!   the names it touches. The removal of a directory also builds on the
//!   changes that emptied it, and so does a rename that replaces an empty
//!   directory. A rename builds as well on what gave the directories its two
//!   names are in, and every directory above them, the names they had when
//!   it was made: of each, its creation or its last rename. So a crash can
//!   bring back a name that was removed or renamed away, such as a temporary
//!   file, but it cannot keep a file created under a name and lose the rename
//!   that freed that name, and it cannot keep a rename into a directory and
//!   lose the one that moved that directory to where it was. Two things
//!   follow. A file or a directory whose name is durable, with the names of
//!   the directories above it, and that is only renamed after that, by its
//!   own name or with a directory above it, and never removed or replaced,
//!   has a name after every crash. And when every directory and every file
//!   has been synced since its last change, and no sync of them failed,
//!   nothing is unsynced: a crash then loses nothing. A sync may make an
//!   earlier change durable sooner than promised here, when a change it makes
//!   durable builds on it, and never later. This rule, like the atomic
//!   rename, assumes a filesystem that journals its directories. A hard link
//!   and the removal of the old name are two changes, not a rename: a crash
//!   that keeps the removal and loses the link loses the file.
//! - **Rename is atomic, synced or not.** A crash keeps a rename whole or not
//!   at all, of a file and of a directory with everything in it: of the two
//!   things a rename does, taking the old name away and giving the new one,
//!   no crash keeps one without the other. The destination name is then as
//!   the rename left it, or as a crash just before the rename would have
//!   left it: never a mix of the two, and never nothing if what it named
//!   before was durable under that name. Which of the two is settled only by
//!   [`Vfs::sync_dir`]. This assumes a filesystem that journals its
//!   directories. A rename makes nothing else durable. Sync a file before
//!   renaming it, or the name may survive without the content, and sync what
//!   is in a directory, its files and its names, before renaming the
//!   directory to a name under which it has to be whole.
//! - **Syncing together.** [`Vfs::sync_together`] takes a list of syncs, each
//!   of a file or of a directory, and returns `Ok` when every one of them has
//!   been made. Each file and each directory of the list is then durable as
//!   its own call would have made it ([`VfsFile::sync_data`],
//!   [`VfsFile::sync_all`] or [`Vfs::sync_dir`]), and nothing more is
//!   promised than those calls promise. Before it returns, a crash may find
//!   any of the syncs made and any not, whatever their order in the list: it
//!   leaves what it could leave if the calls had been made one by one and it
//!   had come after some of them and before the others. The list is not
//!   atomic, and a caller that needs one thing durable before another makes
//!   two calls. What the one call gives is a lower cost where a sync ends with
//!   a flush of a whole device: the file layer may make that flush once for
//!   the list. If the call fails, any of the syncs may have been made, and
//!   the caller is not told which one failed. The next rule then holds for
//!   every file of the list, and no directory of the list is known to be
//!   synced.
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
//!   When the lock file or a directory above it is renamed
//!   ([`Vfs::rename`]), the guard goes with the file: it is held under the
//!   new path, and the old one is free for whatever is put there.
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
//! | [`IsADirectory`] | [`Vfs::open`]: the path is a directory. [`Vfs::rename`] of a file: `to` is a directory. |
//! | [`NotADirectory`] | [`Vfs::read_dir`], [`Vfs::sync_dir`] or a [`SyncItem::Dir`]: the path is not a directory. [`Vfs::rename`] of a directory: `to` is a file. |
//! | [`DirectoryNotEmpty`] | [`Vfs::remove_dir`]: the directory has entries. [`Vfs::rename`] of a directory: `to` is a directory with entries. |
//! | [`WouldBlock`] | [`Vfs::lock`], [`Vfs::lock_existing`]: the lock is held. |
//! | [`UnexpectedEof`] | [`VfsFile::read_exact_at`]: the file ends before the buffer is full. |
//! | [`InvalidInput`] | The handle's [`OpenMode`] does not allow the operation. [`Vfs::rename`] of a directory: `to` is in that directory or below it. |
//! | [`StorageFull`], [`QuotaExceeded`] | The device, or a quota on it, has no space left. |
//! | [`CrossesDevices`] | [`Vfs::hard_link`] or [`Vfs::rename`]: the two paths are on different filesystems. |
//!
//! Every other kind is whatever the system reported and may differ between
//! backends.
//!
//! # Failures that stick
//!
//! Three failures leave a handle unable to vouch for its file. The call that
//! fails reports the cause. From then on the handle keeps failing with
//! [`Other`], instead of reporting a success it cannot back:
//!
//! - Once [`VfsFile::sync_data`] or [`VfsFile::sync_all`] fails on a handle,
//!   both fail on that handle from then on, and so does a
//!   [`Vfs::sync_together`] that has the handle in its list. The rule above
//!   about a failed sync holds for every handle of the file, including the
//!   ones this does not stop.
//! - A [`Vfs::sync_together`] that fails is a failed sync of the handles it
//!   can no longer vouch for: the one whose own sync failed, and every one
//!   whose bytes were waiting for a flush that failed. Those keep failing as
//!   above. The rule about a failed sync holds for every file of the list,
//!   including the ones this does not stop.
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

use std::{any::Any, ffi::OsString, fmt, io, path::Path};

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

/// One of the syncs a [`Vfs::sync_together`] is asked for: a file or a
/// directory, with the single call that would make it durable.
#[derive(Clone, Copy, Debug)]
pub enum SyncItem<'a> {
    /// What [`VfsFile::sync_data`] makes durable of the file.
    Data(&'a dyn VfsFile),
    /// What [`VfsFile::sync_all`] makes durable of the file.
    All(&'a dyn VfsFile),
    /// What [`Vfs::sync_dir`] makes durable of the directory at the path.
    Dir(&'a Path),
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

    /// Gives the file or the directory named `from` the name `to` and
    /// removes the name `from`. A directory moves with everything in it: the
    /// handles of the files in it keep working, and a guard on a lock file
    /// in it is held on that file under its new path
    /// ([`VfsLock::is_at`]).
    ///
    /// What `to` named is replaced atomically: at every moment `to` names
    /// what it named before or what was renamed. After a crash it names what
    /// was renamed, or it is as a crash just before the rename would have
    /// left it, which is what it named before if that name was durable. A file
    /// replaces a file. A directory replaces a directory that has no entries,
    /// and no other. If `from` and `to` are the same name, or two names of
    /// one file, nothing changes and both names stay.
    ///
    /// Fails with
    /// [`IsADirectory`](io::ErrorKind::IsADirectory) if `from` is a file and
    /// `to` a directory, with
    /// [`NotADirectory`](io::ErrorKind::NotADirectory) if `from` is a
    /// directory and `to` a file, with
    /// [`DirectoryNotEmpty`](io::ErrorKind::DirectoryNotEmpty) if `from` is a
    /// directory and `to` a directory with entries, with
    /// [`InvalidInput`](io::ErrorKind::InvalidInput) if `from` is a directory
    /// and `to` is in it or below it, and with
    /// [`CrossesDevices`](io::ErrorKind::CrossesDevices) if the two paths are
    /// on different filesystems. Where more than one of them applies, or a
    /// path is wrong in another way as well, either error may come.
    ///
    /// Until [`sync_dir`](Self::sync_dir) on the directory of `to`, and on the
    /// directory of `from` if it is a different one, a crash may undo the
    /// rename. The directory that was renamed is not one of the two, and the
    /// rename makes nothing in it durable: neither its names nor the content
    /// of its files.
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

    /// Makes the syncs of `items`, and may make them cost less together than
    /// they would one by one: see "Syncing together" under
    /// [Durability](self#durability).
    ///
    /// When it returns `Ok`, each file and each directory of the list is
    /// durable as the call its [`SyncItem`] names would have made it, for
    /// every change that returned before this call began. Until then a crash
    /// may find any of the syncs made and any not. A list with nothing in it
    /// is `Ok`, and a file or a directory may be in the list more than once.
    ///
    /// If it fails, it fails as the call of one of its items does: with
    /// [`NotFound`](io::ErrorKind::NotFound) or
    /// [`NotADirectory`](io::ErrorKind::NotADirectory) for a path that is no
    /// directory, with [`Other`](io::ErrorKind::Other) for a handle whose
    /// sync failed before, or with what the system reported. Any of the
    /// syncs may have been made by then, and the rule for a failed sync
    /// holds for every file of the list.
    ///
    /// The method as it is provided here makes the calls one after another,
    /// in the order of the list, and stops at the first that fails. That is
    /// right for every file layer, and a file layer that is put over another
    /// to count, to fail or to leave out syncs keeps it: what it does to a
    /// single sync is then done to each sync of a list.
    fn sync_together(&self, items: &[SyncItem<'_>]) -> io::Result<()> {
        for item in items {
            match item {
                SyncItem::Data(file) => file.sync_data()?,
                SyncItem::All(file) => file.sync_all()?,
                SyncItem::Dir(path) => self.sync_dir(path)?,
            }
        }
        Ok(())
    }

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
    /// Removing or renaming it, or renaming a directory above it, while
    /// anything may still lock it by this path breaks the exclusion. Keep it
    /// for locking: whether it can be locked while it is
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
/// See the [module documentation](self) for the contract. [`Any`] is what
/// lets a file layer know its own files among those of a
/// [`Vfs::sync_together`].
#[allow(clippy::len_without_is_empty)]
pub trait VfsFile: Any + fmt::Debug + Send + Sync {
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
    /// now. It is not once the lock file was removed or renamed away, or
    /// another file was put under its name: the directory was renamed away,
    /// or removed and made again, or a copy of it took its place. A path that names nothing, or whose
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
