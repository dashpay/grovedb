//! The value-log files of one database directory: their lives from creation
//! to deletion, reading a value, and the counts of live values.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{Arc, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use lignin_format::{
    manifest::{LiveFile, Manifest},
    value_log::{Value, ValuePointer, FIRST_RECORD_OFFSET},
    CommitVersion, FileId, FileKind, FormatVersion, PageId,
};
use lignin_io::{DirEntry, EntryKind, OpenMode, Vfs};

use super::{
    counts::{Account, ChangeMark, FileCount, ValueChange},
    error::{Miscount, ValueStoreError},
    file::ValueFile,
    walk::ValueWalk,
    writer::ValueWriter,
};
use crate::segment::{create_file, list_or_create_dir, FileNames, ManifestStore};

/// The length at which a writer's file is full, unless
/// [`ValueStore::with_target_file_len`] sets another.
pub const DEFAULT_TARGET_FILE_LEN: u64 = 64 << 20;

fn load(
    vfs: &dyn Vfs,
    dir: &Path,
    manifest: &Manifest,
) -> Result<BTreeMap<FileId, Arc<ValueFile>>, ValueStoreError> {
    let mut files = BTreeMap::new();
    for named in manifest.files() {
        if named.kind() == FileKind::ValueLog {
            let file = ValueFile::adopt(vfs, dir, named, manifest.format_version())?;
            files.insert(named.id(), Arc::new(file));
        }
    }
    Ok(files)
}

fn remove_unnamed(
    vfs: &dyn Vfs,
    dir: &Path,
    entries: &[DirEntry],
    named: &BTreeSet<FileId>,
    last_id: Option<FileId>,
) -> Result<(), ValueStoreError> {
    let val_dir = FileNames::VALUE_LOG.dir(dir);
    let mut unnamed = Vec::new();
    for entry in entries {
        if entry.kind != EntryKind::File {
            continue;
        }
        let Some(id) = FileNames::VALUE_LOG.parse(&entry.name) else {
            continue;
        };
        if named.contains(&id) {
            continue;
        }
        let path = val_dir.join(&entry.name);
        if Some(id) > last_id {
            let len = vfs
                .open(&path, OpenMode::Read)
                .and_then(|file| file.len())
                .map_err(ValueStoreError::io("measure", &path))?;
            if len > FIRST_RECORD_OFFSET {
                return Err(ValueStoreError::UnnamedFileHoldsValues { path, len });
            }
        }
        unnamed.push(path);
    }
    for path in unnamed {
        remove(vfs, &path).map_err(ValueStoreError::io("remove", &path))?;
    }
    Ok(())
}

