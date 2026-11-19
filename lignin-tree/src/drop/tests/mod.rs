mod accept;
mod cells;
mod errors;
mod flat;
mod grove;
mod races;

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};

use lignin_format::{
    delta::{Delta, DeltaRun, DeltaRunWriter},
    flat_page::{FlatPage, FlatValue},
    manifest::Manifest,
    segment::{BlockContent, Compression},
    tree_page::{TreePage, TreePageMut},
    value_log::{PendingValue, ValuePlace, ValuePointer},
    wal::{scan, write_file_start, Change, GroupWriter, LogItem, LogStart, ValueMove},
    CommitVersion, FileId, FormatVersion, PageId, PageSize,
};
use lignin_io::{
    sim::{SimConfig, SimVfs},
    Vfs,
};
use lignin_store::{
    alloc::{Epoch, PageAllocator, PageState, Pins},
    cache::{BufferPool, NotMade, OwnerId, PageRef, PoolConfig, PrivateRef},
    load::{LoadBuffers, LoadError, PageLoader, FRAME_FORMAT},
    segment::{ManifestStore, SegmentStore, SegmentWriter},
    table::{Link, PageTable},
    values::{OwnersError, ValueChange, ValueHold, ValueStore, ValueWriter},
};

use super::*;
use crate::{
    directory::{Directory, Dropped},
    flat::{Committed, FlatConfig, Private, StepLog},
    fragment::{FragmentError, PageMut, Pages},
    node::{ChangeSink, Lease, NodeError, NodePage, NodePageMut},
    value::{InlinePolicy, TreeValues, ValueSource},
    OperationGuard,
};

pub(super) use grove::Grove;

use crate::flat::FlatTree;

/// What an operation of these tests holds while it reads.
pub(super) struct Guard;

impl OperationGuard for Guard {}

pub(super) static GUARD: Guard = Guard;

pub(super) type Reader<'d> = Committed<'d, 'd, Guard>;
pub(super) type Writer<'d> = Private<'d, 'd, Guard, &'d PageAllocator>;

pub(super) fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

/// The pins and the guards of a store, as whoever keeps them answers for
/// them: set by hand.
#[derive(Debug, Default)]
pub(super) struct TestPins {
    /// The oldest pinned version, or `u64::MAX` for none: then the latest
    /// version is the answer.
    pub pinned: AtomicU64,
    pub epoch: AtomicU64,
    /// The epoch of the oldest guard, or `u64::MAX` for none.
    pub guard: AtomicU64,
    latest: AtomicU64,
}

impl Pins for TestPins {
    fn oldest_pin(&self) -> CommitVersion {
        let latest = self.latest.load(Ordering::SeqCst);
        version(self.pinned.load(Ordering::SeqCst).min(latest))
    }

    fn next_epoch(&self) -> Epoch {
        Epoch::new(self.epoch.fetch_add(1, Ordering::SeqCst) + 1)
    }

    fn oldest_guard(&self) -> Epoch {
        let current = self.epoch.load(Ordering::SeqCst);
        Epoch::new(self.guard.load(Ordering::SeqCst).min(current))
    }
}

struct Files {
    segment: SegmentWriter,
    value_writer: ValueWriter,
}

/// A store on a simulated file system, with what the tasks after this one
/// build reduced to the steps a drop and its reclaiming take part in: a
/// write transaction over private copies of tree pages and flat pages, a
/// commit that writes one log group and installs the copies, and a
/// write-back that places the pending values and writes the pages to a
/// segment.
pub(super) struct Db {
    pub size: PageSize,
    pub config: FlatConfig,
    segments: SegmentStore,
    pub table: PageTable,
    pub pool: BufferPool,
    pub values: ValueStore,
    pub ids: PageAllocator,
    manifest: Mutex<ManifestStore>,
    pub log: Mutex<Vec<u8>>,
    latest: AtomicU64,
    owners: AtomicU64,
    files: Mutex<Files>,
    /// The tree pages that a commit made and that no commit freed.
    pub tree_pages: Mutex<BTreeSet<PageId>>,
    pub pins: TestPins,
    /// Refuses the next records of page steps, counted down.
    pub refuse_appends: AtomicU64,
    /// A flat keyspace that a commit writes when it has nothing else to
    /// write.
    scratch: Mutex<Option<FlatTree>>,
}

impl Db {
    pub fn new() -> Self {
        Self::with(PageSize::MIN, 4 << 20)
    }

