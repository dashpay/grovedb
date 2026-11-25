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
//! in both directions, the checker, a write and its commit, a flush of the
//! log, rounds of the reclaimer and a pass of the cleaner, a checkpoint and
//! a backup of it, the close, a backup of the closed directory, and a
//! second open. Write-back makes its turns on its own thread while the
//! session is open, and the close makes the last.
//!
//! An error is an answer, and so is a read that returns what was not
//! written: the directory is damaged. What is a finding:
//!
//! - a panic that is raised in the engine's code on the thread of the
//!   case,
//! - a thread of the session that panicked, which the session says when
//!   it is closed ([`DbError::Panicked`]),
//! - a walk that takes more steps than [`WALK_LIMIT`],
//! - a case that does not end within [`CASE_LIMIT`].
//!
//! A panic that a fixture of the tests raises, where a damaged store does
//! not answer it as a sound one does, is counted under the place it was
//! raised at and is no finding.
//!
//! Before its cases a run makes the same steps on the directory of each
//! shape with nothing changed, where every one of them returns, and on one
//! whose manifest ends behind its header, which no open takes. So a
//! directory that is no sound database before it is damaged, and a step
//! that could return on no directory, fail the run there and are not
//! counted among the answers to damage.
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
//! a seed is the same case in every process that reads them: an image is
//! kept once, and where two processes make one, both read the one that was
//! named first.

use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    fmt::Debug,
    fs,
    io::ErrorKind,
    panic::{self, AssertUnwindSafe},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc, Mutex, Once, PoisonError,
    },
    thread,
    time::{Duration, Instant},
};

use lignin_format::{tree_page::Side, Checksum, FileHeader};
use lignin_io::StdVfs;
use lignin_store::cleaner::CleanerConfig;
use lignin_tree::value::ValueBuf;

use super::{halt_background, image, options, temp, wait_for};
use crate::{
    backup::{
        tests::world::{batch, commit, drawn, first, height, key, value, State, SUBTREES},
        Backup,
    },
    checkpoint::CheckpointOptions,
    commit::Committed,
    db::DbError,
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

/// How many times the steps are made on the directory of each shape with
/// nothing changed, each time with another write.
const CONTROLS: u64 = 4;

/// How many times the reclaimer and the cleaner are asked for a turn
/// behind the drops of [`Shape::Cleaned`].
const CLEANED_ROUNDS: usize = 8;

/// How many draws of a file and a change a case is given to find a change
/// that the file takes. A directory of these tests has a file that takes
/// one in a few.
const DRAWS: usize = 10_000;

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
    /// A database after drops, the rounds of the reclaimer that took at
    /// least one dropped Merk out in full, and passes of the cleaner.
    Cleaned,
    /// A database whose process was killed: the files as they were while
    /// the session was open, with segments that write-back wrote and
    /// commits behind them.
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

/// Waits until write-back, on its thread, has written a block into a
/// segment of `db`. The counts of the session do not say which file a turn
/// wrote into, so the directory is looked at.
fn written_back(db: &Db) {
    let segments = db.dir().join("seg");
    wait_for("a segment that write-back wrote a block into", || {
        fs::read_dir(&segments).is_ok_and(|entries| {
            (entries.flatten())
                .filter_map(|entry| entry.metadata().ok())
                .any(|file| file.len() > FileHeader::ENCODED_LEN as u64)
        })
    });
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
                written_back(db);
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
            // The Merks that the session has reclaimed in full, before
            // the drops and behind them: the count is the session's, so
            // only what is reclaimed from here on is the drops'.
            let reclaimed = |db: &Db| db.stats().reclaimed.merks;
            let before = db.map(|(db, _)| reclaimed(db));
            for name in 4..=SUBTREES {
                maker.records(Vec::new(), vec![OwnedSubtree::Drop(model::subtree(name))]);
            }
            if let (Some((db, _)), Some(before)) = (db, before) {
                // Rounds of the reclaimer and passes of the cleaner that
                // are asked for here, as many as are named, whatever the
                // threads of the session have made of them by now.
                for _ in 0..CLEANED_ROUNDS {
                    db.flush().expect("the log is synced");
                    let round = db.reclaim();
                    assert!(!round.failed, "{round:?}\n{}", db.stats());
                    db.clean().expect("a pass of the cleaner");
                }
                let stats = db.stats();
                assert!(
                    stats.reclaimed.merks > before,
                    "no dropped Merk was reclaimed in full\n{stats}"
                );
            }
            maker.drawn(3);
        }
        Shape::Killed => {
            maker.drawn(5);
            if let Some((db, _)) = db {
                written_back(db);
            }
            maker.drawn(3);
        }
    }
    held.unwrap_or(maker.state)
}

