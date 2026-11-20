//! The [`Vfs`] backend over the operating system's files.
//!
//! It is compiled for Linux and macOS only, the platforms LigninDB supports.
//! It holds no `unsafe` code. Its system calls are `std`'s, and `std` picks
//! them for each platform, but for one that `std` does not have: the plain
//! `fsync` of macOS, which [`Vfs::sync_together`] needs there and which comes
//! from `rustix`. What runs on each:
//!
//! | Operation | Linux | macOS |
//! | --- | --- | --- |
//! | [`VfsFile::sync_all`] | `fsync` | `fcntl(F_FULLFSYNC)` |
//! | [`VfsFile::sync_data`] | `fdatasync` | `fcntl(F_FULLFSYNC)` |
//! | [`Vfs::sync_dir`] | `fsync` on the directory, opened read-only | `fcntl(F_FULLFSYNC)` on the directory, opened read-only |
//! | [`Vfs::sync_together`] | for each item of this backend, its call above, in the order of the list, until one fails | for its items on an APFS volume, `fsync` on each and then `fcntl(F_FULLFSYNC)` once for the volume, and for each other item of this backend its call above, until one fails |
//! | [`Vfs::lock`] | `flock(LOCK_EX \| LOCK_NB)` | the same |
//! | [`Vfs::hard_link`] | `linkat` | the same |
//! | [`Vfs::rename`] | `rename` | the same |
//! | [`VfsFile::read_at`], [`VfsFile::write_at`] | `pread64`, `pwrite64` | `pread`, `pwrite` |
//! | [`VfsFile::append`] | `write` on an `O_APPEND` descriptor | the same |
//! | [`VfsFile::set_len`] | `ftruncate` | the same |
//!
//! On macOS `fsync` hands the bytes to the drive without asking it to write
//! them out, and there is no `fdatasync`, so both syncs are the slower
//! `F_FULLFSYNC`. A volume that does not support it cannot be synced.
//!
//! The lock is a `flock`, which belongs to the open file and not to the
//! process: a second attempt in the same process is refused like one from
//! another process. Writers are kept apart the same way: an append handle
//! holds an exclusive `flock` on its file and a write handle a shared one.
//! A `flock` holds back this backend in every process. It does not hold back
//! a program that opens the file some other way.
//!
//! The atomic rename, the changes of names that are kept only with the
//! changes they build on, and the undamaged bytes that the contract promises
//! after a crash are what a journaling filesystem on sector-atomic storage
//! gives: ext4, XFS, btrfs and APFS.
//!
//! An append is a single `write` unless the system writes only part of the
//! buffer, and it never overwrites: the descriptor is `O_APPEND`, so the
//! system places the bytes at the end of the file whatever this backend
//! believes the length to be.
//!
//! [`Vfs::rename`] is `rename` for a directory as for a file. On Linux and
//! on macOS that call replaces a directory only if it has no entries, and
//! refuses to move a directory into itself.
//!
//! # A set synced together
//!
//! On Linux [`Vfs::sync_together`] makes the single calls of its items one
//! after another: `fdatasync` or `fsync` for a file of this backend, `fsync`
//! for a directory. Each is a whole sync when it returns, so what is durable,
//! and when, is what the single calls give. A file of the list that another
//! file layer opened is synced by that layer's own call, on Linux and on
//! macOS.
//!
//! On macOS the items of this backend that are on an APFS volume share a
//! flush: `fsync` is called for each of them, and then `fcntl(F_FULLFSYNC)`
//! once for each volume, on the last item of the list that is on it. A volume
//! is told by the device number of the open file (`st_dev`), and APFS by the
//! name of the file system that `fstatfs` gives. An item on any other file
//! system is synced by its single call, with a full flush of its own.
//!
//! The items that share a flush are synced after the others of the list, those
//! of each volume in one turn apart from every other sync this backend makes
//! in the process. From the first `fsync` of a turn to its flush, and until a
//! failure of that flush is on record for every handle among the turn's items,
//! this backend's `fsync` and `F_FULLFSYNC` calls for a single sync or for
//! another set wait, whatever volume they are for. So in one process no flush
//! of this backend is made, or fails, between a set's hand-overs and its
//! flush, and no sync of a handle is made between a failed flush and the
//! record that stops the handle. Any number of single syncs hold a turn at
//! once. In which order waiting syncs get their turns is the lock's affair and
//! is not promised: a set can wait for as long as single syncs are under way,
//! and single syncs for as long as sets are, so where sets share a flush one
//! slow sync, on any volume, can hold up the others of the process.
//!
//! The directories among the items that share a flush are opened as the list
//! is read and stay open until the call ends, one descriptor for each such
//! item.
//!
//! That one full flush is enough on APFS rests on three things.
//!
//! - **What Apple says of the two calls.** `fsync(2)` on macOS: it "causes
//!   all modified data and attributes of fildes to be moved to a permanent
//!   storage device", but "the drive itself may not physically write the data
//!   to the platters for quite some time". `fcntl(2)` on macOS, under
//!   `F_FULLFSYNC`: "Does the same thing as fsync(2) then asks the drive to
//!   flush all buffered data to the permanent storage device (arg is
//!   ignored). As this drains the entire queue of the device and acts as a
//!   barrier, data that had been fsync'd on the same device before is
//!   guaranteed to be persisted when this call returns. This is currently
//!   implemented on HFS, MS-DOS (FAT), Universal Disk Format (UDF) and APFS
//!   file systems." Under `F_BARRIERFSYNC` the same page gives the pattern
//!   for a set: "fsync(2) each FD and issue a single barrier". Both pages
//!   are quoted as macOS 27.0.1 has them.
//! - **What APFS saves.** It is not published as source. Apple's reference
//!   for it says that a container, which holds the volumes of one partition,
//!   is saved as a whole: "At the end of each transaction, new state is
//!   saved by writing a checkpoint", and a checkpoint whose writing was
//!   interrupted "is ignored the next time the device is mounted, rolling the
//!   file system back to the last valid state" (Apple File System Reference,
//!   2020-06-22, "Container" and "About Apple File System").
//! - **What the device holds afterwards.** The experiment of
//!   `std_fs/tests/device_probe.rs` makes changes of every kind the contract
//!   has on a disk image, syncs them, and mounts a copy of the image file
//!   that is taken right after the sync returned. On an APFS image on macOS
//!   27.0.1 the copy has every one of the changes after `fsync` of each item
//!   and one `F_FULLFSYNC` on the last, in each of the three orders of the
//!   list it tries: the last item a directory that was not changed, a file
//!   that was appended to, and a file that was only read. After the `fsync`
//!   calls alone the copy has none of the changes.
//!
//! HFS+ is why the flush is shared on APFS only, and not wherever
//! `F_FULLFSYNC` works. In the HFS+ source, a write past the end of a file,
//! or a growth of its length, leaves a range that is not yet written and a
//! time until which it need not be (`hfs_readwrite.c`, `c_zftimeout`). A
//! plain `fsync` within that time does not fill the range (`hfs_fsync` in
//! `core/hfs_vnops.c`), and the length it puts on record ends where the
//! range begins ("Trim cf_size to first invalid range",
//! `hfs_prepare_fork_for_update`). `F_FULLFSYNC` on that file fills the
//! range and records the whole length. It then flushes the journal of the
//! whole volume and the cache of the device: "the journal_flush or
//! hfs_metasync_all will push out all of the metadata changes", as a comment
//! in `hfs_fsync` has it. So on HFS+ one `F_FULLFSYNC` after an `fsync` of
//! each item makes durable the shorter length that the `fsync` recorded.
//! The experiment shows it on an HFS+ image: a file written 1 MiB past its
//! start is empty in the copy, and a file grown to 3 MiB has 16 KiB.
//! `F_FULLFSYNC` is thus not everywhere `fsync` and then a flush, whatever
//! the manual page says, and this backend shares a flush only on the file
//! system on which the experiment finds the set whole.
//!
//! Other storage engines do not share a flush. On macOS RocksDB and LevelDB
//! call `F_FULLFSYNC` for every sync of a file and of a directory, and LMDB
//! for every sync. SQLite and PostgreSQL call it only where they are asked
//! to (`PRAGMA fullfsync`, `wal_sync_method = fsync_writethrough`), and
//! SQLite syncs a directory with `fsync` alone. Git does share one: with
//! `core.fsyncMethod=batch` it calls `fsync` for each new object file and
//! then `F_FULLFSYNC` once, on a file it makes for that, and its
//! documentation expects this to be "as safe as `fsync` on macOS for repos
//! stored on HFS+ or APFS filesystems".
//!
//! The limits of this. The flush is made for each volume, not once for a
//! list, because Apple's sentence is about "the same device" and the volume
//! is what an open file tells of its device: two volumes of one APFS
//! container get a flush each. A volume on which `F_FULLFSYNC` fails cannot
//! be synced by this call either. The experiment was made on macOS 27.0.1,
//! on an image of 64 MiB that is one device, with lists on one volume: other
//! versions, a container on several devices and far larger transactions are
//! taken to behave the same on Apple's word, not on a run. And it shows what
//! had reached the image file shortly after a set's calls returned, not that
//! a device keeps it through a power cut: that the flush makes it so is
//! Apple's statement, and no test of this crate cuts the power.
//!
//! A flush that this backend does not make in this process can fail between a
//! set's hand-overs and its flush without the set seeing it: one that other
//! code of the process asks for (another storage engine, another copy of this
//! crate, a file that is synced past this backend), another process, or the
//! system of its own accord. What a device then does with the bytes it was
//! handed is not known here. A single `F_FULLFSYNC` has the same gap between
//! its two halves, and a file whose bytes the system wrote to the device early
//! has it for longer; a set has it from its first hand-over to its flush.

