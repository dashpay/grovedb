mod accept;
mod order;
mod owners;
mod policy;
mod scope;

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap},
    path::Path,
    sync::Arc,
};

use lignin_format::{
    delta::{Delta, DeltaRun, DeltaRunWriter},
    flat_page::FlatPage,
    manifest::Manifest,
    segment::{BlockContent, Compression},
    tree_page::{CellValue, Hash, Side, TreePage, TreePageMut},
    value_log::{PendingValue, ValuePlace, ValuePointer},
    wal::{scan, write_file_start, Change, Entry, GroupWriter, LogItem, LogStart, ValueMove},
    CellAddress, CommitVersion, FileId, FormatVersion, PageId, PageSize, SlotId,
};
use lignin_io::{
    sim::{SimConfig, SimVfs},
    Vfs,
};
use lignin_store::{
    cache::{BufferPool, NotMade, OwnerId, PageRef, PoolConfig, PrivateRef},
    load::{LoadBuffers, PageLoader, FRAME_FORMAT},
    segment::{ManifestStore, SegmentStore, SegmentWriter},
    table::{Link, PageTable},
    values::{FileCount, OwnersError, ValueChange, ValueHold, ValueStore, ValueWriter},
};

use super::*;
use crate::{
    flat::StepLog,
    fragment::{
        FragmentError, Fragments, HostedWrite, Layout, LayoutParams, LinkWrite, NodeWrite, PageMut,
        Pages, Report, RootWrite, Target, ValueWrite, Write,
    },
    node::{ChangeSink, Lease, NodeError, NodePage, NodePageMut, NodeReader},
    OperationGuard,
};

const SIZE: PageSize = PageSize::MIN;
const HASH: Hash = [7; 32];

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

fn page_id(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn slot(raw: u16) -> SlotId {
    SlotId::new(raw).expect("not zero")
}

/// SplitMix64.
#[derive(Debug, Clone)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

/// `len` bytes that depend on `seed`.
fn bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut rng = Rng(seed);
    (0..len).map(|_| rng.next() as u8).collect()
}

/// What an operation of these tests holds while it reads. Nothing is
/// reclaimed in them.
struct Guard;

impl OperationGuard for Guard {}

struct State {
    segment: SegmentWriter,
    value_writer: ValueWriter,
    log: Vec<u8>,
    latest: u64,
    owners: u64,
    next_page: u32,
    /// The tree pages of the latest version.
    pages: BTreeSet<PageId>,
}

/// A store on a simulated file system, with what the tasks after this one
/// build reduced to the steps the values of tree cells take part in: a
/// write scope over private copies, a commit that writes one log group and
/// installs the copies, and a write-back that places the pending values
/// and writes the pages to a segment.
struct Db {
    fs: SimVfs,
    segments: SegmentStore,
    table: PageTable,
    pool: BufferPool,
    values: ValueStore,
    manifest: RefCell<ManifestStore>,
    state: RefCell<State>,
}

impl Db {
    fn new() -> Self {
        Self::on(SimVfs::new(SimConfig::new(8)))
    }

    /// A store whose file system records every operation.
    fn traced() -> Self {
        let trace_ops = true;
        Self::on(SimVfs::new(SimConfig {
            trace_ops,
            ..SimConfig::new(8)
        }))
    }