    pub fn with(size: PageSize, pool: usize) -> Self {
        let fs = SimVfs::new(SimConfig::new(8));
        let dir = Path::new("/db");
        fs.create_dir(dir).expect("a directory");
        let vfs: Arc<dyn Vfs> = Arc::new(fs);
        let empty = Manifest::new(size, Default::default(), None, Vec::new()).expect("a manifest");
        let mut manifest = ManifestStore::create(vfs, dir, empty).expect("a store");
        let segments = SegmentStore::open(&manifest).expect("segments");
        let segment = segments.create().expect("a segment");
        segments.commit(&mut manifest).expect("named");
        let values = ValueStore::open(&manifest, &[]).expect("a value log");
        let value_writer = values.create().expect("a value-log file");
        values.commit(&mut manifest).expect("named");
        let mut log = Vec::new();
        write_file_start(FileId::MIN, &LogStart::FIRST, &mut log).expect("a log file");
        let pool = BufferPool::new(PoolConfig {
            page_size: size,
            capacity: pool,
        });
        let pins = TestPins::default();
        pins.pinned.store(u64::MAX, Ordering::SeqCst);
        pins.guard.store(u64::MAX, Ordering::SeqCst);
        Self {
            size,
            config: FlatConfig::new(size),
            segments,
            table: PageTable::new(),
            pool: pool.expect("a pool"),
            values,
            ids: PageAllocator::new(),
            manifest: Mutex::new(manifest),
            log: Mutex::new(log),
            latest: AtomicU64::new(0),
            owners: AtomicU64::new(0),
            files: Mutex::new(Files {
                segment,
                value_writer,
            }),
            tree_pages: Mutex::new(BTreeSet::new()),
            pins,
            refuse_appends: AtomicU64::new(0),
            scratch: Mutex::new(None),
        }
    }

    /// A commit that changes one record of a keyspace of its own: the
    /// latest version moves on.
    pub fn tick(&self) -> CommitVersion {
        let mut txn = self.begin();
        let mut scratch = self.scratch.lock().expect("the keyspace");
        let tree = scratch
            .get_or_insert_with(|| FlatTree::create(&mut txn.flat, self.config).expect("a tree"));
        let next = self.latest().get().to_le_bytes();
        tree.put(&mut txn.flat, b"tick", &next).expect("a put");
        drop(scratch);
        self.commit(txn)
    }

    /// Gives the ids of the freed pages back, and drops the page versions
    /// that are held for no reader.
    pub fn reclaim_ids(&self) -> Vec<PageId> {
        let mut released = Vec::new();
        self.ids.reclaim(&self.pins, &mut released);
        self.ids.reclaim(&self.pins, &mut released);
        self.pool.reclaim(&self.pins);
        released
    }

    pub fn latest(&self) -> CommitVersion {
        version(self.latest.load(Ordering::SeqCst))
    }

