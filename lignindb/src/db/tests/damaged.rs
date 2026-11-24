//! What the engine does with a database directory that was damaged on disk
//! under checksums that are right.
//!
//! A checksum tells a file that rotted from one that was written. It does
//! not tell who wrote it: whoever can write a file of a database directory
//! can write its checksums too. The lists of `verify/tests/injected` change
//! a page of a store that is open, behind the checksums. The cases here
//! change a file of a directory that the facade made, write every checksum
//! of that file again, and give the directory to an open, so that what was
//! changed goes through the whole of the load: the manifest, the page
//! table, the replay of the log, the segments and the value log.
//!
//! Each case then does with the directory what a node does with its own:
//! the open, every read of a write transaction and of a snapshot, cursors
//! in both directions, the checker, a write and its commit, write-back, the
//! reclaimer and the cleaner, a checkpoint and a backup of it, the close, a
//! backup of the closed directory, and a second open.
//!
//! An error is an answer, and so is a read that returns what was not
//! written: the directory is damaged. What is a finding:
//!
//! - a panic that is raised in the engine's code, on the thread of the case
//!   or on a thread of the session,
//! - a walk that takes more steps than [`WALK_LIMIT`],
//! - a case that does not end within [`CASE_LIMIT`].
//!
//! A process that is ended by a signal (a stack that overflows, memory that
//! is not there) is a finding that the test cannot report: the seed of the
//! case under way is in the file `current-<pid>` of `LIGNIN_DAMAGE_OUT`,
//! which whoever runs the campaign reads.
//!
//! The campaign is this test with `LIGNIN_DAMAGE_SEEDS=first..end`, and
//! `LIGNIN_DAMAGE_SECONDS` to end it by the clock: it prints what its cases
//! came to (`TALLY`, `CAMPAIGN`), and each finding as it is met
//! (`FINDING`), whose directory it keeps in `LIGNIN_DAMAGE_OUT`. With
//! `LIGNIN_DAMAGE_IMAGES` the directories that are damaged are read from
//! that directory, and made and kept there where they are missing, so that
//! a seed is the same case in every process that reads them.

use std::{
    cell::Cell,
    collections::BTreeMap,
    fmt::Debug,
    fs,
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, Once, PoisonError,
    },
    thread,
    time::{Duration, Instant},
};

use lignin_format::{tree_page::Side, Checksum, FileHeader};
use lignin_io::StdVfs;
use lignin_store::cleaner::CleanerConfig;
use lignin_tree::value::ValueBuf;

use super::{image, options, temp};
use crate::{
    backup::{
        tests::world::{batch, commit, drawn, first, height, key, value, State, SUBTREES},
        Backup,
    },
    checkpoint::CheckpointOptions,
    commit::Committed,
    txn::{
        tests::{
            model::{self, OwnedBatch, OwnedSubtree},
            put,
            store::Rng,
        },
        ChildKeys, Column, Unit,
    },
    Db, Options,
};

/// What a walk says when it has taken more steps than a directory of these
/// tests has records.
const ENDLESS: &str = "a walk that does not end";

/// More steps than a walk of anything in these directories takes: the
/// largest keyspace has a few hundred records.
const WALK_LIMIT: usize = 20_000;

/// How long a case may take before it is said not to end. A case on a
/// sound directory takes a fraction of a second.
const CASE_LIMIT: Duration = Duration::from_secs(120);

/// The cases of the test where no range of seeds is given.
const SHORT: u64 = 96;

/// The files of a database directory by their paths from it: the bytes of
/// a file, and `None` for a directory.
type Files = BTreeMap<PathBuf, Option<Vec<u8>>>;

/// What a directory of these tests was made to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Shape {
    /// Merks that were written, entered and dropped over many commits, one
    /// of them hosted by a node of another, with flat records beside them.
    Merks,
    /// Flat records over several pages, with long keys and long values.
    Flat,
    /// Merks whose values are out of line and were rewritten, so that the
    /// value log holds dead values beside the live ones.
    Values,
    /// A checkpoint of a database that went on writing.
    Checkpoint,
    /// A database after drops, their reclaiming and passes of the cleaner.
    Cleaned,
    /// A database whose process was killed: the files as they were while
    /// the session was open.
    Killed,
}

const SHAPES: [Shape; 6] = [
    Shape::Merks,
    Shape::Flat,
    Shape::Values,
    Shape::Checkpoint,
    Shape::Cleaned,
    Shape::Killed,
];

/// The options of the sessions that make the directories: a cleaner that
/// closes a file once it holds 4 KiB.
fn making() -> Options {
    Options {
        cleaner: Some(CleanerConfig {
            min_segment_len: 4 << 10,
            idle: Duration::from_millis(5),
            ..CleanerConfig::new()
        }),
        ..options()
    }
}

/// The options of the sessions that open the damaged directories.
fn opening() -> Options {
    Options {
        create_if_missing: false,
        ..making()
    }
}

/// The commits that make a directory of one shape, to a database or to
/// the model alone: the model's state is the same either way.
struct Maker<'a> {
    db: Option<&'a Db>,
    state: State,
    rng: Rng,
    number: u64,
}