    fn on(fs: SimVfs) -> Self {
        let dir = Path::new("/db");
        fs.create_dir(dir).expect("a directory");
        let vfs: Arc<dyn Vfs> = Arc::new(fs.clone());
        let empty = Manifest::new(SIZE, Default::default(), None, Vec::new()).expect("a manifest");
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
            page_size: SIZE,
            capacity: 1 << 20,
        });
        Self {
            fs,
            segments,
            table: PageTable::new(),
            pool: pool.expect("a pool"),
            values,
            manifest: RefCell::new(manifest),
            state: RefCell::new(State {
                segment,
                value_writer,
                log,
                latest: 0,
                owners: 0,
                next_page: 1,
                pages: BTreeSet::new(),
            }),
        }
    }

    fn latest(&self) -> CommitVersion {
        version(self.state.borrow().latest)
    }

    fn loader(&self) -> PageLoader<'_> {
        PageLoader::new(&self.pool, &self.table, &self.segments, SIZE).expect("a loader")
    }

    fn source(&self) -> ValueSource<'_> {
        ValueSource::new(&self.values, self.loader(), self.latest())
    }

    fn owners(&self) -> TreeOwners<'_, &Self> {
        TreeOwners::new(self.loader(), &self.pool, SIZE, self)
    }

    /// The current version of `page`.
    fn current(&self, page: PageId) -> PageRef<'_> {
        self.at(page, self.latest())
    }

    /// The version of `page` that a reader at `at` sees.
    fn at(&self, page: PageId, at: CommitVersion) -> PageRef<'_> {
        self.loader()
            .get(page, at, &mut LoadBuffers::new())
            .expect("the page")
    }

    /// The tree pages of the latest version.
    fn pages(&self) -> Vec<PageId> {
        self.state.borrow().pages.iter().copied().collect()
    }

    /// A write scope over the latest version.
    fn scope(&self) -> Scope<'_> {
        self.scope_with(InlinePolicy::DEFAULT)
    }

    fn scope_with(&self, policy: InlinePolicy) -> Scope<'_> {
        let owner = {
            let mut state = self.state.borrow_mut();
            state.owners += 1;
            OwnerId::new(state.owners)
        };
        Scope {
            pages: ScopePages {
                db: self,
                owner,
                lease: Lease::take(),
                at: self.latest(),
                copies: BTreeMap::new(),
                freed: BTreeMap::new(),
                sink: Recorder::default(),
                buffers: LoadBuffers::new(),
            },
            values: TreeValues::new(policy),
        }
    }

    /// Commits `scope`: one group in the log, with the values, the runs of
    /// the pages in ascending order and the pointers released, and then
    /// the copies installed, each with the bytes of its pending values, and
    /// the freed pages freed (`transactions.md` CM3).
    fn commit(&self, scope: Scope<'_>) -> CommitVersion {
        let Scope {
            mut pages,
            mut values,
        } = scope;
        let source = ValueSource::new(&self.values, self.loader(), pages.at);
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        let version = version(state.latest + 1);
        let address = LogStart::FIRST
            .address(state.log.len() as u64)
            .expect("an address");
        let mut group = GroupWriter::new(version, address, &mut state.log).expect("a group");
        values
            .log_values(&mut pages, &source, &mut group)
            .expect("the values");
        let mut ids: Vec<(PageId, bool)> = pages
            .copies
            .keys()
            .map(|page| (*page, false))
            .chain(pages.freed.keys().map(|page| (*page, true)))
            .collect();
        ids.sort();
        for (page, freed) in ids {
            if freed {
                group.freed(page).expect("a freed page");
                continue;
            }
            let Some(run) = pages.sink.runs.get(&page) else {
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
        for pointer in values.released_pointers().to_vec() {
            group.released(&pointer).expect("a released pointer");
        }
        group.finish();

        // What a replay of the group makes of the pages as they were is
        // what the commit installs: the cells, with the names of their
        // values, are in the log as they are in the frames.
        let logged = scan(&state.log).expect("a log").last();
        let Some(LogItem::Commit(logged)) = logged else {
            panic!("the log ends with the group");
        };
        for entry in logged.entries() {
            let Entry::Tree(run) = entry else {
                continue;
            };
            let copy = &pages.copies[&run.page()];
            let mut frame = match copy.created {
                true => vec![0; SIZE.get()],
                false => self.at(run.page(), pages.at).to_vec(),
            };
            run.apply(&mut frame, SIZE, FRAME_FORMAT)
                .expect("the run applies");
            let image = |frame: &[u8]| {
                let page = TreePage::from_validated(frame, FRAME_FORMAT);
                page.expect("a tree page").to_image()
            };
            assert_eq!(image(&frame), image(&copy.frame.read()), "{}", run.page());
        }

        for (page, copy) in std::mem::take(&mut pages.copies) {
            if !pages.sink.runs.contains_key(&page) {
                self.pool.discard(&copy.frame).expect("discarded");
                continue;
            }
            values
                .attach(&mut copy.frame.write().expect("the latch"))
                .expect("the values are with their page");
            self.pool.install(&copy.frame, version).expect("installed");
            state.pages.insert(page);
        }
        for (page, copy) in std::mem::take(&mut pages.freed) {
            self.pool.discard(&copy).expect("discarded");
            if self.table.newest(page).is_some() {
                self.table
                    .push(page, Link::free(version))
                    .expect("a free link");
            }
            self.pool.free(page, version).expect("freed");
            state.pages.remove(&page);
        }
        self.values
            .apply(&values.change(), version)
            .expect("every released pointer was counted");
        state.latest += 1;
        version
    }

    /// Gives up `scope`: its copies are discarded.
    fn roll_back(&self, scope: Scope<'_>) {
        let copies = scope.pages.copies.into_values().map(|copy| copy.frame);
        for copy in copies.chain(scope.pages.freed.into_values()) {
            self.pool.discard(&copy).expect("discarded");
        }
    }

    /// The first half of write-back: every pending value of a current page
    /// is appended to the value log with its page as the owner, a manifest
    /// covers it, and its cell gets the pointer in a page step that is
    /// recorded and counted.
    fn place_values(&self) {
        let mut state = self.state.borrow_mut();
        let state = &mut *state;
        let latest = version(state.latest);
        let mut hold = ValueHold::new();
        let mut placed = Vec::new();
        for &page in &state.pages {
            let frame = self.at(page, latest);
            let view = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
            for (slot, pending) in view.pending_values() {
                let bytes = frame.attached(pending.id()).expect("the value's bytes");
                assert_eq!(bytes.len(), pending.value_len() as usize);
                let pointer = state.value_writer.append(&mut hold, page, bytes);
                placed.push((page, slot, pending, pointer.expect("appended")));
            }
        }
        if !placed.is_empty() {
            state.value_writer.sync().expect("synced");
            self.values
                .commit(&mut self.manifest.borrow_mut())
                .expect("covered");
        }
        for (page, slot, pending, pointer) in placed {
            let counts = self.values.step().expect("the counts");
            let step = self.pool.page_step(page);
            let old = step.current().expect("the page");
            let change = ValueMove::in_tree(page, slot, ValuePlace::Pending(pending), pointer)
                .expect("a step");
            let view = TreePage::from_validated(&old, FRAME_FORMAT).expect("a tree page");
            assert!(change.is_due_in_tree(&view));
            change.write(&mut state.log).expect("a record");
            let mut blank = self.pool.blank();
            blank.copy_from(&old);
            let mut copy = TreePageMut::open(&mut blank, SIZE, FRAME_FORMAT).expect("a tree page");
            change
                .delta()
                .expect("a record")
                .apply(&mut copy)
                .expect("placed");
            blank.detach(pending.id()).expect("attached");
            step.replace(&old, blank).expect("replaced");
            let mut acquired = ValueChange::new();
            acquired.acquire(pointer);
            counts.apply(&acquired, latest).expect("counted");
        }
        drop(hold);
    }

    /// Write-back: the values are placed, and then every page version that
    /// no segment holds is written to one. A version that a commit has
    /// superseded with a value that was never placed is kept for its
    /// readers instead.
    fn write_back(&self) {
        self.place_values();
        let mut state = self.state.borrow_mut();
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
                let image = match TreePage::from_validated(&frame, FRAME_FORMAT) {
                    Ok(view) if view.has_pending_value() => {
                        self.pool.hold_for_readers(&frame).expect("held");
                        continue;
                    }
                    Ok(view) => view.to_image(),
                    Err(_) => {
                        let flat = FlatPage::decode(&frame, FRAME_FORMAT).expect("a flat page");
                        flat.as_bytes().to_vec()
                    }
                };
                let block = state
                    .segment
                    .append(page, BlockContent::Base, Compression::Lz4, &image)
                    .expect("a block");
                self.table
                    .push(page, Link::base(born, block))
                    .expect("a link");
                self.pool.mark_written(&frame).expect("written");
            }
        }
    }

    /// Closes the value-log file that write-back appends to, gives
    /// write-back another, and returns the writer of a third, for what a
    /// cleaning pass or a regrouping moves.
    fn close_value_file(&self) -> ValueWriter {
        let mut manifest = self.manifest.borrow_mut();
        let mut create = || {
            let writer = self.values.create().expect("a value-log file");
            self.values.commit(&mut manifest).expect("named");
            writer
        };
        let (next, to) = (create(), create());
        let old = std::mem::replace(&mut self.state.borrow_mut().value_writer, next);
        old.finish().expect("closed");
        self.values
            .commit(&mut manifest)
            .expect("closed in the manifest");
        to
    }

    /// Closes the file of `writer`.
    fn finish(&self, writer: ValueWriter) -> FileId {
        let file = writer.file_id();
        writer.finish().expect("closed");
        self.values
            .commit(&mut self.manifest.borrow_mut())
            .expect("closed in the manifest");
        file
    }

    /// Makes the copies in `to` durable and has the manifest cover them.
    fn cover(&self, to: &mut ValueWriter) {
        to.sync().expect("synced");
        self.values
            .commit(&mut self.manifest.borrow_mut())
            .expect("covered");
    }

    /// Regroups the values of `page` into the file of `to`.
    fn regroup(
        &self,
        page: PageId,
        to: &mut ValueWriter,
        policy: RegroupPolicy,
    ) -> Option<Regrouped> {
        let owners = self.owners();
        let grouped = owners
            .copy_grouped(&self.values, page, to, policy)
            .expect("copied")?;
        self.cover(to);
        Some(
            owners
                .install_grouped(&self.values, grouped)
                .expect("installed"),
        )
    }

    /// The value pointers that the cells of the latest version hold, each
    /// with the page of its cell.
    fn pointers(&self) -> Vec<(PageId, ValuePointer)> {
        let mut found = Vec::new();
        for page in self.pages() {
            let frame = self.current(page);
            let view = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
            let held = view.cells().filter_map(|(_, cell)| cell.value_pointer());
            found.extend(held.map(|pointer| (page, pointer)));
        }
        found
    }

    /// Checks the counts of live values against the cells of the latest
    /// version: each file counts the cells that point into it, and their
    /// bytes, and no count was ever found wrong.
    fn check_counts(&self) {
        let mut expected: BTreeMap<FileId, (u64, u64)> = BTreeMap::new();
        for (_, pointer) in self.pointers() {
            let entry = expected.entry(pointer.file()).or_default();
            entry.0 += 1;
            entry.1 += pointer.extent_len(FormatVersion::CURRENT);
        }
        let counted: BTreeMap<FileId, (u64, u64)> = self
            .values
            .counts()
            .into_iter()
            .map(|count: FileCount| {
                assert!(!count.miscounted, "{count:?}");
                (count.file, (count.values, count.bytes))
            })
            .collect();
        assert_eq!(counted, expected);
    }

    /// The groups of the log, each as its values (owner, name and bytes),
    /// the pages it has runs of or freed, and the pointers it released.
    fn groups(&self) -> Vec<Logged> {
        let state = self.state.borrow();
        let mut groups = Vec::new();
        for item in scan(&state.log).expect("a log") {
            let LogItem::Commit(group) = item else {
                continue;
            };
            let mut logged = Logged::default();
            for (address, value) in group.values() {
                let bytes = value.bytes().expect("the value's bytes").to_vec();
                let pending = address.pending(value.value_len()).expect("a pending value");
                logged.values.push((value.owner(), pending, bytes));
            }
            for entry in group.entries() {
                match entry {
                    Entry::Tree(run) => logged.runs.push(run.page()),
                    Entry::Freed(page) => logged.freed.push(page),
                    Entry::Released(pointer) => logged.released.push(pointer),
                    other => panic!("no {other:?} is written here"),
                }
            }
            groups.push(logged);
        }
        groups
    }

    /// The pointer moves that the log holds as background changes, without
    /// the placings of write-back.
    fn moves(&self) -> Vec<ValueMove> {
        let state = self.state.borrow();
        scan(&state.log)
            .expect("a log")
            .filter_map(|item| match item {
                LogItem::Change(Change::Value(step)) => Some(step),
                _ => None,
            })
            .filter(|step| !step.from().is_pending())
            .collect()
    }
}

