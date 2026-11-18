//! The value-log files of one database directory: their lives from creation
//! to deletion, reading a value, and the counts of live values.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
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
    writer::{ValueHold, ValueWriter},
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

/// A change as it is counted: the pointers cells stop holding and the
/// pointers cells start to hold, each in the order the change lists them,
/// without the ones the change both releases and acquires.
///
/// The releases come first because they are about cells as they were before
/// the change: a release that the count before the change does not include
/// shows the count was too low, whatever the change acquires in the same
/// file.
fn net(change: &ValueChange) -> (Vec<ValuePointer>, Vec<ValuePointer>) {
    if change.acquired().is_empty() || change.released().is_empty() {
        return (change.released().to_vec(), change.acquired().to_vec());
    }
    let mut acquired: HashMap<ValuePointer, usize> = HashMap::new();
    for &pointer in change.acquired() {
        *acquired.entry(pointer).or_default() += 1;
    }
    let mut both: HashMap<ValuePointer, usize> = HashMap::new();
    let mut released = Vec::new();
    for &pointer in change.released() {
        match acquired.get_mut(&pointer) {
            Some(left) if *left != 0 => {
                *left -= 1;
                *both.entry(pointer).or_default() += 1;
            }
            _ => released.push(pointer),
        }
    }
    let mut kept = Vec::new();
    for &pointer in change.acquired() {
        match both.get_mut(&pointer) {
            Some(left) if *left != 0 => *left -= 1,
            _ => kept.push(pointer),
        }
    }
    (released, kept)
}

/// The value log of a database that is being recovered: the files a manifest
/// names, and the counts of live values as recovery brings them up to the
/// version it lands on.
///
/// [`ValueStore::recover`] starts it from a sample of the counts.
/// [`Self::apply`] takes the change of every record that recovery keeps
/// after the sample's place in the log, in the order of the log and each
/// once, whether or not the pages recovery starts from already hold the
/// cell change. [`Self::finish`] gives the store.
///
/// Until [`Self::finish`], nothing in the directory is changed.
#[derive(Debug)]
pub struct ValueRecovery {
    vfs: Arc<dyn Vfs>,
    dir: PathBuf,
    files: BTreeMap<FileId, Arc<ValueFile>>,
    accounts: BTreeMap<FileId, Account>,
    last_id: Option<FileId>,
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
    /// ([`ValueStoreError::Miscounted`]): a release that the count or the
    /// bytes of its file did not include, or a pointer that ends past the
    /// valid length of its file, which no manifest covered.
    ///
    /// No commit version is asked for: no pin is left from before a crash,
    /// so a file that recovery leaves empty is empty for every version.
    pub fn apply(&mut self, change: &ValueChange) -> Result<(), ValueStoreError> {
        let (released, acquired) = net(change);
        let mut first = Ok(());
        for pointer in released {
            let extent = pointer.extent_len(version_of(self.files.get(&pointer.file())));
            let released = self
                .accounts
                .entry(pointer.file())
                .or_insert(Account::EMPTY)
                .release(extent, CommitVersion::EMPTY, RECOVERED);
            first = first.and(released.map_err(|why| miscount(pointer, why)));
        }
        for pointer in acquired {
            let file = self.files.get(&pointer.file());
            let extent = pointer.extent_len(version_of(file));
            self.accounts
                .entry(pointer.file())
                .or_insert(Account::EMPTY)
                .acquire(extent, RECOVERED);
            if file.is_some_and(|file| !file.contains(pointer)) {
                first = first.and(miscounted(pointer, Miscount::OutOfBounds));
            }
        }
        first
    }

