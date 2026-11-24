//! A session of a page store, run the way the layers above run one.
//!
//! [`Session`] opens a database directory with the store's recovery
//! (`lignin_store::recovery`) and then is what `lignin-store` asks its
//! callers to be: the committer of `transactions.md` CM3 (private copies,
//! one log group, write-back told, the copies installed, the counts
//! applied, the version published), the keeper of the pins, the owner of
//! the cells for the value log's cleaner, and whoever makes the page steps
//! that take cells out of a page or free one. It reaches the store through
//! its public API and nothing else.
//!
//! It keeps no record of what the pages should say. [`Session::read`] says
//! what they do say, through the pool, the page table, the segments, the
//! value log and the page ids, and checks on the way what must hold of a
//! store whatever it holds.

use std::{
    collections::{btree_map, BTreeMap, BTreeSet, VecDeque},
    fmt, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, MutexGuard, PoisonError,
    },
};

use lignin_format::{
    delta::{Delta, DeltaRun, DeltaRunWriter},
    flat_page::{FlatLeaf, FlatLeafBuilder, FlatPage, FlatValue},
    manifest::Manifest,
    tree_page::{CellContent, CellValue, HashField, Side, TreeCell, TreePage, TreePageMut},
    value_log::{PendingValue, ValuePlace, ValuePointer},
    wal::{
        write_reclaim, FlatChange, FlatRun, FlatRunWriter, GroupWriter, LogPlace, Removed,
        ValueMove,
    },
    CellAddress, CommitVersion, FileId, FileKind, FormatError, PageId, PageSize, SlotId,
};
use lignin_io::{Vfs, VfsLock};
use lignin_store::{
    alloc::{Epoch, PageAllocator, PageState, Pins},
    cache::{BufferPool, OwnerId, PageRef, PageStep, PrivateRef},
    cleaner::{Cleaner, CleanerConfig, CleanerState, Pass, ValuePass},
    flush::{
        Checkpointed, FlushConfig, FlushError, FlushState, FlushStats, PageValues, Published,
        RegroupError, Stores, WriteBack,
    },
    load::{LoadBuffers, LoadError, PageLoader, FRAME_FORMAT},
    recovery::{recover, Recovered, RecoveryOptions, RecoveryReport},
    segment::{FileNames, ManifestStore, SegmentStore, SegmentStoreError},
    table::{Link, LinkError, LinkKind, PageTable, TableCheckpoints},
    values::{
        OwnersError, PointerRewrite, Refusal, Rewrite, ValueChange, ValueHold, ValueOwners,
        ValueStore, ValueWriter,
    },
    wal::{LogError, LogOptions, LogStore, SyncPolicy, DEFAULT_READ_WINDOW_LEN},
};

use lignin_tree::{
    flat::{FlatConfig, FlatTree},
    OperationGuard,
};
use lignindb::verify;

use super::model::{Cell, Change, Page, Pages, Value};

/// The page size of the campaign's stores: the smallest, so that a store
/// with every kind of page and file stays a few hundred kilobytes.
pub const PAGE_SIZE: PageSize = PageSize::MIN;

/// The name of the file whose lock lets one session into a directory.
pub const LOCK_FILE: &str = "LOCK";

fn text(error: impl fmt::Display) -> String {
    error.to_string()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn slot_id(slot: u16) -> Result<SlotId, String> {
    SlotId::new(slot).ok_or_else(|| "slot 0 is no slot".to_string())
}

/// How the store of a session is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tuning {
    /// The size of the buffer pool in bytes.
    pub pool_capacity: usize,
    /// The most pages that replay keeps rebuilt in memory
    /// ([`RecoveryOptions::max_replay_pages`]).
    pub max_replay_pages: Option<usize>,
    /// How many bytes of the log a recovery fetches at a time.
    pub read_window: usize,
    /// The length at which a log file is full.
    pub log_file_len: u64,
    /// The length at which a value-log file is full.
    pub value_file_len: u64,
    /// The thresholds of write-back.
    pub flush: FlushConfig,
    /// The settings of the cleaner.
    pub cleaner: CleanerConfig,
    /// Whether write-back is handed what regroups the values of a page.
    pub regroup: bool,
}

impl Tuning {
    /// A store that turns its files often: log files of a kilobyte or two,
    /// segments and value-log files of a few pages, chains of two runs,
    /// a table checkpoint that is due after a few commits, and a cleaner
    /// that has a file closed at a kilobyte and moves two pages in a pass.
    pub fn small() -> Self {
        Self {
            pool_capacity: 512 << 10,
            max_replay_pages: None,
            read_window: DEFAULT_READ_WINDOW_LEN,
            log_file_len: 1500,
            value_file_len: 6000,
            flush: FlushConfig {
                max_chain_deltas: 2,
                segment_len: 24 << 10,
                max_unwritten: 6,
                max_pending_bytes: 12 << 10,
                max_log_held: 12 << 10,
                checkpoint_log_bytes: 5 << 10,
                ..FlushConfig::new(PAGE_SIZE)
            },
            cleaner: CleanerConfig {
                target_dead_percent: 10,
                checkpoint_dead_percent: 30,
                seal_dead_percent: 5,
                max_segment_percent: 40,
                min_segment_len: 1 << 10,
                max_pass_bytes: 16 << 10,
                segment_len: 24 << 10,
                value_dead_percent: 20,
                ..CleanerConfig::new()
            },
            regroup: true,
        }
    }

    fn recovery(&self) -> RecoveryOptions {
        RecoveryOptions {
            pool_capacity: self.pool_capacity,
            log: LogOptions {
                target_file_len: self.log_file_len,
                sync: SyncPolicy::Never,
            },
            flush: Some(self.flush),
            max_replay_pages: self.max_replay_pages,
            read_window: self.read_window,
        }
    }
}

/// Why a directory was not opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// The directory has no manifest: no store was ever made in it.
    NoStore,
    /// The store did not open.
    Failed {
        /// Whether the error says that the directory is not what a session
        /// of the store leaves ([`lignin_store::recovery::RecoveryError::is_damage`]),
        /// or that its manifest is none.
        damage: bool,
        /// What the store said.
        error: String,
    },
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoStore => f.write_str("the directory has no manifest"),
            Self::Failed {
                damage: true,
                error,
            } => write!(f, "the store is damaged: {error}"),
            Self::Failed {
                damage: false,
                error,
            } => write!(f, "the store did not open: {error}"),
        }
    }
}

/// Who publishes the commits of a session and keeps its pins: one snapshot
/// at most, and no operation that outlives a call.
#[derive(Debug)]
struct Versions {
    latest: AtomicU64,
    pin: AtomicU64,
    /// The version a write transaction that holds copies was begun at: it
    /// reads that version until it ends (`transactions.md` CM3).
    writer: AtomicU64,
    epoch: AtomicU64,
}

const NO_PIN: u64 = u64::MAX;

impl Versions {
    fn version(raw: u64) -> CommitVersion {
        CommitVersion::new(raw).expect("a version that a commit took")
    }
}

impl Pins for Versions {
    fn oldest_pin(&self) -> CommitVersion {
        let latest = self.latest.load(Ordering::SeqCst);
        let (snapshot, writer) = (
            self.pin.load(Ordering::SeqCst),
            self.writer.load(Ordering::SeqCst),
        );
        Self::version(snapshot.min(writer).min(latest))
    }

    fn next_epoch(&self) -> Epoch {
        Epoch::new(self.epoch.fetch_add(1, Ordering::SeqCst) + 1)
    }

    fn oldest_guard(&self) -> Epoch {
        Epoch::new(self.epoch.load(Ordering::SeqCst))
    }
}

impl Published for Versions {
    fn latest(&self) -> CommitVersion {
        Self::version(self.latest.load(Ordering::SeqCst))
    }

    fn guarded(&self, work: &mut dyn FnMut()) {
        work();
    }
}

/// A write transaction that holds private copies and has not ended.
#[derive(Debug)]
struct Held {
    owner: OwnerId,
    pages: BTreeSet<PageId>,
}

/// What a session tells its caller of the item it appends to the log, at
/// the moment the log decides it: whoever keeps a model of the store
/// learns what a crash can leave from then on, before the session writes
/// anything more.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item<'a> {
    /// The item is written and is about to be appended: from here a crash
    /// can leave it in the log. A commit names the ids of the pages it
    /// makes, in the order of the changes that make them.
    Appending(&'a [PageId]),
    /// The append returned `Ok`: the item is in the log and ends here
    /// (`transactions.md` CM9).
    Appended(LogPlace),
    /// The item is out of the log for good: no crash brings it back.
    Gone,
    /// The sync that the commit asked for returned `Ok`: the log is
    /// durable to its end.
    Synced,
}

/// What became of a commit.
#[derive(Debug)]
pub enum Committed {
    /// Its group is in the log, its pages are installed and its version is
    /// published.
    Yes {
        /// The version it made.
        version: u64,
        /// Where its group ends in the log.
        end: LogPlace,
        /// The ids of the pages it made, in the order of the changes that
        /// made them.
        made: Vec<PageId>,
        /// What the sync of the log came to, if the commit asked for one:
        /// a commit whose sync failed is made and is not durable.
        synced: Option<Result<(), String>>,
    },
    /// Its group is not in the log, and no crash brings it back. Nothing
    /// of it is left.
    No(String),
    /// The log cannot say whether its group is in it, now or after a
    /// crash. The session publishes nothing more.
    InDoubt(String),
    /// Its group is in the log and the store refused to install it, which
    /// no rule lets happen.
    Broken(String),
}

/// What became of a page step that the session made.
#[derive(Debug)]
pub enum Stepped {
    /// Its record is in the log and its change is made.
    Yes {
        /// Where the record ends in the log.
        end: LogPlace,
    },
    /// The page is not as the step needs it: a write transaction holds a
    /// copy of it, it is no page, or it was born at the latest version.
    /// Nothing was written.
    Refused(String),
    /// It was not made, and nothing of it is left: the log did not take
    /// the record, and no crash brings it back.
    No(String),
    /// The log cannot say whether its record is in it. The change was not
    /// made in this session.
    InDoubt(String),
    /// The record is in the log and the store refused the change.
    Broken(String),
}

/// What a reclaiming step takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reclaim {
    /// These cells of a tree page, which keeps others.
    Cells(Vec<u16>),
    /// The page, with whatever it holds.
    Page,
}

impl Reclaim {
    /// The slots of the cells the step takes: none when it frees the page.
    pub fn slots(&self) -> &[u16] {
        match self {
            Self::Cells(slots) => slots,
            Self::Page => &[],
        }
    }
}

/// A page of a commit: its private copy, and the changes made to it.
struct Work<'c, 'p> {
    copy: PrivateRef<'p>,
    made: bool,
    changes: Vec<&'c Change>,
}

impl Work<'_, '_> {
    fn frees(&self) -> bool {
        (self.changes.iter()).any(|change| matches!(change, Change::Free { .. }))
    }

    /// Whether the commit writes the page, a flat leaf, as an image.
    fn writes_whole(&self) -> bool {
        let rewrites = |change: &&Change| matches!(change, Change::Rewrite { .. });
        self.made || self.changes.iter().any(rewrites)
    }
}

/// A value that a commit carries in its group, with the name its cell
/// holds for it until write-back has placed it.
type Logged = VecDeque<(PendingValue, Arc<[u8]>)>;

/// What the entry of a flat leaf holds once its commit is made.
enum Entry<'c> {
    Inline(&'c [u8]),
    /// The value stays where it is in the value log.
    Keep(ValuePointer),
    /// The commit carries the value.
    Log(Arc<[u8]>),
}