impl Maker<'_> {
    fn commit(&mut self, batch: &OwnedBatch) {
        if let Some(db) = self.db {
            match commit(db, batch, false) {
                Ok(Committed::Published(_)) => {}
                other => panic!("commit {}: {other:?}", self.number),
            }
        }
        self.state.commit(batch);
    }

    /// `count` commits drawn from the generator: the first write of the
    /// database, then changes of every kind.
    fn drawn(&mut self, count: u64) {
        for _ in 0..count {
            self.number += 1;
            let batch = match self.number {
                1 => first(&self.state),
                number => drawn(&self.state, &mut self.rng, number),
            };
            self.commit(&batch);
        }
    }

    fn records(&mut self, records: Vec<model::Write>, subtrees: Vec<OwnedSubtree>) {
        self.number += 1;
        let mut all = vec![height(self.number)];
        all.extend(records);
        self.commit(&batch(Vec::new(), all, subtrees));
    }

    /// The Merk `name` as `edit` leaves its items, entered where it is
    /// not in use.
    fn merk(&mut self, name: u8, edit: impl FnOnce(&mut BTreeMap<Vec<u8>, Vec<u8>>)) {
        self.number += 1;
        let enter = match self.state.has(name) {
            true => Vec::new(),
            false => vec![OwnedSubtree::Enter(model::subtree(name), None)],
        };
        let merk = self.state.merk(name, edit);
        self.commit(&batch(vec![merk], vec![height(self.number)], enter));
    }
}

/// A directory of one shape with the model's state of what it holds.
struct Made {
    files: Files,
    state: State,
}

/// Makes the commits of `shape`, and what else the shape asks of the
/// session where there is one, whose checkpoint goes into `place`.
/// Returns the state that the directory of the shape holds.
fn make(shape: Shape, db: Option<(&Db, &Path)>) -> State {
    let mut maker = Maker {
        db: db.map(|(db, _)| db),
        state: State::new(),
        rng: Rng::new(0x5eed_0000 + shape as u64),
        number: 0,
    };
    maker.drawn(12);
    let mut held = None;
    match shape {
        Shape::Merks => maker.drawn(12),
        Shape::Flat => {
            for round in 0..6u32 {
                let mut records = Vec::new();
                for at in 0..60u32 {
                    let column = [Column::Aux, Column::Meta][(at % 2) as usize];
                    let mut name = format!("flat{round:02}{at:03}").into_bytes();
                    if at % 20 == 7 {
                        name.resize(900, b'k');
                    }
                    let len = match at % 15 {
                        3 => 3000,
                        _ => 20 + (at as usize % 90),
                    };
                    let value = value(at, round as u8, len);
                    records.push(put(model::unit(column, 2), &name, &value));
                }
                maker.records(records, Vec::new());
            }
        }
        Shape::Values => {
            for round in 0..5u8 {
                maker.merk(2, |items| {
                    for at in 0..40u32 {
                        let len = 1300 + 53 * at as usize;
                        items.insert(key(b'b', at), value(at, round, len));
                    }
                });
            }
        }
        Shape::Checkpoint => {
            if let Some((db, place)) = db {
                (db.checkpoint(&place.join("checkpoint"), CheckpointOptions::default()))
                    .expect("a checkpoint");
            }
            held = Some(maker.state.clone());
            maker.drawn(6);
        }
        Shape::Cleaned => {
            for name in 4..=SUBTREES {
                maker.merk(name, |items| {
                    for at in 0..24u32 {
                        items.insert(key(b'm', at), value(at, name, 1500));
                    }
                });
            }
            if let Some((db, _)) = db {
                db.flush().expect("write-back");
            }
            for name in 4..=SUBTREES {
                maker.records(Vec::new(), vec![OwnedSubtree::Drop(model::subtree(name))]);
            }
            if let Some((db, _)) = db {
                // The drops are reclaimed and the cleaner gives back what
                // they left dead, on its thread or in the passes asked for
                // here.
                let began = Instant::now();
                let gave_back = |db: &Db| {
                    let cleaner = db.stats().cleaner;
                    cleaner.segments_dropped + cleaner.blocks_moved + cleaner.links_trimmed
                };
                while gave_back(db) == 0 {
                    assert!(began.elapsed() < Duration::from_secs(60), "{}", db.stats());
                    db.flush().expect("write-back");
                    let _ = db.reclaim();
                    db.clean().expect("a pass of the cleaner");
                    thread::sleep(Duration::from_millis(2));
                }
            }
            maker.drawn(3);
        }
        Shape::Killed => maker.drawn(8),
    }
    held.unwrap_or(maker.state)
}

/// The directory of `shape`, made here or read from `LIGNIN_DAMAGE_IMAGES`.
fn made(shape: Shape) -> Made {
    let kept = std::env::var_os("LIGNIN_DAMAGE_IMAGES")
        .map(|dir| PathBuf::from(dir).join(format!("{shape:?}.image")));
    if let Some(bytes) = kept.as_ref().and_then(|kept| fs::read(kept).ok()) {
        return Made {
            files: decode(&bytes),
            state: make(shape, None),
        };
    }
    let place = temp();
    let dir = place.path().join("db");
    let db = Db::open(&dir, &making()).expect("a new database");
    let state = make(shape, Some((&db, place.path())));
    let mut files = match shape {
        // The files as they are while the session is open.
        Shape::Killed => image(&dir),
        Shape::Checkpoint => {
            db.close().expect("the session ends");
            image(&place.path().join("checkpoint"))
        }
        _ => {
            db.close().expect("the session ends");
            image(&dir)
        }
    };
    if let Some(kept) = kept {
        let unfinished = kept.with_extension(format!("{}.tmp", std::process::id()));
        fs::create_dir_all(kept.parent().expect("a directory")).expect("a directory");
        fs::write(&unfinished, encode(&files)).expect("the image is written");
        fs::rename(&unfinished, &kept).expect("the image is named");
        // Whoever wrote the name last wrote the image that is read.
        files = decode(&fs::read(&kept).expect("the image is read"));
    }
    Made { files, state }
}