use std::{
    any::Any,
    fs::{self, File, OpenOptions, TryLockError},
    io::{self, ErrorKind, Write},
    os::unix::fs::{FileExt, MetadataExt},
    path::Path,
    sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use crate::vfs::{Create, DirEntry, EntryKind, OpenMode, SyncItem, Vfs, VfsFile, VfsLock};

const MAX_FILE_LEN: u64 = i64::MAX as u64;

/// The [`Vfs`] over the operating system's files.
#[derive(Clone, Copy, Debug, Default)]
pub struct StdVfs;

impl Vfs for StdVfs {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        Ok(Box::new(StdFile::open(path, mode)?))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        fs::rename(from, to)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        fs::hard_link(original, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        fs::create_dir(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        fs::remove_dir(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                // Removed since it was listed: it is not in the directory.
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let kind = if file_type.is_file() {
                EntryKind::File
            } else if file_type.is_dir() {
                EntryKind::Dir
            } else {
                EntryKind::Other
            };
            entries.push(DirEntry {
                name: entry.file_name(),
                kind,
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        sync_dir_using(&open_dir(path)?, File::sync_all)
    }

    fn sync_together(&self, items: &[SyncItem<'_>]) -> io::Result<()> {
        sync_together_using(items, &SYSTEM)
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        Ok(Box::new(StdLock::acquire(path, true, || {})?))
    }

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        Ok(Box::new(StdLock::acquire(path, false, || {})?))
    }
}

#[derive(Debug)]
struct StdFile {
    file: File,
    access: Access,
    sync_failure: Mutex<Option<String>>,
}

#[derive(Debug)]
enum Access {
    Read,
    Write,
    Append(Mutex<AppendEnd>),
}

#[derive(Debug)]
struct AppendEnd {
    len: u64,
    broken: Option<String>,
}

impl StdFile {
    fn open(path: &Path, mode: OpenMode) -> io::Result<Self> {
        let mut options = OpenOptions::new();
        options.read(true);
        let create = match mode {
            OpenMode::Read => Create::Never,
            OpenMode::Write(create) => {
                options.write(true);
                create
            }
            OpenMode::Append(create) => {
                options.append(true);
                create
            }
        };
        options
            .create(create == Create::IfMissing)
            .create_new(create == Create::New);
        let file = options.open(path)?;
        let sharing = match mode {
            OpenMode::Read => Ok(()),
            OpenMode::Write(_) => file.try_lock_shared(),
            OpenMode::Append(_) => file.try_lock(),
        };
        if let Err(error) = sharing {
            return Err(match error {
                TryLockError::WouldBlock => {
                    path_error(ErrorKind::ResourceBusy, path, "has another writer")
                }
                TryLockError::Error(error) => error,
            });
        }
        let metadata = file.metadata()?;
        if metadata.is_dir() {
            return Err(path_error(ErrorKind::IsADirectory, path, "is a directory"));
        }
        let access = match mode {
            OpenMode::Read => Access::Read,
            OpenMode::Write(_) => Access::Write,
            OpenMode::Append(_) => Access::Append(Mutex::new(AppendEnd {
                len: metadata.len(),
                broken: None,
            })),
        };
        Ok(Self {
            file,
            access,
            sync_failure: Mutex::new(None),
        })
    }

    /// A sync of this handle that is made alone.
    fn sync_using(&self, sync: impl FnOnce(&File) -> io::Result<()>) -> io::Result<()> {
        let _turn = beside_other_syncs();
        self.sync_in_turn(sync)
    }

    /// A sync of this handle by a caller that has taken a turn for it.
    fn sync_in_turn(&self, sync: impl FnOnce(&File) -> io::Result<()>) -> io::Result<()> {
        let mut failure = guard(&self.sync_failure)?;
        still_syncing(&failure)?;
        sync(&self.file).inspect_err(|error| *failure = Some(failed_sync(error)))
    }

    fn append_using(
        &self,
        buf: &[u8],
        mut write: impl FnMut(&File, &[u8]) -> io::Result<usize>,
    ) -> io::Result<u64> {
        let Access::Append(end) = &self.access else {
            return Err(wrong_mode(
                "append needs a file opened with OpenMode::Append",
            ));
        };
        let mut end = guard(end)?;
        if let Some(broken) = &end.broken {
            return Err(io::Error::other(broken.clone()));
        }
        let start = end.len;
        let mut written = 0;
        while written < buf.len() {
            let error = match write(&self.file, &buf[written..]) {
                Ok(0) => ErrorKind::WriteZero.into(),
                Ok(count) => {
                    written += count;
                    continue;
                }
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) => error,
            };
            if let Err(undo) = self.undo_append(start, written) {
                end.broken = Some(format!(
                    "an earlier append to this file failed ({error}) and could not be undone \
                     ({undo}), so this handle no longer knows where the file ends"
                ));
            }
            return Err(error);
        }
        end.len = start + buf.len() as u64;
        Ok(start)
    }

    fn undo_append(&self, start: u64, written: usize) -> io::Result<()> {
        let torn_end = start + written as u64;
        let len = self.file.metadata()?.len();
        if len != torn_end {
            return Err(io::Error::other(format!(
                "the file is {len} bytes long where {torn_end} were expected, so something else \
                 changed it"
            )));
        }
        if written > 0 {
            self.file.set_len(start)?;
        }
        Ok(())
    }
}

impl VfsFile for StdFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        let within_any_file = MAX_FILE_LEN.saturating_sub(offset);
        let buf = match usize::try_from(within_any_file) {
            Ok(len) if len < buf.len() => &mut buf[..len],
            _ => buf,
        };
        let mut filled = 0;
        while filled < buf.len() {
            match self
                .file
                .read_at(&mut buf[filled..], offset + filled as u64)
            {
                Ok(0) => break,
                Ok(count) => filled += count,
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(filled)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        let Access::Write = self.access else {
            return Err(wrong_mode(
                "write_at needs a file opened with OpenMode::Write",
            ));
        };
        self.file.write_all_at(buf, offset)
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        self.append_using(buf, |mut file, bytes| file.write(bytes))
    }

    fn sync_data(&self) -> io::Result<()> {
        self.sync_using(File::sync_data)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.sync_using(File::sync_all)
    }

    fn len(&self) -> io::Result<u64> {
        Ok(self.file.metadata()?.len())
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        match &self.access {
            Access::Read => Err(wrong_mode(
                "set_len needs a file opened with OpenMode::Write or OpenMode::Append",
            )),
            Access::Write => self.file.set_len(len),
            Access::Append(end) => {
                let mut end = guard(end)?;
                self.file.set_len(len)?;
                end.len = len;
                Ok(())
            }
        }
    }
}

impl Drop for StdFile {
    fn drop(&mut self) {
        if !matches!(self.access, Access::Read) {
            unlock_now(&self.file);
        }
    }
}

/// Fails for a handle whose syncs were stopped, as they keep failing.
fn still_syncing(failure: &Option<String>) -> io::Result<()> {
    match failure {
        Some(failure) => Err(io::Error::other(failure.clone())),
        None => Ok(()),
    }
}

fn failed_sync(error: &io::Error) -> String {
    format!(
        "an earlier sync of this file failed ({error}), so this handle can no longer make it \
         durable"
    )
}

fn open_dir(path: &Path) -> io::Result<File> {
    let dir = File::open(path)?;
    if !dir.metadata()?.is_dir() {
        return Err(path_error(
            ErrorKind::NotADirectory,
            path,
            "is not a directory",
        ));
    }
    Ok(dir)
}

/// The turns of the syncs this backend makes, in the whole process.
///
/// A sync that is made alone holds it shared, with any number of others. A
/// set holds it for itself once for the items of each file system that
/// share a flush: from the first of them that it hands to the device to the
/// flush that covers them, and until a failure of that flush is on record.
/// So in this process no flush of this backend is made, or fails, between a
/// set's hand-overs and its flush, and no sync of a handle is made between a
/// failed flush and the record that stops that handle.
///
/// It guards nothing a panic could leave half done, so a turn is taken
/// whether or not an earlier holder panicked.
static SYNC_TURNS: RwLock<()> = RwLock::new(());

fn beside_other_syncs() -> RwLockReadGuard<'static, ()> {
    SYNC_TURNS.read().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(any(target_os = "macos", test))]
fn apart_from_other_syncs() -> RwLockWriteGuard<'static, ()> {
    SYNC_TURNS.write().unwrap_or_else(PoisonError::into_inner)
}

/// The sync of a directory that is made alone.
fn sync_dir_using(dir: &File, sync: impl FnOnce(&File) -> io::Result<()>) -> io::Result<()> {
    let _turn = beside_other_syncs();
    sync(dir)
}

/// Which single sync an item of a set stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Member {
    Data,
    All,
    Dir,
}

type FileCall<'a> = &'a dyn Fn(&File) -> io::Result<()>;
type FileSystemOf<'a> = &'a dyn Fn(&File) -> io::Result<Option<u64>>;
type Turn<'a> = &'a dyn Fn() -> RwLockWriteGuard<'static, ()>;

/// How a set is made durable.
#[derive(Clone, Copy)]
struct SetSync<'a> {
    /// The whole sync of one item, as its single call makes it.
    alone: &'a dyn Fn(&File, Member) -> io::Result<()>,
    /// How the items on one file system share a flush, where they can.
    shared: Option<SharedFlush<'a>>,
}

