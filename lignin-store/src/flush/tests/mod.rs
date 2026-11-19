//! Tests of write-back, on a store with what the tasks after this one
//! build reduced to what write-back meets of it: a commit that writes one
//! log group, tells write-back, installs its copies and is published.

mod acceptance;
mod chains;
mod checkpoint;
mod errors;
mod values;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use lignin_format::{
    delta::Delta,
    manifest::Manifest,
    tree_page::{CellContent, CellValue, HashField, TreeCell, TreePage, TreePageMut},
    value_log::{PendingValue, ValuePointer},
    wal::GroupWriter,
    CommitVersion, FormatVersion, PageId, PageSize, SlotId,
};
use lignin_io::{StdVfs, Vfs};
use tempfile::TempDir;

use super::*;
use crate::{
    alloc::{Epoch, Pins},
    cache::{BufferPool, OwnerId, PageRef, PoolConfig, PrivateRef},
    load::{LoadBuffers, LoadError, PageLoader, FRAME_FORMAT},
    segment::{ManifestStore, SegmentStore},
    table::{Link, LinkKind, PageTable, TableCheckpoints},
    values::{ValueChange, ValueStore},
    wal::{LogOptions, LogReader, LogStore},
};

const V1: FormatVersion = FormatVersion::V1;

pub(super) fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

pub(super) fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("a page id")
}

pub(super) fn slot(raw: u16) -> SlotId {
    SlotId::new(raw).expect("a slot")
}

/// Who publishes and who pins, for a store with one snapshot at most and
/// no operation that outlives a call.
#[derive(Debug)]
pub(super) struct Versions {
    latest: AtomicU64,
    pin: AtomicU64,
    epoch: AtomicU64,
    guards: AtomicU64,
}

impl Versions {
    fn new() -> Self {
        Self {
            latest: AtomicU64::new(0),
            pin: AtomicU64::new(u64::MAX),
            epoch: AtomicU64::new(0),
            guards: AtomicU64::new(0),
        }
    }

    /// Pins `version`, as a snapshot does.
    pub(super) fn pin(&self, version: CommitVersion) {
        self.pin.store(version.get(), Ordering::SeqCst);
    }

    pub(super) fn unpin(&self) {
        self.pin.store(u64::MAX, Ordering::SeqCst);
    }

    /// How many times write-back read blocks under an operation guard.
    pub(super) fn guards(&self) -> u64 {
        self.guards.load(Ordering::SeqCst)
    }
}

impl Pins for Versions {
    fn oldest_pin(&self) -> CommitVersion {
        let latest = self.latest.load(Ordering::SeqCst);
        version(self.pin.load(Ordering::SeqCst).min(latest))
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
        version(self.latest.load(Ordering::SeqCst))
    }

    fn guarded(&self, work: &mut dyn FnMut()) {
        self.guards.fetch_add(1, Ordering::SeqCst);
        work();
    }
}

/// The value of a cell that a test writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Val {
    /// Stored in the cell.
    Inline(Vec<u8>),
    /// Too long for its cell: the commit carries it in its group, and the
    /// cell holds a pending value until write-back has placed it.
    Out(Vec<u8>),
}

/// One change of a commit to a tree page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Op {
    /// A new cell. A page that no commit has written yet is made by its
    /// first insert.
    Insert {
        page: PageId,
        slot: SlotId,
        key: Vec<u8>,
        value: Val,
    },
    /// The value hash of a cell becomes 32 times `byte`.
    Hash { page: PageId, slot: SlotId, byte: u8 },
    /// A cell leaves.
    Remove { page: PageId, slot: SlotId },
    /// The value of a cell is replaced.
    Value {
        page: PageId,
        slot: SlotId,
        value: Val,
    },
    /// The page is freed.
    Free { page: PageId },
}

impl Op {
    fn page(&self) -> PageId {
        match self {
            Self::Insert { page, .. }
            | Self::Hash { page, .. }
            | Self::Remove { page, .. }
            | Self::Value { page, .. }
            | Self::Free { page } => *page,
        }
    }

    fn out(&self) -> Option<&[u8]> {
        match self {
            Self::Insert {
                value: Val::Out(bytes),
                ..
            }
            | Self::Value {
                value: Val::Out(bytes),
                ..
            } => Some(bytes),
            _ => None,
        }
    }
}

pub(super) fn insert(at: (u32, u16), key: &[u8], value: Val) -> Op {
    Op::Insert {
        page: page(at.0),
        slot: slot(at.1),
        key: key.to_vec(),
        value,
    }
}