/// An open store.
pub struct Session {
    vfs: Arc<dyn Vfs>,
    dir: PathBuf,
    manifest: Mutex<ManifestStore>,
    pool: BufferPool,
    table: PageTable,
    segments: SegmentStore,
    values: ValueStore,
    log: LogStore,
    checkpoints: TableCheckpoints,
    allocator: PageAllocator,
    flush: FlushState,
    versions: Versions,
    report: RecoveryReport,
    regroup: bool,
    owners: AtomicU64,
    held: Mutex<Option<Held>>,
    cleaning: CleanerState,
    buffers: Mutex<LoadBuffers>,
    complaints: Mutex<Vec<String>>,
    _lock: Box<dyn VfsLock>,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("dir", &self.dir)
            .field("latest", &self.latest())
            .finish_non_exhaustive()
    }
}

impl Session {
    /// Makes a store in the directory `dir` of `vfs`: its first manifest.
    pub fn create(vfs: Arc<dyn Vfs>, dir: &Path) -> Result<(), String> {
        let empty = Manifest::new(PAGE_SIZE, Default::default(), None, Vec::new()).map_err(text)?;
        ManifestStore::create(vfs, dir, empty)
            .map(drop)
            .map_err(text)
    }

    /// Opens the store in `dir`, whatever the session before left there:
    /// takes the directory's lock, opens the manifest and recovers.
    pub fn open(vfs: Arc<dyn Vfs>, dir: &Path, tuning: &Tuning) -> Result<Self, OpenError> {
        let failed = |damage, error: String| OpenError::Failed { damage, error };
        let locked = vfs
            .lock(&dir.join(LOCK_FILE))
            .map_err(|error| failed(false, text(error)))?;
        let manifest = match ManifestStore::open(vfs.clone(), dir) {
            Ok(manifest) => manifest,
            Err(SegmentStoreError::NoManifest { .. }) => return Err(OpenError::NoStore),
            Err(error) => {
                let damage = !matches!(error, SegmentStoreError::Io { .. });
                return Err(failed(damage, text(error)));
            }
        };
        let recovered = recover(manifest, &tuning.recovery())
            .map_err(|error| failed(error.is_damage(), text(error)))?;
        let Recovered {
            manifest,
            pool,
            table,
            segments,
            values,
            log,
            checkpoints,
            allocator,
            flush,
            report,
            ..
        } = recovered;
        let session = Self {
            vfs,
            dir: dir.to_path_buf(),
            manifest: Mutex::new(manifest),
            pool,
            table,
            segments,
            values: values.with_target_file_len(tuning.value_file_len),
            log,
            checkpoints,
            allocator,
            flush,
            versions: Versions {
                latest: AtomicU64::new(report.landed.get()),
                pin: AtomicU64::new(NO_PIN),
                writer: AtomicU64::new(NO_PIN),
                epoch: AtomicU64::new(0),
            },
            report,
            regroup: tuning.regroup,
            owners: AtomicU64::new(0),
            held: Mutex::new(None),
            cleaning: CleanerState::new(tuning.cleaner),
            buffers: Mutex::default(),
            complaints: Mutex::default(),
            _lock: locked,
        };
        session.reclaim();
        Ok(session)
    }

    /// The file layer the store is on.
    pub fn vfs(&self) -> &Arc<dyn Vfs> {
        &self.vfs
    }

    /// What the recovery that opened the store did.
    pub fn report(&self) -> &RecoveryReport {
        &self.report
    }

    /// The latest published version.
    pub fn latest(&self) -> CommitVersion {
        Published::latest(&self.versions)
    }

    /// The place the log is known durable up to.
    pub fn durable(&self) -> Option<LogPlace> {
        self.log.durable().place
    }

    /// Where the log ends.
    pub fn log_end(&self) -> Option<LogPlace> {
        self.log.end()
    }

    /// Whether the log takes an item now. After an append that failed it
    /// takes none until it has been rolled: the file is cut off where its
    /// last whole item ends, by whoever keeps the log's files, and every
    /// append before that fails (`transactions.md` CM9 item 5,
    /// `LogError::NoActiveFile`).
    pub fn log_takes_items(&self) -> bool {
        !matches!(self.log.next_group(), Err(LogError::NoActiveFile))
    }

    /// Whether the log takes nothing more in this session: it cannot say
    /// whether an item is in it.
    pub fn stopped(&self) -> bool {
        matches!(self.log.next_group(), Err(LogError::Unsettled { .. }))
    }

    /// What write-back has done in this session.
    pub fn flush_stats(&self) -> FlushStats {
        self.flush.stats()
    }