    pub fn loader(&self) -> PageLoader<'_> {
        PageLoader::new(&self.pool, &self.table, &self.segments, self.size).expect("a loader")
    }

    pub fn reader_at(&self, at: CommitVersion) -> Reader<'_> {
        Committed::new(self.loader(), &self.values, &GUARD, at)
    }

    pub fn reader(&self) -> Reader<'_> {
        self.reader_at(self.latest())
    }

    pub fn reclaimer(&self) -> Reclaimer<'_, &Self> {
        Reclaimer::new(
            self.loader(),
            &self.pool,
            &self.table,
            &self.ids,
            &self.values,
            self.size,
            self,
        )
    }

    /// The current version of `page`, or `None` if it is no page.
    pub fn current(&self, page: PageId) -> Option<PageRef<'_>> {
        match self
            .loader()
            .get(page, self.latest(), &mut LoadBuffers::new())
        {
            Ok(frame) => Some(frame),
            Err(LoadError::NoPage { .. }) => None,
            Err(error) => panic!("{error}"),
        }
    }

    fn owner(&self) -> OwnerId {
        OwnerId::new(self.owners.fetch_add(1, Ordering::SeqCst) + 1)
    }

    /// A write transaction over the latest version.
    pub fn begin(&self) -> Txn<'_> {
        let at = self.latest();
        Txn {
            tree: TreePages {
                db: self,
                owner: self.owner(),
                lease: Lease::take(),
                at,
                copies: BTreeMap::new(),
                freed: BTreeMap::new(),
                sink: Recorder::default(),
                buffers: LoadBuffers::new(),
            },
            values: TreeValues::new(InlinePolicy::DEFAULT),
            flat: Private::new(self.reader_at(at), &self.pool, self.owner(), &self.ids),
        }
    }

    /// Commits `txn`: one group in the log, with the values, the runs of
    /// the pages in ascending order and the pointers released, and then the
    /// copies installed and the freed pages freed (`transactions.md` CM3).
    /// It holds the lock on the counts from before its append to its count.
    pub fn commit(&self, txn: Txn<'_>) -> CommitVersion {
        let Txn {
            mut tree,
            mut values,
            mut flat,
        } = txn;
        let counts = self.values.step().expect("the counts");
        let source = ValueSource::new(&self.values, self.loader(), tree.at);
        let version = version(self.latest.load(Ordering::SeqCst) + 1);
        let mut unchanged = Vec::new();
        {
            let mut log = self.log.lock().expect("the log");
            let address = LogStart::FIRST
                .address(log.len() as u64)
                .expect("an address");
            let mut group = GroupWriter::new(version, address, &mut log).expect("a group");
            flat.log_values(&mut group).expect("the flat values");
            values
                .log_values(&mut tree, &source, &mut group)
                .expect("the values");
            let mut ids: BTreeMap<PageId, u8> = BTreeMap::new();
            ids.extend(tree.copies.keys().map(|page| (*page, 0)));
            ids.extend(tree.freed.keys().map(|page| (*page, 2)));
            ids.extend(flat.written().map(|page| (page, 1)));
            ids.extend(flat.freed().map(|page| (page, 2)));
            for (page, kind) in ids {
                match kind {
                    2 => group.freed(page).expect("a freed page"),
                    1 => {
                        if !flat.log_page(page, &mut group).expect("a run") {
                            unchanged.push(page);
                        }
                    }
                    _ => {
                        let Some(run) = tree.sink.runs.get(&page) else {
                            continue;
                        };
                        group
                            .tree_run_with(page, run.created, |out| {
                                for record in &run.records {
                                    for delta in DeltaRun::decode(record, FRAME_FORMAT)?.deltas() {
                                        out.push(&delta)?;
                                    }
                                }
                                Ok(())
                            })
                            .expect("a run");
                    }
                }
            }
            let mut released: Vec<ValuePointer> = values.released_pointers().to_vec();
            released.extend_from_slice(flat.released());
            released.sort_unstable_by_key(|pointer| (pointer.file(), pointer.offset()));
            for pointer in &released {
                group.released(pointer).expect("a released pointer");
            }
            group.finish();
        }

        let mut change = values.change();
        let mut tree_pages = self.tree_pages.lock().expect("the pages");
        for (page, copy) in std::mem::take(&mut tree.copies) {
            if !tree.sink.runs.contains_key(&page) {
                self.pool.discard(&copy.frame).expect("discarded");
                continue;
            }
            values
                .attach(&mut copy.frame.write().expect("the latch"))
                .expect("the values are with their page");
            self.pool.install(&copy.frame, version).expect("installed");
            if copy.created {
                self.ids.publish(page).expect("published");
            }
            tree_pages.insert(page);
        }
        for (page, copy) in std::mem::take(&mut tree.freed) {
            self.pool.discard(&copy).expect("discarded");
            self.free(page, version);
            tree_pages.remove(&page);
        }
        drop(tree_pages);

        let parts = flat.into_parts();
        for (copy, created) in parts.written {
            let page = copy.page();
            if unchanged.contains(&page) {
                self.pool.discard(&copy).expect("discarded");
                continue;
            }
            self.pool.install(&copy, version).expect("installed");
            if created {
                self.ids.publish(page).expect("published");
            }
        }
        for copy in parts.freed {
            let page = copy.page();
            self.pool.discard(&copy).expect("discarded");
            self.free(page, version);
        }
        for page in parts.unused {
            self.ids.release(page).expect("released");
        }
        for pointer in parts.released {
            change.release(pointer);
        }
        counts.apply(&change, version).expect("counted");
        self.pins.latest.store(version.get(), Ordering::SeqCst);
        self.latest.store(version.get(), Ordering::SeqCst);
        version
    }

    fn free(&self, page: PageId, version: CommitVersion) {
        self.table
            .push(page, Link::free(version))
            .expect("a free link");
        self.pool.free(page, version).expect("freed");
        self.ids.free(page, version).expect("freed");
    }

    /// Gives up `txn`: its copies are discarded and its ids go back.
    pub fn roll_back(&self, txn: Txn<'_>) {
        let Txn { tree, mut flat, .. } = txn;
        for (page, copy) in tree.copies {
            self.pool.discard(&copy.frame).expect("discarded");
            if copy.created {
                self.ids.release(page).expect("released");
            }
        }
        for copy in tree.freed.into_values() {
            self.pool.discard(&copy).expect("discarded");
        }
        flat.roll_back().expect("rolled back");
    }

    /// Write-back: every pending value of a current page is appended to
    /// the value log and given its pointer in a page step, and then every
    /// page version that no segment holds is written to one.
    pub fn write_back(&self) {
        let mut files = self.files.lock().expect("the files");
        let files = &mut *files;
        let mut versions = Vec::new();
        self.pool.unwritten_versions(&mut versions);
        let mut owed = Vec::new();
        for born in versions {
            let mut pages = Vec::new();
            self.pool.unwritten_pages(born, &mut pages);
            pages.sort();
            owed.extend(pages.into_iter().map(|page| (born, page)));
        }

        let mut hold = ValueHold::new();
        let mut placed = Vec::new();
        for &(born, page) in &owed {
            let Some(frame) = self.pool.get_version(page, born) else {
                continue;
            };
            let step = self.pool.page_step(page);
            if !step.is_current(&frame) || step.private_copies() != 0 {
                continue;
            }
            drop(step);
            for pending in pending_of(&frame) {
                let bytes = frame.attached(pending.id()).expect("the value's bytes");
                let pointer = files.value_writer.append(&mut hold, page, bytes);
                placed.push((page, pending, pointer.expect("appended")));
            }
        }
        if !placed.is_empty() {
            files.value_writer.sync().expect("synced");
            self.values
                .commit(&mut self.manifest.lock().expect("the manifest"))
                .expect("covered");
        }
        for (page, pending, pointer) in placed {
            let counts = self.values.step().expect("the counts");
            let step = self.pool.page_step(page);
            let Some(old) = step.current() else {
                continue;
            };
            if step.private_copies() != 0 || !pending_of(&old).contains(&pending) {
                continue;
            }
            let from = ValuePlace::Pending(pending);
            let mut blank = self.pool.blank();
            blank.copy_from(&old);
            let change = match TreePage::from_validated(&old, FRAME_FORMAT) {
                Ok(view) => {
                    let slot = view
                        .pending_values()
                        .find(|(_, held)| *held == pending)
                        .expect("the cell")
                        .0;
                    let change = ValueMove::in_tree(page, slot, from, pointer).expect("a step");
                    let mut copy =
                        TreePageMut::open(&mut blank, self.size, FRAME_FORMAT).expect("a page");
                    change
                        .delta()
                        .expect("a record")
                        .apply(&mut copy)
                        .expect("placed");
                    change
                }
                Err(_) => {
                    let change = ValueMove::in_flat_leaf(page, from, pointer).expect("a step");
                    let mut image = Vec::new();
                    change
                        .apply_to_flat_leaf(&old, FRAME_FORMAT, self.config.capacity(), &mut image)
                        .expect("placed");
                    blank[..image.len()].copy_from_slice(&image);
                    change
                }
            };
            change
                .write(&mut self.log.lock().expect("the log"))
                .expect("a record");
            blank.detach(pending.id()).expect("attached");
            step.replace(&old, blank).expect("replaced");
            drop(step);
            let mut acquired = ValueChange::new();
            acquired.acquire(pointer);
            counts.apply(&acquired, self.latest()).expect("counted");
        }
        drop(hold);

        let mut versions = Vec::new();
        self.pool.unwritten_versions(&mut versions);
        for born in versions {
            let mut pages = Vec::new();
            self.pool.unwritten_pages(born, &mut pages);
            pages.sort();
            for page in pages {
                let Some(frame) = self.pool.get_version(page, born) else {
                    continue;
                };
                if !pending_of(&frame).is_empty() {
                    if !self.pool.page_step(page).is_current(&frame) {
                        self.pool.hold_for_readers(&frame).expect("held");
                    }
                    continue;
                }
                let image = match TreePage::from_validated(&frame, FRAME_FORMAT) {
                    Ok(view) => view.to_image(),
                    Err(_) => {
                        let flat = FlatPage::decode(&frame, FRAME_FORMAT).expect("a flat page");
                        flat.as_bytes().to_vec()
                    }
                };
                let block = files
                    .segment
                    .append(page, BlockContent::Base, Compression::Lz4, &image)
                    .expect("a block");
                // A page that a step freed meanwhile takes no link.
                if self.table.push(page, Link::base(born, block)).is_ok() {
                    let _ = self.pool.mark_written(&frame);
                }
            }
        }
    }

    /// The tree pages that are pages at the latest version, each with its
    /// current frame.
    pub fn tree_frames(&self) -> Vec<(PageId, PageRef<'_>)> {
        let pages: Vec<PageId> = self
            .tree_pages
            .lock()
            .expect("the pages")
            .iter()
            .copied()
            .collect();
        // An id that was freed can name a flat page by now.
        let is_tree = |frame: &PageRef<'_>| TreePage::from_validated(frame, FRAME_FORMAT).is_ok();
        pages
            .into_iter()
            .filter_map(|page| Some((page, self.current(page).filter(is_tree)?)))
            .collect()
    }

    /// How many cells the tree pages of the latest version hold.
    pub fn cells(&self) -> usize {
        let frames = self.tree_frames();
        frames
            .iter()
            .map(|(_, frame)| tree(frame).cell_count())
            .sum()
    }

    /// Checks every tree page of the latest version by itself: its marks
    /// and the addresses inside it. No page is left with no cell.
    pub fn check_pages(&self) {
        for (page, frame) in self.tree_frames() {
            let view = tree(&frame);
            assert_eq!(view.id(), page);
            assert!(!view.is_empty(), "page {page} has no cell and is a page");
            view.check_fragments()
                .unwrap_or_else(|error| panic!("page {page}: {error}"));
        }
    }

    /// The value pointers that the cells of the tree pages hold at the
    /// latest version.
    pub fn tree_pointers(&self) -> Vec<ValuePointer> {
        let frames = self.tree_frames();
        let held = |(_, frame): &(PageId, PageRef<'_>)| {
            let cells = tree(frame).cells();
            cells
                .filter_map(|(_, cell)| cell.value_pointer())
                .collect::<Vec<_>>()
        };
        frames.iter().flat_map(held).collect()
    }

    /// Checks the counts of live values against `held`, the value pointers
    /// that cells and entries hold at the latest version.
    pub fn check_counts(&self, held: &[ValuePointer]) {
        let mut expected: BTreeMap<FileId, (u64, u64)> = BTreeMap::new();
        for pointer in held {
            let entry = expected.entry(pointer.file()).or_default();
            entry.0 += 1;
            entry.1 += pointer.extent_len(FormatVersion::CURRENT);
        }
        let counted: BTreeMap<FileId, (u64, u64)> = self
            .values
            .counts()
            .into_iter()
            .map(|count| {
                assert!(!count.miscounted, "{count:?}");
                (count.file, (count.values, count.bytes))
            })
            .filter(|(_, count)| *count != (0, 0))
            .collect();
        assert_eq!(counted, expected);
    }

    /// The reclaiming steps that the log holds, in order: the page, the
    /// version it was freed with, how many value pointers were released
    /// and whether the step listed what it removed.
    pub fn reclaims(&self) -> Vec<(PageId, Option<CommitVersion>, usize, bool)> {
        let log = self.log.lock().expect("the log");
        let steps = scan(&log).expect("a log").filter_map(|item| match item {
            LogItem::Change(Change::Reclaim(step)) => Some(step),
            _ => None,
        });
        steps
            .map(|step| {
                (
                    step.page(),
                    step.freed(),
                    step.released().len(),
                    step.removed().is_some(),
                )
            })
            .collect()
    }

    /// The state of the id of `page`.
    pub fn state(&self, page: PageId) -> PageState {
        self.ids.state(page)
    }
}