impl StepLog for &Db {
    fn latest(&self) -> CommitVersion {
        Db::latest(self)
    }

    fn append(&self, record: &[u8]) -> Result<(), OwnersError> {
        self.state.borrow_mut().log.extend_from_slice(record);
        Ok(())
    }
}

/// What one commit group says.
#[derive(Debug, Default)]
struct Logged {
    values: Vec<(PageId, PendingValue, Vec<u8>)>,
    runs: Vec<PageId>,
    freed: Vec<PageId>,
    released: Vec<ValuePointer>,
}

/// The changes a scope made to one page, each as a run of one record.
#[derive(Debug, Default)]
struct Run {
    created: Option<u64>,
    records: Vec<Vec<u8>>,
}

/// The sink of a scope's pages: it keeps every record it is told.
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

/// The tree pages of one write scope: private copies in the buffer pool,
/// over the committed pages of the version the scope reads.
struct ScopePages<'d> {
    db: &'d Db,
    owner: OwnerId,
    lease: Lease,
    at: CommitVersion,
    copies: BTreeMap<PageId, Copy<'d>>,
    /// The copies of the committed pages the scope freed, kept until the
    /// commit so that no page step changes such a page meanwhile.
    freed: BTreeMap<PageId, PrivateRef<'d>>,
    sink: Recorder,
    buffers: LoadBuffers,
}

