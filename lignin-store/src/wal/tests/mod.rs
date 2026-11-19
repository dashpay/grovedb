mod checkpoint;
mod crash;
mod errors;
mod faults;
mod reader;
mod store;
mod threads;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use lignin_format::{
    delta::Delta,
    flat_page::FlatValue,
    manifest::{FileState, LiveFile, Manifest},
    tree_page::HashField,
    value_log::{PendingValue, ValuePlace, ValuePointer},
    wal::{
        CarriedValue, CheckpointMark, FlatChange, GroupWriter, LogAddress, LogItem, LogPlace,
        ValueMove, FIRST_ITEM_OFFSET,
    },
    CommitVersion, FileHeader, FileId, FileKind, PageId, PageSize, SlotId,
};
use lignin_io::{StdVfs, Vfs};
use tempfile::TempDir;

use super::*;
use crate::segment::{
    read_manifest,
    tests::{
        faulty::{FaultyVfs, Op},
        sim,
    },
    FileNames, ManifestStore,
};

const HEADER: u64 = FileHeader::ENCODED_LEN as u64;
const FIRST: u64 = FIRST_ITEM_OFFSET;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn bytes(&mut self, len: u64) -> Vec<u8> {
        (0..len).map(|_| self.next() as u8).collect()
    }
}

fn file_id(raw: u32) -> FileId {
    FileId::new(raw).expect("not zero")
}

fn page(raw: u32) -> PageId {
    PageId::new(raw).expect("not zero")
}

fn version(raw: u64) -> CommitVersion {
    CommitVersion::new(raw).expect("a version")
}

fn place(file: u32, offset: u64) -> LogPlace {
    LogPlace::new(file_id(file), offset).expect("a place")
}

fn pointer(file: u32, offset: u64, len: u32) -> ValuePointer {
    ValuePointer::new(file_id(file), offset, len).expect("a pointer")
}

fn log_name(file: u32) -> String {
    FileNames::WAL.file_name(file_id(file))
}

fn empty_manifest() -> Manifest {
    Manifest::new(PageSize::DEFAULT, Default::default(), None, Vec::new())
        .expect("a valid manifest")
}

fn log_file(raw_id: u32, state: FileState, valid_len: u64) -> LiveFile {
    LiveFile::new(FileKind::Wal, file_id(raw_id), state, valid_len).expect("a valid entry")
}

/// The log files a manifest names.
fn named(manifest: &Manifest) -> Vec<LiveFile> {
    super::reader::log_files(manifest).copied().collect()
}

/// The bytes of a commit group for the place `at`, with what the values in
/// it are named by: values, carried values, runs of both kinds of page,
/// freed pages and pointers, as they are drawn.
fn group_with_values(at: &GroupPlace, rng: &mut Rng) -> (Vec<u8>, Vec<(PendingValue, Vec<u8>)>) {
    let mut out = Vec::new();
    let mut values = Vec::new();
    let mut group = GroupWriter::new(at.version(), at.address(), &mut out).expect("a group");
    for _ in 0..rng.below(3) {
        if rng.below(4) == 0 {
            let carried = CarriedValue::new(page(9), page(8), pointer(2, 64, 300));
            group.carried(&carried).expect("a carried value");
        } else {
            let len = 1 + rng.below(1500);
            let value = if rng.below(5) == 0 {
                vec![0; len as usize]
            } else {
                rng.bytes(len)
            };
            let pending = group.value(page(9), &value).expect("a value");
            values.push((pending, value));
        }
    }
    let mut next = 0;
    for _ in 0..rng.below(5) {
        next += 1 + rng.below(4) as u32;
        let hash = [rng.next() as u8; 32];
        let (key_len, value_len) = (1 + rng.below(20), rng.below(40));
        let (key, value) = (rng.bytes(key_len), rng.bytes(value_len));
        match rng.below(3) {
            0 => group.freed(page(next)),
            1 => group.flat_run_with(page(next), |run| {
                run.push(&FlatChange::Insert {
                    key: &key,
                    value: FlatValue::Inline(&value),
                })
            }),
            _ => group.tree_run_with(page(next), None, |run| {
                run.push(&Delta::Hash {
                    slot: SlotId::MIN,
                    field: HashField::KeyValue,
                    hash: &hash,
                })
            }),
        }
        .expect("a page");
    }
    if rng.below(3) == 0 {
        group
            .released(&pointer(3, 20 + rng.below(900), 50))
            .expect("a pointer");
    }
    group.finish();
    (out, values)
}