fn encode(files: &Files) -> Vec<u8> {
    let mut out = Vec::new();
    for (path, bytes) in files {
        let path = path.to_str().expect("a path of these tests").as_bytes();
        out.extend_from_slice(&(path.len() as u32).to_le_bytes());
        out.extend_from_slice(path);
        match bytes {
            None => out.extend_from_slice(&u64::MAX.to_le_bytes()),
            Some(bytes) => {
                out.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
                out.extend_from_slice(bytes);
            }
        }
    }
    out
}

fn decode(mut bytes: &[u8]) -> Files {
    let mut files = Files::new();
    while !bytes.is_empty() {
        let (len, rest) = bytes.split_first_chunk::<4>().expect("a length");
        let (path, rest) = rest.split_at(u32::from_le_bytes(*len) as usize);
        let (len, rest) = rest.split_first_chunk::<8>().expect("a length");
        let path = PathBuf::from(std::str::from_utf8(path).expect("a path"));
        bytes = match u64::from_le_bytes(*len) {
            u64::MAX => {
                files.insert(path, None);
                rest
            }
            len => {
                let (file, rest) = rest.split_at(len as usize);
                files.insert(path, Some(file.to_vec()));
                rest
            }
        };
    }
    files
}

/// The kind of a file of a database directory, by its place and its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Manifest,
    Log,
    Segment,
    Values,
    Table,
}

const KINDS: [Kind; 5] = [
    Kind::Manifest,
    Kind::Log,
    Kind::Segment,
    Kind::Values,
    Kind::Table,
];

fn kind_of(path: &Path) -> Option<Kind> {
    let above = path.parent().and_then(Path::file_name);
    match (
        above.and_then(|above| above.to_str()),
        path.file_name()?.to_str()?,
    ) {
        (None, "MANIFEST") => Some(Kind::Manifest),
        (Some("wal"), _) => Some(Kind::Log),
        (Some("seg"), _) => Some(Kind::Segment),
        (Some("val"), _) => Some(Kind::Values),
        (Some("table"), _) => Some(Kind::Table),
        _ => None,
    }
}

/// The length of the body of a frame, from the four bytes the frame begins
/// with: bits 0 to 26 in a segment, whose tag says in its other bits what
/// the block is, and the whole word in every other file.
fn body_len(kind: Kind, word: u32) -> usize {
    match kind {
        Kind::Segment => (word & ((1 << 27) - 1)) as usize,
        _ => word as usize,
    }
}

/// Where each whole frame of a file begins and ends: a frame is four bytes
/// that give its length, the checksum of those four bytes and the body,
/// and the body, and the first one follows the header of the file. The
/// walk ends at the first frame that does not fit.
fn frames(kind: Kind, bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut frames = Vec::new();
    let mut at = FileHeader::ENCODED_LEN;
    while let Some(word) = bytes.get(at..at + 4) {
        let word = u32::from_le_bytes(word.try_into().expect("four bytes"));
        let end = (at + 8).checked_add(body_len(kind, word));
        let Some(end) = end.filter(|end| *end <= bytes.len()) else {
            break;
        };
        frames.push((at, end));
        at = end;
    }
    frames
}

/// Writes every checksum of a file again: that of its header, and that of
/// each whole frame behind it. This is what the fuzz targets of
/// `lignin-format` do to get past the checksums (`lignin-format/fuzz`).
fn seal(kind: Kind, bytes: &mut [u8]) {
    if let Some(header) = bytes.get_mut(..FileHeader::ENCODED_LEN) {
        let (covered, checksum) = header.split_at_mut(16);
        checksum.copy_from_slice(&Checksum::of(&[covered]).encode());
    }
    for (start, end) in frames(kind, bytes) {
        let checksum = Checksum::of(&[&bytes[start..start + 4], &bytes[start + 8..end]]);
        bytes[start + 4..start + 8].copy_from_slice(&checksum.encode());
    }
}

/// How a file is changed before its checksums are written again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum How {
    /// One byte of a body takes another value.
    Byte,
    /// One bit of a body is turned.
    Bit,
    /// A number of one to eight bytes in a body takes a value that a
    /// length, a count, an offset or an id is likely to be wrong at.
    Number,
    /// The bodies of two frames of one length trade places.
    Exchange,
    /// The body of a frame is written over another of its length.
    Stale,
    /// The file ends at the end of an earlier frame.
    Cut,
    /// A frame of the file follows its last frame once more.
    Grown,
    /// A byte of the header of the file takes another value.
    Header,
    /// The length of a frame takes another value, and the frames are
    /// those that the lengths then give.
    Length,
}

const HOWS: [How; 9] = [
    How::Byte,
    How::Bit,
    How::Number,
    How::Exchange,
    How::Stale,
    How::Cut,
    How::Grown,
    How::Header,
    How::Length,
];

/// A place in the body of a frame of the file: a frame that is drawn among
/// the frames, or one that is drawn among the bytes, so that the many
/// small frames and the few large ones are both met.
fn in_a_body(rng: &mut Rng, frames: &[(usize, usize)], len: usize) -> usize {
    let with_body: Vec<(usize, usize)> = (frames.iter().copied())
        .filter(|(start, end)| end - start > 8)
        .collect();
    if with_body.is_empty() || len == 0 {
        return rng.below(len.max(1));
    }
    if rng.chance(50) {
        let (start, end) = with_body[rng.below(with_body.len())];
        return start + 8 + rng.below(end - start - 8);
    }
    let at = rng.below(len);
    match with_body
        .iter()
        .find(|(start, end)| (start + 8..*end).contains(&at))
    {
        Some(_) => at,
        None => {
            let (start, end) = with_body[rng.below(with_body.len())];
            start + 8 + rng.below(end - start - 8)
        }
    }
}