    /// Ends recovery and gives the store, with the counts as they are now.
    ///
    /// Nothing recovery was given counts as durable yet: a process that was
    /// killed leaves a log that reads back whole and that a loss of power
    /// can still cut short. So no file that these counts leave empty is
    /// dropped until the caller has made durable what it recovered from and
    /// has said so, with [`ValueStore::mark_durable`] of
    /// [`ValueStore::applied`].
    ///
    /// Fails, and changes nothing, if the counts speak of a file that the
    /// manifest does not name and that must not be lost:
    ///
    /// - with [`ValueStoreError::LiveValuesInMissingFile`], if such a file is
    ///   left with live values
    /// - with [`ValueStoreError::MiscountedFileNotNamed`], if the count of
    ///   such a file was found to be too low and the file is still in `val/`,
    ///   where the next step would remove it
    ///
    /// Otherwise `val/` is created if it is missing, its name is made
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
        for entry in &entries {
            let unnamed = FileNames::VALUE_LOG
                .parse(&entry.name)
                .filter(|id| !self.files.contains_key(id));
            let miscounted = |file: &FileId| {
                let gone = self.accounts.get(file);
                gone.is_some_and(|gone| gone.miscounted)
            };
            if let Some(file) = unnamed.filter(miscounted) {
                return Err(ValueStoreError::MiscountedFileNotNamed { file });
            }
        }
        let named = self.files.keys().copied().collect();
        remove_unnamed(&*self.vfs, &self.dir, &entries, &named, self.last_id)?;

        let mut accounts = self.accounts;
        let counted = self
            .files
            .iter()
            .map(|(&id, file)| {
                let recovered = accounts.remove(&id).unwrap_or(Account::EMPTY);
                let counted = Counted {
                    file: file.clone(),
                    account: recovered.handed_over(RECOVERED),
                };
                (id, counted)
            })
            .collect();
        Ok(ValueStore::new(
            self.vfs,
            self.dir,
            false,
            self.files,
            counted,
            self.last_id,
            RECOVERED,
        ))
    }
}

/// The place, among the changes of a session, of everything recovery gave
/// it.
const RECOVERED: u64 = 1;

static SESSIONS: AtomicU64 = AtomicU64::new(1);

fn version_of(file: Option<&Arc<ValueFile>>) -> FormatVersion {
    file.map_or(FormatVersion::CURRENT, |file| file.version())
}

fn miscount(pointer: ValuePointer, why: Miscount) -> ValueStoreError {
    ValueStoreError::Miscounted { pointer, why }
}

fn miscounted(pointer: ValuePointer, why: Miscount) -> Result<(), ValueStoreError> {
    Err(miscount(pointer, why))
}

/// The files that are in the store no longer, or not yet for a manifest.
/// Whoever changes which files the store has holds this.
#[derive(Debug)]
struct FileSet {
    /// Files taken out of the store, until they are deleted.
    dropped: BTreeMap<FileId, Arc<ValueFile>>,
    /// The highest id of a file the store has had: what a manifest records.
    last_id: Option<FileId>,
}

#[derive(Debug)]
struct Counted {
    file: Arc<ValueFile>,
    account: Account,
}

/// The counts. The store's locks are taken in the order: the lock of
/// [`ValueStore::create`], counts, file set, file map. A file's own locks
/// are taken with none of those held. While the store holds this one for
/// itself it touches no file and calls nothing, and only
/// [`ValueStore::drop_empty`] takes another lock under it.
#[derive(Debug)]
struct Counts {
    accounts: BTreeMap<FileId, Counted>,
    applied: u64,
    durable: u64,
}

impl Counts {
    fn snapshot(&self) -> Vec<FileCount> {
        self.accounts
            .iter()
            .filter(|(_, counted)| counted.account.is_counted())
            .map(|(&file, counted)| counted.account.count(file))
            .collect()
    }

    fn apply(
        &mut self,
        change: &ValueChange,
        latest: CommitVersion,
    ) -> Result<(), ValueStoreError> {
        let (released, acquired) = net(change);
        self.applied += 1;
        let applied = self.applied;
        let mut first = Ok(());
        for pointer in released {
            let Some(counted) = self.accounts.get_mut(&pointer.file()) else {
                first = first.and(miscounted(pointer, Miscount::UnknownFile));
                continue;
            };
            let extent = pointer.extent_len(counted.file.version());
            let released = counted.account.release(extent, latest, applied);
            first = first.and(released.map_err(|why| miscount(pointer, why)));
        }
        for pointer in acquired {
            let Some(counted) = self.accounts.get_mut(&pointer.file()) else {
                first = first.and(miscounted(pointer, Miscount::UnknownFile));
                continue;
            };
            let extent = pointer.extent_len(counted.file.version());
            counted.account.acquire(extent, applied);
            if !counted.file.contains(pointer) {
                first = first.and(miscounted(pointer, Miscount::OutOfBounds));
            } else if !counted.file.covers(pointer) {
                first = first.and(miscounted(pointer, Miscount::NotCovered));
            }
        }
        first
    }
}