fn group(at: &GroupPlace, rng: &mut Rng) -> Vec<u8> {
    group_with_values(at, rng).0
}

/// The smallest group there is: one part with no entry.
fn empty_group(at: &GroupPlace) -> Vec<u8> {
    let mut out = Vec::new();
    GroupWriter::new(at.version(), at.address(), &mut out)
        .expect("a group")
        .finish();
    out
}

/// The record of a page step that places a value.
fn change(rng: &mut Rng) -> Vec<u8> {
    let mut out = Vec::new();
    let to = pointer(3, 20 + rng.below(9000), 77);
    let from = LogAddress::new(45 + rng.below(9000))
        .expect("an address")
        .pending(77)
        .expect("a pending value");
    ValueMove::in_tree(page(7), SlotId::MIN, ValuePlace::Pending(from), to)
        .expect("a move")
        .write(&mut out)
        .expect("a change");
    out
}

fn mark(covers: CommitVersion) -> Vec<u8> {
    let mut out = Vec::new();
    CheckpointMark::new(covers).write(&mut out).expect("a mark");
    out
}

/// One item of a log: where it lies, and the bytes it was written with.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Item {
    at: LogPlace,
    end: LogPlace,
    bytes: Vec<u8>,
    /// The version of the group, if the item is one.
    version: Option<CommitVersion>,
}

impl Item {
    fn of(read: LogRead<'_>) -> Self {
        let mut bytes = Vec::new();
        let mut version = None;
        match read.item {
            LogItem::Commit(group) => {
                bytes.extend_from_slice(group.as_bytes());
                version = Some(group.version());
            }
            LogItem::Change(change) => change.write(&mut bytes).expect("a change"),
            LogItem::Mark(mark) => mark.write(&mut bytes).expect("a mark"),
        }
        Self {
            at: read.at,
            end: read.end,
            bytes,
            version,
        }
    }

    fn appended(appended: Appended, bytes: Vec<u8>, version: Option<CommitVersion>) -> Self {
        Self {
            at: appended.at,
            end: appended.end,
            bytes,
            version,
        }
    }
}

/// Reads the log of the directory `dir` as a recovery does, with a reader
/// that fetches `window` bytes at a time.
fn read_log_with(
    vfs: &Arc<dyn Vfs>,
    dir: &Path,
    window: usize,
) -> Result<(Vec<Item>, LogEnd), LogError> {
    let manifest = read_manifest(&**vfs, dir)?;
    let mut reader = LogReader::new(vfs.clone(), dir, &manifest).with_window(window);
    let mut items = Vec::new();
    while let Some(item) = reader.next_item(Item::of)? {
        items.push(item);
    }
    assert!(reader.end().is_some(), "the reader has ended");
    assert_eq!(reader.next_item(Item::of)?, None, "an end is final");
    Ok((items, reader.into_end()?))
}

fn read_log(vfs: &Arc<dyn Vfs>, dir: &Path) -> Result<(Vec<Item>, LogEnd), LogError> {
    read_log_with(vfs, dir, DEFAULT_READ_WINDOW_LEN)
}

/// A database directory with its manifest and its log.
struct Db {
    vfs: Arc<dyn Vfs>,
    dir: PathBuf,
    manifest: ManifestStore,
    log: LogStore,
}

impl Db {
    /// A new database in the empty directory `dir`, whose log has turned to
    /// its first file.
    fn create(vfs: Arc<dyn Vfs>, dir: &Path, options: LogOptions) -> Result<Self, LogError> {
        ManifestStore::create(vfs.clone(), dir, empty_manifest())?;
        let mut db = Self::open(vfs, dir, options)?;
        db.roll()?;
        Ok(db)
    }

    /// Opens the database of `dir` as a session does after recovery: it
    /// reads the log to its end and opens the log there. The log has not
    /// turned to a file of this session yet.
    fn open(vfs: Arc<dyn Vfs>, dir: &Path, options: LogOptions) -> Result<Self, LogError> {
        let manifest = ManifestStore::open(vfs.clone(), dir)?;
        let end = LogReader::new(vfs.clone(), dir, manifest.manifest()?).into_end()?;
        let log = LogStore::open(&manifest, &end, options)?;
        Ok(Self {
            vfs,
            dir: dir.to_path_buf(),
            manifest,
            log,
        })
    }

