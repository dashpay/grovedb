//! Tests of recovery, on stores whose log is written by hand: a session
//! here appends groups, the records of page steps and marks to the log,
//! writes the blocks and the table checkpoints it wants a recovery to
//! find, and keeps a model of what each page says. It has no buffer pool
//! of its own to keep right, so a test can leave exactly the files that a
//! rule is about.
//!
//! The tests that run a store as a session runs it, with commits that
//! install pages and with write-back, are write-back's
//! (`flush/tests/crash.rs`, `faults.rs`, `threads.rs`, and
//! `recovered.rs` for two sessions): every state they recover is
//! recovered by [`recover`] as well.

mod acceptance;
mod damage;
mod errors;
mod rules;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use lignin_format::{
    delta::{Delta, DeltaRun, DeltaRunWriter},
    manifest::Manifest,
    segment::{BlockContent, Compression},
    tree_page::{CellContent, CellValue, HashField, TreeCell, TreePage, TreePageMut},
    value_log::{PendingValue, ValuePlace, ValuePointer},
    wal::{write_reclaim, GroupWriter, LogPlace, Removed, ValueMove},
    CommitVersion, FormatVersion, PageId, PageSize, SlotId,
};
use lignin_io::{StdVfs, Vfs};
use tempfile::TempDir;

use super::*;
use crate::{
    alloc::PageState,
    cache::PageRef,
    load::{LoadBuffers, LoadError, FRAME_FORMAT},
    segment::SegmentWriter,
    table::{CheckpointInfo, CountSample, Link, LogDurable, PageTableError, ValueCount},
    values::{ValueChange, ValueHold, ValueWriter},
    wal::Appended,
};

const V1: FormatVersion = FormatVersion::V1;

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("a page id")
}

fn slot(raw: u16) -> SlotId {
    SlotId::new(raw).expect("a slot")
}

/// How the tests recover a store: with a small pool.
fn options() -> RecoveryOptions {
    RecoveryOptions {
        pool_capacity: 8 << 20,
        ..RecoveryOptions::default()
    }
}

/// A database directory, in memory where there is such a place.
struct Dir {
    _tmp: TempDir,
    vfs: Arc<dyn Vfs>,
    path: PathBuf,
    size: PageSize,
}

impl Dir {
    /// A directory with the first manifest of a store of the smallest
    /// pages.
    fn new() -> Self {
        Self::of(PageSize::MIN)
    }

    fn of(size: PageSize) -> Self {
        let memory = Path::new("/dev/shm");
        let tmp = match memory.is_dir() {
            true => tempfile::tempdir_in(memory),
            false => tempfile::tempdir(),
        }
        .expect("a directory");
        let vfs: Arc<dyn Vfs> = Arc::new(StdVfs);
        let empty = Manifest::new(size, Default::default(), None, Vec::new()).expect("a manifest");
        ManifestStore::create(vfs.clone(), tmp.path(), empty).expect("a store");
        Self {
            path: tmp.path().to_path_buf(),
            _tmp: tmp,
            vfs,
            size,
        }
    }

    /// A copy of the directory, every file synced, in the directory that
    /// the environment variable `LIGNIN_STORE_TEST_DIR` names, if it is
    /// set: this is how the acceptance measurement is put on a disk.
    fn on_disk(&self) -> Option<Self> {
        let named = std::env::var_os("LIGNIN_STORE_TEST_DIR")?;
        let tmp = tempfile::tempdir_in(named).expect("a directory");
        let mut dirs = vec![self.path.clone()];
        while let Some(dir) = dirs.pop() {
            let copy = tmp
                .path()
                .join(dir.strip_prefix(&self.path).expect("inside"));
            std::fs::create_dir_all(&copy).expect("a directory");
            for entry in std::fs::read_dir(&dir).expect("a directory") {
                let path = entry.expect("an entry").path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                let to = copy.join(path.file_name().expect("a name"));
                std::fs::copy(&path, &to).expect("copied");
                let synced = std::fs::File::open(&to).and_then(|file| file.sync_all());
                synced.expect("synced");
            }
        }
        Some(Self {
            path: tmp.path().to_path_buf(),
            _tmp: tmp,
            vfs: self.vfs.clone(),
            size: self.size,
        })
    }