/// Two frames of one length that hold different bodies.
fn two_alike(
    rng: &mut Rng,
    bytes: &[u8],
    frames: &[(usize, usize)],
) -> Option<[(usize, usize); 2]> {
    let first = frames[rng.below(frames.len())];
    let others: Vec<(usize, usize)> = (frames.iter().copied())
        .filter(|other| {
            other.1 - other.0 == first.1 - first.0
                && bytes[other.0 + 8..other.1] != bytes[first.0 + 8..first.1]
        })
        .collect();
    (!others.is_empty()).then(|| [first, others[rng.below(others.len())]])
}

/// Changes `bytes` as `how` says, where the file has what that takes, and
/// says whether it did.
fn change(how: How, kind: Kind, bytes: &mut Vec<u8>, rng: &mut Rng) -> bool {
    let frames = frames(kind, bytes);
    match how {
        How::Byte => {
            let at = in_a_body(rng, &frames, bytes.len());
            let Some(byte) = bytes.get_mut(at) else {
                return false;
            };
            *byte ^= 1 + rng.below(255) as u8;
        }
        How::Bit => {
            let at = in_a_body(rng, &frames, bytes.len());
            let Some(byte) = bytes.get_mut(at) else {
                return false;
            };
            *byte ^= 1 << rng.below(8);
        }
        How::Number => {
            let width = [1, 2, 4, 8][rng.below(4)];
            let at = in_a_body(rng, &frames, bytes.len());
            let Some(place) = bytes.get_mut(at..at + width) else {
                return false;
            };
            let mut old = [0; 8];
            old[..width].copy_from_slice(place);
            let old = u64::from_le_bytes(old);
            let new = match rng.below(14) {
                0 => 0,
                1 => 1,
                2 => old.wrapping_add(1),
                3 => old.wrapping_sub(1),
                4 => old.wrapping_mul(2),
                5 => old / 2,
                6 => 0x7f,
                7 => 0x80,
                8 => 0xffff,
                9 => 0x7fff_ffff,
                10 => 0xffff_ffff,
                11 => u64::MAX,
                12 => 1 << rng.below(64),
                _ => rng.next(),
            };
            place.copy_from_slice(&new.to_le_bytes()[..width]);
        }
        How::Exchange => {
            let Some([one, other]) = (!frames.is_empty())
                .then(|| two_alike(rng, bytes, &frames))
                .flatten()
            else {
                return false;
            };
            let body = bytes[one.0 + 8..one.1].to_vec();
            bytes.copy_within(other.0 + 8..other.1, one.0 + 8);
            bytes[other.0 + 8..other.1].copy_from_slice(&body);
        }
        How::Stale => {
            let Some([one, other]) = (!frames.is_empty())
                .then(|| two_alike(rng, bytes, &frames))
                .flatten()
            else {
                return false;
            };
            bytes.copy_within(one.0 + 8..one.1, other.0 + 8);
        }
        How::Cut => {
            if frames.len() < 2 {
                return false;
            }
            let (_, end) = frames[rng.below(frames.len() - 1)];
            bytes.truncate(end);
        }
        How::Grown => {
            let Some((_, end)) = frames.last().copied() else {
                return false;
            };
            let (start, until) = frames[rng.below(frames.len())];
            let again = bytes[start..until].to_vec();
            bytes.truncate(end);
            bytes.extend_from_slice(&again);
        }
        How::Header => {
            let Some(byte) = bytes.get_mut(rng.below(16)) else {
                return false;
            };
            *byte ^= 1 + rng.below(255) as u8;
        }
        How::Length => {
            if frames.is_empty() {
                return false;
            }
            let (start, end) = frames[rng.below(frames.len())];
            let old = u32::from_le_bytes(bytes[start..start + 4].try_into().expect("four bytes"));
            let body = (end - start - 8) as u32;
            let new = match rng.below(6) {
                0 => old.wrapping_sub(1),
                1 => old.wrapping_add(1),
                2 => old - body + rng.below(body.max(1) as usize) as u32,
                3 => old ^ 1 << (27 + rng.below(5)),
                4 => old.wrapping_add(8),
                _ => old - body,
            };
            bytes[start..start + 4].copy_from_slice(&new.to_le_bytes());
        }
    }
    true
}

/// One case: which file of a directory of which shape is changed, and how.
#[derive(Debug, Clone)]
struct Case {
    seed: u64,
    shape: Shape,
    file: PathBuf,
    kind: Kind,
    how: How,
}

impl Case {
    fn told(&self) -> String {
        let Self {
            seed,
            shape,
            file,
            kind,
            how,
        } = self;
        format!(
            "seed {seed}: {shape:?}, {} ({kind:?}), {how:?}",
            file.display()
        )
    }
}

/// The case of `seed` on the directory `made` of its shape: the files with
/// one of them changed and sealed, which is then another file than it was.
fn damaged(seed: u64, shape: Shape, made: &Made) -> (Case, Files) {
    let mut rng = Rng::new(seed ^ 0xda3a_9ed0_0000_0000);
    loop {
        let kind = KINDS[rng.below(KINDS.len())];
        let of_kind: Vec<&PathBuf> = (made.files.iter())
            .filter(|(path, bytes)| bytes.is_some() && kind_of(path) == Some(kind))
            .map(|(path, _)| path)
            .collect();
        if of_kind.is_empty() {
            continue;
        }
        let file = of_kind[rng.below(of_kind.len())].clone();
        let how = HOWS[rng.below(HOWS.len())];
        let was = made.files[&file].as_ref().expect("a file");
        let mut bytes = was.clone();
        if !change(how, kind, &mut bytes, &mut rng) {
            continue;
        }
        seal(kind, &mut bytes);
        if bytes == *was {
            continue;
        }
        let mut files = made.files.clone();
        files.insert(file.clone(), Some(bytes));
        let case = Case {
            seed,
            shape,
            file,
            kind,
            how,
        };
        return (case, files);
    }
}