    fn roll(&mut self) -> Result<bool, LogError> {
        self.log.roll(&mut self.manifest)
    }

    /// Commits a drawn group: asks for its place, writes it and appends it.
    fn commit(&self, rng: &mut Rng) -> Result<Item, LogError> {
        let at = self.log.next_group()?;
        let bytes = group(&at, rng);
        let appended = self.log.append_group(&at, &bytes)?;
        Ok(Item::appended(appended, bytes, Some(at.version())))
    }

    fn change(&self, rng: &mut Rng) -> Result<Item, LogError> {
        let bytes = change(rng);
        let appended = self.log.append_change(&bytes)?;
        Ok(Item::appended(appended, bytes, None))
    }

    fn mark(&self, covers: CommitVersion) -> Result<Item, LogError> {
        let appended = self.log.append_mark(covers)?;
        Ok(Item::appended(appended, mark(covers), None))
    }

    /// A drawn item: mostly groups.
    fn item(&self, rng: &mut Rng) -> Result<Item, LogError> {
        match rng.below(6) {
            0 => self.mark(version(rng.below(self.log.latest().get() + 1))),
            1 => self.change(rng),
            _ => self.commit(rng),
        }
    }

    fn read(&self) -> (Vec<Item>, LogEnd) {
        read_log(&self.vfs, &self.dir).expect("the log reads")
    }

    fn named(&self) -> Vec<LiveFile> {
        named(self.manifest.manifest().expect("a manifest"))
    }
}

/// A database on the operating system's files, behind a file layer that
/// records every call and fails the ones it is told to.
struct StdDb {
    faulty: FaultyVfs,
    db: Db,
    _dir: TempDir,
}

impl StdDb {
    fn new(options: LogOptions) -> Self {
        let dir = tempfile::tempdir().expect("a directory");
        let faulty = FaultyVfs::without_syncs(Arc::new(StdVfs));
        let db = Db::create(Arc::new(faulty.clone()), dir.path(), options).expect("a database");
        faulty.take_calls();
        Self {
            faulty,
            db,
            _dir: dir,
        }
    }

    /// Ends the session and opens the directory again.
    fn reopen(self, options: LogOptions) -> Self {
        let Self { faulty, db, _dir } = self;
        let Db { vfs, dir, .. } = db;
        let db = Db::open(vfs, &dir, options).expect("the database opens");
        faulty.take_calls();
        Self { faulty, db, _dir }
    }
}

fn small_files(target_file_len: u64) -> LogOptions {
    LogOptions {
        target_file_len,
        ..LogOptions::default()
    }
}

/// A new database on a simulated file system.
fn sim_db(config: lignin_io::sim::SimConfig, options: LogOptions) -> (lignin_io::SimVfs, Db) {
    let fs = sim::sim_fs(config);
    let db = Db::create(sim::shared(&fs), sim::db_dir(), options).expect("a database");
    (fs, db)
}

fn log_path(dir: &Path, file: u32) -> PathBuf {
    FileNames::WAL.path(dir, file_id(file))
}

/// The bytes of a file.
fn file_bytes(vfs: &dyn Vfs, path: &Path) -> Vec<u8> {
    let file = vfs
        .open(path, lignin_io::OpenMode::Read)
        .expect("the file opens");
    let mut bytes = vec![0; file.len().expect("a length") as usize];
    file.read_exact_at(&mut bytes, 0).expect("the bytes");
    bytes
}

/// Makes `bytes` the content of a file that nothing has open for
/// appending, creating it if it is missing.
fn put_file(vfs: &dyn Vfs, path: &Path, bytes: &[u8]) {
    use lignin_io::{Create, OpenMode};
    let file = vfs
        .open(path, OpenMode::Write(Create::IfMissing))
        .expect("the file opens");
    file.set_len(0).expect("a length");
    file.write_at(bytes, 0).expect("the bytes");
}