    fn manifest(&self) -> ManifestStore {
        ManifestStore::open(self.vfs.clone(), &self.path).expect("a manifest")
    }

    fn recover_with(&self, options: &RecoveryOptions) -> Result<Recovered, RecoveryError> {
        recover(self.manifest(), options)
    }

    fn recover(&self) -> Result<Recovered, RecoveryError> {
        self.recover_with(&options())
    }

    /// The store, recovered.
    fn open(&self) -> Recovered {
        self.recover()
            .unwrap_or_else(|error| panic!("the store does not recover: {error}"))
    }

    /// The name and the bytes of every file of the directory.
    fn files(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        let mut dirs = vec![self.path.clone()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).expect("a directory") {
                let path = entry.expect("an entry").path();
                if path.is_dir() {
                    dirs.push(path);
                } else {
                    let bytes = std::fs::read(&path).expect("a file");
                    let name = path.strip_prefix(&self.path).expect("in the directory");
                    files.insert(name.to_path_buf(), bytes);
                }
            }
        }
        files
    }
}

/// The value of a cell that a test writes.
#[derive(Debug, Clone)]
enum Value {
    /// Stored in the cell.
    Inline(Vec<u8>),
    /// Carried by the commit's group, and pending in the cell.
    Out(Vec<u8>),
    /// In the value log already, at this pointer.
    At(ValuePointer),
}

/// One change of a commit to a tree page.
#[derive(Debug, Clone)]
enum Op {
    /// A new cell. A page that the model does not have is made by its
    /// first insert.
    Insert {
        page: u32,
        slot: u16,
        key: Vec<u8>,
        value: Value,
    },
    /// The value hash of a cell becomes 32 times `byte`.
    Hash { page: u32, slot: u16, byte: u8 },
    /// A cell leaves.
    Remove { page: u32, slot: u16 },
    /// The page is freed.
    Free { page: u32 },
}

impl Op {
    fn page(&self) -> u32 {
        match self {
            Self::Insert { page, .. }
            | Self::Hash { page, .. }
            | Self::Remove { page, .. }
            | Self::Free { page } => *page,
        }
    }
}

fn insert(at: (u32, u16), key: &[u8], value: Value) -> Op {
    Op::Insert {
        page: at.0,
        slot: at.1,
        key: key.to_vec(),
        value,
    }
}

fn hash(at: (u32, u16), byte: u8) -> Op {
    Op::Hash {
        page: at.0,
        slot: at.1,
        byte,
    }
}

fn remove(at: (u32, u16)) -> Op {
    Op::Remove {
        page: at.0,
        slot: at.1,
    }
}

fn inline(bytes: &[u8]) -> Value {
    Value::Inline(bytes.to_vec())
}

/// A value of `len` bytes that no cell holds in line.
fn long(seed: u8, len: usize) -> Value {
    Value::Out((0..len).map(|at| seed.wrapping_add(at as u8)).collect())
}

/// What the pages say, as the commits and page steps of the tests left
/// them: each page in a buffer, and the bytes of every value that is
/// pending or in the value log.
#[derive(Debug, Default, Clone)]
struct Model {
    pages: BTreeMap<PageId, Vec<u8>>,
    /// The bytes of each value that a commit carried, by what its cell
    /// held for it then.
    pending: BTreeMap<[u8; 8], Vec<u8>>,
}

impl Model {
    fn image(&self, id: PageId) -> Option<Vec<u8>> {
        let frame = self.pages.get(&id)?;
        let tree = TreePage::from_validated(frame, FRAME_FORMAT).expect("a tree page");
        Some(tree.to_image())
    }
}

/// A session that writes its log by hand.
struct Session<'d> {
    dir: &'d Dir,
    store: Recovered,
    model: Model,
    segment: Option<SegmentWriter>,
    values: Option<ValueWriter>,
    /// Keeps the values that were appended and that no page step has
    /// counted yet.
    hold: ValueHold,
}

impl<'d> Session<'d> {
    /// Opens the store of `dir` and goes on from `model`, which is what
    /// its pages say.
    fn begin(dir: &'d Dir, model: Model) -> Self {
        Self::begin_with(dir, model, &options())
    }

    fn begin_with(dir: &'d Dir, model: Model, options: &RecoveryOptions) -> Self {
        Self {
            dir,
            store: dir.recover_with(options).expect("the store recovers"),
            model,
            segment: None,
            values: None,
            hold: ValueHold::new(),
        }
    }