/// The directory of `shape`, made here or read from `LIGNIN_DAMAGE_IMAGES`.
fn made(shape: Shape) -> Made {
    let kept = std::env::var_os("LIGNIN_DAMAGE_IMAGES")
        .map(|dir| PathBuf::from(dir).join(format!("{shape:?}.image")));
    if let Some(files) = kept.as_deref().and_then(kept_image) {
        return Made {
            files,
            state: make(shape, None),
        };
    }
    let place = temp();
    let dir = place.path().join("db");
    let db = Db::open(&dir, &making()).expect("a new database");
    let state = make(shape, Some((&db, place.path())));
    let files = match shape {
        // The files as they are while the session is open and no thread
        // of it writes: each has ended behind the turn it was in, so the
        // files are those of one moment. The handle is dropped with no
        // close, as a process that is killed ends.
        Shape::Killed => {
            halt_background(&db);
            image(&dir)
        }
        Shape::Checkpoint => {
            db.close().expect("the session ends");
            image(&place.path().join("checkpoint"))
        }
        _ => {
            db.close().expect("the session ends");
            image(&dir)
        }
    };
    let files = match &kept {
        Some(kept) => keep(kept, &files),
        None => files,
    };
    if let Err(why) = usable(&files) {
        panic!("the directory of {shape:?} that was made is not one to damage: {why}");
    }
    Made { files, state }
}

/// The image that is kept at `kept`, or `None` where none is. An image
/// that is there and is not one to damage ends the run: whoever runs the
/// campaign removes it.
fn kept_image(kept: &Path) -> Option<Files> {
    let bytes = match fs::read(kept) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return None,
        Err(error) => panic!("the image kept at {}: {error}", kept.display()),
    };
    match decode(&bytes).and_then(|files| usable(&files).map(|()| files)) {
        Ok(files) => Some(files),
        Err(why) => panic!(
            "the image kept at {} is not one to damage ({why}): remove it",
            kept.display()
        ),
    }
}

/// Keeps `files` at `kept` unless an image is kept there, and returns the
/// image that is. Each writer writes a file of its own and gives it the
/// name with a hard link, which is made only where the name is free: the
/// image of the first writer is the one that every writer and every later
/// reader has.
fn keep(kept: &Path, files: &Files) -> Files {
    static WRITERS: AtomicUsize = AtomicUsize::new(0);
    let writer = WRITERS.fetch_add(1, Ordering::Relaxed);
    let unfinished = kept.with_extension(format!("{}-{writer}.tmp", std::process::id()));
    fs::create_dir_all(kept.parent().expect("a directory")).expect("a directory");
    fs::write(&unfinished, encode(files)).expect("the image is written");
    let named = fs::hard_link(&unfinished, kept);
    fs::remove_file(&unfinished).expect("the writer's own file goes");
    match named {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => panic!("the image is named {}: {error}", kept.display()),
    }
    kept_image(kept).expect("an image is kept")
}