/// The tree page in `frame`.
pub(super) fn tree<'a>(frame: &'a [u8]) -> TreePage<'a> {
    TreePage::from_validated(frame, FRAME_FORMAT).expect("a tree page")
}

/// The pending values of the page in `frame`, a tree page or a flat one.
fn pending_of(frame: &[u8]) -> Vec<PendingValue> {
    if let Ok(view) = TreePage::from_validated(frame, FRAME_FORMAT) {
        return view.pending_values().map(|(_, pending)| pending).collect();
    }
    let Some(leaf) = FlatPage::decode(frame, FRAME_FORMAT)
        .expect("a page")
        .leaf()
    else {
        return Vec::new();
    };
    leaf.iter()
        .filter_map(|(_, value)| match value {
            FlatValue::OutOfLine(place) => {
                let place: [u8; ValuePlace::ENCODED_LEN] = place.try_into().expect("a place");
                ValuePlace::decode(place).expect("a place").pending()
            }
            FlatValue::Inline(_) => None,
        })
        .collect()
}

impl StepLog for &Db {
    fn latest(&self) -> CommitVersion {
        Db::latest(self)
    }

    fn append(&self, record: &[u8]) -> Result<(), OwnersError> {
        let refuse = self.refuse_appends.load(Ordering::SeqCst);
        if refuse > 0 {
            self.refuse_appends.store(refuse - 1, Ordering::SeqCst);
            return Err("the log is full".into());
        }
        self.log.lock().expect("the log").extend_from_slice(record);
        Ok(())
    }
}