    fn latest(&self) -> CommitVersion {
        self.store.log.latest()
    }

    /// A commit of `ops`: its group goes to the log, with the values the
    /// ops carry, the runs of its pages in ascending order and the
    /// pointers its cells stop holding, and the model is changed. Nothing
    /// is installed anywhere else.
    fn commit(&mut self, ops: &[Op]) -> Appended {
        let size = self.dir.size;
        let log = &self.store.log;
        let at = log.next_group().expect("a place for the group");
        let mut bytes = Vec::new();
        let mut group = GroupWriter::new(at.version(), at.address(), &mut bytes).expect("a group");
        let pending: Vec<Option<PendingValue>> = ops
            .iter()
            .map(|op| match op {
                Op::Insert {
                    page: id,
                    value: Value::Out(value),
                    ..
                } => {
                    let pending = group.value(page(*id), value).expect("a value");
                    self.model.pending.insert(pending.id(), value.clone());
                    Some(pending)
                }
                _ => None,
            })
            .collect();

        let mut ids: Vec<u32> = ops.iter().map(Op::page).collect();
        ids.sort_unstable();
        ids.dedup();
        let mut released = Vec::new();
        for id in ids {
            let of_page = || ops.iter().zip(&pending).filter(|(op, _)| op.page() == id);
            if of_page().any(|(op, _)| matches!(op, Op::Free { .. })) {
                let frame = self.model.pages.remove(&page(id)).expect("the page");
                let tree = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
                released.extend(tree.cells().filter_map(|(_, cell)| cell.value_pointer()));
                group.freed(page(id)).expect("a freed page");
                continue;
            }
            let created = !self.model.pages.contains_key(&page(id));
            let frame = self
                .model
                .pages
                .entry(page(id))
                .or_insert_with(|| vec![0; size.get()]);
            if created {
                TreePageMut::init(frame, size, page(id), 0).expect("an empty page");
            }
            let run = group.tree_run_with(page(id), created.then_some(0), |run| {
                let mut tree = TreePageMut::open(frame, size, FRAME_FORMAT)?;
                for (op, pending) in of_page() {
                    let encoded;
                    let delta = match op {
                        Op::Insert {
                            slot: at,
                            key,
                            value,
                            ..
                        } => {
                            let value = match (value, pending) {
                                (Value::Inline(bytes), _) => CellValue::Inline(bytes),
                                (Value::At(pointer), _) => CellValue::OutOfLine(*pointer),
                                (Value::Out(_), Some(pending)) => CellValue::Pending(*pending),
                                (Value::Out(_), None) => unreachable!("the group carries it"),
                            };
                            encoded = CellContent {
                                key,
                                feature: &[],
                                value_hash: &[0x11; 32],
                                kv_hash: &[0x22; 32],
                                left: None,
                                right: None,
                                hosted_roots: &[],
                                value,
                                fragment_root: true,
                            }
                            .encode()?;
                            Delta::Insert {
                                slot: slot(*at),
                                cell: TreeCell::decode(&encoded, V1)?,
                            }
                        }
                        Op::Hash { slot: at, byte, .. } => Delta::Hash {
                            slot: slot(*at),
                            field: HashField::Value,
                            hash: &[*byte; 32],
                        },
                        Op::Remove { slot: at, .. } => {
                            let held = tree
                                .page()
                                .cell(slot(*at))
                                .and_then(|cell| cell.value_pointer());
                            released.extend(held);
                            Delta::Remove { slot: slot(*at) }
                        }
                        Op::Free { .. } => unreachable!("a freed page has no run"),
                    };
                    delta.apply(&mut tree)?;
                    run.push(&delta)?;
                }
                Ok(())
            });
            run.expect("a run");
        }
        released.sort_by_key(|pointer| (pointer.file(), pointer.offset()));
        for pointer in &released {
            group.released(pointer).expect("a released pointer");
        }
        group.finish();
        let appended = log.append_group(&at, &bytes).expect("appended");
        let mut change = ValueChange::new();
        released.iter().for_each(|&pointer| change.release(pointer));
        self.store
            .values
            .apply(&change, at.version())
            .expect("counted");
        appended
    }