#[derive(Clone, Copy)]
struct SharedFlush<'a> {
    /// The file system an open file or directory is on, if the items on it
    /// can share a flush. An item on any other is synced alone.
    file_system: FileSystemOf<'a>,
    /// Hands one item to its device: its sync but for the flush.
    hand_over: FileCall<'a>,
    /// The flush. Asked on one item, it makes durable what was handed over
    /// of every item on that item's file system.
    flush: FileCall<'a>,
    /// Takes the turn in which the items on one file system are handed over
    /// and flushed, apart from every other sync (see [`SYNC_TURNS`]).
    turn: Turn<'a>,
}

impl SetSync<'_> {
    /// The file system `file` is on, if the items on it share a flush.
    fn shared_on(&self, file: &File) -> io::Result<Option<u64>> {
        match &self.shared {
            Some(shared) => (shared.file_system)(file),
            None => Ok(None),
        }
    }
}

/// On Linux each item is synced by its single call.
#[cfg(target_os = "linux")]
const SYSTEM: SetSync<'static> = SetSync {
    alone: &sync_alone,
    shared: None,
};

/// On macOS the items on one APFS volume are handed to the device with
/// `fsync` and share one `fcntl(F_FULLFSYNC)`. The module documentation says
/// what that rests on, and why an item on another file system is synced
/// alone.
#[cfg(target_os = "macos")]
const SYSTEM: SetSync<'static> = SetSync {
    alone: &sync_alone,
    shared: Some(SharedFlush {
        file_system: &apfs_volume,
        hand_over: &hand_to_device,
        flush: &full_flush,
        turn: &apart_from_other_syncs,
    }),
};