fn unreachable_page(error: impl std::fmt::Display) -> FragmentError {
    FragmentError::Pages(error.to_string().into())
}

impl<'d> ScopePages<'d> {
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
}

impl Pages for ScopePages<'_> {
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
        let lease = self.lease;
        self.own(page)?;
        let mut latch = self.copies[&page].frame.write().map_err(unreachable_page)?;
        let sink: &mut dyn ChangeSink = &mut self.sink;
        let mut nodes = NodePageMut::open(&mut latch, SIZE, lease, sink)?;
        Ok(change(&mut nodes)?)
    }

    fn create(&mut self) -> Result<PageId, FragmentError> {
        let page = {
            let mut state = self.db.state.borrow_mut();
            state.next_page += 1;
            page_id(state.next_page - 1)
        };
        let frame = self.db.pool.new_private(self.owner, page);
        let frame = frame.map_err(unreachable_page)?;
        let mut latch = frame.write().map_err(unreachable_page)?;
        let sink: &mut dyn ChangeSink = &mut self.sink;
        NodePageMut::create(&mut latch, SIZE, page, self.lease, sink)?;
        drop(latch);
        let created = true;
        self.copies.insert(page, Copy { frame, created });
        Ok(page)
    }

    fn free(&mut self, page: PageId) -> Result<(), FragmentError> {
        let copy = self.own(page).map(|copy| copy.created)?;
        let copy = self.copies.remove(&page).filter(|_| !copy);
        match copy {
            Some(copy) => drop(self.freed.insert(page, copy.frame)),
            None => drop(self.sink.runs.remove(&page)),
        }
        Ok(())
    }
}