    /// Appends `bytes` to the value log as a value of the page `id`, under
    /// a manifest that covers it, and returns its pointer. No cell holds
    /// it yet.
    fn append_value(&mut self, id: u32, bytes: &[u8]) -> ValuePointer {
        let pointer = self.append_uncovered(id, bytes);
        self.cover_values();
        pointer
    }

    /// Appends `bytes` to the value log as a value of the page `id`. No
    /// manifest covers it until [`Self::cover_values`].
    fn append_uncovered(&mut self, id: u32, bytes: &[u8]) -> ValuePointer {
        let store = &mut self.store;
        if let Some(full) = self.values.take_if(|writer| writer.is_full()) {
            full.finish().expect("finished");
        }
        if self.values.is_none() {
            let writer = store.values.create().expect("a value-log file");
            store.values.commit(&mut store.manifest).expect("named");
            self.values = Some(writer);
        }
        let writer = self.values.as_mut().expect("a writer");
        writer
            .append(&mut self.hold, page(id), bytes)
            .expect("appended")
    }

    /// Makes the values that were appended durable, and has a manifest
    /// cover them.
    fn cover_values(&mut self) {
        let store = &mut self.store;
        store.values.sync().expect("synced");
        store.values.commit(&mut store.manifest).expect("covered");
    }

    /// The pending value that the cell at `at` holds.
    fn pending_of(&self, at: (u32, u16)) -> PendingValue {
        let frame = &self.model.pages[&page(at.0)];
        let tree = TreePage::from_validated(frame, FRAME_FORMAT).expect("a tree page");
        let cell = tree.cell(slot(at.1)).expect("the cell");
        match cell.value() {
            CellValue::Pending(pending) => pending,
            other => panic!("the cell holds {other:?}"),
        }
    }

    /// The page step that write-back makes for a pending value: the value
    /// of the cell goes to the value log, and the cell gets its pointer.
    /// Returns the step and where its record lies.
    fn place(&mut self, at: (u32, u16)) -> (ValueMove, Appended) {
        let pending = self.pending_of(at);
        let bytes = self.model.pending[&pending.id()].clone();
        let to = self.append_value(at.0, &bytes);
        let from = ValuePlace::Pending(pending);
        let step = ValueMove::in_tree(page(at.0), slot(at.1), from, to).expect("a step");
        let appended = self.step(&step);
        (step, appended)
    }

    /// Places the pending values of the cells `cells` as one run of
    /// write-back does: the values are appended, one manifest covers them
    /// all, and then each cell gets its pointer in a page step.
    fn place_all(&mut self, cells: &[(u32, u16)]) {
        let mut steps = Vec::with_capacity(cells.len());
        for &at in cells {
            let pending = self.pending_of(at);
            let bytes = self.model.pending.remove(&pending.id()).expect("its bytes");
            let to = self.append_uncovered(at.0, &bytes);
            let from = ValuePlace::Pending(pending);
            steps.push(ValueMove::in_tree(page(at.0), slot(at.1), from, to).expect("a step"));
        }
        self.cover_values();
        for step in &steps {
            self.step(step);
        }
    }

    /// Makes the page step `step`: its record goes to the log, the model
    /// is changed, and the counts take what the step does to them.
    fn step(&mut self, step: &ValueMove) -> Appended {
        let mut record = Vec::new();
        step.write(&mut record).expect("a record");
        let counts = self.store.values.step().expect("the counts");
        let appended = self.store.log.append_change(&record).expect("appended");
        let frame = self.model.pages.get_mut(&step.page()).expect("the page");
        let delta = step.delta().expect("a step of a tree page");
        let mut tree = TreePageMut::open(frame, self.dir.size, FRAME_FORMAT).expect("a tree page");
        delta.apply(&mut tree).expect("the cell takes the pointer");
        let mut change = ValueChange::new();
        step.released()
            .into_iter()
            .for_each(|from| change.release(from));
        change.acquire(step.acquired());
        counts.apply(&change, self.latest()).expect("counted");
        appended
    }