/// The changes a transaction made to one tree page, each as a run of one
/// record.
#[derive(Debug, Default)]
struct Run {
    created: Option<u64>,
    records: Vec<Vec<u8>>,
}

#[derive(Debug, Default)]
struct Recorder {
    runs: BTreeMap<PageId, Run>,
}

impl ChangeSink for Recorder {
    fn created(&mut self, page: PageId, departures: u64) {
        self.runs.entry(page).or_default().created = Some(departures);
    }

    fn changed(&mut self, page: PageId, change: &Delta<'_>) {
        let mut record = Vec::new();
        let mut run = DeltaRunWriter::new(page, None, &mut record);
        run.push(change).expect("a record");
        run.finish().expect("a run");
        self.runs.entry(page).or_default().records.push(record);
    }
}

struct Copy<'d> {
    frame: PrivateRef<'d>,
    created: bool,
}

/// The tree pages of one write transaction: private copies in the buffer
/// pool, over the committed pages of the version it reads.
pub(super) struct TreePages<'d> {
    db: &'d Db,
    owner: OwnerId,
    lease: Lease,
    pub at: CommitVersion,
    copies: BTreeMap<PageId, Copy<'d>>,
    freed: BTreeMap<PageId, PrivateRef<'d>>,
    sink: Recorder,
    buffers: LoadBuffers,
}