/// One write scope: its pages, and the values its cells hold out of line.
struct Scope<'d> {
    pages: ScopePages<'d>,
    values: TreeValues,
}

impl Scope<'_> {
    /// The value of the node at `address`, as the scope reads it: from its
    /// own copy of the page if it has one, and from the committed page
    /// otherwise.
    fn read(&mut self, address: CellAddress) -> Result<Vec<u8>, ValueError> {
        let db = self.pages.db;
        let source = ValueSource::new(&db.values, db.loader(), self.pages.at);
        let mut buf = ValueBuf::new();
        if let Some(copy) = self.pages.copies.get(&address.page()) {
            let latch = copy.frame.read();
            let nodes = NodePage::open(&latch, self.pages.lease)?;
            let node = nodes.node(address.slot()).expect("a node");
            return Ok(self.values.read(&source, node, &mut buf)?.to_vec());
        }
        let reader = NodeReader::new(db.loader(), &Guard, self.pages.lease, self.pages.at);
        let node = reader.node(address, &mut self.pages.buffers)?;
        Ok(read_value(&db.values, &node, &mut buf)?.to_vec())
    }

    /// What the cell at `address` holds for its value, in the scope's copy
    /// of the page or in the committed page.
    fn place(&mut self, address: CellAddress) -> Option<ValuePlace> {
        let read =
            |nodes: NodePage<'_>| nodes.node(address.slot()).expect("a node").value().place();
        self.pages.read(address.page(), read).expect("the page")
    }
}