    /// The page step of a reclaimer: the cells in `slots` of the page `id`
    /// leave, and with `freed` the page is freed at that version. The
    /// record names the pointers the cells held. Returns where it lies.
    fn reclaim(&mut self, id: u32, slots: &[u16], freed: Option<u64>) -> Appended {
        let size = self.dir.size;
        let frame = self.model.pages.get_mut(&page(id)).expect("the page");
        let mut released = Vec::new();
        let mut run = Vec::new();
        {
            let mut tree = TreePageMut::open(frame, size, FRAME_FORMAT).expect("a tree page");
            let mut records = DeltaRunWriter::new(page(id), None, &mut run);
            for &at in slots {
                let held = tree
                    .page()
                    .cell(slot(at))
                    .and_then(|cell| cell.value_pointer());
                released.extend(held);
                let delta = Delta::Remove { slot: slot(at) };
                delta.apply(&mut tree).expect("the cell leaves");
                records.push(&delta).expect("a record");
            }
            if !slots.is_empty() {
                records.finish().expect("a run");
            }
        }
        if freed.is_some() {
            let frame = self.model.pages.remove(&page(id)).expect("the page");
            let tree = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
            released.extend(tree.cells().filter_map(|(_, cell)| cell.value_pointer()));
        }
        released.sort_by_key(|pointer| (pointer.file(), pointer.offset()));
        let removed =
            (!slots.is_empty()).then(|| Removed::Tree(DeltaRun::decode(&run, V1).expect("a run")));
        let mut record = Vec::new();
        write_reclaim(
            page(id),
            removed,
            freed.map(version),
            released.iter().copied(),
            &mut record,
        )
        .expect("a record");
        let counts = self.store.values.step().expect("the counts");
        let appended = self.store.log.append_change(&record).expect("appended");
        let mut change = ValueChange::new();
        released.iter().for_each(|&pointer| change.release(pointer));
        counts.apply(&change, self.latest()).expect("counted");
        appended
    }

    /// Writes the page `id` to a segment as the model has it, as its base
    /// image of `born`, and links the block in the session's page table.
    fn write_back(&mut self, id: u32, born: u64) {
        let store = &mut self.store;
        if self.segment.is_none() {
            let writer = store.segments.create().expect("a segment");
            store.segments.commit(&mut store.manifest).expect("named");
            self.segment = Some(writer);
        }
        let image = self.model.image(page(id)).expect("the page");
        let writer = self.segment.as_mut().expect("a segment");
        let block = writer
            .append(page(id), BlockContent::Base, Compression::Lz4, &image)
            .expect("appended");
        store
            .table
            .push(page(id), Link::base(version(born), block))
            .expect("linked");
    }

    /// Takes a table checkpoint of the session's page table that says it
    /// covers `covers`: the sample of the counts with its mark, the copy,
    /// the sync of the log, and the manifest that names it. Returns the
    /// place of the sample.
    fn checkpoint(&mut self, covers: u64) -> LogPlace {
        let sample = self.sample(covers);
        self.checkpoint_with(covers, &sample, None)
    }

    /// Takes the sample of the value counts for a checkpoint that covers
    /// `covers`: the counts as they are, and the mark whose end is their
    /// place.
    fn sample(&mut self, covers: u64) -> CountSample {
        let step = self.store.values.step().expect("the counts");
        let counts = step.counts();
        let mark = self.store.log.append_mark(version(covers)).expect("a mark");
        drop(step);
        self.sample_at(mark.end, &counts)
    }

    /// A sample that says the counts `counts` are those of `place`.
    fn sample_at(&self, place: LogPlace, counts: &[FileCount]) -> CountSample {
        CountSample {
            place: Some(place),
            counts: counts
                .iter()
                .map(|count| ValueCount {
                    file: count.file,
                    values: count.values,
                    bytes: count.bytes,
                    miscounted: count.miscounted,
                })
                .collect(),
        }
    }

    /// Writes and installs a table checkpoint of the session's page table
    /// with the sample `sample`. `durable` is what the checkpoint is told
    /// of the log, if that is not what a sync of it says. Returns the
    /// place of the sample.
    fn checkpoint_with(
        &mut self,
        covers: u64,
        sample: &CountSample,
        durable: Option<LogDurable>,
    ) -> LogPlace {
        let info = self.write_checkpoint(covers, sample);
        let synced = self.sync();
        self.install(&info, durable.unwrap_or(synced))
            .expect("installed");
        sample.place.expect("a place")
    }