/// A database directory made by hand on a simulated file system: log files
/// with these bytes, and a manifest that names them so.
fn crafted(files: &[(LiveFile, &[u8])]) -> (lignin_io::SimVfs, Arc<dyn Vfs>) {
    crafted_on(lignin_io::sim::SimConfig::new(1), files)
}

/// As [`crafted`], on a file system with the configuration `config`.
fn crafted_on(
    config: lignin_io::sim::SimConfig,
    files: &[(LiveFile, &[u8])],
) -> (lignin_io::SimVfs, Arc<dyn Vfs>) {
    let fs = sim::sim_fs(config);
    let vfs = sim::shared(&fs);
    let dir = sim::db_dir();
    fs.create_dir(&FileNames::WAL.dir(dir)).expect("wal/");
    let mut last = lignin_format::manifest::LastFileIds::default();
    for (named, bytes) in files {
        put_file(&fs, &FileNames::WAL.path(dir, named.id()), bytes);
        last.wal = last.wal.max(Some(named.id()));
    }
    let named = files.iter().map(|(named, _)| *named).collect();
    let manifest = Manifest::new(PageSize::DEFAULT, last, None, named).expect("a manifest");
    ManifestStore::create(vfs.clone(), dir, manifest).expect("a manifest");
    (fs, vfs)
}

/// The bytes of a log file by hand: its start, and `items` drawn items
/// behind it. Returns the bytes, where each item ends, and the version of
/// the last group.
fn crafted_file(
    file: u32,
    start: lignin_format::wal::LogStart,
    items: usize,
    rng: &mut Rng,
) -> (Vec<u8>, Vec<u64>, CommitVersion) {
    let mut bytes = Vec::new();
    lignin_format::wal::write_file_start(file_id(file), &start, &mut bytes).expect("a start");
    let mut latest = start.after();
    let mut ends = Vec::new();
    for _ in 0..items {
        match rng.below(5) {
            0 => bytes.extend_from_slice(&mark(latest)),
            1 => bytes.extend_from_slice(&change(rng)),
            _ => {
                latest = latest.next().expect("a version");
                let address = start.address(bytes.len() as u64).expect("an address");
                let mut out = Vec::new();
                let mut group = GroupWriter::new(latest, address, &mut out).expect("a group");
                group.value(page(4), &rng.bytes(300)).expect("a value");
                group.freed(page(5)).expect("a page");
                group.finish();
                bytes.extend_from_slice(&out);
            }
        }
        ends.push(bytes.len() as u64);
    }
    (bytes, ends, latest)
}

/// What a session did to a log, as far as a recovery can be held to it.
#[derive(Debug, Clone, Default)]
struct Model {
    /// The items whose append returned `Ok`, in order.
    items: Vec<Item>,
    /// How many of them a sync has reported durable.
    durable: usize,
    /// How many of them lie in files that were dropped: they need not be
    /// read any more.
    first: usize,
    /// Where a log that is read may begin: for each manifest that a crash
    /// can leave in force, how many of the items lie before its first log
    /// file. The model knows them from what the session was told, not from
    /// what its manifest holds.
    starts: Vec<usize>,
    /// An item whose append failed and that can still be in the log,
    /// behind the last item, or come back there in a crash: one whose
    /// append was left unsettled, or was taken back and not made durable.
    /// A crash settles it, and so does the first manifest of the next
    /// session, which names the file closed where that session found its
    /// last item.
    maybe: Option<Item>,
    /// Whether `maybe` was taken back: its bytes are not in the file, and
    /// only a crash brings them back.
    taken_back: bool,
    /// Whether `maybe` was taken back in the session that is running: a
    /// sync of the log that succeeds keeps a crash from bringing it back.
    cut_by_sync: bool,
}

impl Model {
    fn all(&self) -> Vec<Item> {
        let mut all = self.items.clone();
        all.extend(self.maybe.clone());
        all
    }