pub(super) fn hash(at: (u32, u16), byte: u8) -> Op {
    Op::Hash {
        page: page(at.0),
        slot: slot(at.1),
        byte,
    }
}

pub(super) fn inline(bytes: &[u8]) -> Val {
    Val::Inline(bytes.to_vec())
}

/// `len` bytes that do not compress.
pub(super) fn noise(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

/// A value of `len` bytes that no cell of the smallest page holds.
pub(super) fn long(seed: u8, len: usize) -> Val {
    Val::Out((0..len).map(|at| seed.wrapping_add(at as u8)).collect())
}

/// What a cell says, with its value as a reader gets it: wherever it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Cell {
    slot: u16,
    key: Vec<u8>,
    value_hash: [u8; 32],
    value: Vec<u8>,
}

/// What every version of every page says, as the commits left it.
#[derive(Debug, Default)]
struct Model {
    pages: BTreeMap<PageId, Vec<(u64, Option<Vec<Cell>>)>>,
}

impl Model {
    fn at(&self, page: PageId, at: CommitVersion) -> Option<(u64, &[Cell])> {
        let versions = self.pages.get(&page)?;
        let (born, cells) = versions.iter().rev().find(|(born, _)| *born <= at.get())?;
        cells.as_deref().map(|cells| (*born, cells))
    }
}

/// What a cell stores for `value`, which is pending under `pending` if it
/// is out of line.
fn cell_value<'a>(value: &'a Val, pending: &Option<PendingValue>) -> CellValue<'a> {
    match (value, pending) {
        (Val::Inline(bytes), _) => CellValue::Inline(bytes),
        (Val::Out(_), Some(pending)) => CellValue::Pending(*pending),
        (Val::Out(_), None) => unreachable!("a value out of line is in the group"),
    }
}

/// A write transaction: the private copies it has made so far.
pub(super) struct Txn<'d> {
    db: &'d Db,
    owner: OwnerId,
    copies: BTreeMap<PageId, (PrivateRef<'d>, bool)>,
}

/// A store in a directory.
pub(super) struct Db {
    _dir: Option<TempDir>,
    pub(super) vfs: Arc<dyn Vfs>,
    pub(super) path: PathBuf,
    pub(super) size: PageSize,
    pub(super) manifest: Mutex<ManifestStore>,
    pub(super) segments: SegmentStore,
    pub(super) values: ValueStore,
    pub(super) log: LogStore,
    pub(super) checkpoints: TableCheckpoints,
    pub(super) table: PageTable,
    pub(super) pool: BufferPool,
    pub(super) versions: Versions,
    pub(super) state: FlushState,
    owners: AtomicU64,
    model: Mutex<Model>,
}

impl Db {
    pub(super) const SIZE: PageSize = PageSize::MIN;

    /// A new store with the default thresholds.
    pub(super) fn new() -> Self {
        Self::with(FlushConfig::new(Self::SIZE))
    }

    /// A new store in a directory of its own, in memory where there is
    /// such a place: the syncs of a store are most of what these tests
    /// would wait for.
    pub(super) fn with(config: FlushConfig) -> Self {
        let memory = Path::new("/dev/shm");
        let dir = match memory.is_dir() {
            true => tempfile::tempdir_in(memory),
            false => tempfile::tempdir(),
        }
        .expect("a directory");
        let path = dir.path().to_path_buf();
        let mut db = Self::create(Arc::new(StdVfs), &path, config);
        db._dir = Some(dir);
        db
    }

    /// A new store in the directory `path` of `vfs`, which exists.
    pub(super) fn create(vfs: Arc<dyn Vfs>, path: &Path, config: FlushConfig) -> Self {
        let size = Self::SIZE;
        let empty = Manifest::new(size, Default::default(), None, Vec::new()).expect("a manifest");
        let mut manifest = ManifestStore::create(vfs.clone(), path, empty).expect("a store");
        let segments = SegmentStore::open(&manifest).expect("segments");
        let values = ValueStore::open(&manifest, &[]).expect("a value log");
        let checkpoints = TableCheckpoints::open(&manifest).expect("checkpoints");
        let end = LogReader::new(vfs.clone(), path, manifest.manifest().expect("a manifest"))
            .into_end()
            .expect("an empty log");
        let log = LogStore::open(&manifest, &end, LogOptions::default()).expect("a log");
        log.roll(&mut manifest).expect("a log file");
        let pool = BufferPool::new(PoolConfig {
            page_size: size,
            capacity: 4 << 20,
        })
        .expect("a pool");
        let db = Self {
            _dir: None,
            vfs,
            path: path.to_path_buf(),
            size,
            manifest: Mutex::new(manifest),
            segments,
            values,
            log,
            checkpoints,
            table: PageTable::new(),
            pool,
            versions: Versions::new(),
            state: FlushState::new(config),
            owners: AtomicU64::new(0),
            model: Mutex::default(),
        };
        db.wb().adopt(None).expect("the whole log is kept");
        db
    }