    /// The files in the directories of the log, the segments, the value
    /// log and the table checkpoints that the manifest in force does not
    /// name. A store leaves none behind for good: a file it has dropped
    /// and could not remove goes with a later call, and what a session
    /// left is removed by the recovery of the next
    /// (`lignin_store::recovery`, step 2).
    pub fn strays(&self) -> Result<Vec<PathBuf>, String> {
        let manifest = lock(&self.manifest);
        let manifest = manifest.manifest().map_err(text)?;
        let table = manifest.table_checkpoint();
        let named = |kind: FileKind, id: FileId| match kind {
            FileKind::PageTable => table.is_some_and(|table| table.file() == id),
            _ => (manifest.files().iter()).any(|file| file.kind() == kind && file.id() == id),
        };
        let mut strays = Vec::new();
        for names in [
            FileNames::WAL,
            FileNames::SEGMENT,
            FileNames::VALUE_LOG,
            FileNames::PAGE_TABLE,
        ] {
            let dir = names.dir(&self.dir);
            let entries = match self.vfs.read_dir(&dir) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("{}: {error}", dir.display())),
            };
            for entry in entries {
                let id = names.parse(&entry.name);
                if !id.is_some_and(|id| named(names.kind(), id)) {
                    strays.push(dir.join(&entry.name));
                }
            }
        }
        Ok(strays)
    }

    /// Whether a write transaction holds private copies.
    pub fn holds(&self) -> bool {
        lock(&self.held).is_some()
    }

    /// What write-back answered with an error where the session went on
    /// all the same, since the last call: when it was told of a commit, and
    /// in the turns a committer makes for it once its transaction has
    /// ended.
    pub fn complaints(&self) -> Vec<String> {
        std::mem::take(&mut lock(&self.complaints))
    }

    fn complain<T>(&self, what: &str, result: Result<T, impl fmt::Display>) {
        if let Err(error) = result {
            lock(&self.complaints).push(format!("{what}: {error}"));
        }
    }

    fn wb(&self) -> WriteBack<'_> {
        let stores = Stores {
            pool: &self.pool,
            table: &self.table,
            segments: &self.segments,
            values: &self.values,
            log: &self.log,
            checkpoints: &self.checkpoints,
            manifest: &self.manifest,
            versions: &self.versions,
            page_values: self.regroup.then_some(self as &dyn PageValues),
        };
        WriteBack::new(stores, &self.flush, PAGE_SIZE).expect("a pool of the store's page size")
    }

    fn loader(&self) -> PageLoader<'_> {
        PageLoader::new(&self.pool, &self.table, &self.segments, PAGE_SIZE)
            .expect("a pool of the store's page size")
    }

    /// The current version of `page`, or `None` if it is no page.
    fn current(&self, page: PageId) -> Result<Option<PageRef<'_>>, LoadError> {
        match self
            .loader()
            .get(page, self.latest(), &mut lock(&self.buffers))
        {
            Ok(frame) => Ok(Some(frame)),
            Err(LoadError::NoPage { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn owner(&self) -> OwnerId {
        OwnerId::new(self.owners.fetch_add(1, Ordering::SeqCst) + 1)
    }

    /// What a session does once a commit is published or a pin is dropped:
    /// the ids and the page versions that no reader can reach are taken
    /// back.
    pub fn reclaim(&self) -> usize {
        let mut released = Vec::new();
        self.allocator.reclaim(&self.versions, &mut released);
        released.len() + self.pool.reclaim(&self.versions)
    }

    /// The state of every page id the allocator has given out.
    fn page_states(&self) -> Vec<(PageId, PageState)> {
        let mut states = Vec::new();
        let mut from = Some(PageId::MIN);
        while let Some(at) = from {
            from = self.allocator.scan(at, &mut states);
        }
        states
    }

    /// The newest link of every page the table has a chain of.
    fn newest_links(&self) -> BTreeMap<PageId, Link> {
        let mut links = Vec::new();
        let mut from = Some(PageId::MIN);
        while let Some(at) = from {
            from = self.table.scan(at, &mut links);
        }
        links.into_iter().collect()
    }

    /// Pins the latest version, as a snapshot does. Returns whether a pin
    /// was taken: a session has one at most.
    pub fn pin(&self) -> bool {
        let latest = self.versions.latest.load(Ordering::SeqCst);
        self.versions
            .pin
            .compare_exchange(NO_PIN, latest, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    /// Drops the pin, if there is one, and reclaims.
    pub fn unpin(&self) -> bool {
        let pinned = self.versions.pin.swap(NO_PIN, Ordering::SeqCst) != NO_PIN;
        self.reclaim();
        pinned
    }
}

/// Reading: what the pages say.
impl Session {
    /// What every page of the latest version says, read through the pool
    /// and, for what it does not hold, from the segments and the value
    /// log. `expected` are ids to look at besides those the store itself
    /// has in use.
    ///
    /// It fails if the store is not at one with itself:
    ///
    /// - a page id is in use exactly when its page is read
    ///   (`transactions.md` ID4), and none is private to a transaction
    ///   unless one holds copies
    /// - a chain that ends in a block is of a page in use
    /// - a page holds its own id, and nothing that no commit of a session
    ///   writes: a link, a hosted root, a cell that is no fragment root, a
    ///   branch
    /// - every value out of line is read, with the length its cell says:
    ///   a pending one from the bytes with its frame (FL7), a placed one
    ///   from the value log
    /// - the counts of live values are the pointers the pages hold (DU5),
    ///   and none was found wrong
    pub fn read(&self, expected: &BTreeSet<PageId>) -> Result<Pages, String> {
        let states: BTreeMap<PageId, PageState> = self.page_states().into_iter().collect();
        let newest = self.newest_links();

        let ids: BTreeSet<PageId> = (states.keys().chain(newest.keys()).chain(expected))
            .copied()
            .collect();
        let private = self.holds();
        let mut pages = Pages::new();
        let mut pointers: BTreeMap<FileId, u64> = BTreeMap::new();
        let mut buf = Vec::new();
        for id in ids {
            let state = states.get(&id).copied().unwrap_or(PageState::Free);
            let frame = self
                .current(id)
                .map_err(|error| format!("page {id}: {error}"))?;
            match (&frame, state) {
                (Some(_), PageState::Live) => {}
                (None, PageState::Free | PageState::Retired) => {}
                (None, PageState::Private) if private => {}
                (Some(_), state) => {
                    return Err(format!("page {id} is read and its id is {state}"));
                }
                (None, state) => {
                    return Err(format!("the id {id} is {state} and names no page"));
                }
            }
            let Some(frame) = frame else {
                if newest
                    .get(&id)
                    .is_some_and(|link| link.kind != LinkKind::Free)
                {
                    return Err(format!(
                        "the chain of page {id} ends in a block and the page is not read"
                    ));
                }
                continue;
            };
            let page = self
                .decode(id, &frame, &mut pointers, &mut buf)
                .map_err(|error| format!("page {id}: {error}"))?;
            pages.insert(id, Arc::new(page));
        }

        let mut counted = BTreeMap::new();
        for count in self.values.counts() {
            if count.miscounted {
                return Err(format!("a count of live values was found wrong: {count:?}"));
            }
            if count.values > 0 {
                counted.insert(count.file, count.values);
            }
        }
        if counted != pointers {
            return Err(format!(
                "the counts of live values are {counted:?} and the pages hold {pointers:?}"
            ));
        }
        if self.report.miscounts != 0 {
            return Err(format!(
                "recovery found {} records that released a value no count included",
                self.report.miscounts
            ));
        }
        Ok(pages)
    }

    /// The bytes of a value out of line, from where its place says they
    /// are.
    fn value_at(
        &self,
        id: PageId,
        frame: &PageRef<'_>,
        place: ValuePlace,
        pointers: &mut BTreeMap<FileId, u64>,
        buf: &mut Vec<u8>,
    ) -> Result<Value, String> {
        let bytes: Arc<[u8]> = match place {
            ValuePlace::Placed(pointer) => {
                *pointers.entry(pointer.file()).or_default() += 1;
                let value = self
                    .values
                    .read(pointer, id, buf)
                    .map_err(|error| format!("a value in the value log is not read: {error}"))?;
                Arc::from(value.to_vec())
            }
            ValuePlace::Pending(pending) => match frame.attached(pending.id()) {
                Some(bytes) => Arc::from(bytes),
                None => return Err("a pending value has no bytes with its frame".to_string()),
            },
        };
        if bytes.len() != place.value_len() as usize {
            return Err(format!(
                "a value of {} bytes where the cell says {}",
                bytes.len(),
                place.value_len()
            ));
        }
        Ok(Value::Out(bytes))
    }

    fn decode(
        &self,
        id: PageId,
        frame: &PageRef<'_>,
        pointers: &mut BTreeMap<FileId, u64>,
        buf: &mut Vec<u8>,
    ) -> Result<Page, String> {
        if !is_tree(frame) {
            let page = FlatPage::from_validated(frame, FRAME_FORMAT).map_err(text)?;
            let leaf = page.leaf().ok_or("a branch, which no commit wrote")?;
            if page.id() != id {
                return Err(format!("the image of page {}", page.id()));
            }
            let mut entries = BTreeMap::new();
            for (key, value) in leaf.iter() {
                let value = match value {
                    FlatValue::Inline(bytes) => Value::Inline(bytes.to_vec()),
                    FlatValue::OutOfLine(place) => {
                        self.value_at(id, frame, place_of(place)?, pointers, buf)?
                    }
                };
                entries.insert(key.to_vec(), value);
            }
            let born = frame.born().get();
            return Ok(Page::Flat { entries, born });
        }
        let tree = TreePage::from_validated(frame, FRAME_FORMAT).map_err(text)?;
        tree.check(PAGE_SIZE).map_err(text)?;
        if tree.id() != id {
            return Err(format!("the image of page {}", tree.id()));
        }
        let mut cells = BTreeMap::new();
        for (slot, cell) in tree.cells() {
            let linked = Side::BOTH.iter().any(|side| cell.has_link(*side));
            if linked || cell.hosted_root_count() != 0 || !cell.is_fragment_root() {
                return Err(format!("slot {slot} holds what no commit wrote"));
            }
            let value = match cell.value() {
                CellValue::Inline(bytes) => Value::Inline(bytes.to_vec()),
                CellValue::OutOfLine(pointer) => {
                    self.value_at(id, frame, ValuePlace::Placed(pointer), pointers, buf)?
                }
                CellValue::Pending(pending) => {
                    self.value_at(id, frame, ValuePlace::Pending(pending), pointers, buf)?
                }
            };
            cells.insert(
                slot.get(),
                Cell {
                    key: cell.key().to_vec(),
                    feature: cell.feature().to_vec(),
                    value_hash: *cell.value_hash(),
                    kv_hash: *cell.kv_hash(),
                    value,
                },
            );
        }
        Ok(Page::Tree {
            cells,
            departures: tree.departures(),
            born: frame.born().get(),
        })
    }
}

/// The guard of a session's own reads: no operation runs beside them.
struct Alone;

impl OperationGuard for Alone {}

/// The physical invariant checker as a second look at a store that is
/// still.
impl Session {
    /// Runs the engine's physical invariant checker (`lignindb::verify`)
    /// over the latest version, in the scope of a store that is still: the
    /// pages, the values and their owners, the page ids, the counts of
    /// live values, and the page table against its blocks.
    ///
    /// The store has no tree above its pages, so the checker is told what
    /// the workload writes: every cell is a Merk of one node, and every
    /// flat leaf a keyspace of one page. No hash is recomputed.
    ///
    /// The caller vouches that no write transaction holds a copy.
    pub fn verify(&self) -> Result<(), String> {
        let states = self.page_states();
        let (mut roots, mut flat) = (Vec::new(), Vec::new());
        for (id, _) in states.iter().filter(|(_, state)| *state == PageState::Live) {
            let Some(frame) = self.current(*id).map_err(text)? else {
                continue;
            };
            if is_tree(&frame) {
                let tree = TreePage::from_validated(&frame, FRAME_FORMAT).map_err(text)?;
                roots.extend(tree.cells().map(|(slot, _)| CellAddress::new(*id, slot)));
            } else {
                flat.push(FlatTree::open(*id, FlatConfig::new(PAGE_SIZE)));
            }
        }
        let store = verify::Store {
            loader: self.loader(),
            table: &self.table,
            segments: &self.segments,
            values: &self.values,
            page_size: PAGE_SIZE,
            ids: Some(&self.allocator),
            grove: verify::Grove::Roots(&roots),
            flat_drops: None,
            flat: &flat,
            values_counted: true,
        };
        let options = verify::Options::new(self.latest(), verify::Scope::Still);
        let report = verify::check(&store, &Alone, options, None);
        if report.is_sound() {
            return Ok(());
        }
        const SHOWN: usize = 3;
        let mut said: Vec<String> = (report.findings.iter().take(SHOWN))
            .map(|finding| finding.to_string())
            .collect();
        let more = report.findings.len().saturating_sub(SHOWN) as u64 + report.more_findings;
        if more > 0 {
            said.push(format!("and {more} more"));
        }
        Err(format!("the invariant checker: {}", said.join("; ")))
    }
}

fn is_tree(frame: &[u8]) -> bool {
    frame.first() == Some(&TreePage::KIND_CODE)
}

fn place_of(bytes: &[u8]) -> Result<ValuePlace, String> {
    let bytes: [u8; ValuePlace::ENCODED_LEN] = bytes
        .try_into()
        .map_err(|_| "a value out of line that is no place".to_string())?;
    ValuePlace::decode(bytes).map_err(text)
}

fn leaf_of(frame: &[u8]) -> Result<FlatLeaf<'_>, String> {
    FlatPage::from_validated(frame, FRAME_FORMAT)
        .map_err(text)?
        .leaf()
        .ok_or_else(|| "a branch where a leaf must be".to_string())
}

/// The value pointers the page in `frame` holds.
fn pointers_of(frame: &[u8]) -> Result<Vec<ValuePointer>, String> {
    if is_tree(frame) {
        let tree = TreePage::from_validated(frame, FRAME_FORMAT).map_err(text)?;
        return Ok(tree
            .cells()
            .filter_map(|(_, cell)| cell.value_pointer())
            .collect());
    }
    let mut pointers = Vec::new();
    for (_, value) in leaf_of(frame)?.iter() {
        if let FlatValue::OutOfLine(place) = value {
            pointers.extend(place_of(place)?.pointer());
        }
    }
    Ok(pointers)
}

/// Committing.
impl Session {
    /// A write transaction copies `pages` and holds the copies: until it
    /// commits or rolls back, no page step is made on them. Returns whether
    /// the copies were made.
    pub fn hold(&self, pages: &[PageId]) -> Result<bool, String> {
        let mut held = lock(&self.held);
        if held.is_some() {
            return Ok(false);
        }
        let mut txn = Held {
            owner: self.owner(),
            pages: BTreeSet::new(),
        };
        let base = self.versions.latest.load(Ordering::SeqCst);
        self.versions.writer.store(base, Ordering::SeqCst);
        for &page in pages {
            if let Err(error) = self.copy_of(&mut txn, page) {
                self.give_up(&txn, &[]);
                self.versions.writer.store(NO_PIN, Ordering::SeqCst);
                return Err(error);
            }
        }
        *held = Some(txn);
        Ok(true)
    }

    /// The transaction that holds copies rolls back.
    pub fn release(&self) -> bool {
        let Some(txn) = lock(&self.held).take() else {
            return false;
        };
        self.give_up(&txn, &[]);
        self.versions.writer.store(NO_PIN, Ordering::SeqCst);
        self.after();
        true
    }

    /// The private copy that `txn` has of `page`, made now if it has none.
    fn copy_of(&self, txn: &mut Held, page: PageId) -> Result<PrivateRef<'_>, String> {
        if let Some(copy) = self.pool.private(txn.owner, page) {
            return Ok(copy);
        }
        let latest = self.latest();
        let frame = self
            .current(page)
            .map_err(|error| format!("page {page} was not read: {error}"))?
            .ok_or_else(|| format!("page {page} is no page"))?;
        let copy = self
            .pool
            .make_private(txn.owner, page, latest)
            .map_err(|miss| format!("page {} left the pool while it was pinned", miss.page()))?;
        drop(frame);
        txn.pages.insert(page);
        Ok(copy)
    }

    /// Gives up what a transaction holds, as a rollback does.
    fn give_up(&self, txn: &Held, made: &[PageId]) {
        for &page in txn.pages.iter().chain(made) {
            if let Some(copy) = self.pool.private(txn.owner, page) {
                let _ = self.pool.discard(&copy);
            }
        }
        for &page in made {
            let _ = self.allocator.release(page);
        }
    }

    /// Looks at the log before a commit takes its transaction: a file whose
    /// last append failed is cut off first, by whoever keeps the log's
    /// files, and a log that cannot say what it holds takes no commit
    /// (`transactions.md` CM9).
    fn log_takes_a_group(&self) -> Result<(), String> {
        match self.log.next_group() {
            Err(LogError::NoActiveFile) => self.wb().roll_log().map(drop).map_err(text),
            Err(error @ LogError::Unsettled { .. }) => Err(text(error)),
            _ => Ok(()),
        }
    }

    /// Commits `changes` (`transactions.md` CM3): private copies, one group
    /// in the log with the values, the pages in ascending order and the
    /// pointers released, write-back told, the copies installed and the
    /// freed pages freed, the counts applied, the version published. With
    /// `held`, the transaction that holds copies commits, and the pages it
    /// copied and did not change are given up.
    ///
    /// With `durable`, the log is synced for the commit where the engine's
    /// committer syncs it: once the commit is installed and counted, and
    /// before its version is published and anything is written back.
    ///
    /// `tell` is told what the log decides of the group, when it does
    /// ([`Item`]).
    pub fn commit(
        &self,
        changes: &[Change],
        held: bool,
        durable: bool,
        tell: &mut dyn FnMut(Item<'_>),
    ) -> Committed {
        if let Err(error) = self.log_takes_a_group() {
            return Committed::No(format!("the log takes no group: {error}"));
        }
        let mut txn = match held {
            true => match lock(&self.held).take() {
                Some(txn) => txn,
                None => return Committed::No("no transaction holds a copy".to_string()),
            },
            false => Held {
                owner: self.owner(),
                pages: BTreeSet::new(),
            },
        };
        let mut made = Vec::new();
        let committed = match self.commit_with(&mut txn, changes, &mut made, durable, tell) {
            Ok(committed) => committed,
            Err(error) => Committed::No(error),
        };
        if !matches!(committed, Committed::Yes { .. }) {
            self.give_up(&txn, &made);
        }
        // The transaction has ended, and with it the pin on its base.
        if held {
            self.versions.writer.store(NO_PIN, Ordering::SeqCst);
        }
        self.after();
        committed
    }

    /// What a committer does for write-back once its transaction has ended
    /// and it holds no copy: the ids and frames no reader needs are taken
    /// back, the values that waited for its copies are placed, and it
    /// waits while write-back is over its bounds, making the turns itself.
    /// An error of write-back here is not the commit's, and is kept for
    /// [`Self::complaints`].
    fn after(&self) {
        self.reclaim();
        self.complain("placing the values that waited", self.wb().place_ready());
        self.complain("waiting for room", self.wb().wait_for_room());
    }

    fn commit_with<'c>(
        &self,
        txn: &mut Held,
        changes: &'c [Change],
        made: &mut Vec<PageId>,
        durable: bool,
        tell: &mut dyn FnMut(Item<'_>),
    ) -> Result<Committed, String> {
        let latest = self.latest();
        let version = latest.next().ok_or("the versions are used up")?;
        let mut works: BTreeMap<PageId, Work<'c, '_>> = BTreeMap::new();
        for change in changes {
            let id = match change.page() {
                Some(id) => id,
                None => {
                    let id = self.allocator.allocate().map_err(text)?;
                    made.push(id);
                    id
                }
            };
            let work = match works.entry(id) {
                btree_map::Entry::Occupied(work) => work.into_mut(),
                btree_map::Entry::Vacant(place) => {
                    let copy = match change.makes_page() {
                        true => self.pool.new_private(txn.owner, id).map_err(text)?,
                        false => self.copy_of(txn, id)?,
                    };
                    if matches!(change, Change::NewTree { .. }) {
                        let mut latch = copy.write().map_err(text)?;
                        TreePageMut::init(&mut latch, PAGE_SIZE, id, 0).map_err(text)?;
                    }
                    place.insert(Work {
                        copy,
                        made: change.makes_page(),
                        changes: Vec::new(),
                    })
                }
            };
            work.changes.push(change);
        }

        // The counts lock keeps the page steps and the marks out of the
        // log between the place and the group (`wal`, "Locks").
        let counts = self.values.step().map_err(text)?;
        let at = self.log.next_group().map_err(text)?;
        if at.version() != version {
            return Ok(Committed::Broken(format!(
                "the log gives version {} to the commit after version {latest}",
                at.version()
            )));
        }
        let mut bytes = Vec::new();
        let mut group = GroupWriter::new(at.version(), at.address(), &mut bytes).map_err(text)?;

        // The values first: each is named by where it lies in the group.
        let mut wholes: BTreeMap<PageId, Vec<(&'c [u8], Entry<'c>)>> = BTreeMap::new();
        let mut logged: BTreeMap<PageId, Logged> = BTreeMap::new();
        for (&id, work) in &works {
            if work.frees() {
                continue;
            }
            let latch = work.copy.read();
            let mut values: Vec<Arc<[u8]>> = Vec::new();
            if is_tree(&latch) {
                for change in &work.changes {
                    match change {
                        Change::NewTree { cells } => {
                            values.extend(cells.iter().filter_map(|(_, cell)| out(&cell.value)));
                        }
                        Change::Insert { cell, .. } => values.extend(out(&cell.value)),
                        Change::SetValue { value, .. } => values.extend(out(value)),
                        Change::Remove { .. } | Change::Hash { .. } | Change::Feature { .. } => {}
                        other => return Err(format!("{other:?} of the tree page {id}")),
                    }
                }
            } else if work.writes_whole() {
                let attached = |pending: PendingValue| latch.attached(pending.id());
                let entries = whole_leaf(id, work, &latch, &attached)?;
                values.extend(entries.iter().filter_map(|(_, entry)| match entry {
                    Entry::Log(bytes) => Some(bytes.clone()),
                    _ => None,
                }));
                wholes.insert(id, entries);
            } else {
                for change in &work.changes {
                    match change {
                        Change::Put { value, .. } => values.extend(out(value)),
                        Change::Delete { .. } => {}
                        other => return Err(format!("{other:?} of the flat leaf {id}")),
                    }
                }
            }
            drop(latch);
            let mut named = Logged::new();
            for value in values {
                let pending = group.value(id, &value).map_err(text)?;
                named.push_back((pending, value));
            }
            logged.insert(id, named);
        }

        // The pages, in ascending order, and what their cells stop holding.
        let mut released: Vec<ValuePointer> = Vec::new();
        let mut freed = BTreeSet::new();
        for (&id, work) in &works {
            let mut latch = work.copy.write().map_err(text)?;
            if work.frees() {
                if work.changes.len() != 1 || work.made {
                    return Err(format!("page {id} is freed and changed in one commit"));
                }
                released.extend(pointers_of(&latch)?);
                group.freed(id).map_err(text)?;
                freed.insert(id);
                continue;
            }
            let mut named = logged.remove(&id).unwrap_or_default();
            let attach: Vec<_> = named.iter().cloned().collect();
            if is_tree(&latch) {
                let created = work.made.then_some(0);
                let changes = &work.changes;
                let written = group.tree_run_with(id, created, |run| {
                    tree_run(&mut latch, changes, &mut named, &mut released, run)
                });
                written.map_err(|error| format!("the run of page {id}: {error}"))?;
            } else {
                let before = match work.made {
                    true => Vec::new(),
                    false => pointers_of(&latch)?,
                };
                let image = match wholes.remove(&id) {
                    Some(entries) => {
                        let image = leaf_image(id, &entries, &mut named)?;
                        group
                            .flat_run_with(id, |run| run.push(&FlatChange::Image(&image)))
                            .map_err(text)?;
                        image
                    }
                    None => {
                        let run = leaf_changes(id, &work.changes, &latch, &mut named)?;
                        let run = FlatRun::decode(&run, FRAME_FORMAT).map_err(text)?;
                        let mut image = Vec::new();
                        let capacity = PAGE_SIZE.image_capacity();
                        run.apply(Some(&latch[..]), FRAME_FORMAT, capacity, &mut image)
                            .map_err(text)?;
                        group.flat_run(&run).map_err(text)?;
                        image
                    }
                };
                let after = pointers_of(&image)?;
                released.extend(before.into_iter().filter(|held| !after.contains(held)));
                put_image(&mut latch[..], &image);
            }
            if !named.is_empty() {
                return Err(format!("page {id}: a value was logged for no cell"));
            }
            for (pending, value) in attach {
                latch.attach(pending.id(), value);
            }
        }
        released.sort_by_key(|pointer| (pointer.file(), pointer.offset()));
        for pointer in &released {
            group.released(pointer).map_err(text)?;
        }
        group.finish();

        tell(Item::Appending(made));
        let appended = match self.log.append_group(&at, &bytes) {
            Ok(appended) => appended,
            Err(error) => {
                // The lock on the counts is given back before the log is
                // asked to settle, which syncs.
                drop(counts);
                return Ok(match self.settle_after(error) {
                    Ok(reason) => {
                        tell(Item::Gone);
                        Committed::No(reason)
                    }
                    Err(reason) => Committed::InDoubt(reason),
                });
            }
        };
        tell(Item::Appended(appended.end));
        // Write-back is told before the first page is installed, and the
        // commit goes on whatever it answers.
        let told = self.wb().committed(version, appended.at, &bytes);
        self.complain("telling write-back of the commit", told);

        for (id, work) in works {
            let installed = match freed.contains(&id) {
                true => self.free_page(&work.copy, version),
                false => self.install(&work, version),
            };
            if let Err(error) = installed {
                return Ok(Committed::Broken(format!("page {id}: {error}")));
            }
        }
        for &page in txn.pages.iter() {
            if let Some(copy) = self.pool.private(txn.owner, page)
                && let Err(error) = self.pool.discard(&copy)
            {
                return Ok(Committed::Broken(format!("page {page}: {error}")));
            }
        }
        let mut change = ValueChange::new();
        released.iter().for_each(|&pointer| change.release(pointer));
        if let Err(error) = counts.apply(&change, version) {
            return Ok(Committed::Broken(format!("the counts: {error}")));
        }
        // The sync of the commit: before anything is published, and under
        // no lock (CM7, and the engine's committer).
        let synced = durable.then(|| self.wb().sync_log().map(drop).map_err(text));
        if matches!(synced, Some(Ok(()))) {
            tell(Item::Synced);
        }
        self.versions.latest.store(version.get(), Ordering::SeqCst);
        Ok(Committed::Yes {
            version: version.get(),
            end: appended.end,
            made: made.clone(),
            synced,
        })
    }

    fn install(&self, work: &Work<'_, '_>, version: CommitVersion) -> Result<(), String> {
        drop(self.pool.install(&work.copy, version).map_err(text)?);
        match work.made {
            true => self.allocator.publish(work.copy.page()).map_err(text),
            false => Ok(()),
        }
    }

    /// Frees the page of `copy` with `version`: the copy goes, the page
    /// table gets the free link, then the pool is told, and the id is
    /// retired.
    fn free_page(&self, copy: &PrivateRef<'_>, version: CommitVersion) -> Result<(), String> {
        self.pool.discard(copy).map_err(text)?;
        self.push_free(copy.page(), version)
    }

    fn push_free(&self, page: PageId, version: CommitVersion) -> Result<(), String> {
        match self.table.push(page, Link::free(version)) {
            // A page that was never written back has no chain to end.
            Ok(()) | Err(LinkError::AlreadyFree) => {}
            Err(error) => return Err(text(error)),
        }
        self.pool.free(page, version).map_err(text)?;
        self.allocator.free(page, version).map_err(text)
    }

    /// Finds out what became of an item whose append failed with `error`,
    /// with every lock given back: `Ok` if the item is out of the log for
    /// good, `Err` if the log cannot say.
    fn settle_after(&self, error: LogError) -> Result<String, String> {
        match error {
            LogError::TakenBack { .. } => match self.log.settle() {
                Ok(()) => Ok(format!("{error}, and the log is settled")),
                Err(unsettled) => Err(format!("{error}; {unsettled}")),
            },
            LogError::Unsettled { .. } => Err(text(error)),
            other => Ok(text(other)),
        }
    }
}

/// Makes `frame` the image, with nothing after it.
fn put_image(frame: &mut [u8], image: &[u8]) {
    let (head, tail) = frame.split_at_mut(image.len());
    head.copy_from_slice(image);
    tail.fill(0);
}

/// The version a table checkpoint covers, or `None` if it was put off.
fn covered(checkpointed: Checkpointed) -> Option<u64> {
    match checkpointed {
        Checkpointed::Installed { covers, .. } => Some(covers.get()),
        Checkpointed::PutOff { .. } => None,
    }
}

fn out(value: &Value) -> Option<Arc<[u8]>> {
    match value {
        Value::Out(bytes) => Some(bytes.clone()),
        Value::Inline(_) => None,
    }
}

fn misfit() -> FormatError {
    lignin_format::wal::WalError::InvalidChange.into()
}

/// What a cell holds for `value`: the value, or the name of the next of
/// the values the group carries for the page.
fn cell_value<'v>(value: &'v Value, named: &mut Logged) -> Result<CellValue<'v>, FormatError> {
    match value {
        Value::Inline(bytes) => Ok(CellValue::Inline(bytes)),
        Value::Out(_) => match named.pop_front() {
            Some((pending, _)) => Ok(CellValue::Pending(pending)),
            None => Err(misfit()),
        },
    }
}

fn insert_cell(
    tree: &mut TreePageMut<'_>,
    run: &mut DeltaRunWriter<'_>,
    slot: u16,
    cell: &Cell,
    named: &mut Logged,
) -> Result<(), FormatError> {
    let encoded = CellContent {
        key: &cell.key,
        feature: &cell.feature,
        value_hash: &cell.value_hash,
        kv_hash: &cell.kv_hash,
        left: None,
        right: None,
        hosted_roots: &[],
        value: cell_value(&cell.value, named)?,
        fragment_root: true,
    }
    .encode()?;
    let delta = Delta::Insert {
        slot: SlotId::new(slot).ok_or_else(misfit)?,
        cell: TreeCell::decode(&encoded, FRAME_FORMAT)?,
    };
    delta.apply(tree)?;
    run.push(&delta)
}

/// Makes the changes of a commit to the tree page in `frame` and writes
/// them into `run`. A value out of line is pending under the next name of
/// `named`, and a pointer that a cell stops holding goes to `released`.
fn tree_run(
    frame: &mut [u8],
    changes: &[&Change],
    named: &mut Logged,
    released: &mut Vec<ValuePointer>,
    run: &mut DeltaRunWriter<'_>,
) -> Result<(), FormatError> {
    let slot_of = |slot: &u16| SlotId::new(*slot).ok_or_else(misfit);
    let mut tree = TreePageMut::open(frame, PAGE_SIZE, FRAME_FORMAT)?;
    for change in changes {
        let held = |tree: &TreePageMut<'_>, slot: SlotId| {
            tree.page().cell(slot).and_then(|cell| cell.value_pointer())
        };
        let delta = match change {
            Change::NewTree { cells } => {
                for (slot, cell) in cells {
                    insert_cell(&mut tree, run, *slot, cell, named)?;
                }
                continue;
            }
            Change::Insert { slot, cell, .. } => {
                insert_cell(&mut tree, run, *slot, cell, named)?;
                continue;
            }
            Change::Remove { slot, .. } => {
                let slot = slot_of(slot)?;
                released.extend(held(&tree, slot));
                Delta::Remove { slot }
            }
            Change::Hash { slot, kv, hash, .. } => Delta::Hash {
                slot: slot_of(slot)?,
                field: match kv {
                    true => HashField::KeyValue,
                    false => HashField::Value,
                },
                hash,
            },
            Change::Feature { slot, feature, .. } => Delta::Feature {
                slot: slot_of(slot)?,
                feature,
            },
            Change::SetValue { slot, value, .. } => {
                let slot = slot_of(slot)?;
                released.extend(held(&tree, slot));
                Delta::Value {
                    slot,
                    value: cell_value(value, named)?,
                }
            }
            _ => return Err(misfit()),
        };
        delta.apply(&mut tree)?;
        run.push(&delta)?;
    }
    Ok(())
}