/// Where a panic was raised, and what it said.
#[derive(Debug, Clone)]
struct Raised {
    file: String,
    line: u32,
    message: String,
    thread: String,
}

thread_local! {
    /// Whether this thread runs a case.
    static WATCHED: Cell<bool> = const { Cell::new(false) };
}

/// Whether a case runs.
static RUNS: AtomicBool = AtomicBool::new(false);

/// The panics of the case that runs, in the order they were raised.
static RAISED: Mutex<Vec<Raised>> = Mutex::new(Vec::new());

fn raised() -> Vec<Raised> {
    std::mem::take(&mut *RAISED.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Records the panics of the thread that runs a case, and those that are
/// raised while a case runs on a thread that is no test's: the threads of
/// a session have names of their own, and a test's thread has the path of
/// its test. Every other panic is left to the hook that was there.
fn watch_panics() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let before = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let thread = thread::current().name().unwrap_or("").to_owned();
            let of_a_session = RUNS.load(Ordering::Acquire) && !thread.contains("::");
            if !WATCHED.with(Cell::get) && !of_a_session {
                return before(info);
            }
            let at = info.location();
            let raised = Raised {
                file: at.map_or_else(String::new, |at| at.file().to_owned()),
                line: at.map_or(0, |at| at.line()),
                message: info.payload_as_str().unwrap_or("no message").to_owned(),
                thread,
            };
            (RAISED.lock().unwrap_or_else(PoisonError::into_inner)).push(raised);
        }));
    });
}

/// How a step of a case ended.
#[derive(Debug, Clone)]
enum Said {
    /// It returned with no error.
    Returned,
    /// The engine answered an error: the name of its kind.
    Answered(String),
    /// A fixture of the tests did not get the answer it expects of a sound
    /// directory.
    Fixture(Raised),
    /// A walk took more steps than the directory has records.
    Endless(String),
    /// A panic in the engine's code.
    Panicked(Raised),
}

impl Raised {
    fn told(&self) -> String {
        let Self {
            file,
            line,
            message,
            thread,
        } = self;
        format!("a panic at {file}:{line} on the thread {thread:?}: {message}")
    }
}

impl Said {
    fn is_a_finding(&self) -> bool {
        matches!(self, Self::Endless(_) | Self::Panicked(_))
    }

    /// The outcome in full.
    fn told(&self) -> String {
        match self {
            Self::Returned => "returned".to_owned(),
            Self::Answered(kind) => format!("the error {kind}"),
            Self::Fixture(raised) => format!("in a fixture, {}", raised.told()),
            Self::Endless(what) => what.clone(),
            Self::Panicked(raised) => raised.told(),
        }
    }

    /// The outcome with no place and no text, for the counts.
    fn name(&self) -> String {
        match self {
            Self::Returned => "returned".to_owned(),
            Self::Answered(kind) => format!("error {kind}"),
            Self::Fixture(_) => "a fixture's panic".to_owned(),
            Self::Endless(_) => "ENDLESS".to_owned(),
            Self::Panicked(_) => "PANICKED".to_owned(),
        }
    }
}

/// Whether `file` is a file of tests: a panic raised there is a fixture's.
fn of_the_tests(file: &str) -> bool {
    file.contains("/tests/") || file.ends_with("/tests.rs")
}

/// The name of the kind of an error: the first word of how it is shown.
fn kind(error: impl Debug) -> String {
    let shown = format!("{error:?}");
    let end = shown.find(|c: char| !c.is_alphanumeric());
    shown[..end.unwrap_or(shown.len())].to_owned()
}

/// Runs `step` and says how it ended. A panic of a thread of the session
/// while the step ran is the step's.
fn ended(step: impl FnOnce() -> Result<(), String>) -> Said {
    raised();
    let caught = panic::catch_unwind(AssertUnwindSafe(step));
    let raised = raised();
    let of_the_engine = raised
        .iter()
        .find(|raised| !raised.message.starts_with(ENDLESS) && !of_the_tests(&raised.file));
    if let Some(raised) = of_the_engine {
        return Said::Panicked(raised.clone());
    }
    match (caught, raised.first()) {
        (Ok(Ok(())), _) => Said::Returned,
        (Ok(Err(kind)), _) => Said::Answered(kind),
        (Err(_), Some(raised)) if raised.message.starts_with(ENDLESS) => {
            Said::Endless(raised.message.clone())
        }
        (Err(_), Some(raised)) => Said::Fixture(raised.clone()),
        (Err(_), None) => Said::Panicked(Raised {
            file: String::new(),
            line: 0,
            message: "a panic that the hook did not see".to_owned(),
            thread: String::new(),
        }),
    }
}

fn counted(steps: &mut usize, of: &str) {
    *steps += 1;
    assert!(*steps <= WALK_LIMIT, "{ENDLESS}: {of}");
}

/// The keys the reads ask for: those the directories were written under,
/// and some that none was.
fn keys() -> Vec<Vec<u8>> {
    let mut keys = Vec::new();
    for lead in *b"abcm" {
        keys.extend((0..48).map(|number| key(lead, number)));
    }
    keys.extend((0..24).map(|at| format!("r{at:02}").into_bytes()));
    for round in 0..6 {
        for at in [0, 3, 7, 27, 59] {
            let mut name = format!("flat{round:02}{at:03}").into_bytes();
            if at % 20 == 7 {
                name.resize(900, b'k');
            }
            keys.push(name);
        }
    }
    keys.extend([b"note".to_vec(), b"short".to_vec(), b"height".to_vec()]);
    keys.extend([Vec::new(), vec![b'k'; 3001], vec![0xff; 40]]);
    keys
}