fn unreachable_page(error: impl std::fmt::Display) -> FragmentError {
    FragmentError::Pages(error.to_string().into())
}

impl<'d> TreePages<'d> {
    fn own(&mut self, page: PageId) -> Result<&Copy<'d>, FragmentError> {
        if !self.copies.contains_key(&page) {
            let mut loaded = None;
            let frame = loop {
                match self.db.pool.make_private_once(self.owner, page, self.at) {
                    Ok(frame) => break frame,
                    Err(NotMade::AlreadyPrivate) => return Err(unreachable_page("copied twice")),
                    Err(NotMade::Miss(miss)) => {
                        let frame = self.db.loader().load(miss, &mut self.buffers);
                        loaded = Some(frame.map_err(unreachable_page)?);
                    }
                }
            };
            drop(loaded);
            let created = false;
            self.copies.insert(page, Copy { frame, created });
        }
        Ok(&self.copies[&page])
    }

    /// Makes the transaction's copy of `page`, as a write to it would.
    pub fn copy(&mut self, page: PageId) {
        self.own(page).expect("a copy");
    }

    /// The tree pages the transaction has copied or made, and those it
    /// freed.
    pub fn touched(&self) -> Vec<PageId> {
        let freed = self.freed.keys();
        self.copies.keys().chain(freed).copied().collect()
    }
}