/// The entries of the flat leaf `id` that the one change of `work` writes
/// whole, in key order. A value out of line that an entry keeps stays
/// where it is if write-back has placed it, and is carried again by the
/// commit if it is still pending: `attached` gives its bytes.
fn whole_leaf<'c, 'f>(
    id: PageId,
    work: &Work<'c, '_>,
    frame: &'f [u8],
    attached: &dyn Fn(PendingValue) -> Option<&'f [u8]>,
) -> Result<Vec<(&'c [u8], Entry<'c>)>, String> {
    let of = |value: &'c Value| match value {
        Value::Inline(bytes) => Entry::Inline(bytes),
        Value::Out(bytes) => Entry::Log(bytes.clone()),
    };
    let mut entries: BTreeMap<&'c [u8], Entry<'c>> = BTreeMap::new();
    match work.changes[..] {
        [Change::NewFlat { entries: given }] => {
            entries.extend(given.iter().map(|(key, value)| (key.as_slice(), of(value))));
        }
        [Change::Rewrite { entries: given, .. }] => {
            let leaf = leaf_of(frame)?;
            for (key, value) in given {
                let entry = match (value, leaf.get(key)) {
                    (Some(value), _) => of(value),
                    (None, Some(FlatValue::OutOfLine(place))) => match place_of(place)? {
                        ValuePlace::Placed(pointer) => Entry::Keep(pointer),
                        ValuePlace::Pending(pending) => match attached(pending) {
                            Some(bytes) => Entry::Log(Arc::from(bytes)),
                            None => return Err(format!("page {id}: a pending value has no bytes")),
                        },
                    },
                    (None, _) => return Err(format!("page {id}: no value out of line to keep")),
                };
                entries.insert(key.as_slice(), entry);
            }
        }
        _ => {
            return Err(format!(
                "page {id} is written whole and changed in one commit"
            ))
        }
    }
    Ok(entries.into_iter().collect())
}