/// Every read of a write transaction that has written nothing: the entry
/// of each subtree, its nodes by key and under a cursor in both
/// directions, each node's value and the child on each side, and the flat
/// records of each column by key and under a cursor.
fn reads(db: &Db) {
    let txn = db.begin();
    let keys = keys();
    let mut out = Vec::new();
    for name in 1..=SUBTREES + 1 {
        let subtree = model::subtree(name);
        let _ = txn.subtree(subtree, |entry| entry.is_some());
        for key in &keys {
            let handle = txn.node(subtree, key, |node| {
                node.and_then(|node| {
                    let mut value = ValueBuf::new();
                    let _ = node.value(&mut value);
                    let _ = (node.link(Side::Left), node.link(Side::Right));
                    node.handle()
                })
            });
            let Ok(Some(handle)) = handle else { continue };
            for side in Side::BOTH {
                let _ = txn.child(subtree, handle, side, |child| {
                    child.map(|child| {
                        let mut value = ValueBuf::new();
                        let _ = child.value(&mut value);
                        child.key().len()
                    })
                });
            }
        }
        if let Ok(mut cursor) = txn.merk_cursor(subtree) {
            for back in [false, true] {
                let mut steps = 0;
                let mut moved = match back {
                    false => cursor.seek_to_first(),
                    true => cursor.seek_to_last(),
                };
                while moved.is_ok() && cursor.key().is_some() {
                    counted(&mut steps, "a cursor over a Merk");
                    let _ = cursor.node(|node| {
                        node.map(|node| {
                            let mut value = ValueBuf::new();
                            let _ = node.value(&mut value);
                        })
                    });
                    moved = match back {
                        false => cursor.next(),
                        true => cursor.prev(),
                    };
                }
            }
            for key in keys.iter().step_by(17) {
                let _ = cursor.seek(key);
                let _ = cursor.seek_for_prev(key);
            }
        }
        for column in Column::ALL {
            let unit = Unit::new(column, subtree);
            for key in &keys {
                out.clear();
                let _ = txn.get(unit, key, &mut out);
            }
            let mut cursor = txn.cursor(unit);
            for back in [false, true] {
                let mut steps = 0;
                let mut moved = match back {
                    false => cursor.seek_to_first(),
                    true => cursor.seek_to_last(),
                };
                while moved.is_ok() && cursor.key().is_some() {
                    counted(&mut steps, "a cursor over flat records");
                    out.clear();
                    let _ = cursor.value(&mut out);
                    moved = match back {
                        false => cursor.next(),
                        true => cursor.prev(),
                    };
                }
            }
            for key in keys.iter().step_by(17) {
                let _ = cursor.seek(key);
                let _ = cursor.seek_for_prev(key);
            }
        }
    }
    let _ = db.rollback(&txn);
}

/// Every read of a snapshot: the entries of the directory, and of each
/// subtree its nodes by key and under a walk in both directions, and its
/// flat records of each column by key and under a cursor.
fn snapshot_reads(db: &Db) {
    let snapshot = db.snapshot();
    let operation = snapshot.operation();
    if let Ok(mut entries) = operation.entries() {
        let mut steps = 0;
        while let Ok(Some(_)) = entries.next() {
            counted(&mut steps, "the entries of the directory");
        }
    }
    let keys = keys();
    let mut out = Vec::new();
    for name in 1..=SUBTREES + 1 {
        let Ok(Some(opened)) = operation.open(model::subtree(name)) else {
            continue;
        };
        let _ = opened.root();
        for key in &keys {
            let _ = opened.node(key, None, |view| {
                view.map(|view| {
                    let mut value = ValueBuf::new();
                    let _ = view.value(&mut value);
                    view.key().len()
                })
            });
            for column in Column::ALL {
                out.clear();
                let _ = opened.get(column, key, &mut out);
            }
        }
        for column in Column::ALL {
            let mut cursor = opened.cursor(column);
            for back in [false, true] {
                let mut steps = 0;
                let mut moved = match back {
                    false => cursor.seek_to_first(),
                    true => cursor.seek_to_last(),
                };
                while moved.is_ok() && cursor.key().is_some() {
                    counted(&mut steps, "a cursor over the records of a snapshot");
                    out.clear();
                    let _ = cursor.value(&mut out);
                    moved = match back {
                        false => cursor.next(),
                        true => cursor.prev(),
                    };
                }
            }
        }
        let Ok(mut walk) = opened.walk() else {
            continue;
        };
        for back in [false, true] {
            let mut steps = 0;
            let mut moved = match back {
                false => walk.seek_to_first(&operation),
                true => walk.seek_to_last(&operation),
            };
            while moved.is_ok() && walk.key().is_some() {
                counted(&mut steps, "a walk over the nodes of a snapshot");
                let mut children = ChildKeys::new();
                let _ = walk.node_with_children(&operation, &mut children, |view| {
                    view.map(|view| {
                        let mut value = ValueBuf::new();
                        let _ = view.value(&mut value);
                    })
                });
                moved = match back {
                    false => walk.next(&operation),
                    true => walk.prev(&operation),
                };
            }
        }
    }
}

/// Whether the checker finds the latest version sound.
fn checked(db: &Db) -> Result<(), String> {
    match db.verify(None).is_sound() {
        true => Ok(()),
        false => Err("unsound".to_owned()),
    }
}

/// Opens the database in `dir`. The check that a close makes of a store in
/// a debug build is left out for the session: it panics on a store that is
/// not sound, which is what it is for, and these stores are damaged on
/// purpose. [`closed`] makes the same check and says what it found.
fn opened(dir: &Path) -> Result<Db, String> {
    let db = Db::open(dir, &opening()).map_err(kind)?;
    db.unchecked_at_close();
    Ok(db)
}