    /// Checks what a recovery read against what was appended: it is a run
    /// of the items, in order and with nothing else in it, that holds every
    /// durable item which was not dropped. With `whole`, nothing was lost:
    /// it holds every item that was not dropped.
    fn check(&self, read: &[Item], whole: bool) -> Result<(), String> {
        let all = self.all();
        let must_reach = if whole {
            self.items.len()
        } else {
            self.durable
        };
        let Some(head) = read.first() else {
            // A log that begins where nothing must be read may read as
            // nothing.
            return if self.starts.iter().any(|&start| must_reach <= start) {
                Ok(())
            } else {
                Err(format!(
                    "nothing was read, and the items up to {must_reach} must be, from one of {:?}",
                    self.starts
                ))
            };
        };
        let Some(at) = all.iter().position(|item| item.at == head.at) else {
            return Err(format!(
                "the first item read, at {:?}, was never appended",
                head.at
            ));
        };
        let Some(appended) = all.get(at..at + read.len()) else {
            return Err(format!(
                "{} items were read from item {at} on, and {} were appended",
                read.len(),
                all.len()
            ));
        };
        if let Some(wrong) = (0..read.len()).find(|&item| read[item] != appended[item]) {
            return Err(format!(
                "item {} was read as {:?} and appended as {:?}",
                at + wrong,
                read[wrong],
                appended[wrong]
            ));
        }
        if !self.starts.contains(&at) {
            return Err(format!(
                "the log was read from item {at} on, and it begins at one of {:?}",
                self.starts
            ));
        }
        if at + read.len() < must_reach {
            return Err(format!(
                "items {at}..{} were read, and items {}..{must_reach} must be",
                at + read.len(),
                self.first
            ));
        }
        Ok(())
    }

    /// The manifest was replaced, with one that names no dropped file: no
    /// crash leaves another in force.
    fn manifest_replaced(&mut self) {
        self.starts = vec![self.first];
    }

    /// A replacement of the manifest was left in doubt: a crash can leave
    /// the manifest that was to replace the one in force.
    fn manifest_in_doubt(&mut self) {
        if !self.starts.contains(&self.first) {
            self.starts.push(self.first);
        }
    }

    /// Takes the log's word for how far it is durable: every item that
    /// ends at or before that place must survive a crash.
    fn claimed(&mut self, durable: crate::table::LogDurable) {
        let covered = |item: &Item| Some(item.end) <= durable.place;
        let count = self.items.partition_point(covered);
        self.durable = self.durable.max(count);
        let groups = self.items[..count].iter().rev();
        let last = groups.filter_map(|item| item.version).next();
        assert_eq!(
            durable.version,
            last.unwrap_or(CommitVersion::EMPTY),
            "{durable:?} is durable through that version"
        );
    }

    /// Takes what a recovery read for the log from now on. `crashed` says
    /// that it read what a crash left, in which nothing is unsettled.
    fn recovered(&mut self, read: &[Item], crashed: bool) {
        let all = self.all();
        let unsettled = self.maybe.take().filter(|_| !crashed);
        let kept = match read.first() {
            Some(head) => {
                let at = all.iter().position(|item| item.at == head.at).unwrap_or(0);
                self.first = at;
                if crashed {
                    self.starts.clear();
                }
                if !self.starts.contains(&at) {
                    self.starts.push(at);
                }
                at + read.len()
            }
            None => self.first.min(all.len()),
        };
        let read_it = kept == all.len();
        self.items = all;
        self.items.truncate(kept);
        self.durable = self.durable.min(kept);
        self.first = self.first.min(kept);
        for start in &mut self.starts {
            *start = (*start).min(kept);
        }
        self.maybe = unsettled.filter(|_| !read_it);
        self.cut_by_sync = false;
        self.taken_back &= self.maybe.is_some();
    }

    /// An append was taken back in the session that is running.
    fn took_back(&mut self, item: Item) {
        self.maybe = Some(item);
        self.taken_back = true;
        self.cut_by_sync = true;
    }