/// The image of the flat leaf `id` with `entries`, each value out of line
/// at the place it keeps or under the next name of `named`.
fn leaf_image(
    id: PageId,
    entries: &[(&[u8], Entry<'_>)],
    named: &mut Logged,
) -> Result<Vec<u8>, String> {
    let mut places = Vec::with_capacity(entries.len());
    for (_, entry) in entries {
        places.push(match entry {
            Entry::Inline(_) => None,
            Entry::Keep(pointer) => Some(ValuePlace::Placed(*pointer).encode()),
            Entry::Log(_) => match named.pop_front() {
                Some((pending, _)) => Some(ValuePlace::Pending(pending).encode()),
                None => return Err(format!("page {id}: a value of an entry was not logged")),
            },
        });
    }
    let mut leaf = FlatLeafBuilder::new(id, b"", PAGE_SIZE.image_capacity()).map_err(text)?;
    for ((key, entry), place) in entries.iter().zip(&places) {
        let value = match (entry, place) {
            (Entry::Inline(bytes), _) => FlatValue::Inline(bytes),
            (_, Some(place)) => FlatValue::OutOfLine(place),
            (_, None) => return Err(format!("page {id}: a value out of line has no place")),
        };
        leaf.push(key, value).map_err(text)?;
    }
    Ok(leaf.finish())
}

/// The run of the puts and deletes that a commit makes to the flat leaf in
/// `frame`.
fn leaf_changes(
    id: PageId,
    changes: &[&Change],
    frame: &[u8],
    named: &mut Logged,
) -> Result<Vec<u8>, String> {
    let mut keys: BTreeSet<Vec<u8>> = leaf_of(frame)?
        .iter()
        .map(|(key, _)| key.to_vec())
        .collect();
    let mut bytes = Vec::new();
    let mut run = FlatRunWriter::new(id, &mut bytes);
    for change in changes {
        match change {
            Change::Put { key, value, .. } => {
                let place;
                let value = match value {
                    Value::Inline(bytes) => FlatValue::Inline(bytes),
                    Value::Out(_) => {
                        let (pending, _) = named
                            .pop_front()
                            .ok_or_else(|| format!("page {id}: a value was not logged"))?;
                        place = ValuePlace::Pending(pending).encode();
                        FlatValue::OutOfLine(&place)
                    }
                };
                let change = match keys.insert(key.clone()) {
                    true => FlatChange::Insert { key, value },
                    false => FlatChange::Replace { key, value },
                };
                run.push(&change).map_err(text)?;
            }
            Change::Delete { key, .. } => {
                if !keys.remove(key) {
                    return Err(format!("page {id}: no entry to delete"));
                }
                run.push(&FlatChange::Remove { key }).map_err(text)?;
            }
            other => return Err(format!("{other:?} of the flat leaf {id}")),
        }
    }
    run.finish().map_err(text)?;
    Ok(bytes)
}

/// Why a page step was not made.
enum StepError {
    /// The log did not take its record.
    Log(LogError),
    /// Anything else.
    Other(String),
}

impl From<FormatError> for StepError {
    fn from(error: FormatError) -> Self {
        Self::Other(text(error))
    }
}

/// Page steps: what the layers above the store change of a published page
/// without a commit (`transactions.md` PV7).
impl Session {
    /// One page step: the cell or entry of the page in `frame` that holds
    /// `from` gets `to`, if `frame` is still how the current version of the
    /// page is stored, is of the version `found` bore, and no write
    /// transaction holds a private copy of the page. The record is
    /// appended once the checks have passed, and the frame is replaced
    /// only if the log took it.
    fn repoint(
        &self,
        step: &PageStep<'_>,
        frame: &PageRef<'_>,
        found: CommitVersion,
        from: ValuePointer,
        to: ValuePointer,
    ) -> Result<Result<(), Refusal>, StepError> {
        if !step.is_current(frame) || frame.born() != found || step.private_copies() != 0 {
            return Ok(Err(Refusal::Busy));
        }
        let page = step.page();
        let mut blank = self.pool.blank();
        blank.copy_from(frame);
        let change = if is_tree(frame) {
            let tree = TreePage::from_validated(frame, FRAME_FORMAT)?;
            let Some(slot) = tree.find_value_pointer(&from) else {
                return Ok(Err(Refusal::OwnerGone));
            };
            let change = ValueMove::in_tree(page, slot, ValuePlace::Placed(from), to)?;
            let delta = change.delta().ok_or_else(misfit)?;
            delta.apply(&mut TreePageMut::open(&mut blank, PAGE_SIZE, FRAME_FORMAT)?)?;
            change
        } else {
            let leaf = leaf_of(frame).map_err(StepError::Other)?;
            if from.find_in_flat_leaf(&leaf).is_none() {
                return Ok(Err(Refusal::OwnerGone));
            }
            let change = ValueMove::in_flat_leaf(page, ValuePlace::Placed(from), to)?;
            let mut image = Vec::new();
            let capacity = PAGE_SIZE.image_capacity();
            change.apply_to_flat_leaf(frame, FRAME_FORMAT, capacity, &mut image)?;
            put_image(&mut blank[..], &image);
            change
        };
        let mut record = Vec::new();
        change.write(&mut record)?;
        self.wb().append_change(&record).map_err(StepError::Log)?;
        step.replace(frame, blank)
            .map_err(|refusal| StepError::Other(text(refusal)))?;
        Ok(Ok(()))
    }

    /// A reclaiming step, as whoever removes what a dropped subtree left
    /// makes it: cells leave the tree page `page`, or the page is freed
    /// with the latest version. Its record goes to the log under the lock
    /// on the counts and the page's own, and the change is made only if the
    /// log took it.
    ///
    /// `tell` is told what the log decides of the record, when it does
    /// ([`Item`]).
    pub fn reclaim_step(
        &self,
        page: PageId,
        what: &Reclaim,
        tell: &mut dyn FnMut(Item<'_>),
    ) -> Stepped {
        match self.reclaim_with(page, what, tell) {
            Ok(stepped) => stepped,
            Err(error) => Stepped::No(error),
        }
    }

    fn reclaim_with(
        &self,
        page: PageId,
        what: &Reclaim,
        tell: &mut dyn FnMut(Item<'_>),
    ) -> Result<Stepped, String> {
        let refused = |why: &str| Ok(Stepped::Refused(format!("page {page}: {why}")));
        // A page that a writer holds is not looked at any closer.
        if self.pool.page_step(page).private_copies() != 0 {
            return refused("a write transaction holds a copy");
        }
        let Some(frame) = self.current(page).map_err(text)? else {
            return refused("no page");
        };

        // What the page is afterwards is made before any lock is taken.
        let mut released = Vec::new();
        let mut run = Vec::new();
        let after = match what {
            Reclaim::Page => {
                released = pointers_of(&frame)?;
                None
            }
            Reclaim::Cells(slots) => {
                if !is_tree(&frame) {
                    return refused("cells are taken out of a tree page");
                }
                let mut blank = self.pool.blank();
                blank.copy_from(&frame);
                let mut pending = Vec::new();
                {
                    let mut tree =
                        TreePageMut::open(&mut blank, PAGE_SIZE, FRAME_FORMAT).map_err(text)?;
                    let mut records = DeltaRunWriter::new(page, None, &mut run);
                    for &slot in slots {
                        let slot = slot_id(slot)?;
                        let Some(cell) = tree.page().cell(slot) else {
                            return refused("no cell in the slot");
                        };
                        released.extend(cell.value_pointer());
                        pending.extend(cell.pending_value().map(|pending| pending.id()));
                        let delta = Delta::Remove { slot };
                        delta.apply(&mut tree).map_err(text)?;
                        records.push(&delta).map_err(text)?;
                    }
                    records.finish().map_err(text)?;
                }
                for name in pending {
                    blank.detach(name);
                }
                Some(blank)
            }
        };
        released.sort_by_key(|pointer| (pointer.file(), pointer.offset()));

        let counts = self.values.step().map_err(text)?;
        let step = self.pool.page_step(page);
        if step.private_copies() != 0 || !step.is_current(&frame) {
            return refused("the page changed while the step was made ready");
        }
        let latest = self.latest();
        let freed = after.is_none().then_some(latest);
        if freed.is_some() && latest <= frame.born() {
            return refused("a page is not freed at the version that bore it");
        }
        let removed = match &after {
            Some(_) => Some(Removed::Tree(
                DeltaRun::decode(&run, FRAME_FORMAT).map_err(text)?,
            )),
            None => None,
        };
        let mut record = Vec::new();
        write_reclaim(page, removed, freed, released.iter().copied(), &mut record).map_err(text)?;

        tell(Item::Appending(&[]));
        let appended = match self.wb().append_change(&record) {
            Ok(appended) => appended,
            Err(error) => {
                drop(step);
                drop(counts);
                return Ok(match self.settle_after(error) {
                    Ok(reason) => {
                        tell(Item::Gone);
                        Stepped::No(reason)
                    }
                    Err(reason) => Stepped::InDoubt(reason),
                });
            }
        };
        tell(Item::Appended(appended.end));
        if let Some(blank) = after
            && let Err(refusal) = step.replace(&frame, blank)
        {
            return Ok(Stepped::Broken(format!("page {page}: {refusal}")));
        }
        drop(step);
        if let Some(version) = freed
            && let Err(error) = self.push_free(page, version)
        {
            return Ok(Stepped::Broken(format!("page {page}: {error}")));
        }
        let mut change = ValueChange::new();
        released.iter().for_each(|&pointer| change.release(pointer));
        if let Err(error) = counts.apply(&change, latest) {
            return Ok(Stepped::Broken(format!("the counts: {error}")));
        }
        Ok(Stepped::Yes { end: appended.end })
    }

    fn cleaner(&self) -> Cleaner<'_> {
        Cleaner::new(self.wb(), &self.cleaning)
    }

    /// One pass of the cleaner over the segments
    /// (`lignin_store::cleaner`): the chains that hold links no reader
    /// needs are trimmed, live blocks are moved out of the segments that
    /// are mostly dead, and the segments that nothing points into are
    /// deleted once no table checkpoint in force names them.
    pub fn clean_segments(&self) -> Result<Pass, String> {
        self.cleaner().pass().map_err(text)
    }

    /// One pass of the cleaner over the value log: the files that are
    /// empty for every reader are dropped, the closed file with the
    /// largest share of dead bytes is cleaned if the dead space is above
    /// its target, with each owner given the pointer to the copy of its
    /// value in a page step, and the log is made durable before the value
    /// log is asked which files are empty from then on.
    pub fn clean_values(&self) -> Result<ValuePass, String> {
        let owners = Owners {
            session: self,
            log_failed: Mutex::new(None),
        };
        let passed = self.cleaner().pass_values(&owners);
        // A record that the log took back is settled before anything
        // relies on its being gone, with every lock given back.
        if let Some(error) = lock(&owners.log_failed).take() {
            let _ = self.settle_after(error);
        }
        passed.map_err(text)
    }
}

/// The cells and entries of the pages as the owners of values, for the
/// value log's cleaner.
struct Owners<'s> {
    session: &'s Session,
    /// The error of a record that the log did not take.
    log_failed: Mutex<Option<LogError>>,
}

impl ValueOwners for Owners<'_> {
    /// The version that bore the page version that was looked at.
    type Found = CommitVersion;

    fn find(
        &self,
        page: PageId,
        pointer: ValuePointer,
    ) -> Result<Option<CommitVersion>, OwnersError> {
        let Some(frame) = self.session.current(page)? else {
            return Ok(None);
        };
        let holds = pointers_of(&frame)?.contains(&pointer);
        Ok(holds.then(|| frame.born()))
    }

    fn rewrite(
        &self,
        step: PointerRewrite<'_>,
        found: CommitVersion,
    ) -> Result<Rewrite, OwnersError> {
        let session = self.session;
        let (page, from, to) = (step.page(), step.from(), step.to());
        let Some(frame) = session.current(page)? else {
            return Ok(Rewrite::Refused(Refusal::OwnerGone));
        };
        let guard = session.pool.page_step(page);
        match session.repoint(&guard, &frame, found, from, to) {
            Ok(Ok(())) => Ok(Rewrite::Installed(step.install(session.latest()))),
            Ok(Err(refusal)) => Ok(Rewrite::Refused(refusal)),
            Err(StepError::Log(error)) => {
                let said = text(&error);
                *lock(&self.log_failed) = Some(error);
                Err(said.into())
            }
            Err(StepError::Other(error)) => Err(error.into()),
        }
    }
}

/// What write-back is handed for the values of a tree page: a scan reads
/// the cells in the order of their keys.
impl PageValues for Session {
    fn scan_order(&self, frame: &[u8], out: &mut Vec<SlotId>) {
        let Ok(tree) = TreePage::from_validated(frame, FRAME_FORMAT) else {
            return;
        };
        let mut cells: Vec<_> = tree
            .cells()
            .map(|(slot, cell)| (cell.key(), slot))
            .collect();
        cells.sort();
        out.extend(cells.into_iter().map(|(_, slot)| slot));
    }

    fn regroup(
        &self,
        id: PageId,
        to: &mut ValueWriter,
        cover: &mut dyn FnMut(&mut ValueWriter) -> Result<(), FlushError>,
    ) -> Result<u64, RegroupError> {
        let Some(frame) = self.current(id)? else {
            return Ok(0);
        };
        if !is_tree(&frame) {
            return Ok(0);
        }
        let born = frame.born();
        let placed: Vec<ValuePointer> = {
            let tree = TreePage::from_validated(&frame, FRAME_FORMAT)?;
            let mut slots = Vec::new();
            self.scan_order(&frame, &mut slots);
            let pointer = |slot: SlotId| tree.cell(slot)?.value_pointer();
            slots.into_iter().filter_map(pointer).collect()
        };
        drop(frame);
        let back_to_back = placed.windows(2).all(|pair| {
            let (before, after) = (pair[0], pair[1]);
            (after.file(), after.offset()) == (before.file(), before.end(FRAME_FORMAT))
        });
        if back_to_back {
            return Ok(0);
        }
        let mut hold = ValueHold::new();
        let mut buf = Vec::new();
        let mut moves = Vec::new();
        for from in placed {
            let bytes = self.values.read(from, id, &mut buf)?.to_vec();
            moves.push((from, to.append(&mut hold, id, &bytes)?));
        }
        cover(to)?;
        let mut moved = 0;
        for (from, to) in moves {
            let Some(frame) = self.current(id)? else {
                break;
            };
            let counts = self.values.step()?;
            let step = self.pool.page_step(id);
            match self.repoint(&step, &frame, born, from, to) {
                Ok(Ok(())) => {
                    drop(step);
                    counts.apply(&ValueChange::moved(from, to), self.latest())?;
                    moved += 1;
                }
                Ok(Err(_)) => break,
                Err(StepError::Log(error)) => {
                    drop(step);
                    drop(counts);
                    let said = match self.settle_after(error) {
                        Ok(said) | Err(said) => said,
                    };
                    return Err(said.into());
                }
                Err(StepError::Other(error)) => return Err(error.into()),
            }
        }
        Ok(moved)
    }
}

/// Write-back, driven by the session's own thread: the store has no
/// thread of its own, and the campaign runs none beside the committer.
impl Session {
    /// One pass of write-back. Returns whether it moved anything on.
    pub fn pass(&self) -> Result<bool, String> {
        (self.wb().pass().map(|pass| pass.progressed())).map_err(text)
    }

    /// One turn as the background loop makes it: the log is rolled if that
    /// is due, a pass is made that leaves the pages that changed last for
    /// a later turn (`FlushConfig::max_deferred`), and a table checkpoint
    /// is taken if one is due.
    pub fn tick(&self) -> Result<bool, String> {
        (self.wb().tick_deferring())
            .map(|tick| tick.progressed())
            .map_err(text)
    }

    /// Takes a table checkpoint. Returns the version it covers, or `None`
    /// if it was put off.
    pub fn checkpoint(&self) -> Result<Option<u64>, String> {
        self.wb().checkpoint().map(covered).map_err(text)
    }

    /// Makes the log durable, as `flush()` does.
    pub fn sync_log(&self) -> Result<(), String> {
        self.wb().sync_log().map(drop).map_err(text)
    }

    /// Keeps the log's files going: a failed append is cut off, a full
    /// file is turned away from, and the next file is made ready.
    pub fn roll_log(&self) -> Result<bool, String> {
        self.wb().roll_log().map_err(text)
    }

    /// Brings the chains that an earlier session left longer than this
    /// one's thresholds within them.
    pub fn consolidate(&self) -> Result<usize, String> {
        self.wb().consolidate_long_chains().map_err(text)
    }

    /// Ends the session as a store that is closed ends it. Returns the
    /// version its last table checkpoint covers, or `None` if that was put
    /// off.
    pub fn finish(&self) -> Result<Option<u64>, String> {
        // The files that took the cleaner's copies are closed first, so
        // that the session's last manifest names them as they are.
        self.cleaner().finish().map_err(text)?;
        self.wb().finish().map(covered).map_err(text)
    }

    /// How much of the log no table checkpoint covers.
    pub fn log_held(&self) -> u64 {
        self.wb().log_held()
    }
}

#[cfg(test)]
mod tests {
    use lignin_io::{
        sim::{SimConfig, SimVfs},
        Create, OpenMode,
    };

    use super::*;

    fn dir() -> &'static Path {
        Path::new("/db")
    }

    /// A simulated file system with a store that was just made.
    fn new_store() -> Arc<dyn Vfs> {
        let fs = SimVfs::new(SimConfig::new(9));
        fs.create_dir(dir()).expect("a directory");
        fs.sync_dir(Path::new("/")).expect("synced");
        let vfs: Arc<dyn Vfs> = Arc::new(fs);
        Session::create(vfs.clone(), dir()).expect("a store");
        vfs
    }

    fn open(vfs: &Arc<dyn Vfs>) -> Session {
        Session::open(vfs.clone(), dir(), &Tuning::small()).expect("the store opens")
    }

    fn page(raw: u32) -> PageId {
        PageId::new(raw).expect("a page id")
    }

    fn cell(key: &[u8], value: Value) -> Cell {
        Cell {
            key: key.to_vec(),
            feature: vec![1, 2],
            value_hash: [3; 32],
            kv_hash: [4; 32],
            value,
        }
    }

    fn inline(bytes: &[u8]) -> Value {
        Value::Inline(bytes.to_vec())
    }

    fn long(seed: u8, len: usize) -> Value {
        Value::Out((0..len).map(|at| seed.wrapping_add(at as u8)).collect())
    }

    /// Commits `changes`, and returns the ids of the pages it made.
    fn commit(session: &Session, changes: &[Change]) -> Vec<PageId> {
        match session.commit(changes, false, false, &mut |_| {}) {
            Committed::Yes { made, .. } => made,
            other => panic!("the commit was not made: {other:?}"),
        }
    }

    fn refused(session: &Session, changes: &[Change], held: bool) -> String {
        match session.commit(changes, held, false, &mut |_| {}) {
            Committed::No(why) => why,
            other => panic!("the commit was not refused: {other:?}"),
        }
    }

    fn read(session: &Session) -> Pages {
        session
            .read(&BTreeSet::new())
            .expect("the store is at one with itself")
    }

    /// A tree page with two cells and a flat leaf with two entries, each
    /// with one value out of line.
    fn two_pages(session: &Session) -> (PageId, PageId) {
        let made = commit(
            session,
            &[
                Change::NewTree {
                    cells: vec![
                        (1, cell(b"a", inline(b"one"))),
                        (2, cell(b"b", long(1, 700))),
                    ],
                },
                Change::NewFlat {
                    entries: vec![
                        (b"k".to_vec(), inline(b"flat")),
                        (b"m".to_vec(), long(2, 600)),
                    ],
                },
            ],
        );
        (made[0], made[1])
    }

    #[test]
    fn a_session_commits_and_reads_what_the_model_says() {
        let vfs = new_store();
        let session = open(&vfs);
        assert_eq!(session.latest(), CommitVersion::EMPTY);
        assert!(read(&session).is_empty());
        assert!(format!("{session:?}").starts_with("Session"));
        assert!(Arc::ptr_eq(session.vfs(), &vfs));

        fn apply(session: &Session, model: &mut Pages, changes: &[Change]) -> Vec<PageId> {
            let made = commit(session, changes);
            let version = session.latest().get();
            *model = super::super::model::apply(model, changes, &made, version).expect("fits");
            assert_eq!(read(session), *model, "after {changes:?}");
            made
        }
        let mut model = Pages::new();
        let made = apply(
            &session,
            &mut model,
            &[
                Change::NewTree {
                    cells: vec![
                        (1, cell(b"a", inline(b"one"))),
                        (2, cell(b"b", long(1, 700))),
                    ],
                },
                Change::NewFlat {
                    entries: vec![
                        (b"k".to_vec(), inline(b"flat")),
                        (b"m".to_vec(), long(2, 600)),
                    ],
                },
            ],
        );
        let (tree, flat) = (made[0], made[1]);
        apply(
            &session,
            &mut model,
            &[
                Change::Insert {
                    page: tree,
                    slot: 3,
                    cell: cell(b"c", long(3, 5000)),
                },
                Change::Hash {
                    page: tree,
                    slot: 1,
                    kv: true,
                    hash: [7; 32],
                },
                Change::Hash {
                    page: tree,
                    slot: 1,
                    kv: false,
                    hash: [8; 32],
                },
                Change::Feature {
                    page: tree,
                    slot: 2,
                    feature: vec![9; 5],
                },
                Change::Put {
                    page: flat,
                    key: b"n".to_vec(),
                    value: long(4, 300),
                },
                Change::Put {
                    page: flat,
                    key: b"k".to_vec(),
                    value: inline(b"over"),
                },
            ],
        );
        // Write-back places the values, and the pages say the same.
        assert!(session.pass().expect("a pass"));
        assert_eq!(read(&session), model);
        assert_eq!(session.flush_stats().values_placed, 4);
        apply(
            &session,
            &mut model,
            &[
                Change::SetValue {
                    page: tree,
                    slot: 2,
                    value: inline(b"now in line"),
                },
                Change::Remove {
                    page: tree,
                    slot: 3,
                },
                Change::Delete {
                    page: flat,
                    key: b"n".to_vec(),
                },
            ],
        );
        // A leaf that is written whole keeps a value where it lies.
        apply(
            &session,
            &mut model,
            &[Change::Rewrite {
                page: flat,
                entries: vec![(b"m".to_vec(), None), (b"z".to_vec(), Some(long(5, 400)))],
            }],
        );
        // And carries a kept value again while it is still pending.
        apply(
            &session,
            &mut model,
            &[Change::Rewrite {
                page: flat,
                entries: vec![(b"m".to_vec(), None), (b"z".to_vec(), None)],
            }],
        );
        assert_eq!(session.checkpoint().expect("a checkpoint"), Some(5));
        apply(&session, &mut model, &[Change::Free { page: tree }]);
        apply(&session, &mut model, &[Change::Free { page: flat }]);
        assert!(model.is_empty());
        assert!(session.durable().is_some());
        assert!(!session.stopped());
        assert!(session.complaints().is_empty());
        assert_eq!(session.finish().expect("the session ends"), Some(7));
        drop(session);

        // The store that was closed opens with nothing to replay.
        let session = open(&vfs);
        assert_eq!((session.latest().get(), session.report().commits), (7, 0));
        assert_eq!(session.log_held(), 0);
        assert!(read(&session).is_empty());
    }

    #[test]
    fn a_store_that_was_killed_recovers_what_was_committed() {
        let vfs = new_store();
        let session = open(&vfs);
        let (tree, flat) = two_pages(&session);
        let before = read(&session);
        drop(session);
        let session = open(&vfs);
        assert_eq!(session.report().commits, 1);
        assert_eq!(read(&session), before);
        // The pending values came back with their frames, and are placed.
        assert_eq!(session.report().pending_values, 2);
        assert!(session.tick().expect("a turn"));
        assert_eq!(read(&session), before);
        assert_eq!(session.consolidate().expect("consolidated"), 0);
        assert!(!session.roll_log().expect("rolled"));
        session.sync_log().expect("synced");
        let ids: BTreeSet<PageId> = [tree, flat, page(40)].into();
        assert_eq!(session.read(&ids).expect("read"), before);
    }

    #[test]
    fn a_transaction_that_holds_copies_keeps_page_steps_off_its_pages() {
        let vfs = new_store();
        let session = open(&vfs);
        let (tree, flat) = two_pages(&session);
        assert!(session.hold(&[tree]).expect("copied"));
        assert!(session.holds());
        // One transaction holds copies at a time.
        assert!(!session.hold(&[flat]).expect("asked"));
        // The value of the held page waits, and a step on it is refused.
        session.pass().expect("a pass");
        assert_eq!(session.flush_stats().values_placed, 1);
        let reclaimed = session.reclaim_step(tree, &Reclaim::Cells(vec![1]), &mut |_| {});
        assert!(matches!(reclaimed, Stepped::Refused(_)), "{reclaimed:?}");
        let before = read(&session);

        // The held transaction commits a change to its page.
        let mut told = Vec::new();
        let committed = session.commit(
            &[Change::Hash {
                page: tree,
                slot: 1,
                kv: false,
                hash: [6; 32],
            }],
            true,
            false,
            &mut |item| told.push(format!("{item:?}")),
        );
        assert!(
            matches!(
                committed,
                Committed::Yes {
                    version: 2,
                    synced: None,
                    ..
                }
            ),
            "{committed:?}"
        );
        // The session said that the group was about to be appended, with
        // no page made, and then where it ends.
        assert_eq!(told.len(), 2, "{told:?}");
        assert!(
            told[0] == "Appending([])" && told[1].starts_with("Appended("),
            "{told:?}"
        );
        assert!(!session.holds());
        assert_ne!(read(&session), before);
        // Its end placed the value that had waited.
        assert_eq!(session.flush_stats().values_placed, 2);

        // A transaction that rolls back leaves the pages as they were.
        let before = read(&session);
        assert!(session.hold(&[tree, flat]).expect("copied"));
        assert!(session.release());
        assert!(!session.release());
        assert_eq!(read(&session), before);
        // A held page that its commit does not change is given up.
        assert!(session.hold(&[tree, flat]).expect("copied"));
        let committed = session.commit(&[Change::Free { page: flat }], true, false, &mut |_| {});
        assert!(matches!(committed, Committed::Yes { .. }), "{committed:?}");
        assert!(session.hold(&[tree]).expect("the page can be copied again"));
        assert!(session.hold(&[page(77)]).is_ok_and(|held| !held));
        assert!(session.release());
        assert!(session.hold(&[page(77)]).is_err());
        assert!(!session.holds());
    }

    #[test]
    fn a_commit_that_asks_for_it_is_synced_before_it_is_published() {
        let session = Session::open(new_store(), dir(), &Tuning::small()).expect("opened");
        let mut told = Vec::new();
        let entries = vec![(b"k".to_vec(), inline(b"v"))];
        let committed = session.commit(&[Change::NewFlat { entries }], false, true, &mut |item| {
            // The version is published last: not while the log says
            // what became of the group, nor when it was synced.
            assert_eq!(session.latest().get(), 0);
            let durable = session.durable();
            told.push(match item {
                Item::Appending(made) => format!("appending {}", made.len()),
                Item::Appended(end) => format!("appended, durable {}", durable >= Some(end)),
                Item::Gone => "gone".to_string(),
                Item::Synced => format!("synced, durable {}", durable == session.log_end()),
            });
        });
        let Committed::Yes {
            version: 1,
            end,
            synced: Some(Ok(())),
            ..
        } = committed
        else {
            panic!("the commit was not made and synced: {committed:?}");
        };
        assert_eq!(
            told,
            [
                "appending 1",
                "appended, durable false",
                "synced, durable true"
            ]
        );
        assert!(session.durable() >= Some(end));
        assert_eq!(session.latest().get(), 1);
    }

    #[test]
    fn a_writer_that_holds_copies_pins_the_version_it_was_begun_at() {
        let session = Session::open(new_store(), dir(), &Tuning::small()).expect("opened");
        let made = commit(
            &session,
            &[
                Change::NewFlat { entries: vec![] },
                Change::NewFlat { entries: vec![] },
            ],
        );
        let (held, freed) = (made[0], made[1]);
        let state = |session: &Session| {
            let states = session.page_states();
            let found = states.iter().find(|(page, _)| *page == freed);
            found.map(|(_, state)| *state)
        };
        // A writer is begun at version 1, and a commit beside it frees a
        // page at version 2: the writer can still read the page, so its id
        // is not given out again.
        assert!(session.hold(&[held]).expect("copied"));
        commit(&session, &[Change::Free { page: freed }]);
        assert_eq!(state(&session), Some(PageState::Retired));
        let other = commit(&session, &[Change::NewFlat { entries: vec![] }])[0];
        assert_ne!(other, freed);
        // The writer ends, and with it the pin.
        assert!(session.release());
        assert_ne!(state(&session), Some(PageState::Retired));

        // The same of a writer that commits.
        let again = commit(&session, &[Change::NewFlat { entries: vec![] }])[0];
        assert!(session.hold(&[held]).expect("copied"));
        commit(&session, &[Change::Free { page: again }]);
        let retired = |session: &Session| {
            let states = session.page_states();
            states
                .iter()
                .filter(|(_, state)| *state == PageState::Retired)
                .count()
        };
        assert_eq!(retired(&session), 1);
        let put = Change::Put {
            page: held,
            key: b"k".to_vec(),
            value: inline(b"v"),
        };
        let committed = session.commit(&[put], true, false, &mut |_| {});
        assert!(matches!(committed, Committed::Yes { .. }), "{committed:?}");
        assert_eq!(retired(&session), 0);
    }

    #[test]
    fn a_commit_that_does_not_fit_its_pages_leaves_nothing() {
        let vfs = new_store();
        let session = open(&vfs);
        let (tree, flat) = two_pages(&session);
        let before = read(&session);
        let hash = Change::Hash {
            page: tree,
            slot: 1,
            kv: true,
            hash: [1; 32],
        };
        let put = Change::Put {
            page: flat,
            key: b"q".to_vec(),
            value: inline(b"v"),
        };
        let whole = Change::Rewrite {
            page: flat,
            entries: vec![(b"q".to_vec(), Some(inline(b"v")))],
        };
        let cases: Vec<(Vec<Change>, &str)> = vec![
            (
                vec![Change::Free { page: tree }, hash.clone()],
                "freed and changed",
            ),
            (vec![Change::Free { page: page(50) }], "no page"),
            (
                vec![Change::Put {
                    page: tree,
                    key: vec![],
                    value: inline(b""),
                }],
                "of the tree page",
            ),
            (
                vec![Change::Remove {
                    page: flat,
                    slot: 1,
                }],
                "of the flat leaf",
            ),
            (
                vec![whole.clone(), put.clone()],
                "written whole and changed",
            ),
            (
                vec![Change::Rewrite {
                    page: flat,
                    entries: vec![(b"k".to_vec(), None)],
                }],
                "no value out of line to keep",
            ),
            (
                vec![Change::Delete {
                    page: flat,
                    key: b"none".to_vec(),
                }],
                "no entry to delete",
            ),
            (
                vec![Change::Remove {
                    page: tree,
                    slot: 9,
                }],
                "the run of page",
            ),
            (
                vec![Change::Insert {
                    page: tree,
                    slot: 0,
                    cell: cell(b"x", inline(b"")),
                }],
                "the run of page",
            ),
        ];
        for (changes, why) in cases {
            let said = refused(&session, &changes, false);
            assert!(said.contains(why), "{changes:?}: {said}");
            assert_eq!(read(&session), before, "{changes:?}");
            assert_eq!(session.latest().get(), 1);
        }
        assert!(refused(&session, &[hash], true).contains("no transaction holds a copy"));
        // The ids that the refused commits took are free again.
        let made = commit(&session, &[Change::NewFlat { entries: vec![] }]);
        assert_eq!(made, [page(3)]);
    }

    #[test]
    fn a_reclaiming_step_takes_cells_out_and_frees_pages() {
        let vfs = new_store();
        let session = open(&vfs);
        let (tree, flat) = two_pages(&session);
        // A page is not freed at the version that bore it, and a flat leaf
        // gives no cells.
        for (id, what) in [
            (tree, Reclaim::Page),
            (flat, Reclaim::Cells(vec![1])),
            (tree, Reclaim::Cells(vec![7])),
            (page(60), Reclaim::Page),
        ] {
            let stepped = session.reclaim_step(id, &what, &mut |_| panic!("nothing is appended"));
            assert!(matches!(stepped, Stepped::Refused(_)), "{stepped:?}");
        }
        let slot_zero = session.reclaim_step(tree, &Reclaim::Cells(vec![0]), &mut |_| {});
        assert!(matches!(slot_zero, Stepped::No(_)), "{slot_zero:?}");
        session.pass().expect("a pass");

        // The cell with the placed value leaves, and its value is counted
        // out.
        let born = |session: &Session| match &*read(session)[&tree] {
            Page::Tree { born, .. } | Page::Flat { born, .. } => *born,
        };
        let before = born(&session);
        let mut told = Vec::new();
        let stepped = session.reclaim_step(tree, &Reclaim::Cells(vec![2]), &mut |item| {
            told.push(matches!(item, Item::Appending([]) | Item::Appended(_)));
        });
        let Stepped::Yes { end } = stepped else {
            panic!("the step was not made: {stepped:?}");
        };
        // The session said that the record was about to be appended, and
        // then where it ends.
        assert_eq!(told, [true, true]);
        assert!(session.log_end() >= Some(end));
        let pages = read(&session);
        let Page::Tree {
            cells, departures, ..
        } = &*pages[&tree]
        else {
            panic!("a tree page");
        };
        assert_eq!(cells.keys().copied().collect::<Vec<_>>(), [1]);
        // The cell has left the page, which is born when it was: a step is
        // no commit.
        assert_eq!((*departures, born(&session)), (1, before));

        // The pages are freed once a later commit has made the latest
        // version: here one that makes a third page.
        let other = commit(&session, &[Change::NewFlat { entries: vec![] }])[0];
        for id in [tree, flat] {
            let stepped = session.reclaim_step(id, &Reclaim::Page, &mut |_| {});
            assert!(matches!(stepped, Stepped::Yes { .. }), "{stepped:?}");
        }
        let left: Vec<PageId> = read(&session).keys().copied().collect();
        assert_eq!(left, [other]);
        // The chains of the freed pages are trimmed away, and a killed
        // store recovers the steps from their records.
        session.checkpoint().expect("a checkpoint");
        let trimmed = |session: &Session| session.clean_segments().expect("a pass").links_trimmed;
        assert!(trimmed(&session) >= 2);
        assert_eq!(trimmed(&session), 0);
        drop(session);
        let session = open(&vfs);
        assert_eq!(session.latest().get(), 2);
        let left: Vec<PageId> = read(&session).keys().copied().collect();
        assert_eq!(left, [other]);
    }

    #[test]
    fn the_cleaner_moves_the_live_values_of_a_file_and_the_file_is_dropped() {
        let vfs = new_store();
        let tuning = Tuning {
            value_file_len: 1500,
            regroup: false,
            cleaner: CleanerConfig {
                value_dead_percent: 0,
                ..Tuning::small().cleaner
            },
            ..Tuning::small()
        };
        let session = Session::open(vfs.clone(), dir(), &tuning).expect("the store opens");
        let made = commit(
            &session,
            &[
                Change::NewTree {
                    cells: (1..=4)
                        .map(|slot| (slot, cell(b"t", long(slot as u8, 600))))
                        .collect(),
                },
                Change::NewFlat {
                    entries: vec![(b"k".to_vec(), long(9, 600))],
                },
            ],
        );
        let (tree, flat) = (made[0], made[1]);
        session.pass().expect("a pass");
        let moved = |pass: &ValuePass| pass.cleaned.map_or(0, |(_, cleaned)| cleaned.moved);
        assert_eq!(moved(&session.clean_values().expect("a pass")), 0);
        // Two of the three values of the first file die: the file is
        // closed, and has a live value and dead bytes.
        commit(
            &session,
            &[
                Change::Remove {
                    page: tree,
                    slot: 1,
                },
                Change::Remove {
                    page: tree,
                    slot: 2,
                },
            ],
        );
        let before = read(&session);
        // A page that a transaction holds a copy of keeps its pointer: the
        // copy of the value is dead space.
        assert!(session.hold(&[tree, flat]).expect("copied"));
        assert_eq!(moved(&session.clean_values().expect("a pass")), 0);
        assert_eq!(read(&session), before);
        assert!(session.release());
        // The file is set aside for a while, cleaned by a later pass, and
        // dropped once the log has made the rewrite durable.
        let (mut all, mut dropped) = (0, 0);
        for _ in 0..24 {
            let pass = session.clean_values().expect("a pass");
            all += moved(&pass);
            dropped += pass.dropped.len();
            assert_eq!(read(&session), before);
        }
        assert!(all >= 1 && dropped >= 1, "{all} moved, {dropped} dropped");
        drop(session);
        let session = open(&vfs);
        assert_eq!(read(&session), before);
    }

    #[test]
    fn write_back_regroups_the_values_of_a_page_in_the_order_of_its_keys() {
        let vfs = new_store();
        let session = open(&vfs);
        // Three values that are placed one commit after another, in
        // another order than their keys have.
        let made = commit(
            &session,
            &[Change::NewTree {
                cells: vec![(1, cell(b"c", long(1, 300)))],
            }],
        );
        let tree = made[0];
        session.pass().expect("a pass");
        for (slot, key) in [(2, b"a"), (3, b"b")] {
            commit(
                &session,
                &[Change::Insert {
                    page: tree,
                    slot,
                    cell: cell(key, long(slot as u8, 300)),
                }],
            );
            session.pass().expect("a pass");
        }
        let before = read(&session);
        assert_eq!(session.flush_stats().values_regrouped, 0);
        // The chain is as long as it may be: the next change is written as
        // a base image, and the values are regrouped for it.
        for byte in 0..3 {
            commit(
                &session,
                &[Change::Hash {
                    page: tree,
                    slot: 1,
                    kv: false,
                    hash: [byte; 32],
                }],
            );
            session.pass().expect("a pass");
        }
        assert_eq!(session.flush_stats().values_regrouped, 3);
        let mut slots = Vec::new();
        let frame = session.current(tree).expect("read").expect("the page");
        session.scan_order(&frame, &mut slots);
        assert_eq!(
            slots.iter().map(|slot| slot.get()).collect::<Vec<_>>(),
            [2, 3, 1]
        );
        session.scan_order(&[0; 16], &mut slots);
        assert_eq!(slots.len(), 3);
        drop(frame);
        let mut after = before.clone();
        after = super::super::model::apply(
            &after,
            &[Change::Hash {
                page: tree,
                slot: 1,
                kv: false,
                hash: [2; 32],
            }],
            &[],
            session.latest().get(),
        )
        .expect("fits");
        assert_eq!(read(&session), after);
    }

    #[test]
    fn a_pin_holds_back_what_a_reader_can_still_reach() {
        let vfs = new_store();
        let session = open(&vfs);
        let (tree, _) = two_pages(&session);
        session.checkpoint().expect("a checkpoint");
        assert!(session.pin());
        assert!(!session.pin());
        commit(&session, &[Change::Free { page: tree }]);
        // The pinned version can still reach the page: its chain stays,
        // and its id is not handed out.
        let trimmed = |session: &Session| session.clean_segments().expect("a pass").links_trimmed;
        assert_eq!(trimmed(&session), 0);
        let made = commit(&session, &[Change::NewFlat { entries: vec![] }]);
        assert_eq!(made, [page(3)]);
        assert!(session.unpin());
        assert!(!session.unpin());
        assert!(trimmed(&session) >= 1);
        session.reclaim();
        let made = commit(&session, &[Change::NewFlat { entries: vec![] }]);
        assert_eq!(made, [tree]);
    }

    #[test]
    fn a_directory_with_no_store_or_with_a_damaged_one_is_not_opened() {
        let fs = SimVfs::new(SimConfig::new(9));
        fs.create_dir(dir()).expect("a directory");
        let vfs: Arc<dyn Vfs> = Arc::new(fs);
        let none = Session::open(vfs.clone(), dir(), &Tuning::small()).expect_err("no store");
        assert_eq!(none, OpenError::NoStore);
        assert_eq!(none.to_string(), "the directory has no manifest");
        // A lock that is held keeps a second session out.
        let held = vfs.lock(&dir().join(LOCK_FILE)).expect("locked");
        let locked = Session::open(vfs.clone(), dir(), &Tuning::small()).expect_err("locked");
        assert!(
            matches!(&locked, OpenError::Failed { damage: false, .. }),
            "{locked}"
        );
        assert!(locked.to_string().starts_with("the store did not open"));
        drop(held);

        let manifest = dir().join(ManifestStore::FILE_NAME);
        let file = vfs
            .open(&manifest, OpenMode::Write(Create::New))
            .expect("created");
        file.write_at(b"not a manifest at all", 0).expect("written");
        drop(file);
        let damaged = Session::open(vfs.clone(), dir(), &Tuning::small()).expect_err("damaged");
        assert!(
            matches!(&damaged, OpenError::Failed { damage: true, .. }),
            "{damaged}"
        );
        assert!(damaged.to_string().starts_with("the store is damaged"));
        assert!(Session::create(vfs, dir()).is_err());
    }

    #[test]
    fn a_store_that_is_not_at_one_with_itself_is_told() {
        let vfs = new_store();
        let session = open(&vfs);
        let (tree, flat) = two_pages(&session);
        session.pass().expect("a pass");
        session.verify().expect("the checker finds the store sound");
        // An id that is in use and names no page.
        let stray = session.allocator.allocate().expect("an id");
        let said = session.read(&BTreeSet::new()).expect_err("a private id");
        assert!(said.contains("is private and names no page"), "{said}");
        session.allocator.publish(stray).expect("published");
        let said = session.read(&BTreeSet::new()).expect_err("a live id");
        assert!(said.contains("is live and names no page"), "{said}");
        // The engine's checker says the same of it, in its own words.
        let said = session.verify().expect_err("a live id");
        assert!(said.starts_with("the invariant checker: "), "{said}");
        session
            .allocator
            .free(stray, session.latest())
            .expect("freed");
        read(&session);

        // A page that is read and whose id is not in use.
        session
            .allocator
            .free(tree, session.latest())
            .expect("freed");
        let said = session.read(&BTreeSet::new()).expect_err("a retired id");
        assert!(said.contains("is read and its id is retired"), "{said}");
        drop(session);

        // Counts that are not the pointers the pages hold.
        let session = open(&vfs);
        let frame = session.current(flat).expect("read").expect("the page");
        let pointer = pointers_of(&frame).expect("a leaf")[0];
        drop(frame);
        let mut change = ValueChange::new();
        change.release(pointer);
        session
            .values
            .apply(&change, session.latest())
            .expect("counted");
        let said = session.read(&BTreeSet::new()).expect_err("a count too few");
        assert!(said.contains("the counts of live values are"), "{said}");
        let said = session.verify().expect_err("a count too few");
        assert!(said.starts_with("the invariant checker: "), "{said}");
        session
            .values
            .apply(&change, session.latest())
            .expect_err("no value to release");
        let said = session
            .read(&BTreeSet::new())
            .expect_err("a count found wrong");
        assert!(said.contains("was found wrong"), "{said}");
    }
}