impl Pages for TreePages<'_> {
    fn read<R>(
        &mut self,
        page: PageId,
        read: impl FnOnce(NodePage<'_>) -> R,
    ) -> Result<R, FragmentError> {
        if let Some(copy) = self.copies.get(&page) {
            let latch = copy.frame.read();
            return Ok(read(NodePage::open(&latch, self.lease)?));
        }
        let frame = self.db.loader().get(page, self.at, &mut self.buffers);
        let frame = frame.map_err(unreachable_page)?;
        Ok(read(NodePage::open(&frame, self.lease)?))
    }

    fn write<R>(
        &mut self,
        page: PageId,
        change: impl FnOnce(&mut PageMut<'_, '_>) -> Result<R, NodeError>,
    ) -> Result<R, FragmentError> {
        let (lease, size) = (self.lease, self.db.size);
        self.own(page)?;
        let mut latch = self.copies[&page].frame.write().map_err(unreachable_page)?;
        let sink: &mut dyn ChangeSink = &mut self.sink;
        let mut nodes = NodePageMut::open(&mut latch, size, lease, sink)?;
        Ok(change(&mut nodes)?)
    }

    fn create(&mut self) -> Result<PageId, FragmentError> {
        let page = self.db.ids.allocate().map_err(unreachable_page)?;
        let frame = self.db.pool.new_private(self.owner, page);
        let frame = frame.map_err(unreachable_page)?;
        let mut latch = frame.write().map_err(unreachable_page)?;
        let sink: &mut dyn ChangeSink = &mut self.sink;
        NodePageMut::create(&mut latch, self.db.size, page, self.lease, sink)?;
        drop(latch);
        let created = true;
        self.copies.insert(page, Copy { frame, created });
        Ok(page)
    }

    fn free(&mut self, page: PageId) -> Result<(), FragmentError> {
        self.own(page)?;
        let Some(copy) = self.copies.remove(&page) else {
            return Ok(());
        };
        if copy.created {
            self.db
                .pool
                .discard(&copy.frame)
                .map_err(unreachable_page)?;
            self.db.ids.release(page).map_err(unreachable_page)?;
            self.sink.runs.remove(&page);
        } else {
            self.freed.insert(page, copy.frame);
        }
        Ok(())
    }
}

/// One write transaction: its tree pages with the values of their cells,
/// and its flat pages.
pub(super) struct Txn<'d> {
    pub tree: TreePages<'d>,
    pub values: TreeValues,
    pub flat: Writer<'d>,
}

/// `len` bytes that depend on `seed`.
pub(super) fn bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

/// The dropped and cleared Merks whose records the latest version has.
pub(super) fn drops(db: &Db, directory: &Directory) -> Vec<Dropped> {
    let pages = db.reader();
    let mut walk = directory.drops();
    let mut found = Vec::new();
    while let Some(dropped) = walk.next(&pages).expect("a record") {
        found.push(dropped);
    }
    found
}

/// Takes `reclaim` on until it is done, `steps` page steps at a time, with
/// a commit wherever a page waits for one.
pub(super) fn finish(db: &Db, directory: &Directory, reclaim: &mut Reclaim, steps: usize) {
    let reclaimer = db.reclaimer();
    for _ in 0..1_000_000 {
        match reclaimer
            .reclaim(reclaim, directory, &GUARD, steps)
            .expect("a step")
        {
            Progress::Done => return,
            Progress::More => {}
            Progress::Blocked(Busy::Newborn) => drop(db.tick()),
            Progress::Blocked(busy) => panic!("nothing holds a page: {busy:?}"),
        }
    }
    panic!("the reclaiming does not end");
}

/// Reclaims what every drop of the latest version left, and returns what
/// each reclaiming took out.
pub(super) fn reclaim_all(db: &Db, grove: &Grove) -> Vec<Reclaimed> {
    let directory = grove.directory(db);
    let mut done = Vec::new();
    for dropped in drops(db, &directory) {
        let mut wait = Wait::new(db.latest());
        assert!(wait.poll(&db.pins), "nothing is pinned");
        let mut reclaim = Reclaim::new(dropped);
        finish(db, &directory, &mut reclaim, usize::MAX);
        assert!(reclaim.is_done() && reclaim.miscount().is_none());
        done.push(reclaim.reclaimed().clone());
    }
    done
}

/// Checks the store after everything was reclaimed: the tree pages hold
/// the cells of the nodes in use and no other, every page passes its own
/// check and none is empty, the directory holds the Merks in use and
/// nothing else, and the counts of live values are those of the pointers
/// that cells hold.
pub(super) fn check_reclaimed(db: &Db, grove: &Grove) {
    assert_eq!(db.cells(), grove.alive());
    db.check_pages();
    grove.check(db);
    let directory = grove.directory(db);
    let census = directory.check(&db.reader()).expect("a directory");
    let live = grove.merks.iter().filter(|merk| merk.alive).count() as u64;
    assert_eq!(
        (census.live, census.left_over, census.dropped),
        (live, 0, 0)
    );
    db.check_counts(&db.tree_pointers());
}