    /// The item that could still come back cannot any more.
    fn settled(&mut self) {
        self.maybe = None;
        self.taken_back = false;
        self.cut_by_sync = false;
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct Stats {
    appended: u64,
    not_appended: u64,
    taken_back: u64,
    unsettled: u64,
    syncs: u64,
    sync_failed: u64,
    turns: u64,
    dropped: u64,
    other_errors: u64,
    restarts: u64,
    crashes: u64,
    lost_in_a_crash: u64,
    states: u64,
}

/// A database on a simulated file system that is driven by drawn steps,
/// with a model of what its log must hold.
struct Run {
    fs: lignin_io::SimVfs,
    db: Option<Db>,
    options: LogOptions,
    model: Model,
    rng: Rng,
    stats: Stats,
    plans: sim::Plans,
    faults: lignin_io::sim::FaultOdds,
}

impl Run {
    fn new(config: lignin_io::sim::SimConfig, options: LogOptions, plans: sim::Plans) -> Self {
        let rng = Rng(config.seed ^ 0xA5A5);
        let faults = config.faults.clone();
        let quiet = lignin_io::sim::SimConfig {
            faults: lignin_io::sim::FaultOdds::NONE,
            ..config
        };
        let (fs, db) = sim_db(quiet, options);
        fs.set_faults(faults.clone());
        Self {
            fs,
            db: Some(db),
            options,
            model: Model {
                starts: vec![0],
                ..Model::default()
            },
            rng,
            stats: Stats::default(),
            plans,
            faults,
        }
    }

    /// Whether the session can go on after `error`. It cannot when the log
    /// is unsettled or the manifest is in doubt: both are settled by
    /// opening the directory again.
    fn ends_session(error: &LogError) -> bool {
        use crate::segment::SegmentStoreError;
        matches!(
            error,
            LogError::Unsettled { .. }
                | LogError::Manifest(SegmentStoreError::ManifestInDoubt { .. })
        )
    }

    fn failed(&mut self, error: &LogError) {
        match error {
            LogError::NotAppended { .. } => self.stats.not_appended += 1,
            LogError::TakenBack { .. } => self.stats.taken_back += 1,
            LogError::Unsettled { .. } => self.stats.unsettled += 1,
            LogError::SyncFailed { .. } => self.stats.sync_failed += 1,
            _ => self.stats.other_errors += 1,
        }
        assert!(
            error.io_error().is_some()
                || matches!(error, LogError::NoActiveFile | LogError::Manifest(_)),
            "an error the file layer did not cause: {error}"
        );
        // A session whose log cannot be synced any more ends sooner or
        // later: the next one writes the files again.
        let gives_up = matches!(error, LogError::SyncFailed { .. }) && self.rng.below(2) == 0;
        if Self::ends_session(error) || gives_up {
            self.db = None;
        }
    }

    /// Sees to it that the log has a file to turn to, and syncs nothing.
    fn prepare(&mut self) {
        let prepared = self.with_manifest(|db| match db.log.create() {
            Ok(_) | Err(LogError::SpareExists { .. }) => db.log.commit(&mut db.manifest),
            Err(error) => Err(error),
        });
        if let Some(Err(error)) = prepared {
            self.failed(&error);
        }
    }

    /// Appends a drawn item, and keeps the model as the outcome says.
    fn append(&mut self) {
        let Some(db) = self.db.as_ref() else { return };
        let Some(end) = db.log.end() else {
            return self.roll();
        };
        if matches!(db.log.next_group(), Err(LogError::NoActiveFile)) {
            // The session has no file yet, or an append to its file
            // failed: the log is turned first.
            return self.roll();
        }
        let (bytes, version, appended) = match self.rng.below(6) {
            0 => {
                let covers = version(self.rng.below(db.log.latest().get() + 1));
                (mark(covers), None, db.log.append_mark(covers))
            }
            1 => {
                let bytes = change(&mut self.rng);
                let appended = db.log.append_change(&bytes);
                (bytes, None, appended)
            }
            _ => match db.log.next_group() {
                Ok(at) => {
                    let bytes = group(&at, &mut self.rng);
                    let appended = db.log.append_group(&at, &bytes);
                    (bytes, Some(at.version()), appended)
                }
                Err(error) => (Vec::new(), None, Err(error)),
            },
        };
        match appended {
            Ok(appended) => {
                self.stats.appended += 1;
                assert_eq!(appended.at, end);
                let item = Item::appended(appended, bytes, version);
                let synced = db.log.sync_for_commit(&appended);
                self.model.items.push(item);
                match synced {
                    Ok(Some(_)) => self.synced(),
                    Ok(None) => {}
                    Err(error) => self.failed(&error),
                }
            }
            Err(error) => {
                // An append that was taken back can come back in a crash
                // until the log is synced, and an unsettled one until the
                // next session has named its file closed. A log that is
                // unsettled already wrote nothing.
                let taken_back = matches!(error, LogError::TakenBack { .. });
                let unsettled = matches!(error, LogError::Unsettled { .. });
                let after = place(end.file().get(), end.offset() + bytes.len() as u64);
                let item = Item {
                    at: end,
                    end: after,
                    bytes,
                    version,
                };
                if taken_back {
                    self.model.took_back(item);
                } else if unsettled && self.model.maybe.is_none() {
                    self.model.maybe = Some(item);
                }
                self.failed(&error);
            }
        }
    }

    fn synced(&mut self) {
        self.stats.syncs += 1;
    }

    /// Runs a step that may replace the manifest, and tells the model
    /// which manifests a crash can leave in force afterwards.
    fn with_manifest<T>(
        &mut self,
        step: impl FnOnce(&mut Db) -> Result<T, LogError>,
    ) -> Option<Result<T, LogError>> {
        let db = self.db.as_mut()?;
        let before = db.manifest.manifest().ok().cloned();
        let result = step(db);
        match db.manifest.manifest() {
            Ok(now) if Some(now) != before.as_ref() => self.model.manifest_replaced(),
            Ok(_) => {}
            Err(_) => self.model.manifest_in_doubt(),
        }
        Some(result)
    }

    fn sync(&mut self) {
        let Some(db) = self.db.as_ref() else { return };
        match db.log.sync() {
            Ok(durable) => {
                if let Some(end) = db.log.end() {
                    assert_eq!(durable.place, Some(end));
                }
                assert_eq!(durable.version, db.log.latest());
                assert_eq!(durable, db.log.durable());
                self.synced();
            }
            Err(error) => self.failed(&error),
        }
    }

    fn roll(&mut self) {
        match self.with_manifest(Db::roll) {
            Some(Ok(turned)) => {
                self.stats.turns += u64::from(turned);
                // A session's first roll names every earlier file closed.
                self.model.settled();
                self.synced();
            }
            Some(Err(error)) => self.failed(&error),
            None => {}
        }
    }

    fn rotate(&mut self) {
        let Some(db) = self.db.as_ref() else { return };
        match db.log.rotate() {
            Ok(turned) => self.stats.turns += u64::from(turned),
            Err(
                LogError::NoSpare | LogError::SpareNotNamed { .. } | LogError::CutNotDurable { .. },
            ) => {}
            Err(error) => self.failed(&error),
        }
    }

    /// Drops every file before the one the log is durable up to, and
    /// replaces the manifest.
    fn drop_files(&mut self) {
        if self.rng.below(2) == 0 {
            self.sync();
        }
        let Some(db) = self.db.as_mut() else { return };
        let Some(durable) = db.log.durable().place else {
            return;
        };
        let dropped = db.log.drop_before(durable).expect("a durable place");
        if let Some(last) = dropped.last() {
            self.stats.dropped += dropped.len() as u64;
            let kept = |item: &Item| item.at.file() > *last;
            let first = self.model.items.iter().position(kept);
            self.model.first = first.unwrap_or(self.model.items.len());
        }
        let committed = self.with_manifest(|db| db.log.commit(&mut db.manifest));
        if let Some(Err(error)) = committed {
            self.failed(&error);
        }
    }

    /// One drawn step of a session, or a new session if the last one
    /// ended.
    fn step(&mut self) {
        if self.db.is_none() {
            return self.restart();
        }
        self.draw_step();
        self.note_durable();
    }

    /// Holds the log to what it says is durable, whatever the step that
    /// made it so reported.
    fn note_durable(&mut self) {
        if let Some(db) = &self.db {
            self.model.claimed(db.log.durable());
            // A sync has made the file durable as a taken-back append
            // left it: no crash brings the item back.
            if self.model.cut_by_sync && !db.log.waits_for_sync() {
                self.model.settled();
            }
            // So has a manifest that names the item's file closed where
            // the item would begin, or names the file no more: a recovery
            // does not read that far.
            if let (Some(maybe), Ok(manifest)) = (&self.model.maybe, db.manifest.manifest()) {
                let named = named(manifest);
                let file = named.iter().find(|file| file.id() == maybe.at.file());
                let reaches = |file: &LiveFile| {
                    file.state() == FileState::Open || file.valid_len() > maybe.at.offset()
                };
                if !file.is_some_and(reaches) {
                    self.model.settled();
                }
            }
        }
    }

    fn draw_step(&mut self) {
        match self.rng.below(32) {
            0 => self.sync(),
            1 => self.roll(),
            2 | 3 => self.prepare(),
            4..=6 => self.rotate(),
            7 => self.drop_files(),
            8 if self.model.durable < self.model.items.len() => {
                let item = &self.model.items[self.model.durable];
                let through = item.end;
                let db = self.db.as_ref().expect("a session");
                match db.log.sync_through(through) {
                    Ok(durable) => {
                        assert!(durable.place >= Some(through), "{durable:?}");
                        self.synced();
                    }
                    Err(error) => self.failed(&error),
                }
            }
            _ => self.append(),
        }
    }

    /// Reads the log as it is, with no fault injected.
    fn read(fs: &lignin_io::SimVfs) -> Result<Vec<Item>, LogError> {
        let odds = fs.config().faults;
        fs.set_faults(lignin_io::sim::FaultOdds::NONE);
        let read = read_log(&sim::shared(fs), sim::db_dir());
        fs.set_faults(odds);
        read.map(|(items, _)| items)
    }

    /// Ends the session as a process that exits does, and starts the next
    /// one: nothing is lost.
    fn restart(&mut self) {
        self.db = None;
        self.stats.restarts += 1;
        let read = Self::read(&self.fs).unwrap_or_else(|error| panic!("{}: {error}", self.at()));
        if let Err(wrong) = self.model.check(&read, true) {
            panic!("{} after a restart: {wrong}", self.at());
        }
        // What was taken back is not in the file: only a crash brings it
        // back.
        let taken_back = self.model.maybe.as_ref().filter(|_| self.model.taken_back);
        assert!(
            taken_back.is_none() || read.last() != taken_back,
            "{} after a restart: an append that was taken back is in the log",
            self.at()
        );
        self.model.recovered(&read, false);
        self.open();
    }

    /// Opens the directory for the next session. If the file layer fails
    /// that, a later step tries again.
    fn open(&mut self) {
        match Db::open(sim::shared(&self.fs), sim::db_dir(), self.options) {
            Ok(db) => {
                // The manifest of a directory that opens is durable.
                self.model.manifest_replaced();
                self.db = Some(db);
                self.roll();
                self.note_durable();
            }
            Err(error) => {
                assert!(error.io_error().is_some(), "{}: {error}", self.at());
                self.stats.other_errors += 1;
            }
        }
    }

    /// The power fails, and the next session starts from one of the states
    /// that can leave.
    fn crash(&mut self, draw: u64) {
        self.db = None;
        self.stats.crashes += 1;
        self.fs = self.fs.crash(draw);
        self.fs.set_faults(lignin_io::sim::FaultOdds::NONE);
        let read = Self::read(&self.fs).unwrap_or_else(|error| panic!("{}: {error}", self.at()));
        if let Err(wrong) = self.model.check(&read, false) {
            panic!("{} after a crash: {wrong}", self.at());
        }
        let before = self.model.items.len();
        self.model.recovered(&read, true);
        self.stats.lost_in_a_crash += before.saturating_sub(self.model.items.len()) as u64;
        self.fs.set_faults(self.faults.clone());
        self.open();
    }

    fn at(&self) -> String {
        format!(
            "seed {}, {:?}, trace {:x}",
            self.fs.config().seed,
            self.stats,
            self.fs.trace_digest()
        )
    }

    /// Checks every state a crash at this moment can leave, or the ones
    /// the plans pick: the log reads, and it holds what the model says.
    fn check_crash_states(&mut self) {
        self.crash_states_with(|_| ());
    }

    /// As [`Self::check_crash_states`], and hands what each state reads as
    /// to `seen`.
    fn crash_states_with(&mut self, mut seen: impl FnMut(&[Item])) {
        let image = self.fs.crash_image();
        let states = sim::crash_states(&image, &self.plans);
        for (plan, state) in &states.states {
            state.set_faults(lignin_io::sim::FaultOdds::NONE);
            let read = read_log(&sim::shared(state), sim::db_dir())
                .unwrap_or_else(|error| panic!("{}, plan {plan:?}: {error}", self.at()));
            if let Err(wrong) = self.model.check(&read.0, false) {
                panic!("{}, plan {plan:?}: {wrong}", self.at());
            }
            seen(&read.0);
        }
        self.stats.states += states.states.len() as u64;
    }
}