fn sync_alone(file: &File, member: Member) -> io::Result<()> {
    match member {
        Member::Data => file.sync_data(),
        Member::All | Member::Dir => file.sync_all(),
    }
}

/// The volume `file` is on, by its device number, if it is an APFS volume.
#[cfg(target_os = "macos")]
fn apfs_volume(file: &File) -> io::Result<Option<u64>> {
    let on = rustix::fs::fstatfs(file)?;
    let kind = on.f_fstypename.iter().take_while(|byte| **byte != 0);
    if !kind.map(|byte| *byte as u8).eq(*b"apfs") {
        return Ok(None);
    }
    Ok(Some(file.metadata()?.dev()))
}

/// `fsync`, which `std` does not call on macOS.
#[cfg(target_os = "macos")]
fn hand_to_device(file: &File) -> io::Result<()> {
    loop {
        match rustix::fs::fsync(file) {
            Err(rustix::io::Errno::INTR) => {}
            done => return done.map_err(io::Error::from),
        }
    }
}

/// `fcntl(F_FULLFSYNC)`, which is what `std` syncs a file with on macOS.
#[cfg(target_os = "macos")]
fn full_flush(file: &File) -> io::Result<()> {
    file.sync_all()
}

/// The items of a set on one file system on which they share a flush, in
/// the order of the list.
struct Sharing<'a> {
    file_system: u64,
    items: Vec<Shared<'a>>,
}