fn remove(vfs: &dyn Vfs, path: &Path) -> std::io::Result<()> {
    match vfs.remove_file(path) {
        Err(error) if error.kind() != ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn named_files(manifest: &Manifest) -> BTreeSet<FileId> {
    manifest
        .files()
        .iter()
        .filter(|file| file.kind() == FileKind::ValueLog)
        .map(LiveFile::id)
        .collect()
}

/// The value log of a database that is being recovered: the files a manifest
/// names, and the counts of live values as recovery brings them up to the
/// version it lands on.
///
/// [`ValueStore::recover`] starts it from the counts a checkpoint of the
/// pages holds. [`Self::apply`] takes each change that recovery replays
/// after that checkpoint, in order. [`Self::finish`] gives the store.
///
/// Until [`Self::finish`], nothing in the directory is changed.
#[derive(Debug)]
pub struct ValueRecovery {
    vfs: Arc<dyn Vfs>,
    dir: PathBuf,
    files: BTreeMap<FileId, Arc<ValueFile>>,
    accounts: BTreeMap<FileId, Account>,
    last_id: Option<FileId>,
    applied: u64,
}

impl ValueRecovery {
    /// Applies one replayed change to the counts.
    ///
    /// A change may name a file the manifest no longer names: a file is
    /// deleted once the changes that emptied it are durable, and the
    /// checkpoint recovery starts from can be older than those. Its count
    /// has to be back at zero by [`Self::finish`].
    ///
    /// The whole change is applied whatever this returns. The error is the
    /// first pointer that could not be counted as reported
    /// ([`ValueStoreError::Miscounted`]): one that ends past the valid length
    /// of its file, which no manifest covered, or a release from a count
    /// that was zero.
    pub fn apply(
        &mut self,
        change: &ValueChange,
        latest: CommitVersion,
    ) -> Result<(), ValueStoreError> {
        self.applied += 1;
        let mut first = Ok(());
        for &pointer in change.acquired() {
            let file = self.files.get(&pointer.file());
            let extent = pointer.extent_len(version_of(file));
            self.accounts
                .entry(pointer.file())
                .or_insert(Account::EMPTY)
                .acquire(extent, self.applied);
            if file.is_some_and(|file| !file.contains(pointer)) {
                first = first.and(miscounted(pointer, Miscount::OutOfBounds));
            }
        }
        for &pointer in change.released() {
            let extent = pointer.extent_len(version_of(self.files.get(&pointer.file())));
            let counted = self
                .accounts
                .entry(pointer.file())
                .or_insert(Account::EMPTY)
                .release(extent, latest, self.applied);
            if !counted {
                first = first.and(miscounted(pointer, Miscount::CountIsZero));
            }
        }
        first
    }

    /// Ends recovery and gives the store, with the counts as they are now.
    /// The changes applied so far count as durable: the caller has them from
    /// what a crash left.
    ///
    /// Fails with [`ValueStoreError::LiveValuesInMissingFile`], and changes
    /// nothing, if a file the manifest does not name is left with live
    /// values. Otherwise `val/` is created if it is missing, its name is made
    /// durable, and every file in it with a value-log file's name that the
    /// manifest does not name is removed, as [`ValueStore::open`] describes.
    pub fn finish(self) -> Result<ValueStore, ValueStoreError> {
        for (&file, account) in &self.accounts {
            if account.values != 0 && !self.files.contains_key(&file) {
                return Err(ValueStoreError::LiveValuesInMissingFile {
                    file,
                    values: account.values,
                });
            }
        }
        let entries = list_or_create_dir(&*self.vfs, &self.dir, &FileNames::VALUE_LOG)?;
        let named = self.files.keys().copied().collect();
        remove_unnamed(&*self.vfs, &self.dir, &entries, &named, self.last_id)?;

        let mut accounts = self.accounts;
        let live = self
            .files
            .into_iter()
            .map(|(id, file)| {
                let recovered = accounts.remove(&id).unwrap_or(Account::EMPTY);
                let account = Account {
                    values: recovered.values,
                    bytes: recovered.bytes,
                    miscounted: recovered.miscounted,
                    ..Account::EMPTY
                };
                (id, Entry { file, account })
            })
            .collect();
        Ok(ValueStore::new(
            self.vfs,
            self.dir,
            false,
            live,
            self.last_id,
        ))
    }
}

fn version_of(file: Option<&Arc<ValueFile>>) -> FormatVersion {
    file.map_or(FormatVersion::CURRENT, |file| file.version())
}

fn miscounted(pointer: ValuePointer, why: Miscount) -> Result<(), ValueStoreError> {
    Err(ValueStoreError::Miscounted { pointer, why })
}

#[derive(Debug)]
struct Entry {
    file: Arc<ValueFile>,
    account: Account,
}

#[derive(Debug)]
struct State {
    live: BTreeMap<FileId, Entry>,
    dropped: BTreeMap<FileId, PathBuf>,
    last_id: Option<FileId>,
    applied: u64,
    durable: u64,
}

/// A file that could be cleaned, from [`ValueStore::cleanable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cleanable {
    /// The file.
    pub file: FileId,
    /// The bytes of the file after its header.
    pub len: u64,
    /// How many of those bytes belong to live values.
    pub live_bytes: u64,
}

impl Cleanable {
    fn dead_bytes(&self) -> u64 {
        self.len.saturating_sub(self.live_bytes)
    }

    fn is_deader_than(&self, other: &Self) -> bool {
        let share = |file: &Self, of: &Self| u128::from(file.dead_bytes()) * u128::from(of.len);
        share(self, other) > share(other, self)
    }
}

/// The value log of one database directory.
///
/// It appends values to the files this session creates, reads a value by
/// its pointer, keeps each file's count of live values, cleans a file by
/// moving its live values, and deletes a file once nothing can reach it. See
/// [the module documentation](super) for the life of a file, for the counts
/// and for what the caller does.
///
/// Every method takes `&self` and may be called from any thread. Reads do
/// not wait for appends or syncs.
#[derive(Debug)]
pub struct ValueStore {
    vfs: Arc<dyn Vfs>,
    dir: PathBuf,
    read_only: bool,
    target_file_len: u64,
    state: RwLock<State>,
}

impl ValueStore {
    /// Opens the value-log files named by `manifest`, in its directory, for
    /// reading and writing, with `counts` as the live values of each.
    ///
    /// `counts` must be the counts of the page state the database is opened
    /// at: the ones [`Self::counts`] gave when those pages were stored. A
    /// file with no entry holds no live value, and is deleted as soon as
    /// [`Self::drop_empty`] is called. A new database has no file and no
    /// count. When changes have to be replayed on top of `counts`, use
    /// [`Self::recover`].
    ///
    /// Each named file must exist, be at least as long as its valid length,
    /// and start with the header of a value-log file of that id. A file an
    /// earlier session left open is closed at its valid length: this session
    /// does not append to it, and values past that length are not read.
    ///
    /// Then every file in `val/` with a value-log file's name that the
    /// manifest does not name is removed: a crash can leave one that was
    /// created and never named, or one that was dropped and not yet deleted.
    /// If such a file has an id above the last id in the manifest and holds
    /// more than a file header, the open fails with
    /// [`ValueStoreError::UnnamedFileHoldsValues`] and removes nothing. Other
    /// entries of `val/` are left alone.
    ///
    /// `val/` is created if it is missing, and its name is made durable
    /// either way.
    pub fn open(manifest: &ManifestStore, counts: &[FileCount]) -> Result<Self, ValueStoreError> {
        Self::recover(manifest, counts)?.finish()
    }

    /// Starts the recovery of the value log: opens the files named by
    /// `manifest`, as [`Self::open`] does, and takes `counts` as the live
    /// values at the checkpoint of the pages that recovery starts from.
    ///
    /// `counts` may name a file the manifest does not: see
    /// [`ValueRecovery::apply`].
    pub fn recover(
        manifest: &ManifestStore,
        counts: &[FileCount],
    ) -> Result<ValueRecovery, ValueStoreError> {
        let current = manifest.manifest()?;
        let vfs = manifest.vfs().clone();
        let files = load(&*vfs, manifest.dir(), current)?;
        Ok(ValueRecovery {
            vfs,
            dir: manifest.dir().to_path_buf(),
            files,
            accounts: counts
                .iter()
                .map(|count| (count.file, Account::restored(count)))
                .collect(),
            last_id: current.last_file_ids().value_log,
            applied: 0,
        })
    }

    /// Opens the value-log files named by `manifest` in the database
    /// directory `dir`, for reading only. It checks the files as
    /// [`Self::open`] does and changes nothing in the directory. It keeps no
    /// counts.
    pub fn open_read_only(
        vfs: Arc<dyn Vfs>,
        dir: &Path,
        manifest: &Manifest,
    ) -> Result<Self, ValueStoreError> {
        let live = load(&*vfs, dir, manifest)?
            .into_iter()
            .map(|(id, file)| {
                let account = Account::EMPTY;
                (id, Entry { file, account })
            })
            .collect();
        let last_id = manifest.last_file_ids().value_log;
        Ok(Self::new(vfs, dir.to_path_buf(), true, live, last_id))
    }

    fn new(
        vfs: Arc<dyn Vfs>,
        dir: PathBuf,
        read_only: bool,
        live: BTreeMap<FileId, Entry>,
        last_id: Option<FileId>,
    ) -> Self {
        Self {
            vfs,
            dir,
            read_only,
            target_file_len: DEFAULT_TARGET_FILE_LEN,
            state: RwLock::new(State {
                live,
                dropped: BTreeMap::new(),
                last_id,
                applied: 0,
                durable: 0,
            }),
        }
    }

    /// Sets the length at which a writer's file is full
    /// ([`ValueWriter::is_full`]), for the writers created from now on.
    pub fn with_target_file_len(mut self, len: u64) -> Self {
        self.target_file_len = len;
        self
    }

    fn state(&self) -> RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn state_mut(&self) -> RwLockWriteGuard<'_, State> {
        self.state.write().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn file(&self, file: FileId) -> Result<Arc<ValueFile>, ValueStoreError> {
        self.state()
            .live
            .get(&file)
            .map(|entry| entry.file.clone())
            .ok_or(ValueStoreError::UnknownFile { file })
    }

    fn live(&self) -> Vec<Arc<ValueFile>> {
        let state = self.state();
        state
            .live
            .values()
            .map(|entry| entry.file.clone())
            .collect()
    }

    pub(super) fn check_writable(&self) -> Result<(), ValueStoreError> {
        if self.read_only {
            Err(ValueStoreError::ReadOnly)
        } else {
            Ok(())
        }
    }

    pub(super) fn check_same_dir(&self, manifest: &ManifestStore) -> Result<(), ValueStoreError> {
        if manifest.dir() == self.dir {
            Ok(())
        } else {
            Err(ValueStoreError::WrongDirectory {
                expected: self.dir.clone(),
                found: manifest.dir().to_path_buf(),
            })
        }
    }

    /// The database directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Creates the next value-log file and returns its writer.
    ///
    /// The file is created under an id above every id used so far, with its
    /// header, and both are durable when this returns. The file takes no
    /// value until a manifest names it: replace the manifest
    /// ([`Self::commit`]) before the first [`ValueWriter::append`].
    pub fn create(&self) -> Result<ValueWriter, ValueStoreError> {
        self.check_writable()?;
        let id = {
            let mut state = self.state_mut();
            let next = state.last_id.map_or(1, |last| last.get().wrapping_add(1));
            let id = FileId::new(next).ok_or(ValueStoreError::FileIdsExhausted)?;
            state.last_id = Some(id);
            id
        };
        let appender = create_file(&*self.vfs, &self.dir, &FileNames::VALUE_LOG, id)?;
        let path = FileNames::VALUE_LOG.path(&self.dir, id);
        let reader = match self.vfs.open(&path, OpenMode::Read) {
            Ok(reader) => reader,
            Err(error) => {
                drop(appender);
                let _ = self.vfs.remove_file(&path);
                return Err(ValueStoreError::io("open", &path)(error));
            }
        };
        let file = Arc::new(ValueFile::created(id, path, reader));
        let entry = Entry {
            file: file.clone(),
            account: Account::EMPTY,
        };
        self.state_mut().live.insert(id, entry);
        Ok(ValueWriter::new(file, appender, self.target_file_len))
    }

    /// Makes durable every value appended so far to the files this session
    /// is writing.
    ///
    /// It may run while writers append. A value is covered if its append
    /// returned before this was called.
    ///
    /// Every file is synced, whatever happens to another one, and the first
    /// error is returned. A file whose sync has failed keeps failing this
    /// call until its writer is dropped.
    pub fn sync(&self) -> Result<(), ValueStoreError> {
        let mut result = Ok(());
        for file in self.live() {
            result = result.and(file.sync().map(drop));
        }
        result
    }

    /// Reads the value `pointer` names into `buf` and returns it, with every
    /// record of it verified, for a cell of the page `owner` that holds the
    /// pointer.
    ///
    /// It is one positional read of the bytes the value's records take,
    /// whether the value is one record or several. `buf` is cleared first and
    /// can be reused from one read to the next.
    ///
    /// The value must lie inside its file, have the pointer's length and
    /// name `owner` as its owner. A pointer that leads to another value, or
    /// that a cell carried to another page without its value, is an error:
    /// [`ValueLogError::WrongOwner`] inside [`ValueStoreError::Format`].
    ///
    /// [`ValueLogError::WrongOwner`]: lignin_format::value_log::ValueLogError::WrongOwner
    pub fn read<'a>(
        &self,
        pointer: ValuePointer,
        owner: PageId,
        buf: &'a mut Vec<u8>,
    ) -> Result<Value<'a>, ValueStoreError> {
        self.file(pointer.file())?.read(pointer, owner, buf)
    }

    /// Starts a walk over the values of the file `file`, from its first
    /// value to the end of its values as it is now. Values appended later
    /// are not part of it.
    pub fn walk(&self, file: FileId) -> Result<ValueWalk, ValueStoreError> {
        Ok(ValueWalk::new(self.file(file)?))
    }

    /// Applies one change point's change to the counts: one more live value
    /// in its file for each pointer a cell starts to hold, one fewer for each
    /// pointer a cell stops holding.
    ///
    /// `latest` is the latest commit version from this change point on: the
    /// version of the commit that makes the change, or, for a page step of
    /// background work, the latest version when the step is taken. A file
    /// whose count this brings to zero became empty at `latest`
    /// ([`Self::drop_empty`]).
    ///
    /// Call it at the change point, as one unit with the cell change, and
    /// after the change is in whatever makes the pages durable. It touches no
    /// file.
    ///
    /// The whole change is applied whatever this returns, so a commit that is
    /// already in the log goes on. The error is the first pointer that could
    /// not be counted as reported ([`ValueStoreError::Miscounted`]): its file
    /// is not in the store, it ends past the end of its file, or its file's
    /// count was already zero. In the last case the count is known to be too
    /// low, and the file is kept from then on whatever its count says.
    pub fn apply(
        &self,
        change: &ValueChange,
        latest: CommitVersion,
    ) -> Result<(), ValueStoreError> {
        self.check_writable()?;
        let mut state = self.state_mut();
        state.applied += 1;
        let applied = state.applied;
        let mut first = Ok(());
        for &pointer in change.acquired() {
            let Some(entry) = state.live.get_mut(&pointer.file()) else {
                first = first.and(miscounted(pointer, Miscount::UnknownFile));
                continue;
            };
            let extent = pointer.extent_len(entry.file.version());
            entry.account.acquire(extent, applied);
            if !entry.file.contains(pointer) {
                first = first.and(miscounted(pointer, Miscount::OutOfBounds));
            }
        }
        for &pointer in change.released() {
            let Some(entry) = state.live.get_mut(&pointer.file()) else {
                first = first.and(miscounted(pointer, Miscount::UnknownFile));
                continue;
            };
            let extent = pointer.extent_len(entry.file.version());
            if !entry.account.release(extent, latest, applied) {
                first = first.and(miscounted(pointer, Miscount::CountIsZero));
            }
        }
        first
    }

    /// The place of the last change applied to the counts.
    pub fn applied(&self) -> ChangeMark {
        ChangeMark(self.state().applied)
    }

    /// Tells the store that every change applied up to `mark` is durable: a
    /// crash from now on recovers pages and counts that include it.
    ///
    /// A file is deleted only once the changes that emptied it are durable,
    /// since a crash would otherwise bring back pages that point into a file
    /// that is gone. So take the mark with [`Self::applied`], make durable
    /// what the changes up to it were stored in, and then call this.
    pub fn mark_durable(&self, mark: ChangeMark) {
        let mut state = self.state_mut();
        state.durable = state.durable.max(mark.0.min(state.applied));
    }

    /// The live values of each file, as of the last change applied. A file
    /// that is not listed holds no live value.
    ///
    /// A checkpoint of the pages stores this next to the pages it was taken
    /// with, and [`Self::open`] or [`Self::recover`] takes it back.
    pub fn counts(&self) -> Vec<FileCount> {
        let state = self.state();
        state
            .live
            .iter()
            .filter(|(_, entry)| entry.account.is_counted())
            .map(|(&file, entry)| entry.account.count(file))
            .collect()
    }

    /// The live values of the file `file`, or `None` if the store does not
    /// hold it.
    pub fn count(&self, file: FileId) -> Option<FileCount> {
        let state = self.state();
        state.live.get(&file).map(|entry| entry.account.count(file))
    }

    /// The closed files that hold live values and of whose bytes at least
    /// `min_dead_percent` in a hundred belong to no live value, the one with
    /// the largest dead share first.
    ///
    /// These are the files worth cleaning ([`Self::clean`]). A file with no
    /// live value needs none: [`Self::drop_empty`] takes it. A file is left
    /// out while cleaning it could not help: its count is known to be wrong,
    /// or the last pass over it found no owner for the values that are still
    /// counted and no change has touched its count since.
    pub fn cleanable(&self, min_dead_percent: u8) -> Vec<Cleanable> {
        let state = self.state();
        let mut files: Vec<_> = state
            .live
            .iter()
            .filter(|(_, entry)| {
                let account = &entry.account;
                let settled = account.cleaned.is_some_and(|at| account.changed <= at);
                account.values != 0 && !account.miscounted && !settled && entry.file.is_closed()
            })
            .map(|(&file, entry)| Cleanable {
                file,
                len: entry.file.end() - FIRST_RECORD_OFFSET,
                live_bytes: entry.account.bytes,
            })
            .filter(|file| {
                u128::from(file.dead_bytes()) * 100
                    >= u128::from(file.len) * u128::from(min_dead_percent)
            })
            .collect();
        files.sort_by(|a, b| match (a.is_deader_than(b), b.is_deader_than(a)) {
            (true, _) => Ordering::Less,
            (_, true) => Ordering::Greater,
            _ => a.file.cmp(&b.file),
        });
        files
    }

    pub(super) fn cleaned(&self, file: FileId, conclusive: bool) -> u64 {
        let mut state = self.state_mut();
        let applied = state.applied;
        let Some(entry) = state.live.get_mut(&file) else {
            return 0;
        };
        if conclusive && entry.account.values != 0 {
            entry.account.cleaned = Some(applied);
        }
        entry.account.values
    }

    /// Takes out of the store every file that nothing can reach any more,
    /// and returns their ids. Such a file is no longer read, and
    /// [`Self::describe`] no longer lists it. Its file is deleted by
    /// [`Self::manifest_replaced`], once the durable manifest does not name
    /// it.
    ///
    /// `oldest_pin` is the oldest commit version that a snapshot, a cursor, a
    /// write transaction or a checkpoint being created pins, or the latest
    /// commit version if nothing is pinned.
    ///
    /// A file is taken out only if all of this holds:
    ///
    /// - it is closed: no writer appends to it
    /// - its count of live values is zero, and is not known to be wrong
    /// - no [`ValueHold`](super::ValueHold) is left on it
    /// - the change that brought its count to zero is durable
    ///   ([`Self::mark_durable`])
    /// - it became empty at a version no later than `oldest_pin`, so no pin
    ///   is on a version in which a cell still pointed into it
    ///
    /// The caller also waits until every operation that was running when the
    /// file became empty has ended: one of them may have read a pointer into
    /// the file and not yet followed it.
    pub fn drop_empty(&self, oldest_pin: CommitVersion) -> Result<Vec<FileId>, ValueStoreError> {
        self.check_writable()?;
        let mut state = self.state_mut();
        let durable = state.durable;
        let unreachable: Vec<FileId> = state
            .live
            .iter()
            .filter(|(_, entry)| {
                let account = &entry.account;
                !account.is_counted()
                    && account.emptied.change <= durable
                    && account.emptied.version <= oldest_pin
                    && !entry.file.is_held()
                    && entry.file.is_closed()
            })
            .map(|(&id, _)| id)
            .collect();
        for id in &unreachable {
            if let Some(entry) = state.live.remove(id) {
                state.dropped.insert(*id, entry.file.path().to_path_buf());
            }
        }
        Ok(unreachable)
    }

    /// The files in the store, as a manifest written now would name them:
    /// each with its state and the length of it that is durable.
    pub fn files_named(&self) -> Result<Vec<LiveFile>, ValueStoreError> {
        self.live().iter().map(|file| file.live_file()).collect()
    }

    /// The manifest `base` with its value-log files replaced by the ones in
    /// the store, and its last value-log file id raised to the last one used
    /// here.
    ///
    /// A file being written is named open, with the length that was durable
    /// when this was called. Everything else in `base` is kept.
    pub fn describe(&self, base: &Manifest) -> Result<Manifest, ValueStoreError> {
        let mut files: Vec<_> = base
            .files()
            .iter()
            .filter(|file| file.kind() != FileKind::ValueLog)
            .copied()
            .collect();
        let (live, last_id) = {
            let state = self.state();
            let live: Vec<_> = state
                .live
                .values()
                .map(|entry| entry.file.clone())
                .collect();
            (live, state.last_id)
        };
        for file in live {
            files.push(file.live_file()?);
        }
        let mut last_file_ids = base.last_file_ids();
        last_file_ids.value_log = last_file_ids.value_log.max(last_id);
        Manifest::new(last_file_ids, base.table_checkpoint(), files).map_err(
            ValueStoreError::format(&self.dir.join(ManifestStore::FILE_NAME), 0),
        )
    }

    pub(super) fn name_files(&self, manifest: &Manifest) -> BTreeSet<FileId> {
        let named = named_files(manifest);
        for file in self.live() {
            if named.contains(&file.id()) {
                file.name();
            }
        }
        named
    }

    /// Tells the store that the manifest was replaced. Call it after every
    /// [`ManifestStore::update`] that returned `Ok`.
    ///
    /// A new file that the durable manifest names takes values from now on.
    /// The file of a dropped value-log file that the durable manifest no
    /// longer names is deleted.
    ///
    /// A file that cannot be deleted does not stop the rest: the call ends
    /// with [`ValueStoreError::NotDeleted`], and the next call tries the
    /// deletion again.
    pub fn manifest_replaced(&self, manifest: &ManifestStore) -> Result<(), ValueStoreError> {
        self.check_writable()?;
        self.check_same_dir(manifest)?;
        let named = self.name_files(manifest.manifest()?);

        let unnamed = {
            let mut state = self.state_mut();
            let dropped = std::mem::take(&mut state.dropped);
            let (kept, unnamed): (BTreeMap<_, _>, BTreeMap<_, _>) =
                dropped.into_iter().partition(|(id, _)| named.contains(id));
            state.dropped = kept;
            unnamed
        };
        let mut result = Ok(());
        for (id, path) in unnamed {
            if let Err(source) = remove(&*self.vfs, &path) {
                self.state_mut().dropped.insert(id, path.clone());
                result = result.and(Err(ValueStoreError::NotDeleted { path, source }));
            }
        }
        result
    }

    /// Replaces the manifest with one that names the store's files as they
    /// are now: [`ManifestStore::update`] with [`Self::describe`], then
    /// [`Self::manifest_replaced`].
    ///
    /// After [`ValueStoreError::NotDeleted`] the new manifest is in force.
    /// After any other error [`ManifestStore::manifest`] tells where things
    /// stand: it gives the old manifest, and this call can be made again, or
    /// it fails with `ManifestInDoubt`, and the directory has the old
    /// manifest or the new one.
    pub fn commit(&self, manifest: &mut ManifestStore) -> Result<(), ValueStoreError> {
        self.check_writable()?;
        self.check_same_dir(manifest)?;
        manifest.update(|current| self.describe(current))?;
        self.manifest_replaced(manifest)
    }
}
