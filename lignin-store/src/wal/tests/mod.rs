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
    manifest
        .files()
        .iter()
        .filter(|file| file.kind() == FileKind::Wal)
        .copied()
        .collect()
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