/// The store's lock on the counts, from [`ValueStore::step`]. While it is
/// held no change is counted, so the counts stand still.
///
/// It is taken for one of two things:
///
/// - **A change point.** Whoever makes the change takes the step, writes
///   the change to the log, makes the cell change, and applies the change
///   with [`Self::apply`]. A page step takes it before its own lock on the
///   page, so that nothing is acquired under the page's lock. Because the
///   record is written while the step is held, the changes are in the log
///   in the order the counts took them.
/// - **A sample of the counts for a checkpoint.** [`Self::counts`] gives the
///   counts, and the end of the log, read while the step is held, is their
///   place: the counts include every change that is in the log before it
///   and none after it.
///
/// While it is held, call no method of the store that reads or changes the
/// counts: it would wait for this step. Those are [`ValueStore::apply`],
/// [`ValueStore::step`], [`ValueStore::applied`],
/// [`ValueStore::mark_durable`], [`ValueStore::counts`],
/// [`ValueStore::count`], [`ValueStore::cleanable`], [`ValueStore::retire`],
/// [`ValueStore::drop_empty`], [`ValueStore::create`], which gives the new
/// file its count, and the cleaning calls. Every other method can be
/// called, [`ValueStore::hold_files`] among them, so a checkpoint may take
/// its hold on the files and its sample of the counts in either order.
#[derive(Debug)]
pub struct CountStep<'a> {
    counts: MutexGuard<'a, Counts>,
}

impl CountStep<'_> {
    /// The live values of each file, as [`ValueStore::counts`] gives them.
    pub fn counts(&self) -> Vec<FileCount> {
        self.counts.snapshot()
    }

    /// Applies the change point's change to the counts, as
    /// [`ValueStore::apply`] does, and gives the lock back.
    pub fn apply(
        mut self,
        change: &ValueChange,
        latest: CommitVersion,
    ) -> Result<(), ValueStoreError> {
        self.counts.apply(change, latest)
    }
}

/// What [`ValueStore::drop_empty`] may take, once every operation that was
/// running when this was made has ended: from [`ValueStore::retire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retired {
    session: u64,
    change: u64,
    oldest_pin: CommitVersion,
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
}

/// The value log of one database directory.
///
/// It appends values to the files this session creates, reads a value by
/// its pointer, keeps each file's count of live values, cleans a file by
/// moving its live values, and deletes a file once nothing can reach it. See
/// [the module documentation](super) for the life of a file, for the counts
/// and for what the caller does.
///
/// Every method takes `&self` and may be called from any thread. A read
/// waits for no append, no sync and no change to the counts, and a change to
/// the counts waits for no file operation.
#[derive(Debug)]
pub struct ValueStore {
    vfs: Arc<dyn Vfs>,
    dir: PathBuf,
    read_only: bool,
    target_file_len: u64,
    /// Tells this store's writers, copies and marks from another store's.
    session: u64,
    /// The highest id a creation has taken. It is held for the length of a
    /// creation, so that files enter the store in the order of their ids.
    creating: Mutex<Option<FileId>>,
    /// The files the store reads. A reader takes this, for one lookup, and
    /// nothing else, and whoever changes it holds it for one entry.
    live: RwLock<BTreeMap<FileId, Arc<ValueFile>>>,
    set: Mutex<FileSet>,
    counts: Mutex<Counts>,
}