enum Shared<'a> {
    Handle(&'a StdFile),
    Dir(File),
}

impl Shared<'_> {
    fn file(&self) -> &File {
        match self {
            Self::Handle(handle) => &handle.file,
            Self::Dir(dir) => dir,
        }
    }

    fn handle(&self) -> Option<&StdFile> {
        match self {
            Self::Handle(handle) => Some(handle),
            Self::Dir(_) => None,
        }
    }
}

/// Puts an item among those that share the flush of its file system.
fn share<'a>(sharing: &mut Vec<Sharing<'a>>, file_system: u64, item: Shared<'a>) {
    let on_it = |group: &Sharing<'_>| group.file_system == file_system;
    match sharing.iter().position(on_it) {
        Some(at) => sharing[at].items.push(item),
        None => sharing.push(Sharing {
            file_system,
            items: vec![item],
        }),
    }
}

/// Syncs a set: in the order of the list every item that shares no flush, by
/// its single call, and then the items of each file system that do share
/// one.
fn sync_together_using(items: &[SyncItem<'_>], set: &SetSync<'_>) -> io::Result<()> {
    let mut sharing = Vec::new();
    for item in items {
        let (member, file) = match *item {
            SyncItem::Data(file) => (Member::Data, file),
            SyncItem::All(file) => (Member::All, file),
            SyncItem::Dir(path) => {
                let dir = open_dir(path)?;
                match set.shared_on(&dir)? {
                    Some(file_system) => share(&mut sharing, file_system, Shared::Dir(dir)),
                    None => sync_dir_using(&dir, |dir| (set.alone)(dir, Member::Dir))?,
                }
                continue;
            }
        };
        let any: &dyn Any = file;
        let Some(handle) = any.downcast_ref::<StdFile>() else {
            // A file of another file layer: that layer's own call syncs it
            // whole, so it shares no flush of this one.
            if member == Member::Data {
                file.sync_data()?;
            } else {
                file.sync_all()?;
            }
            continue;
        };
        still_syncing(&*guard(&handle.sync_failure)?)?;
        match set.shared_on(&handle.file)? {
            Some(file_system) => share(&mut sharing, file_system, Shared::Handle(handle)),
            None => handle.sync_using(|file| (set.alone)(file, member))?,
        }
    }
    let Some(shared) = &set.shared else {
        return Ok(());
    };
    for group in &sharing {
        sync_sharing(group, shared)?;
    }
    Ok(())
}

/// Syncs the items of `group` with one flush.
///
/// It is one turn apart from every other sync (see [`SYNC_TURNS`]). In it
/// the handles are looked at, so that one whose syncs were stopped fails the
/// set before any item of the group is handed over; every item is handed to
/// the device; the flush is made on the last of them; and a failure of the
/// flush is put on record for every handle of the group.
fn sync_sharing(group: &Sharing<'_>, shared: &SharedFlush<'_>) -> io::Result<()> {
    let Some(last) = group.items.last() else {
        return Ok(());
    };
    let handles = || group.items.iter().filter_map(Shared::handle);
    let _turn = (shared.turn)();
    for handle in handles() {
        still_syncing(&*guard(&handle.sync_failure)?)?;
    }
    for item in &group.items {
        match item {
            Shared::Handle(handle) => handle.sync_in_turn(shared.hand_over)?,
            Shared::Dir(dir) => (shared.hand_over)(dir)?,
        }
    }
    (shared.flush)(last.file()).inspect_err(|error| {
        for handle in handles() {
            if let Ok(mut failure) = guard(&handle.sync_failure) {
                *failure = Some(failed_sync(error));
            }
        }
    })
}

#[derive(Debug)]
struct StdLock {
    file: File,
}

impl StdLock {
    /// Takes the lock of the file at `path`, which is made if it is not
    /// there and `create` says so. `locked` is run each time a file was
    /// locked, before it is looked at again.
    fn acquire(path: &Path, create: bool, mut locked: impl FnMut()) -> io::Result<Self> {
        loop {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(create)
                .truncate(false)
                .open(path)?;
            if let Err(error) = file.try_lock() {
                return Err(match error {
                    TryLockError::WouldBlock => {
                        path_error(ErrorKind::WouldBlock, path, "is locked")
                    }
                    TryLockError::Error(error) => error,
                });
            }
            locked();
            let lock = Self { file };
            // Otherwise the file was removed or replaced after it was opened,
            // and the lock that counts is on the file the path names now.
            if lock.is_named(path)? {
                return Ok(lock);
            }
        }
    }

    fn is_named(&self, path: &Path) -> io::Result<bool> {
        let held = self.file.metadata()?;
        match fs::metadata(path) {
            Ok(named) => Ok(named.dev() == held.dev() && named.ino() == held.ino()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

impl VfsLock for StdLock {
    fn is_at(&self, path: &Path) -> io::Result<bool> {
        match self.is_named(path) {
            // A directory above the name is gone, or is a file now.
            Err(error) if error.kind() == ErrorKind::NotADirectory => Ok(false),
            named => named,
        }
    }
}

impl Drop for StdLock {
    fn drop(&mut self) {
        unlock_now(&self.file);
    }
}

/// Closing a file releases its `flock` only once no copy of the descriptor is
/// left, and a child process forked on another thread holds one until it
/// calls exec. Unlocking releases it at once.
fn unlock_now(file: &File) {
    let _ = file.unlock();
}

fn guard<T>(state: &Mutex<T>) -> io::Result<MutexGuard<'_, T>> {
    state
        .lock()
        .map_err(|_| io::Error::other("an earlier operation on this file panicked"))
}

fn wrong_mode(message: &'static str) -> io::Error {
    io::Error::new(ErrorKind::InvalidInput, message)
}

fn path_error(kind: ErrorKind, path: &Path, problem: &str) -> io::Error {
    io::Error::new(kind, format!("{} {problem}", path.display()))
}

#[cfg(test)]
mod tests;