/// Closes `db` and checks the store that is still, with what it keeps
/// beside its pages.
fn closed(db: Db) -> Result<(), String> {
    let report = db.close_and_verify(None).map_err(kind)?;
    match report.is_sound() {
        true => Ok(()),
        false => Err("unsound".to_owned()),
    }
}

/// What a node does with the directory `dir`, step by step, with how each
/// step ended. `place` takes the checkpoint and the backups.
fn drill(dir: &Path, place: &Path, state: &State, seed: u64) -> Vec<(&'static str, Said)> {
    let mut said = Vec::new();
    let backup = || Backup::new(Arc::new(StdVfs));
    let mut session = None;
    let open = ended(|| {
        session = Some(opened(dir)?);
        Ok(())
    });
    said.push(("open", open));
    if let Some(db) = &session {
        said.push((
            "reads",
            ended(|| {
                reads(db);
                Ok(())
            }),
        ));
        said.push((
            "snapshot reads",
            ended(|| {
                snapshot_reads(db);
                Ok(())
            }),
        ));
        said.push(("check", ended(|| checked(db))));
        let write = ended(|| {
            let batch = drawn(state, &mut Rng::new(seed), state.version() + 1);
            commit(db, &batch, false).map(drop).map_err(kind)
        });
        said.push(("write", write));
        said.push(("write-back", ended(|| db.flush().map(drop).map_err(kind))));
        let reclaim = ended(|| {
            for _ in 0..4 {
                let _ = db.reclaim();
            }
            Ok(())
        });
        said.push(("reclaim", reclaim));
        said.push(("clean", ended(|| db.clean().map(drop).map_err(kind))));
        said.push(("check after the write", ended(|| checked(db))));
        let checkpoint = place.join("checkpoint");
        let made = ended(|| {
            (db.checkpoint(&checkpoint, CheckpointOptions::default()))
                .map(drop)
                .map_err(kind)
        });
        if matches!(made, Said::Returned) {
            let copy = place.join("backup");
            let copied = ended(|| backup().copy(&checkpoint, &copy).map(drop).map_err(kind));
            said.push(("backup of a checkpoint", copied));
        }
        said.push(("checkpoint", made));
    }
    if let Some(db) = session {
        said.push(("close", ended(|| closed(db))));
    }
    let copy = place.join("copy");
    let copied = ended(|| backup().copy(dir, &copy).map(drop).map_err(kind));
    said.push(("backup", copied));
    let again = ended(|| closed(opened(dir)?));
    said.push(("open again", again));
    said
}

/// Writes `files` as the directory `dir`.
fn write(dir: &Path, files: &Files) {
    fs::create_dir_all(dir).expect("a directory");
    for (path, bytes) in files {
        match bytes {
            None => fs::create_dir_all(dir.join(path)).expect("a directory"),
            Some(bytes) => fs::write(dir.join(path), bytes).expect("a file"),
        }
    }
}

/// Runs the case of `files` on a thread of its own and returns how each
/// step ended, or `None` if the case did not end within [`CASE_LIMIT`].
/// With `keep`, the damaged directory is kept there as it was given to the
/// open.
fn run(
    files: Files,
    state: State,
    seed: u64,
    keep: Option<PathBuf>,
) -> Option<Vec<(&'static str, Said)>> {
    watch_panics();
    let (sender, receiver) = mpsc::channel();
    let case = move || {
        WATCHED.with(|watched| watched.set(true));
        let place = temp();
        let dir = place.path().join("db");
        write(&dir, &files);
        if let Some(keep) = keep {
            write(&keep, &files);
        }
        let _ = sender.send(drill(&dir, place.path(), &state, seed));
    };
    RUNS.store(true, Ordering::Release);
    let thread = (thread::Builder::new().name("a damaged directory".to_owned()))
        .spawn(case)
        .expect("a thread");
    let said = receiver.recv_timeout(CASE_LIMIT).ok();
    if said.is_some() {
        let _ = thread.join();
    }
    RUNS.store(false, Ordering::Release);
    said
}

/// The range of seeds that `LIGNIN_DAMAGE_SEEDS=first..end` names, or the
/// short one of the test.
fn seeds() -> std::ops::Range<u64> {
    let Ok(given) = std::env::var("LIGNIN_DAMAGE_SEEDS") else {
        return 0..SHORT;
    };
    let (first, end) = given.split_once("..").expect("first..end");
    first.parse().expect("a seed")..end.parse().expect("a seed")
}