#[derive(Debug, Clone)]
struct TreeNode {
    key: Vec<u8>,
    value: Vec<u8>,
    left: Option<usize>,
    right: Option<usize>,
    at: Option<CellAddress>,
    new_value: bool,
    alive: bool,
}

/// A binary search tree that states its writes to fragment maintenance, as
/// a Merk would: every write holds the nodes on the way from the root to
/// what it changed.
#[derive(Debug, Clone)]
struct Tree {
    nodes: Vec<TreeNode>,
    root: Option<usize>,
    root_at: Option<CellAddress>,
    dirty: BTreeSet<usize>,
    removed: Vec<CellAddress>,
    fragments: Fragments,
}

impl Tree {
    fn new() -> Self {
        let layout = Layout::new(SIZE, LayoutParams::default()).expect("a layout");
        Self {
            nodes: Vec::new(),
            root: None,
            root_at: None,
            dirty: BTreeSet::new(),
            removed: Vec::new(),
            fragments: Fragments::new(layout),
        }
    }

    fn child(&self, node: usize, side: Side) -> Option<usize> {
        match side {
            Side::Left => self.nodes[node].left,
            Side::Right => self.nodes[node].right,
        }
    }

    fn set_child(&mut self, parent: Option<(usize, Side)>, child: Option<usize>) {
        match parent {
            None => self.root = child,
            Some((node, Side::Left)) => self.nodes[node].left = child,
            Some((node, Side::Right)) => self.nodes[node].right = child,
        }
    }

    /// The node with `key` and the node above it, with every node on the
    /// way marked as written.
    fn find(&mut self, key: &[u8]) -> (Option<usize>, Option<(usize, Side)>) {
        let mut parent = None;
        let mut at = self.root;
        while let Some(node) = at {
            self.dirty.insert(node);
            let side = match key.cmp(&self.nodes[node].key) {
                std::cmp::Ordering::Equal => break,
                std::cmp::Ordering::Less => Side::Left,
                std::cmp::Ordering::Greater => Side::Right,
            };
            parent = Some((node, side));
            at = self.child(node, side);
        }
        (at, parent)
    }

    fn put(&mut self, key: &[u8], value: &[u8]) {
        let (found, parent) = self.find(key);
        if let Some(node) = found {
            self.nodes[node].value = value.to_vec();
            self.nodes[node].new_value = true;
            return;
        }
        let node = self.nodes.len();
        self.nodes.push(TreeNode {
            key: key.to_vec(),
            value: value.to_vec(),
            left: None,
            right: None,
            at: None,
            new_value: true,
            alive: true,
        });
        self.dirty.insert(node);
        self.set_child(parent, Some(node));
    }

    fn delete(&mut self, key: &[u8]) -> bool {
        let (Some(node), parent) = self.find(key) else {
            return false;
        };
        let (left, right) = (self.nodes[node].left, self.nodes[node].right);
        let replacement = match (left, right) {
            (None, other) | (other, None) => other,
            (Some(left), Some(right)) => {
                let mut above = None;
                let mut least = right;
                self.dirty.insert(least);
                while let Some(next) = self.nodes[least].left {
                    above = Some(least);
                    least = next;
                    self.dirty.insert(least);
                }
                if let Some(above) = above {
                    self.nodes[above].left = self.nodes[least].right;
                    self.nodes[least].right = Some(right);
                }
                self.nodes[least].left = Some(left);
                Some(least)
            }
        };
        self.set_child(parent, replacement);
        self.dirty.remove(&node);
        self.nodes[node].alive = false;
        self.removed.extend(self.nodes[node].at.take());
        true
    }

    /// The node with `key`.
    fn node(&self, key: &[u8]) -> &TreeNode {
        let found = self.nodes.iter().find(|node| node.alive && node.key == key);
        found.expect("the key is in the tree")
    }

    /// Where the cell of the node with `key` is.
    fn at(&self, key: &[u8]) -> CellAddress {
        self.node(key).at.expect("a written node")
    }