impl ValueStore {
    /// Opens the value-log files named by `manifest`, in its directory, for
    /// reading and writing, with `counts` as the live values of each.
    ///
    /// `counts` must be the counts of the page state the database is opened
    /// at: the ones [`Self::counts`] gave when those pages were stored. A
    /// file with no entry holds no live value, and [`Self::drop_empty`] takes
    /// it once the caller has said that what it opened from is durable
    /// ([`ValueRecovery::finish`]). A new database has no file and no count.
    /// When changes have to be replayed on top of `counts`, use
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
    /// `manifest`, as [`Self::open`] does, and takes `counts` as a sample of
    /// the live values: every change before the sample's place in the log
    /// is in it, and recovery applies every change after it.
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
                .map(|count| (count.file, Account::from_count(count)))
                .collect(),
            last_id: current.last_file_ids().value_log,
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
        let files = load(&*vfs, dir, manifest)?;
        let last_id = manifest.last_file_ids().value_log;
        let counted = BTreeMap::new();
        Ok(Self::new(
            vfs,
            dir.to_path_buf(),
            true,
            files,
            counted,
            last_id,
            0,
        ))
    }

    fn new(
        vfs: Arc<dyn Vfs>,
        dir: PathBuf,
        read_only: bool,
        live: BTreeMap<FileId, Arc<ValueFile>>,
        accounts: BTreeMap<FileId, Counted>,
        last_id: Option<FileId>,
        applied: u64,
    ) -> Self {
        Self {
            vfs,
            dir,
            read_only,
            target_file_len: DEFAULT_TARGET_FILE_LEN,
            session: SESSIONS.fetch_add(1, Ordering::Relaxed),
            creating: Mutex::new(last_id),
            live: RwLock::new(live),
            set: Mutex::new(FileSet {
                dropped: BTreeMap::new(),
                last_id,
            }),
            counts: Mutex::new(Counts {
                accounts,
                applied,
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

    fn live(&self) -> RwLockReadGuard<'_, BTreeMap<FileId, Arc<ValueFile>>> {
        self.live.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn live_mut(&self) -> RwLockWriteGuard<'_, BTreeMap<FileId, Arc<ValueFile>>> {
        self.live.write().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_mut(&self) -> MutexGuard<'_, FileSet> {
        self.set.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn counts_mut(&self) -> MutexGuard<'_, Counts> {
        self.counts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn file(&self, file: FileId) -> Result<Arc<ValueFile>, ValueStoreError> {
        self.live()
            .get(&file)
            .cloned()
            .ok_or(ValueStoreError::UnknownFile { file })
    }

    fn files(&self) -> Vec<Arc<ValueFile>> {
        self.live().values().cloned().collect()
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

    /// Fails unless `session` is this store's: a writer, a set of copies or a
    /// mark of another store, or of an earlier session, means nothing here.
    pub(super) fn check_own(&self, session: u64) -> Result<(), ValueStoreError> {
        if session == self.session {
            Ok(())
        } else {
            Err(ValueStoreError::WrongStore)
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
    ///
    /// One file is created at a time. A manifest records an id as used only
    /// once the file is in the store, so that it never records an id above
    /// that of a file it has yet to name.
    pub fn create(&self) -> Result<ValueWriter, ValueStoreError> {
        self.check_writable()?;
        let mut taken = self.creating.lock().unwrap_or_else(PoisonError::into_inner);
        let next = taken.map_or(1, |last| last.get().wrapping_add(1));
        let id = FileId::new(next).ok_or(ValueStoreError::FileIdsExhausted)?;
        *taken = Some(id);

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
        let counted = Counted {
            file: file.clone(),
            account: Account::EMPTY,
        };
        self.counts_mut().accounts.insert(id, counted);
        let mut set = self.set_mut();
        self.live_mut().insert(id, file.clone());
        set.last_id = Some(id);
        drop(set);
        Ok(ValueWriter::new(
            file,
            appender,
            self.target_file_len,
            self.session,
        ))
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
    ///
    /// This and [`ValueWriter::sync`] are the only syncs of a file that is
    /// being written. A sync of it through any other handle that failed
    /// would leave values that a later sync here reports durable and that
    /// are not.
    pub fn sync(&self) -> Result<(), ValueStoreError> {
        let mut result = Ok(());
        for file in self.files() {
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

    /// Whether the durable manifest covers the value `pointer` names: it
    /// gives the value's file a valid length at or past the value's end, so
    /// a later session reads the value.
    ///
    /// It speaks for the manifest the store was last told of
    /// ([`Self::manifest_replaced`]). A pointer is stored where a later
    /// session will find it only once this holds: check it for every pointer
    /// a change acquires before the change is written to the log.
    pub fn covers(&self, pointer: ValuePointer) -> bool {
        let live = self.live();
        live.get(&pointer.file())
            .is_some_and(|file| file.covers(pointer))
    }

    /// Starts a walk over the values of the file `file`, from its first
    /// value to the end of its values as it is now. Values appended later
    /// are not part of it.
    pub fn walk(&self, file: FileId) -> Result<ValueWalk, ValueStoreError> {
        Ok(ValueWalk::new(self.file(file)?))
    }

    /// Applies one change point's change to the counts: one fewer live value
    /// in its file for each pointer a cell stops holding, one more for each
    /// pointer a cell starts to hold. A pointer the change both releases and
    /// acquires counts for nothing.
    ///
    /// `latest` is the latest commit version from this change point on: the
    /// version of the commit that makes the change, or, for a page step of
    /// background work, the latest version when the step is taken. A file
    /// whose count this brings to zero became empty at `latest`
    /// ([`Self::drop_empty`]).
    ///
    /// Call it at the change point, as one unit with the cell change, and
    /// after the change is in whatever makes the pages durable. It touches no
    /// file. It takes the store's lock on the counts, so call it while no
    /// lock on a page is held: a commit applies its change where it installs
    /// its pages, outside any one page's step. A page step uses
    /// [`Self::step`].
    ///
    /// The whole change is applied whatever this returns, so a commit that is
    /// already in the log goes on. The error is the first pointer that could
    /// not be counted as reported ([`ValueStoreError::Miscounted`]): its file
    /// is not in the store, its file's count or bytes did not include it, it
    /// ends past the end of its file, or no durable manifest covers it
    /// ([`Self::covers`]). A release the counts did not include shows the
    /// count was too low, and the file is kept from then on whatever its
    /// count says.
    pub fn apply(
        &self,
        change: &ValueChange,
        latest: CommitVersion,
    ) -> Result<(), ValueStoreError> {
        self.step()?.apply(change, latest)
    }

    /// Takes the store's lock on the counts for one page step of the
    /// caller's that changes them: other background work that rewrites a
    /// value pointer takes it, then its lock on the page, installs the
    /// pointer and applies the change with [`CountStep::apply`].
    pub fn step(&self) -> Result<CountStep<'_>, ValueStoreError> {
        self.check_writable()?;
        Ok(self.count_step())
    }

    pub(super) fn count_step(&self) -> CountStep<'_> {
        CountStep {
            counts: self.counts_mut(),
        }
    }

    /// The place of the last change applied to the counts.
    ///
    /// It is taken to say later that everything up to here is durable
    /// ([`Self::mark_durable`]).
    pub fn applied(&self) -> ChangeMark {
        ChangeMark {
            session: self.session,
            change: self.applied_so_far(),
        }
    }

    /// Tells the store that every change applied up to `mark` is durable: a
    /// crash from now on recovers pages and counts that include it.
    ///
    /// A file is deleted only once the changes that emptied it are durable,
    /// since a crash would otherwise bring back pages that point into a file
    /// that is gone. So take the mark with [`Self::applied`], make durable
    /// what the changes up to it were stored in, and then call this.
    ///
    /// Fails with [`ValueStoreError::WrongStore`] for a mark of another
    /// store or of an earlier session.
    pub fn mark_durable(&self, mark: ChangeMark) -> Result<(), ValueStoreError> {
        self.check_own(mark.session)?;
        let mut counts = self.counts_mut();
        counts.durable = counts.durable.max(mark.change.min(counts.applied));
        Ok(())
    }

    /// The live values of each file, as of the last change applied. A file
    /// that is not listed holds no live value.
    ///
    /// [`Self::open`] or [`Self::recover`] takes it back. A sample that is
    /// stored for recovery needs its place in the log, and is taken with
    /// [`CountStep::counts`].
    pub fn counts(&self) -> Vec<FileCount> {
        self.counts_mut().snapshot()
    }

    /// The live values of the file `file`, or `None` if the store does not
    /// hold it.
    pub fn count(&self, file: FileId) -> Option<FileCount> {
        let counts = self.counts_mut();
        let counted = counts.accounts.get(&file);
        counted.map(|counted| counted.account.count(file))
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
        let closed: Vec<Cleanable> = {
            let counts = self.counts_mut();
            counts
                .accounts
                .iter()
                .filter(|(_, counted)| {
                    let account = &counted.account;
                    let settled = account.cleaned.is_some_and(|at| account.changed <= at);
                    account.values != 0
                        && !account.miscounted
                        && !settled
                        && counted.file.is_closed()
                })
                .map(|(&file, counted)| Cleanable {
                    file,
                    len: counted.file.end() - FIRST_RECORD_OFFSET,
                    live_bytes: counted.account.bytes,
                })
                .collect()
        };
        let mut files: Vec<Cleanable> = closed
            .into_iter()
            .filter(|file| {
                u128::from(file.dead_bytes()) * 100
                    >= u128::from(file.len) * u128::from(min_dead_percent)
            })
            .collect();
        files.sort_by(|a, b| {
            let a_share = u128::from(a.dead_bytes()) * u128::from(b.len);
            let b_share = u128::from(b.dead_bytes()) * u128::from(a.len);
            b_share.cmp(&a_share).then(a.file.cmp(&b.file))
        });
        files
    }

    pub(super) fn session(&self) -> u64 {
        self.session
    }

    pub(super) fn applied_so_far(&self) -> u64 {
        self.counts_mut().applied
    }

    /// Ends a pass over `file` that began when the changes had reached
    /// `began`. Returns the file's count, and whether the pass has found
    /// every owner there is to find: it was refused no rewrite, and no value
    /// of the file was newly counted while it ran.
    pub(super) fn end_pass(&self, file: FileId, began: u64, refused: bool) -> (u64, bool) {
        let mut counts = self.counts_mut();
        let applied = counts.applied;
        let Some(counted) = counts.accounts.get_mut(&file) else {
            return (0, true);
        };
        let account = &mut counted.account;
        let conclusive = !refused && account.acquired <= began;
        if conclusive && account.values != 0 {
            account.cleaned = Some(applied);
        }
        (account.values, conclusive)
    }

    /// Keeps `file` whatever its count says from now on.
    pub(super) fn mark_miscounted(&self, file: FileId) {
        if let Some(counted) = self.counts_mut().accounts.get_mut(&file) {
            counted.account.miscounted = true;
        }
    }

    /// A hold on every file the store has now, the ones it has dropped and
    /// not yet deleted included. None of them is dropped or deleted while
    /// the hold lives.
    ///
    /// A checkpoint takes it in its sampling step, with the manifest it
    /// samples, and drops it once it has linked the files (CK3 of
    /// `docs/lignindb/transactions.md`).
    pub fn hold_files(&self) -> ValueHold {
        let mut hold = ValueHold::new();
        let set = self.set_mut();
        for file in self.live().values().chain(set.dropped.values()) {
            hold.add(file);
        }
        hold
    }

    /// Says what [`Self::drop_empty`] may take later: the files that are
    /// empty now, for every version from `oldest_pin` on.
    ///
    /// `oldest_pin` is the oldest commit version that a snapshot, a cursor, a
    /// write transaction or a checkpoint being created pins now, or the
    /// latest commit version if nothing is pinned.
    ///
    /// The caller then waits until every operation that is running now has
    /// ended, and passes the result to [`Self::drop_empty`]. An operation
    /// that begins after this call reads no pointer into those files: no
    /// cell of the latest version holds one, and no version before
    /// `oldest_pin` can be pinned any more. A file that becomes empty later,
    /// or that only a later, higher `oldest_pin` lets go, waits for the next
    /// call.
    pub fn retire(&self, oldest_pin: CommitVersion) -> Retired {
        Retired {
            session: self.session,
            change: self.applied_so_far(),
            oldest_pin,
        }
    }

    /// Takes out of the store every file that nothing can reach any more,
    /// and returns their ids. Such a file is no longer read, and
    /// [`Self::describe`] no longer lists it. Its file is deleted by
    /// [`Self::manifest_replaced`], once the durable manifest does not name
    /// it.
    ///
    /// `retired` is what [`Self::retire`] gave, and every operation that was
    /// running then has ended.
    ///
    /// A file is taken out only if all of this holds:
    ///
    /// - it is closed: no writer appends to it
    /// - its count of live values is zero, and is not known to be wrong
    /// - no [`ValueHold`] is on it
    /// - the change that brought its count to zero is durable
    ///   ([`Self::mark_durable`])
    /// - that change had been applied when `retired` was made
    /// - it became empty at a version no later than the oldest version that
    ///   was pinned when `retired` was made, so no pin was on a version in
    ///   which a cell still pointed into it
    ///
    /// Fails with [`ValueStoreError::WrongStore`] for what another store, or
    /// an earlier session, retired.
    pub fn drop_empty(&self, retired: Retired) -> Result<Vec<FileId>, ValueStoreError> {
        self.check_writable()?;
        self.check_own(retired.session)?;
        let mut counts = self.counts_mut();
        let mut set = self.set_mut();
        let unreachable: Vec<Arc<ValueFile>> = {
            let through = counts.durable.min(retired.change);
            let ids: Vec<FileId> = counts
                .accounts
                .iter()
                .filter(|(_, counted)| {
                    let account = &counted.account;
                    !account.is_counted()
                        && account.emptied.change <= through
                        && account.emptied.version <= retired.oldest_pin
                        && counted.file.is_closed_and_free()
                })
                .map(|(&id, _)| id)
                .collect();
            ids.iter()
                .filter_map(|id| counts.accounts.remove(id))
                .map(|counted| counted.file)
                .collect()
        };
        drop(counts);
        for file in &unreachable {
            self.live_mut().remove(&file.id());
            set.dropped.insert(file.id(), file.clone());
        }
        Ok(unreachable.iter().map(|file| file.id()).collect())
    }

    /// The files in the store, as a manifest written now would name them:
    /// each with its state and the length of it that is durable.
    pub fn files_named(&self) -> Result<Vec<LiveFile>, ValueStoreError> {
        self.files().iter().map(|file| file.live_file()).collect()
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
            let set = self.set_mut();
            (self.files(), set.last_id)
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
        let named: BTreeMap<FileId, u64> = manifest
            .files()
            .iter()
            .filter(|file| file.kind() == FileKind::ValueLog)
            .map(|file| (file.id(), file.valid_len()))
            .collect();
        for file in self.files() {
            if let Some(&valid_len) = named.get(&file.id()) {
                file.name();
                file.cover(valid_len);
            }
        }
        named.into_keys().collect()
    }

    /// Tells the store that the manifest was replaced. Call it after every
    /// [`ManifestStore::update`] that returned `Ok`.
    ///
    /// A new file that the durable manifest names takes values from now on.
    /// The file of a dropped value-log file that the durable manifest no
    /// longer names is deleted, unless a [`ValueHold`] is on it: then a later
    /// call deletes it.
    ///
    /// A file that cannot be deleted does not stop the rest: the call ends
    /// with [`ValueStoreError::NotDeleted`], and the next call tries the
    /// deletion again.
    pub fn manifest_replaced(&self, manifest: &ManifestStore) -> Result<(), ValueStoreError> {
        self.check_writable()?;
        self.check_same_dir(manifest)?;
        let named = self.name_files(manifest.manifest()?);

        let unnamed: Vec<Arc<ValueFile>> = {
            let mut set = self.set_mut();
            let ids: Vec<FileId> = set
                .dropped
                .iter()
                .filter(|(id, file)| !named.contains(id) && !file.is_held())
                .map(|(&id, _)| id)
                .collect();
            ids.iter().filter_map(|id| set.dropped.remove(id)).collect()
        };
        let mut result = Ok(());
        for file in unnamed {
            if let Err(source) = remove(&*self.vfs, file.path()) {
                let path = file.path().to_path_buf();
                self.set_mut().dropped.insert(file.id(), file);
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