    pub(super) fn wb(&self) -> WriteBack<'_> {
        let stores = Stores {
            pool: &self.pool,
            table: &self.table,
            segments: &self.segments,
            values: &self.values,
            log: &self.log,
            checkpoints: &self.checkpoints,
            manifest: &self.manifest,
            versions: &self.versions,
        };
        WriteBack::new(stores, &self.state).expect("write-back")
    }

    pub(super) fn latest(&self) -> CommitVersion {
        Published::latest(&self.versions)
    }

    pub(super) fn stats(&self) -> FlushStats {
        self.state.stats()
    }

    fn loader<'s>(&'s self, pool: &'s BufferPool) -> PageLoader<'s> {
        PageLoader::new(pool, &self.table, &self.segments, self.size).expect("a loader")
    }

    /// The page as a reader at `at` gets it from this store's pool.
    pub(super) fn frame(&self, page: PageId, at: CommitVersion) -> Result<PageRef<'_>, LoadError> {
        self.loader(&self.pool).get(page, at, &mut LoadBuffers::new())
    }

    /// The chain of `page`, oldest link first.
    pub(super) fn chain(&self, page: PageId) -> Vec<Link> {
        let mut links = Vec::new();
        self.table.chain(page, &mut links);
        links
    }

    /// The kinds of the links of `page`'s chain, as a string: `B` for a
    /// base image, `d` for a run of deltas, `F` for a free link.
    pub(super) fn shape(&self, page: PageId) -> String {
        self.chain(page)
            .iter()
            .map(|link| match link.kind {
                LinkKind::Base(_) => 'B',
                LinkKind::Delta(_) => 'd',
                LinkKind::Free => 'F',
            })
            .collect()
    }

    /// What the cells of the tree page in `frame` say, with each value read
    /// from where it is: the cell, the frame, or the value log.
    fn cells(&self, id: PageId, frame: &[u8], attached: Option<&PageRef<'_>>) -> Vec<Cell> {
        let tree = TreePage::from_validated(frame, FRAME_FORMAT).expect("a tree page");
        assert_eq!(tree.id(), id);
        let mut buf = Vec::new();
        tree.cells()
            .map(|(slot, cell)| {
                let value = match cell.value() {
                    CellValue::Inline(bytes) => bytes.to_vec(),
                    CellValue::OutOfLine(pointer) => {
                        let value = self.values.read(pointer, id, &mut buf);
                        value.expect("the value is in the value log").to_vec()
                    }
                    CellValue::Pending(pending) => attached
                        .and_then(|frame| frame.attached(pending.id()))
                        .expect("the bytes of a pending value are with its frame")
                        .to_vec(),
                };
                assert_eq!(value.len(), cell.value_len() as usize);
                Cell {
                    slot: slot.get(),
                    key: cell.key().to_vec(),
                    value_hash: *cell.value_hash(),
                    value,
                }
            })
            .collect()
    }

    /// The pending values of the current version of `page`.
    pub(super) fn pending(&self, page: PageId) -> Vec<(SlotId, PendingValue)> {
        let frame = self.frame(page, self.latest()).expect("the page");
        let tree = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
        tree.pending_values().collect()
    }

    /// The value pointers the cells of the current version of `page` hold.
    pub(super) fn pointers(&self, page: PageId) -> Vec<ValuePointer> {
        let frame = self.frame(page, self.latest()).expect("the page");
        let tree = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
        tree.cells()
            .filter_map(|(_, cell)| cell.value_pointer())
            .collect()
    }

    pub(super) fn begin(&self) -> Txn<'_> {
        Txn {
            db: self,
            owner: OwnerId::new(self.owners.fetch_add(1, Ordering::SeqCst) + 1),
            copies: BTreeMap::new(),
        }
    }

    /// A transaction that makes `ops` and commits.
    pub(super) fn commit(&self, ops: &[Op]) -> CommitVersion {
        self.begin().commit(ops)
    }

    /// Checks every version from the oldest a reader can ask for to the
    /// latest, as a reader gets it through this store's pool.
    pub(super) fn check_warm(&self) {
        self.check(&self.pool, false);
    }

    /// The same, through an empty pool: every page comes from its
    /// segments, so every version must have been written back, and no
    /// value may be pending.
    pub(super) fn check_cold(&self) {
        let pool = BufferPool::new(PoolConfig {
            page_size: self.size,
            capacity: 4 << 20,
        });
        self.check(&pool.expect("a pool"), true);
    }

    fn check(&self, pool: &BufferPool, cold: bool) {
        let model = self.model.lock().expect("the model");
        let loader = self.loader(pool);
        let mut buffers = LoadBuffers::new();
        let oldest = self.versions.oldest_pin().get();
        for at in (oldest..=self.latest().get()).map(version) {
            for &id in model.pages.keys() {
                let expected = model.at(id, at);
                match loader.get(id, at, &mut buffers) {
                    Ok(frame) => {
                        let (born, cells) = expected.unwrap_or_else(|| {
                            panic!("page {id} has no version at {at} and was read")
                        });
                        assert_eq!(frame.born().get(), born, "page {id} at version {at}");
                        let found = self.cells(id, &frame, (!cold).then_some(&frame));
                        assert_eq!(found, cells, "page {id} at version {at}");
                        if cold {
                            let tree = TreePage::from_validated(&frame, FRAME_FORMAT);
                            assert!(!tree.expect("a tree page").has_pending_value());
                        }
                    }
                    Err(LoadError::NoPage { .. }) => {
                        assert!(expected.is_none(), "page {id} at version {at} was not found");
                    }
                    Err(error) => panic!("page {id} at version {at}: {error}"),
                }
            }
        }
    }

    /// Checks that no chain is longer than the thresholds allow: for every
    /// link, the runs between it and the base image under it.
    pub(super) fn check_chains(&self) {
        let config = self.state.config();
        let pages: Vec<PageId> = self.model.lock().expect("the model").pages.keys().copied().collect();
        for id in pages {
            let (mut deltas, mut bytes) = (0, 0);
            for link in self.chain(id) {
                match link.kind {
                    LinkKind::Delta(location) => {
                        deltas += 1;
                        bytes += location.len as usize;
                    }
                    LinkKind::Base(_) | LinkKind::Free => (deltas, bytes) = (0, 0),
                }
                assert!(deltas <= config.max_chain_deltas, "page {id}: {deltas} runs");
                assert!(bytes <= config.max_chain_bytes, "page {id}: {bytes} bytes of runs");
            }
        }
    }
}