    /// States what changed since the last write to fragment maintenance,
    /// with the scope's values, and takes the addresses it reports.
    fn flush(&mut self, scope: &mut Scope<'_>) -> Report {
        let order: Vec<usize> = self.dirty.iter().copied().collect();
        let index_of: HashMap<usize, usize> = order
            .iter()
            .enumerate()
            .map(|(index, node)| (*node, index))
            .collect();
        let nodes = &self.nodes;
        let target = |node: usize| match index_of.get(&node) {
            Some(index) => Target::Node(*index),
            None => Target::At(nodes[node].at.expect("a node outside the write has a cell")),
        };
        let link = |child: Option<usize>| {
            child.map(|child| LinkWrite {
                child: target(child),
                hash: &HASH,
                heights: (1, 1),
                child_key_len: nodes[child].key.len() as u8,
                aggregate: &[],
            })
        };
        let writes: Vec<NodeWrite<'_>> = order
            .iter()
            .map(|&node| {
                let node = &nodes[node];
                let value = match node.new_value {
                    true => scope.values.write(&node.value).expect("a value"),
                    false => ValueWrite::Keep,
                };
                NodeWrite {
                    at: node.at,
                    key: &node.key,
                    feature: &[0],
                    value_hash: &HASH,
                    kv_hash: &HASH,
                    left: link(node.left),
                    right: link(node.right),
                    hosted: HostedWrite::Keep,
                    value,
                }
            })
            .collect();
        let roots = [RootWrite {
            was: self.root_at,
            now: self.root.map(target),
            anchored: false,
        }];
        let write = Write {
            nodes: &writes,
            removed: &self.removed,
            dropped: &[],
            roots: &roots,
        };
        let report = self
            .fragments
            .apply(&mut scope.pages, &mut scope.values, &write)
            .expect("the write is applied");

        let by_address: HashMap<CellAddress, usize> = nodes
            .iter()
            .enumerate()
            .filter_map(|(index, node)| Some((node.at.filter(|_| node.alive)?, index)))
            .collect();
        let others: Vec<(usize, CellAddress)> = report
            .carried
            .iter()
            .filter(|carried| carried.node.is_none())
            .map(|carried| (by_address[&carried.from], carried.to.address()))
            .collect();
        for (index, handle) in report.nodes.iter().enumerate() {
            let node = &mut self.nodes[order[index]];
            node.at = Some(handle.address());
            node.new_value = false;
        }
        for (node, at) in others {
            self.nodes[node].at = Some(at);
        }
        self.root_at = report.roots[0];
        self.dirty.clear();
        self.removed.clear();
        report
    }

    /// Every record of the tree as the committed pages have it at the
    /// latest version, in key order: the key, the value as it is read, and
    /// where the cell is and what it holds for the value.
    fn scan(&self, db: &Db) -> Vec<Scanned> {
        let reader = NodeReader::new(db.loader(), &Guard, Lease::take(), db.latest());
        let mut buffers = LoadBuffers::new();
        let mut buf = ValueBuf::new();
        let mut found = Vec::new();
        let mut stack = Vec::new();
        let mut next = self.root_at;
        loop {
            while let Some(address) = next {
                let node = reader.node(address, &mut buffers).expect("a node");
                next = node.node().link(Side::Left).map(|link| link.child);
                stack.push(node);
            }
            let Some(node) = stack.pop() else {
                return found;
            };
            let view = node.node();
            found.push(Scanned {
                key: view.key().to_vec(),
                value: read_value(&db.values, &node, &mut buf)
                    .expect("the value is read")
                    .to_vec(),
                at: view.address(),
                held: view.value().place(),
            });
            next = view.link(Side::Right).map(|link| link.child);
        }
    }
}

/// One record of a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Scanned {
    key: Vec<u8>,
    value: Vec<u8>,
    at: CellAddress,
    held: Option<ValuePlace>,
}

fn key(index: u32) -> Vec<u8> {
    index.to_be_bytes().to_vec()
}

/// A value of `index` that is too long for its cell under the default
/// policy.
fn big(index: u32) -> Vec<u8> {
    bytes(u64::from(index), 300 + (index as usize * 37) % 400)
}