    /// Writes a table checkpoint of the session's page table with the
    /// sample `sample`. No manifest names it yet.
    fn write_checkpoint(&mut self, covers: u64, sample: &CountSample) -> CheckpointInfo {
        let store = &self.store;
        let log = &store.log;
        store
            .checkpoints
            .write(&store.table, version(covers), sample, || log.end())
            .expect("a checkpoint")
    }

    /// Makes the log and the values durable, and says how far the log is.
    fn sync(&mut self) -> LogDurable {
        let synced = self.store.log.sync().expect("the log is durable");
        self.store.values.sync().expect("the values are durable");
        synced
    }

    /// Has a manifest name the checkpoint `info`, which is told that the
    /// log is durable as `durable` says.
    fn install(
        &mut self,
        info: &CheckpointInfo,
        durable: LogDurable,
    ) -> Result<(), PageTableError> {
        let store = &mut self.store;
        store
            .checkpoints
            .install(info, durable, &mut store.manifest, &store.segments)
    }

    /// Gives the page `id`, which a step freed with the version `freed`,
    /// its free link, and trims its chain away, as the table does once no
    /// reader needs the page (`page-table.md` T11, K10).
    fn trim_freed(&mut self, id: u32, freed: u64) {
        let table = &self.store.table;
        let freed = version(freed);
        table
            .push(page(id), Link::free(freed))
            .expect("a free link");
        assert!(table.trim(page(id), freed, &mut Vec::new()), "page {id}");
        assert_eq!(table.trimmed_free(), Some(freed));
    }

    /// Turns the log to its next file, as the committer does whose file is
    /// full, and has the file after that made ready.
    fn turn(&mut self) {
        let store = &mut self.store;
        assert!(store.log.is_due(), "the file is not full");
        assert!(store.log.rotate().expect("a turn"));
        store
            .log
            .roll(&mut store.manifest)
            .expect("a file is ready");
    }

    /// Ends the session as a process that is killed ends it: nothing more
    /// is synced, and no manifest is written. Returns the model.
    fn kill(self) -> Model {
        self.model
    }
}

/// A page of a recovered store at the version it landed on, or `None` if
/// it is no page there.
fn frame_of(store: &Recovered, id: PageId) -> Option<PageRef<'_>> {
    let size = store.manifest.manifest().expect("a manifest").page_size();
    let loader =
        PageLoader::new(&store.pool, &store.table, &store.segments, size).expect("a loader");
    match loader.get(id, store.landed(), &mut LoadBuffers::new()) {
        Ok(frame) => Some(frame),
        Err(LoadError::NoPage { .. }) => None,
        Err(error) => panic!("page {id}: {error}"),
    }
}

/// Checks that a recovered store says of every page what `model` says:
/// the same image, the bytes of every pending value with the frame, a
/// page id in use exactly when its page is, and counts of live values that
/// are the pointers the pages hold. The pages `gone` are no pages.
fn check(store: &Recovered, model: &Model, gone: &[u32]) {
    let mut pointers: BTreeMap<_, u64> = BTreeMap::new();
    for &id in model.pages.keys() {
        let frame = frame_of(store, id).unwrap_or_else(|| panic!("page {id} is not there"));
        let tree = TreePage::from_validated(&frame, FRAME_FORMAT).expect("a tree page");
        assert_eq!(Some(tree.to_image()), model.image(id), "page {id}");
        assert_eq!(store.allocator.state(id), PageState::Live, "page {id}");
        for (_, pending) in tree.pending_values() {
            assert_eq!(
                frame.attached(pending.id()),
                model.pending.get(&pending.id()).map(Vec::as_slice),
                "a pending value of page {id}"
            );
        }
        for pointer in tree.cells().filter_map(|(_, cell)| cell.value_pointer()) {
            *pointers.entry(pointer.file()).or_default() += 1;
        }
    }
    for &id in gone {
        assert!(frame_of(store, page(id)).is_none(), "page {id} is there");
        assert_ne!(store.allocator.state(page(id)), PageState::Live);
    }
    let counted: BTreeMap<_, u64> = store
        .values
        .counts()
        .iter()
        .inspect(|count| assert!(!count.miscounted, "{count:?}"))
        .filter(|count| count.values > 0)
        .map(|count| (count.file, count.values))
        .collect();
    assert_eq!(
        counted, pointers,
        "the counts are the pointers of the pages"
    );
    assert_eq!(store.report.miscounts, 0);
}