/// Whether `files` are a database directory that the cases can damage: a
/// manifest with a record, and every file of a kind under the checksums
/// that [`seal`] writes.
fn usable(files: &Files) -> Result<(), String> {
    let manifest = files.get(Path::new("MANIFEST")).and_then(Option::as_ref);
    if !manifest.is_some_and(|bytes| !frames(Kind::Manifest, bytes).is_empty()) {
        return Err("it has no manifest with a record".to_owned());
    }
    for (path, bytes) in files {
        let (Some(bytes), Some(kind)) = (bytes, kind_of(path)) else {
            continue;
        };
        let mut sealed = bytes.clone();
        seal(kind, &mut sealed);
        if sealed != *bytes {
            return Err(format!(
                "{} is not under the checksums the engine writes",
                path.display()
            ));
        }
    }
    Ok(())
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

/// The files of an image, or why the bytes are none. A kept image is read
/// from a directory that anybody may have written into: an image that is
/// cut is told, and so is a path that would not be written below the
/// directory of a case.
fn decode(mut bytes: &[u8]) -> Result<Files, String> {
    let mut files = Files::new();
    while !bytes.is_empty() {
        let (len, rest) = bytes
            .split_first_chunk::<4>()
            .ok_or("the length of a path is cut")?;
        let len = u32::from_le_bytes(*len) as usize;
        let (path, rest) = rest.split_at_checked(len).ok_or("a path is cut")?;
        let (len, rest) = rest
            .split_first_chunk::<8>()
            .ok_or("the length of a file is cut")?;
        let path = Path::new(std::str::from_utf8(path).map_err(|_| "a path is no text")?);
        let mut parts = path.components();
        let below = parts.all(|part| matches!(part, Component::Normal(_)));
        if !below || path.as_os_str().is_empty() {
            return Err(format!("{} is no path below a directory", path.display()));
        }
        bytes = match u64::from_le_bytes(*len) {
            u64::MAX => {
                files.insert(path.to_path_buf(), None);
                rest
            }
            len => {
                let len = usize::try_from(len).ok();
                let (file, rest) = len
                    .and_then(|len| rest.split_at_checked(len))
                    .ok_or("a file is cut")?;
                files.insert(path.to_path_buf(), Some(file.to_vec()));
                rest
            }
        };
    }
    Ok(files)
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
    for _ in 0..DRAWS {
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
    panic!("seed {seed}: no file of {shape:?} took a change in {DRAWS} draws");
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
    /// The panics that were raised on this thread since it began a case
    /// and that no step has taken yet, in the order they were raised.
    /// `None` on a thread that runs no case.
    static RAISED: RefCell<Option<Vec<Raised>>> = const { RefCell::new(None) };
}

/// How many cases run now. The cases of a process run one at a time, on
/// the thread of the one test that runs them.
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// How many panics of other threads are kept for the text of a finding.
const BESIDE_KEPT: usize = 16;

/// The last panics that were raised while a case ran on a thread that is
/// not the case's. The hook cannot say whose such a thread is: a session
/// of the case has threads, and so has every other test of the process.
/// So none of these is a finding. A thread of the case's session that
/// panicked is told by the session at its close, and these are then what
/// the finding has to show for it.
static BESIDE: Mutex<VecDeque<Raised>> = Mutex::new(VecDeque::new());

fn beside() -> std::sync::MutexGuard<'static, VecDeque<Raised>> {
    BESIDE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The panics of this thread that no step has taken yet.
fn raised() -> Vec<Raised> {
    RAISED
        .with(|raised| raised.borrow_mut().as_mut().map(std::mem::take))
        .unwrap_or_default()
}

/// Keeps the panics of a thread that runs a case, which are that case's,
/// and says nothing of them. A panic of any other thread is passed to the
/// hook that was there, as if this one were not, and is kept beside the
/// case that runs, if one does ([`BESIDE`]).
fn watch_panics() {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let before = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let at = info.location();
            let raised = Raised {
                file: at.map_or_else(String::new, |at| at.file().to_owned()),
                line: at.map_or(0, |at| at.line()),
                message: info.payload_as_str().unwrap_or("no message").to_owned(),
                thread: thread::current().name().unwrap_or("").to_owned(),
            };
            // A thread whose own record is gone, or is borrowed, runs no
            // step of a case at this moment.
            let of_a_case = RAISED.try_with(|kept| match kept.try_borrow_mut().as_deref_mut() {
                Ok(Some(kept)) => {
                    kept.push(raised.clone());
                    true
                }
                _ => false,
            });
            if of_a_case.unwrap_or(false) {
                return;
            }
            if RUNNING.load(Ordering::Acquire) > 0 {
                let mut seen = beside();
                if seen.len() == BESIDE_KEPT {
                    seen.pop_front();
                }
                seen.push_back(raised);
            }
            before(info);
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
    /// A panic in the engine's code, on the thread of the case.
    Panicked(Raised),
    /// A thread of the session panicked: what the session said of it, and
    /// the panics of other threads that were seen while the case ran.
    Session(String),
}

/// What the engine's error of a thread that panicked is called
/// ([`DbError::Panicked`]).
const PANICKED: &str = "Panicked";

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
        matches!(
            self,
            Self::Endless(_) | Self::Panicked(_) | Self::Session(_)
        )
    }

    /// The outcome in full.
    fn told(&self) -> String {
        match self {
            Self::Returned => "returned".to_owned(),
            Self::Answered(kind) => format!("the error {kind}"),
            Self::Fixture(raised) => format!("in a fixture, {}", raised.told()),
            Self::Endless(what) => what.clone(),
            Self::Panicked(raised) => raised.told(),
            Self::Session(what) => what.clone(),
        }
    }

    /// The outcome with no text, for the counts: a fixture's panic by the
    /// place it was raised at, which says what the fixture expected.
    fn name(&self) -> String {
        match self {
            Self::Returned => "returned".to_owned(),
            Self::Answered(kind) => format!("error {kind}"),
            Self::Fixture(raised) => {
                format!("a fixture's panic at {}:{}", raised.file, raised.line)
            }
            Self::Endless(_) => "ENDLESS".to_owned(),
            Self::Panicked(_) => "PANICKED".to_owned(),
            Self::Session(_) => "A THREAD OF THE SESSION PANICKED".to_owned(),
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

/// Runs `step` on the thread of a case and says how it ended. The panics
/// it takes are those of this thread since the step before it: every one
/// of them was raised in a step, since a panic outside one ends the case
/// ([`run`]).
fn ended(step: impl FnOnce() -> Result<(), String>) -> Said {
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
        (Ok(Err(kind)), _) if kind.starts_with(PANICKED) => {
            let seen: Vec<String> = beside().iter().map(Raised::told).collect();
            Said::Session(format!(
                "the session answered {kind}; on other threads while the case ran: {seen:?}"
            ))
        }
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
/// beside its pages. A close joins the threads of the session, and says
/// which of them panicked.
fn closed(db: Db) -> Result<(), String> {
    let report = db.close_and_verify(None).map_err(|error| match error {
        DbError::Panicked(thread) => format!("{PANICKED}: the {thread} thread"),
        error => kind(error),
    })?;
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
        said.push(("flush", ended(|| db.flush().map(drop).map_err(kind))));
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
/// step ended, or `None` if the case did not end within [`CASE_LIMIT`]:
/// its directory is written, the steps are made and the directory is
/// removed within that time. A panic of the thread outside a step is the
/// harness's own, and ends the run with what it said.
fn run(files: Files, state: State, seed: u64) -> Option<Vec<(&'static str, Said)>> {
    watch_panics();
    beside().clear();
    let (sender, receiver) = mpsc::channel();
    let case = move || {
        RAISED.with(|raised| *raised.borrow_mut() = Some(Vec::new()));
        let said = panic::catch_unwind(AssertUnwindSafe(|| {
            let place = temp();
            let dir = place.path().join("db");
            write(&dir, &files);
            drill(&dir, place.path(), &state, seed)
        }));
        let said = said.map_err(|_| {
            let raised: Vec<String> = raised().iter().map(Raised::told).collect();
            raised.join("; ")
        });
        let _ = sender.send(said);
    };
    RUNNING.fetch_add(1, Ordering::AcqRel);
    let thread = (thread::Builder::new().name("a damaged directory".to_owned()))
        .spawn(case)
        .expect("a thread");
    let said = receiver.recv_timeout(CASE_LIMIT);
    RUNNING.fetch_sub(1, Ordering::AcqRel);
    match said {
        Ok(Ok(said)) => {
            let _ = thread.join();
            Some(said)
        }
        Ok(Err(why)) => panic!("seed {seed}: the harness panicked outside a step: {why}"),
        Err(mpsc::RecvTimeoutError::Timeout) => None,
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            panic!("seed {seed}: the thread of the case ended with no answer")
        }
    }
}

/// The steps of a case in the order they are made on a directory that
/// opens.
const STEPS: [&str; 14] = [
    "open",
    "reads",
    "snapshot reads",
    "check",
    "write",
    "flush",
    "reclaim",
    "clean",
    "check after the write",
    "backup of a checkpoint",
    "checkpoint",
    "close",
    "backup",
    "open again",
];

/// Makes the steps of a case on the directory of each shape with nothing
/// changed, [`CONTROLS`] times with a write of its own each, and on one
/// whose manifest ends behind its header. On the first every step returns;
/// on the second the open is refused, and so are the backup and the second
/// open. Neither is counted as a case.
fn controls(made: &[Made]) {
    for (shape, made) in SHAPES.iter().zip(made) {
        for seed in 0..CONTROLS {
            let what = format!("{shape:?} with nothing changed, the write of seed {seed}");
            let said = run(made.files.clone(), made.state.clone(), seed)
                .unwrap_or_else(|| panic!("{what}: did not end in {CASE_LIMIT:?}"));
            let steps: Vec<&str> = said.iter().map(|(step, _)| *step).collect();
            assert_eq!(steps, STEPS, "{what}");
            for (step, said) in &said {
                assert!(
                    matches!(said, Said::Returned),
                    "{what}: {step}: {}",
                    said.told()
                );
            }
        }
        let what = format!("{shape:?} with a manifest that ends behind its header");
        let mut files = made.files.clone();
        let manifest = files
            .get_mut(Path::new("MANIFEST"))
            .and_then(Option::as_mut);
        manifest
            .expect("a manifest")
            .truncate(FileHeader::ENCODED_LEN);
        let said = run(files, made.state.clone(), 0)
            .unwrap_or_else(|| panic!("{what}: did not end in {CASE_LIMIT:?}"));
        let steps: Vec<&str> = said.iter().map(|(step, _)| *step).collect();
        assert_eq!(steps, ["open", "backup", "open again"], "{what}");
        for (step, said) in &said {
            assert!(
                matches!(said, Said::Answered(_)),
                "{what}: {step}: {}",
                said.told()
            );
        }
    }
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
/// byte of every file the engine wrote: a file of a directory of these
/// tests that is sealed with nothing changed is the file it was, and the
/// frames that the seal walks end where the file ends. So a change
/// anywhere in such a file is under a right checksum after the seal,
/// unless the change itself ends the walk: a length that is changed can
/// leave bytes behind the last frame that fits, which no checksum covers
/// and no frame of the file holds.
#[test]
fn a_sealed_file_is_as_the_engine_wrote_it() {
    for shape in SHAPES {
        let made = made(shape);
        let mut kinds: BTreeMap<Kind, (u64, usize)> = BTreeMap::new();
        for (path, bytes) in &made.files {
            let (Some(bytes), Some(kind)) = (bytes, kind_of(path)) else {
                continue;
            };
            let mut sealed = bytes.clone();
            seal(kind, &mut sealed);
            assert!(sealed == *bytes, "{shape:?}: {}", path.display());
            let frames = frames(kind, bytes);
            let walked = frames.last().map_or(FileHeader::ENCODED_LEN, |last| last.1);
            assert_eq!(
                walked,
                bytes.len(),
                "{shape:?}: the frames of {} end before the file does",
                path.display()
            );
            let (files, framed) = kinds.entry(kind).or_default();
            *files += 1;
            *framed += frames.len();
        }
        println!("SEALED {shape:?}: files and frames by kind: {kinds:?}");
        for kind in [Kind::Manifest, Kind::Log, Kind::Segment] {
            let framed = kinds.get(&kind).map_or(0, |(_, framed)| *framed);
            assert!(framed > 0, "{shape:?} has no frame of {kind:?}");
        }
    }
}

/// The engine answers on every directory that was changed under right
/// checksums: no step of a case panics in the engine's code, no walk goes
/// on past its bound, and every case ends.
#[test]
fn the_engine_answers_on_directories_damaged_under_right_checksums() {
    let seeds = seeds();
    let out = std::env::var_os("LIGNIN_DAMAGE_OUT").map(PathBuf::from);
    let until = std::env::var("LIGNIN_DAMAGE_SECONDS")
        .ok()
        .map(|seconds| Instant::now() + Duration::from_secs(seconds.parse().expect("seconds")));
    // The whole of the short range, which no clock cuts.
    let short = std::env::var_os("LIGNIN_DAMAGE_SEEDS").is_none() && until.is_none();
    if let Some(out) = &out {
        fs::create_dir_all(out).expect("a directory");
    }
    let current = (out.as_ref()).map(|out| out.join(format!("current-{}", std::process::id())));
    let made: Vec<Made> = SHAPES.into_iter().map(made).collect();
    controls(&made);
    println!(
        "CONTROLS shapes={} unchanged={CONTROLS} each, and one with no record in its manifest",
        SHAPES.len()
    );
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
        if let Some(current) = &current {
            fs::write(current, format!("{}\n", case.told())).expect("the case is written");
        }
        let findings = tally.findings.len();
        let said = run(files.clone(), made.state.clone(), seed);
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
    if let Some(current) = &current {
        // No case is under way any more.
        let _ = fs::remove_file(current);
    }
    tally.show();
    println!(
        "CAMPAIGN seeds={}..{last} cases={} findings={} seconds={}",
        seeds.start,
        tally.cases,
        tally.findings.len(),
        began.elapsed().as_secs()
    );
    assert!(tally.findings.is_empty(), "{:#?}", tally.findings);
    if short {
        // The seeds of the short range take the shapes in turn. Which
        // file of a directory a seed changes, and what the engine answers
        // to the change, is the layout's of the files that were made,
        // which the threads of the making sessions decide between them:
        // those counts are printed above and not asserted. What a step
        // answers on a directory that is sound, and on one that no open
        // takes, is asserted of the controls.
        let each = SHORT / SHAPES.len() as u64;
        for shape in SHAPES {
            assert_eq!(tally.shapes.get(&shape), Some(&each), "{shape:?}");
        }
    }
}