impl<'d> Txn<'d> {
    /// Makes the private copy of `id` now, as a write of the transaction
    /// to that page does. Returns whether the page is new.
    pub(super) fn copy(&mut self, id: PageId) -> bool {
        let Db { pool, size, .. } = self.db;
        if let Some((_, created)) = self.copies.get(&id) {
            return *created;
        }
        let latest = self.db.latest();
        let exists = self.db.frame(id, latest);
        let (copy, created) = match exists {
            Ok(frame) => {
                let copy = pool.make_private(self.owner, id, latest);
                drop(frame);
                (copy.expect("the page was just loaded"), false)
            }
            Err(LoadError::NoPage { .. }) => {
                let copy = pool.new_private(self.owner, id).expect("a new page");
                TreePageMut::init(&mut copy.write().expect("private"), *size, id, 0)
                    .expect("an empty page");
                (copy, true)
            }
            Err(error) => panic!("page {id}: {error}"),
        };
        self.copies.insert(id, (copy, created));
        created
    }

    /// Gives the copies up: nothing of the transaction is left.
    pub(super) fn roll_back(self) {
        for (copy, _) in self.copies.into_values() {
            self.db.pool.discard(&copy).expect("discarded");
        }
    }

    /// Makes `ops` and commits: one group in the log, with the values, the
    /// runs of the pages in ascending order and the pointers released;
    /// write-back is told; then the copies are installed, the freed pages
    /// freed and the version published (`transactions.md` CM3).
    pub(super) fn commit(mut self, ops: &[Op]) -> CommitVersion {
        let db = self.db;
        let ids: BTreeSet<PageId> = ops.iter().map(Op::page).collect();
        let freed: BTreeSet<PageId> = ops
            .iter()
            .filter_map(|op| matches!(op, Op::Free { .. }).then(|| op.page()))
            .collect();
        for &id in &ids {
            self.copy(id);
        }

        // The counts lock keeps the page steps and the marks out of the
        // log between the place and the group (`wal`, "Locks").
        let counts = db.values.step().expect("the counts");
        let at = db.log.next_group().expect("a place for the group");
        let mut bytes = Vec::new();
        let mut group = GroupWriter::new(at.version(), at.address(), &mut bytes).expect("a group");
        let pending: Vec<Option<PendingValue>> = ops
            .iter()
            .map(|op| Some(group.value(op.page(), op.out()?).expect("a value")))
            .collect();

        let mut released = Vec::new();
        for &id in &ids {
            let (copy, created) = &self.copies[&id];
            let mut latch = copy.write().expect("private");
            if freed.contains(&id) {
                let tree = TreePage::from_validated(&latch, FRAME_FORMAT).expect("a tree page");
                released.extend(tree.cells().filter_map(|(_, cell)| cell.value_pointer()));
                group.freed(id).expect("a freed page");
                continue;
            }
            let mut attach = Vec::new();
            let run = group.tree_run_with(id, created.then_some(0), |run| {
                let mut tree = TreePageMut::open(&mut latch, db.size, FRAME_FORMAT)?;
                for (op, pending) in ops.iter().zip(&pending).filter(|(op, _)| op.page() == id) {
                    let placed = |slot: &SlotId| tree.page().cell(*slot)?.value_pointer();
                    let value = |value| cell_value(value, pending);
                    let encoded;
                    let delta = match op {
                        Op::Insert {
                            slot, key, value: of, ..
                        } => {
                            let cell = CellContent {
                                key,
                                feature: &[],
                                value_hash: &[0x11; 32],
                                kv_hash: &[0x22; 32],
                                left: None,
                                right: None,
                                hosted_roots: &[],
                                value: value(of),
                                fragment_root: true,
                            };
                            encoded = cell.encode()?;
                            Delta::Insert {
                                slot: *slot,
                                cell: TreeCell::decode(&encoded, V1)?,
                            }
                        }
                        Op::Hash { slot, byte, .. } => Delta::Hash {
                            slot: *slot,
                            field: HashField::Value,
                            hash: &[*byte; 32],
                        },
                        Op::Remove { slot, .. } => {
                            released.extend(placed(slot));
                            Delta::Remove { slot: *slot }
                        }
                        Op::Value { slot, value: of, .. } => {
                            released.extend(placed(slot));
                            Delta::Value {
                                slot: *slot,
                                value: value(of),
                            }
                        }
                        Op::Free { .. } => unreachable!("a freed page has no run"),
                    };
                    delta.apply(&mut tree)?;
                    run.push(&delta)?;
                    if let (Some(bytes), Some(pending)) = (op.out(), pending) {
                        attach.push((pending.id(), Arc::<[u8]>::from(bytes)));
                    }
                }
                Ok(())
            });
            run.expect("a run");
            for (id, bytes) in attach {
                latch.attach(id, bytes);
            }
        }
        released.sort_by_key(|pointer| (pointer.file(), pointer.offset()));
        for pointer in &released {
            group.released(pointer).expect("a released pointer");
        }
        group.finish();
        let appended = db.log.append_group(&at, &bytes).expect("appended");
        let committed = at.version();
        db.wb()
            .committed(committed, appended.at, &bytes)
            .expect("write-back was told");

        let mut model = db.model.lock().expect("the model");
        for (id, (copy, _)) in std::mem::take(&mut self.copies) {
            if freed.contains(&id) {
                db.pool.discard(&copy).expect("discarded");
                if db.table.newest(id).is_some() {
                    db.table.push(id, Link::free(committed)).expect("a free link");
                }
                db.pool.free(id, committed).expect("freed");
                model.pages.entry(id).or_default().push((committed.get(), None));
            } else {
                let frame = db.pool.install(&copy, committed).expect("installed");
                let cells = db.cells(id, &frame, Some(&frame));
                let versions = model.pages.entry(id).or_default();
                versions.push((committed.get(), Some(cells)));
            }
        }
        drop(model);
        let mut change = ValueChange::new();
        released.iter().for_each(|&pointer| change.release(pointer));
        counts.apply(&change, committed).expect("counted");
        db.versions.latest.store(committed.get(), Ordering::SeqCst);
        committed
    }
}