/// What the cases of a run came to.
#[derive(Debug, Default)]
struct Tally {
    cases: u64,
    shapes: BTreeMap<Shape, u64>,
    kinds: BTreeMap<Kind, u64>,
    hows: BTreeMap<How, u64>,
    /// How each step ended, by the name of the outcome.
    steps: BTreeMap<(&'static str, String), u64>,
    /// The cases whose open returned and whose first check found the
    /// directory sound: the change was in bytes that nothing reads, or in
    /// what a session mends.
    sound: u64,
    findings: Vec<String>,
}

impl Tally {
    fn take(&mut self, case: &Case, said: Option<&[(&'static str, Said)]>) {
        self.cases += 1;
        *self.shapes.entry(case.shape).or_default() += 1;
        *self.kinds.entry(case.kind).or_default() += 1;
        *self.hows.entry(case.how).or_default() += 1;
        let Some(said) = said else {
            self.findings
                .push(format!("{}: did not end in {CASE_LIMIT:?}", case.told()));
            return;
        };
        for (step, said) in said {
            *self.steps.entry((*step, said.name())).or_default() += 1;
            if said.is_a_finding() {
                self.findings
                    .push(format!("{}: {step}: {}", case.told(), said.told()));
            }
        }
        let is = |step: &str| {
            (said.iter()).any(|(name, said)| *name == step && matches!(said, Said::Returned))
        };
        self.sound += u64::from(is("open") && is("check"));
    }

    fn show(&self) {
        println!("TALLY cases={} sound={}", self.cases, self.sound);
        println!("TALLY shapes={:?}", self.shapes);
        println!("TALLY kinds={:?}", self.kinds);
        println!("TALLY hows={:?}", self.hows);
        for ((step, said), count) in &self.steps {
            println!("TALLY step={step:?} said={said:?} count={count}");
        }
        println!("TALLY findings={}", self.findings.len());
    }
}

/// The seal writes the checksums that the engine wrote, and reaches every
/// byte of every file: a file that is sealed with nothing changed is the
/// file it was, and the frames that the seal walks end where the file
/// ends. So a change anywhere in a file is under a right checksum after
/// the seal, unless the change itself ends the walk.
#[test]
fn a_sealed_file_is_as_the_engine_wrote_it() {
    for shape in SHAPES {
        let made = made(shape);
        let mut kinds = BTreeMap::new();
        for (path, bytes) in &made.files {
            let (Some(bytes), Some(kind)) = (bytes, kind_of(path)) else {
                continue;
            };
            let mut sealed = bytes.clone();
            seal(kind, &mut sealed);
            assert!(sealed == *bytes, "{shape:?}: {}", path.display());
            let walked = frames(kind, bytes)
                .last()
                .map_or(FileHeader::ENCODED_LEN, |last| last.1);
            let (files, behind): &mut (u64, usize) = kinds.entry(kind).or_default();
            *files += 1;
            *behind += bytes.len() - walked.min(bytes.len());
        }
        println!("SEALED {shape:?}: files and bytes behind the last frame, by kind: {kinds:?}");
        for kind in [Kind::Manifest, Kind::Log, Kind::Segment] {
            assert!(kinds.contains_key(&kind), "{shape:?} has no {kind:?}");
        }
    }
}

/// The engine answers on every directory that was changed under right
/// checksums: no step of a case panics in the engine's code, no walk goes
/// on past its bound, and every case ends.
#[test]
fn the_engine_answers_on_directories_damaged_under_right_checksums() {
    let seeds = seeds();
    let short = std::env::var_os("LIGNIN_DAMAGE_SEEDS").is_none();
    let out = std::env::var_os("LIGNIN_DAMAGE_OUT").map(PathBuf::from);
    let until = std::env::var("LIGNIN_DAMAGE_SECONDS")
        .ok()
        .map(|seconds| Instant::now() + Duration::from_secs(seconds.parse().expect("seconds")));
    if let Some(out) = &out {
        fs::create_dir_all(out).expect("a directory");
    }
    let made: Vec<Made> = SHAPES.into_iter().map(made).collect();
    let mut tally = Tally::default();
    let began = Instant::now();
    let mut last = seeds.start;
    for seed in seeds.clone() {
        if until.is_some_and(|until| Instant::now() >= until) {
            break;
        }
        last = seed + 1;
        let shape = SHAPES[(seed % SHAPES.len() as u64) as usize];
        let made = &made[shape as usize];
        let (case, files) = damaged(seed, shape, made);
        if let Some(out) = &out {
            let current = out.join(format!("current-{}", std::process::id()));
            fs::write(current, format!("{}\n", case.told())).expect("the case is written");
        }
        let findings = tally.findings.len();
        let said = run(files.clone(), made.state.clone(), seed, None);
        tally.take(&case, said.as_deref());
        if tally.findings.len() > findings {
            for finding in &tally.findings[findings..] {
                println!("FINDING {finding}");
            }
            if let Some(out) = &out {
                let kept = out.join(format!("finding-{seed}"));
                write(&kept.join("db"), &files);
                let steps: Vec<String> = (said.iter().flatten())
                    .map(|(step, said)| format!("{step}: {}", said.told()))
                    .collect();
                let text = format!("{}\n{}\n", case.told(), steps.join("\n"));
                fs::write(kept.join("case.txt"), text).expect("the finding is written");
            }
        }
        if said.is_none() {
            // The thread of the case is still there, with its session.
            break;
        }
        if tally.cases % 5000 == 0 {
            println!(
                "PROGRESS seeds={}..{last} cases={} findings={} seconds={}",
                seeds.start,
                tally.cases,
                tally.findings.len(),
                began.elapsed().as_secs()
            );
        }
    }
    tally.show();
    println!(
        "CAMPAIGN seeds={}..{last} cases={} findings={} seconds={}",
        seeds.start,
        tally.cases,
        tally.findings.len(),
        began.elapsed().as_secs()
    );
    if short {
        // What the short run is taken to have met.
        for shape in SHAPES {
            assert!(tally.shapes.contains_key(&shape), "no case of {shape:?}");
        }
        for kind in KINDS {
            assert!(tally.kinds.contains_key(&kind), "no case of {kind:?}");
        }
        let said = |step: &'static str, said: &str| {
            let count = tally.steps.get(&(step, said.to_owned()));
            assert!(count.is_some(), "no case whose {step} {said}");
        };
        said("open", "returned");
        said("check", "error unsound");
        said("close", "error unsound");
        said("write", "returned");
        said("close", "returned");
        said("backup", "returned");
        said("open again", "returned");
        let refused = (tally.steps.iter())
            .any(|((step, said), _)| *step == "open" && said.starts_with("error"));
        assert!(refused, "no case whose open was refused");
    }
    assert!(tally.findings.is_empty(), "{:#?}", tally.findings);
}
