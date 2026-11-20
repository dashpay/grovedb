//! One step of the background work of a database against commits, with
//! the commits let in at each point at which the step reads what a commit
//! changes (VER-05; D-116, D-145).
//!
//! The steps of the background work read a store in several looks: a walk
//! reads a tree level by level, write-back looks at a page and later holds
//! or links it, the cleaner copies a block and later points its link at the
//! copy. A commit can land between any two looks. Each of the defects that
//! threads found by chance in this code was a step that took two looks for
//! one (the list is at the end).
//!
//! # A schedule
//!
//! A schedule is one background step, one burst of commits, and one point
//! of the step at which the burst is let in. The points are every place at
//! which the step asks for something that a commit changes, and at which a
//! test can stand between the two without a change of the engine:
//!
//! | Point | Seam |
//! | --- | --- |
//! | a read of the latest version by the reclaimer of what drops left, and by the owners of values | [`StepLog::latest`] |
//! | a question about versions, pins and guards by write-back and the cleaner | [`Published`], [`Pins`] |
//! | an operation on a file: a page that is read from a segment, a block or a record that is appended, a manifest that is replaced, a sync | [`Vfs`] |
//!
//! The step runs on the thread of the test. At its point it wakes a second
//! thread, which makes the burst, and waits for it. Where the step holds a
//! lock that the burst needs, the step goes on after a while and the burst
//! lands behind the lock, which is a schedule as well. [`sweep`] tries
//! every point of a step, each from the same state: a database that was
//! written by both writers of the stress test, closed, opened again with
//! nothing in memory, and written a little more with no write-back, so that
//! page versions are owed, values are pending and drops wait.
//!
//! # The oracle
//!
//! **The physical invariant checker decides** (`crate::verify`,
//! [`Scope::Still`]), because the damaging form of D-145 counted no error:
//! after the step and the burst, everything that was dropped is reclaimed,
//! write-back is made to pass, and the checker reads the store at rest:
//! no finding, and no cell that nothing reaches. Beside it: no error of
//! the step, of write-back or of a reclaiming; every subtree read through a
//! snapshot is what the model of the stress test has; a checkpoint that was
//! the step holds exactly its version.
//!
//! # What a schedule does not reach
//!
//! **A point is a seam, not an instruction.** Between two reads of pages
//! that are both in the buffer pool there is no point unless the step asks
//! for the latest version between them, so a step that reads two levels of
//! a tree at one version it asked for once is swept before and after the
//! walk and not inside it. That is the form such a walk has to have
//! (D-145), and what makes it safe there is that it reads one version; the
//! races inside the pool's own calls are the loom models' of
//! `lignin-store` (`cache::tests::races`). One burst is let in at one
//! point: two bursts at two points of one step are the stress test's
//! (`super::stress`), by chance.
//!
//! # What was found by chance, and which schedule finds it on purpose
//!
//! | Defect | Schedule |
//! | --- | --- |
//! | D-145: the reclaimer walked to a record's leaf with each level at the then-latest version; a commit that freed or split the leaf between two levels gave an error, or left a root record with no entry | a reclaiming against a burst that merges, and one that splits, the leaves of the directory |
//! | D-116: a page freed under a reused id while a pass of write-back held its frame was linked | a turn of write-back against blocks that drop, reclaim and write again |
//! | a page that a commit freed, and whose frames the pool dropped, between write-back's look and its hold, was an error of the pass (`Replaced`) | the same |
//! | STORE-08: the owners of values read a page at the published version where a newer one was installed; a value-log file was stamped empty at too old a version | a pass over the value log against blocks |

use std::{
    io,
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, ThreadId},
};

use lignin_format::CommitVersion;
use lignin_io::{
    sim::{SimConfig, SimVfs},
    DirEntry, OpenMode, Vfs, VfsFile, VfsLock,
};
use lignin_store::{
    alloc::{Epoch, Pins},
    cleaner::{BothOwners, Cleaner, CleanerConfig, CleanerState},
    flush::{Published, Stores, WriteBack},
    values::OwnersError,
};
use lignin_tree::{
    drop::{Progress, Reclaim, Reclaimer},
    flat::{FlatOwners, StepLog},
    value::TreeOwners,
};

use super::{
    stress::{
        compare_all,
        history::History,
        stress_options, verify,
        work::{joined, shared, Expected, Piece, Read, Writer},
    },
    *,
};
use crate::{
    checkpoint::{self, Checkpointer},
    db::store::{lock, Steps},
    snapshot::{LatestLease, Snapshot, Versions},
    txn::{
        tests::model::{OwnedBatch, OwnedSubtree, Val, Write},
        Unit,
    },
    verify::Scope,
};

/// Where a burst of commits is let into a step: at the `at`th point that
/// the paced thread passes, counted from one.
#[derive(Debug)]
struct Gate {
    /// The thread whose steps are points.
    paced: Mutex<Option<ThreadId>>,
    at: u64,
    passed: AtomicU64,
    /// What each point was.
    kinds: Mutex<Vec<&'static str>>,
    /// Set while the paced thread is in a call whose file operations are
    /// under a lock that every commit takes: they are no points.
    quiet: AtomicBool,
    go: Mutex<Option<mpsc::Sender<()>>>,
    made: Mutex<mpsc::Receiver<()>>,
    /// Whether the burst is made, and whether it was made before the step
    /// went on from its point.
    finished: AtomicBool,
    landed: AtomicBool,
    /// How long the step waits for the burst at its point.
    patience: Duration,
}

impl Gate {
    /// A gate, with what the thread of the burst waits on and answers by.
    fn new(at: u64, patience: Duration) -> (Arc<Self>, mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (go, gone) = mpsc::channel();
        let (finished, made) = mpsc::channel();
        let gate = Self {
            paced: Mutex::new(None),
            at,
            passed: AtomicU64::new(0),
            kinds: Mutex::new(Vec::new()),
            quiet: AtomicBool::new(false),
            go: Mutex::new(Some(go)),
            made: Mutex::new(made),
            finished: AtomicBool::new(false),
            landed: AtomicBool::new(false),
            patience,
        };
        (Arc::new(gate), gone, finished)
    }

    /// From here on what the calling thread does is counted in points.
    fn pace(&self) {
        *lock(&self.paced) = Some(thread::current().id());
    }

    fn rest(&self) {
        *lock(&self.paced) = None;
    }

    /// A point of the kind `kind`, if the paced thread is here.
    fn point(&self, kind: &'static str) {
        if *lock(&self.paced) != Some(thread::current().id()) {
            return;
        }
        if self.quiet.load(Ordering::Relaxed) {
            return;
        }
        let passed = self.passed.fetch_add(1, Ordering::Relaxed) + 1;
        lock(&self.kinds).push(kind);
        if passed == self.at && self.let_in(Some(self.patience)) {
            self.landed.store(true, Ordering::Relaxed);
        }
    }

    /// Lets the burst in, if it was not, and waits until it is made: for
    /// `patience`, or for as long as it takes. Returns whether it is made.
    fn let_in(&self, patience: Option<Duration>) -> bool {
        if let Some(go) = lock(&self.go).take() {
            let _ = go.send(());
        }
        if self.finished.load(Ordering::Relaxed) {
            return true;
        }
        let made = lock(&self.made);
        let made = match patience {
            Some(patience) => !matches!(
                made.recv_timeout(patience),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            // A thread that ended without a word has failed, and says so
            // where it is joined.
            None => {
                let _ = made.recv();
                true
            }
        };
        self.finished.store(made, Ordering::Relaxed);
        made
    }
}

/// A file layer whose every operation is a point of the paced thread.
#[derive(Debug)]
struct Paced {
    inner: Arc<dyn Vfs>,
    gate: Arc<Gate>,
}

#[derive(Debug)]
struct PacedFile {
    file: Box<dyn VfsFile>,
    gate: Arc<Gate>,
}

impl VfsFile for PacedFile {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<usize> {
        self.gate.point("a read of a file");
        self.file.read_at(buf, offset)
    }

    fn write_at(&self, buf: &[u8], offset: u64) -> io::Result<()> {
        self.gate.point("a write to a file");
        self.file.write_at(buf, offset)
    }

    fn append(&self, buf: &[u8]) -> io::Result<u64> {
        self.gate.point("an append to a file");
        self.file.append(buf)
    }

    fn sync_data(&self) -> io::Result<()> {
        self.gate.point("a sync");
        self.file.sync_data()
    }

    fn sync_all(&self) -> io::Result<()> {
        self.gate.point("a sync");
        self.file.sync_all()
    }

    fn len(&self) -> io::Result<u64> {
        self.file.len()
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.gate.point("a cut of a file");
        self.file.set_len(len)
    }
}

impl Vfs for Paced {
    fn open(&self, path: &Path, mode: OpenMode) -> io::Result<Box<dyn VfsFile>> {
        self.gate.point("an open of a file");
        let file = self.inner.open(path, mode)?;
        Ok(Box::new(PacedFile {
            file,
            gate: self.gate.clone(),
        }))
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.gate.point("a rename");
        self.inner.rename(from, to)
    }

    fn hard_link(&self, original: &Path, link: &Path) -> io::Result<()> {
        self.gate.point("a link");
        self.inner.hard_link(original, link)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.gate.point("a removal of a file");
        self.inner.remove_file(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_dir(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        self.gate.point("a listing");
        self.inner.read_dir(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.gate.point("a sync");
        self.inner.sync_dir(path)
    }

    fn lock(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.inner.lock(path)
    }

    fn lock_existing(&self, path: &Path) -> io::Result<Box<dyn VfsLock>> {
        self.inner.lock_existing(path)
    }
}

/// The versions of a database as write-back and the cleaner ask for them,
/// each question a point.
struct Asked<'a> {
    versions: &'a Versions,
    gate: &'a Gate,
}

impl Pins for Asked<'_> {
    fn oldest_pin(&self) -> CommitVersion {
        self.gate.point("the oldest pin");
        self.versions.oldest_pin()
    }

    fn next_epoch(&self) -> Epoch {
        self.gate.point("the next epoch");
        self.versions.next_epoch()
    }

    fn oldest_guard(&self) -> Epoch {
        self.gate.point("the oldest guard");
        self.versions.oldest_guard()
    }
}

impl Published for Asked<'_> {
    fn latest(&self) -> CommitVersion {
        self.gate.point("the latest version");
        Published::latest(self.versions)
    }

    fn guarded(&self, work: &mut dyn FnMut()) {
        self.gate.point("an operation guard");
        self.versions.guarded(work)
    }
}

/// The log of the page steps of a reclaimer and of the owners of values,
/// whose every read of the latest version is a point.
struct Interrupted<'a> {
    steps: Steps<'a>,
    gate: &'a Gate,
}

impl StepLog for Interrupted<'_> {
    fn latest(&self) -> CommitVersion {
        self.gate.point("the latest version");
        self.steps.latest()
    }

    fn append(&self, record: &[u8]) -> Result<(), OwnersError> {
        // The record is appended under the page step's lock and the lock
        // on the counts, which every commit takes: no commit comes here.
        self.gate.quiet.store(true, Ordering::Relaxed);
        let appended = self.steps.append(record);
        self.gate.quiet.store(false, Ordering::Relaxed);
        appended
    }
}

/// The step of the background work that a schedule is of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// The reclaiming of what the drops of the state left, each to its
    /// end (`lignin_tree::drop`, CL6).
    Reclaim,
    /// One turn of write-back: a pass, and a table checkpoint if one is
    /// due (`lignin_store::flush`).
    WriteBack,
    /// One pass of the cleaner over the segments.
    CleanSegments,
    /// One pass of the cleaner over the value log, with the owners of
    /// values of both kinds.
    CleanValues,
    /// The creation of a checkpoint of the database.
    Checkpoint,
    /// One operation that pins nothing and reads every subtree of the
    /// latest version of its beginning, under its guard (RD4, PN5). The
    /// pool was empty when the database was opened, so its points are the
    /// pages it reads from the segments.
    Read,
    /// The same through a snapshot, which pins its version (RD1).
    ReadPinned,
}

impl Step {
    fn reads(self) -> bool {
        matches!(self, Self::Read | Self::ReadPinned)
    }
}

/// The commits that are let into a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Burst {
    /// Blocks of both writers: documents, generations that are entered and
    /// dropped.
    Blocks,
    /// A commit that drops most of a crowd of small subtrees: the leaves of
    /// the directory's keyspace are merged and freed.
    Merge,
    /// A commit that enters a crowd of small subtrees: the leaves of the
    /// directory's keyspace are split.
    Split,
}

/// The pool of a database under a schedule: less than the database holds.
const POOL: usize = 1 << 20;
/// The members of the crowd that the state has, and that a splitting burst
/// adds.
const CROWD: u64 = 240;
/// The blocks that the state is written by, and those written after it is
/// opened again, before and after a reader pins a version.
const BASE_BLOCKS: u64 = 14;
const PRELUDE: u64 = 4;
const PINNED: u64 = 2;
const GENERATIONS: usize = 2;

fn cleaner_config() -> CleanerConfig {
    CleanerConfig {
        min_segment_len: 16 << 10,
        segment_len: 128 << 10,
        ..CleanerConfig::new()
    }
}

/// A database's model and its two writers.
struct World {
    history: History,
    writers: [Writer; 2],
}

/// Commits `pieces` of `writer` in one batch with no transaction, and tells
/// the model.
fn commit(db: &Db, history: &History, writer: &mut Writer, pieces: Vec<Piece>) -> (usize, usize) {
    let batch = joined(&pieces);
    let committed = batch.with(|batch| db.write(batch, &mut NoStaleHashes));
    let Ok(Committed::Published(published)) = committed else {
        panic!("a commit of a schedule: {committed:?}");
    };
    assert!(published.miscounted.is_none(), "{published:?}");
    let writes = pieces.iter().flat_map(|piece| piece.writes.iter().cloned());
    history.commit(published.version.get(), writes.collect());
    writer.committed(pieces);
    (published.created.len(), published.freed.len())
}

/// One block of `writer`: documents, a new generation every other block,
/// and with `drops` the drop of the oldest generation beyond those kept.
fn block(
    db: &Db,
    history: &History,
    writer: &mut Writer,
    number: u64,
    drops: bool,
) -> (usize, usize) {
    let mut pieces = vec![writer.documents(number as usize % Writer::MERKS)];
    if number % 2 == 0 {
        pieces.push(writer.generation());
    }
    if drops && writer.generations() > GENERATIONS {
        pieces.extend(writer.drop_oldest());
    }
    commit(db, history, writer, pieces)
}

/// A commit that writes one record, so that the version moves on: a page
/// that the latest commit wrote is freed after the next one.
fn tick(db: &Db, history: &History, round: u32) {
    let mut batch = OwnedBatch::default();
    let unit = Unit::new(Column::Meta, shared());
    let value = Val::Flat(round.to_be_bytes().to_vec());
    batch
        .records
        .push(Write::Put(unit, b"tick".to_vec(), value));
    let committed = batch.with(|batch| db.write(batch, &mut NoStaleHashes));
    let Ok(Committed::Published(published)) = committed else {
        panic!("a commit of one record: {committed:?}");
    };
    history.commit(published.version.get(), batch.writes());
}

/// Reclaims everything that the database knows to be dropped, with the
/// engine's own rounds and with commits between them, until nothing waits.
fn settle(db: &Db, history: &History, errors: &mut Vec<String>) {
    for round in 0u32..400 {
        if db.reclaim().waiting == 0 {
            return;
        }
        tick(db, history, round);
    }
    errors.push(format!(
        "the drops are not reclaimed: {:?}",
        db.stats().reclaimed
    ));
}

/// A few rounds of the engine's reclaimer with commits between them, as a
/// burst has them beside a step: what waits for the step itself, which
/// holds an operation guard at its point, is left waiting.
fn ease(db: &Db, history: &History) {
    for round in 0u32..6 {
        if db.reclaim().waiting == 0 {
            return;
        }
        tick(db, history, 1_000 + round);
    }
}

/// The state every schedule of a sweep begins from: a database on files in
/// memory, closed, with its model.
struct Base {
    fs: SimVfs,
    world: World,
}

/// The options of the stress test, with write-back's bounds on what is
/// unwritten far away: a committer that is slowed (FL5) waits for the turn
/// of write-back that is the step, and its burst would land behind it.
fn options(vfs: Arc<dyn Vfs>) -> Options {
    let mut options = Options {
        vfs,
        ..stress_options(false, POOL)
    };
    let flush = options
        .flush
        .as_mut()
        .expect("the stress test names its bounds");
    flush.max_unwritten = 1 << 16;
    flush.max_pending_bytes = 1 << 28;
    flush.max_log_held = 1 << 30;
    options
}

fn clean_values(db: &Db, cleaner: &Cleaner<'_>, log: &dyn StepLog) -> Result<String, String> {
    let store = &*db.shared.store;
    let owners = BothOwners(
        TreeOwners::new(store.loader(), &store.pool, store.page_size, &log),
        FlatOwners::new(store.loader(), &store.pool, store.config(), &log),
    );
    match cleaner.pass_values(&owners) {
        Ok(pass) => Ok(format!("{pass:?}")),
        Err(error) => Err(format!("the cleaner's pass over the value log: {error}")),
    }
}

/// What a close does for a database whose threads are ended.
fn close(db: Db, cleaner_state: &CleanerState, errors: &mut Vec<String>) {
    let cleaner = Cleaner::new(db.shared.write_back(), cleaner_state);
    if let Err(error) = cleaner.finish() {
        errors.push(format!("the cleaner's finish: {error}"));
    }
    db.shared.finish_reclaiming();
    if let Err(error) = db.shared.write_back().finish() {
        errors.push(format!("the close: {error}"));
    }
}

fn base(seed: u64) -> Base {
    let fs = SimVfs::new(SimConfig::new(seed));
    let db = Db::open("/db", &options(Arc::new(fs.clone()))).expect("a new database");
    halt_background(&db);
    let history = History::new(1);
    let mut writers = [Writer::new(1, seed), Writer::new(2, seed)];
    let mut first = OwnedBatch::default();
    first.subtrees.push(OwnedSubtree::Enter(shared(), None));
    first.subtrees.extend(writers[0].enters());
    first.subtrees.extend(writers[1].enters());
    let committed = first.with(|batch| db.write(batch, &mut NoStaleHashes));
    let Ok(Committed::Published(published)) = committed else {
        panic!("the first commit: {committed:?}");
    };
    history.commit(published.version.get(), first.writes());
    let crowd = writers[0].crowd(CROWD);
    commit(&db, &history, &mut writers[0], vec![crowd]);

    let mut errors = Vec::new();
    let cleaner_state = CleanerState::new(cleaner_config());
    let cleaner = Cleaner::new(db.shared.write_back(), &cleaner_state);
    let steps = db.shared.steps();
    for number in 0..BASE_BLOCKS {
        block(
            &db,
            &history,
            &mut writers[number as usize % 2],
            number,
            true,
        );
        (db.shared.write_back().tick()).expect("a turn of write-back");
        db.reclaim();
        cleaner.pass().expect("a pass over the segments");
        // The last blocks leave dead values where they are: a pass over
        // the value log that is swept has a file to clean.
        if number + 4 < BASE_BLOCKS {
            clean_values(&db, &cleaner, &steps).expect("a pass over the value log");
        }
    }
    settle(&db, &history, &mut errors);
    close(db, &cleaner_state, &mut errors);
    assert!(errors.is_empty(), "the state of a sweep: {errors:?}");
    Base {
        fs,
        world: World { history, writers },
    }
}

/// What one schedule gave.
#[derive(Debug, Default)]
struct Outcome {
    /// The points the step passed, and what each was.
    kinds: Vec<&'static str>,
    /// Whether the burst was made before the step went on from its point.
    landed: bool,
    /// What the step did, in its own words.
    did: String,
    /// How long the burst took, and the pages its first commit made and
    /// freed.
    burst: Duration,
    pages: (usize, usize),
    /// Everything that is no sound end of a schedule.
    errors: Vec<String>,
}

/// The burst, on its own thread: the reader that pinned a version before
/// the step goes, the commits are made, and what else runs beside the step
/// in an open database has its turn.
fn act(
    db: &Db,
    history: &History,
    writers: &mut [Writer; 2],
    pinned: &Mutex<Option<Snapshot<'_>>>,
    (step, burst): (Step, Burst),
    cleaner_state: &CleanerState,
    errors: &Mutex<Vec<String>>,
) -> (usize, usize) {
    drop(lock(pinned).take());
    let pages = match burst {
        Burst::Blocks => {
            for number in 1..3u64 {
                block(
                    db,
                    history,
                    &mut writers[number as usize % 2],
                    2_000 + number,
                    true,
                );
            }
            let mut pieces = vec![writers[0].documents(0), writers[0].generation()];
            pieces.extend(writers[0].drop_oldest());
            commit(db, history, &mut writers[0], pieces)
        }
        Burst::Merge => {
            let leaving = writers[0].crowded().saturating_sub(6);
            let thinned = writers[0].thin(leaving).expect("the crowd of the state");
            commit(db, history, &mut writers[0], vec![thinned])
        }
        Burst::Split => {
            let crowd = writers[1].crowd(CROWD);
            commit(db, history, &mut writers[1], vec![crowd])
        }
    };
    let mut failed = Vec::new();
    let write_back = db.shared.write_back();
    if step.reads() || step == Step::Checkpoint {
        // Everything of the background work has its turns under a reader
        // and under the creation of a checkpoint, more than once: a
        // reclaiming that waits for the step is left waiting.
        let cleaner = Cleaner::new(write_back, cleaner_state);
        let steps = db.shared.steps();
        for round in 0..3 {
            db.reclaim();
            block(
                db,
                history,
                &mut writers[round % 2],
                3_000 + round as u64,
                true,
            );
            let turns = [
                write_back
                    .tick()
                    .map(drop)
                    .map_err(|error| error.to_string()),
                cleaner.pass().map(drop).map_err(|error| error.to_string()),
                clean_values(db, &cleaner, &steps).map(drop),
            ];
            failed.extend(turns.into_iter().filter_map(Result::err));
        }
    } else {
        if step != Step::Reclaim {
            // The engine's reclaimer takes out what the commits dropped,
            // the page ids come back, and pages are made under them again.
            ease(db, history);
            block(db, history, &mut writers[1], 3_000, true);
        }
        // Against a reclaiming, a burst that changes the leaves of the
        // directory is that commit and nothing else: what the reclaimer's
        // walks do beside a commit is told apart from what they do beside
        // a pass of write-back, which the blocks bring.
        let alone = step == Step::Reclaim && burst != Burst::Blocks;
        if step != Step::WriteBack && !alone {
            if let Err(error) = write_back.pass() {
                failed.push(format!("a pass of write-back beside the step: {error}"));
            }
        }
    }
    lock(errors).extend(failed);
    pages
}

/// Lets the thread of a burst go when the step ends without having let it
/// in: it hears that nobody will.
struct Released<'a>(&'a Gate);

impl Drop for Released<'_> {
    fn drop(&mut self) {
        drop(lock(&self.0.go).take());
    }
}

/// A reclaiming that a schedule makes itself, with how often it failed.
struct Job {
    reclaim: Reclaim,
    done: bool,
    failed: u8,
}

/// Takes the reclaimings in `jobs` on with `reclaimer`, each until it is
/// done or held back. Returns how many are not done.
fn reclaim_jobs<L: StepLog>(
    db: &Db,
    reclaimer: &Reclaimer<'_, L>,
    jobs: &mut [Job],
    errors: &mut Vec<String>,
) -> usize {
    let store = &*db.shared.store;
    let columns = store.catalog().keyspaces.columns();
    for job in jobs.iter_mut().filter(|job| !job.done && job.failed < 3) {
        for _ in 0..10_000 {
            let guard = db.shared.versions.guard();
            let directory = (store.directories.at(guard.latest())).expect("a handle is kept");
            match reclaimer.reclaim(&mut job.reclaim, &directory, &columns, &guard, 256) {
                Ok(Progress::Done) => {
                    job.done = true;
                    break;
                }
                Ok(Progress::More) => {}
                Ok(Progress::Blocked(_)) => break,
                Err(error) => {
                    job.failed += 1;
                    let (began, now) = (guard.latest(), db.version());
                    errors.push(format!(
                        "the reclaimer, in an operation that began at version {began}, with \
                         version {now} the latest: {error}"
                    ));
                    break;
                }
            }
        }
    }
    jobs.iter().filter(|job| !job.done).count()
}

fn reclaimer<'a, L: StepLog>(db: &'a Db, log: L) -> Reclaimer<'a, L> {
    let store = &*db.shared.store;
    Reclaimer::new(
        store.loader(),
        &store.pool,
        &store.table,
        &store.ids,
        &store.values,
        store.page_size,
        log,
    )
}

/// The step, on the calling thread, which the gate paces.
fn work(
    (db, history): (&Db, &History),
    gate: &Gate,
    step: Step,
    jobs: &mut [Job],
    cleaner_state: &CleanerState,
    made: &mut Option<CommitVersion>,
    errors: &mut Vec<String>,
) -> String {
    let store = &*db.shared.store;
    let asked = Asked {
        versions: &db.shared.versions,
        gate,
    };
    let stores = Stores {
        versions: &asked,
        ..db.shared.write_back().stores()
    };
    let write_back = WriteBack::new(stores, &store.flush, store.page_size);
    let write_back = write_back.expect("the pool was made of the store's page size");
    let log = Interrupted {
        steps: db.shared.steps(),
        gate,
    };
    let did = match step {
        Step::Reclaim => {
            let left = reclaim_jobs(db, &reclaimer(db, &log), jobs, errors);
            Ok(format!(
                "{} reclaimings, {left} of them held back",
                jobs.len()
            ))
        }
        Step::WriteBack => match write_back.tick() {
            Ok(tick) => Ok(format!("{tick:?}")),
            Err(error) => Err(format!("write-back: {error}")),
        },
        Step::CleanSegments => match Cleaner::new(write_back, cleaner_state).pass() {
            Ok(pass) => Ok(format!("{pass:?}")),
            Err(error) => Err(format!("the cleaner's pass over the segments: {error}")),
        },
        Step::CleanValues => clean_values(db, &Cleaner::new(write_back, cleaner_state), &log),
        Step::Checkpoint => {
            let committer = db.shared.committer();
            match Checkpointer::new(&committer).create(Path::new(CHECKPOINT)) {
                Ok(created) => {
                    *made = Some(created.version);
                    Ok(format!("{created:?}"))
                }
                Err(error) => Err(format!("the checkpoint: {error}")),
            }
        }
        Step::Read | Step::ReadPinned => {
            let mut lease = LatestLease::new();
            let snapshot = (step == Step::ReadPinned).then(|| db.snapshot());
            let operation = match &snapshot {
                Some(snapshot) => snapshot.operation(),
                None => db.read(&mut lease),
            };
            let at = operation.at().get();
            let state = history.through(at);
            let mut subtrees = state.subtrees();
            subtrees.push(shared());
            let expected: Vec<Expected> = (subtrees.into_iter())
                .map(|subtree| Expected::of(&state, subtree, at))
                .collect();
            drop(state);
            let mut read = Read::default();
            for expected in &expected {
                read += expected.check(db, &operation);
            }
            Ok(format!("{read:?} at version {at}"))
        }
    };
    did.unwrap_or_else(|error| {
        errors.push(error);
        String::new()
    })
}

const CHECKPOINT: &str = "/checkpoint";

/// One schedule: the state is opened, written a little more, and `step`
/// runs with `burst` let in at its point `at`, or after it with zero. Then
/// everything is brought to rest and judged.
fn run(base: &Base, step: Step, burst: Burst, at: u64, patience: Duration) -> Outcome {
    let (gate, gone, finished) = Gate::new(at, patience);
    let vfs: Arc<dyn Vfs> = Arc::new(Paced {
        inner: Arc::new(base.fs.crash(0)),
        gate: gate.clone(),
    });
    let options = options(vfs.clone());
    let db = Db::open("/db", &options).expect("the state is opened");
    halt_background(&db);
    let history = base.world.history.copy();
    let mut writers = base.world.writers.clone();
    assert_eq!(db.version().get(), history.version());
    let mut errors = Vec::new();

    // Written with no write-back: page versions are owed, values are
    // pending, and what is dropped waits. A reader pins a version after
    // the drops, and the blocks after it supersede what it reads.
    for number in 0..PRELUDE {
        block(
            &db,
            &history,
            &mut writers[number as usize % 2],
            1_000 + number,
            true,
        );
    }
    let thinned = writers[0].thin(3).expect("the crowd of the state");
    commit(&db, &history, &mut writers[0], vec![thinned]);
    let pinned = Mutex::new(Some(db.snapshot()));
    for number in PRELUDE..PRELUDE + PINNED {
        block(
            &db,
            &history,
            &mut writers[number as usize % 2],
            1_000 + number,
            false,
        );
    }

    // The reclaimings that are the step are the schedule's own, under its
    // pace: the engine's rounds are not given them.
    let mut jobs: Vec<Job> = match step {
        Step::Reclaim => (lock(&db.shared.store.dropped).drain(..))
            .map(|(_, dropped)| Job {
                reclaim: Reclaim::new(dropped),
                done: false,
                failed: 0,
            })
            .collect(),
        _ => Vec::new(),
    };
    let cleaner_state = CleanerState::new(cleaner_config());
    let burst_errors = Mutex::new(Vec::new());
    let mut made = None;
    let (did, (took, pages)) = thread::scope(|threads| {
        let (db, history, writers) = (&db, &history, &mut writers);
        let (pinned, burst_errors, cleaner_state) = (&pinned, &burst_errors, &cleaner_state);
        // If the step fails, the thread of the burst is not left waiting.
        let _released = Released(&gate);
        let beside = threads.spawn(move || {
            if gone.recv().is_err() {
                return (Duration::ZERO, (0, 0));
            }
            let began = Instant::now();
            let pages = act(
                db,
                history,
                writers,
                pinned,
                (step, burst),
                cleaner_state,
                burst_errors,
            );
            let _ = finished.send(());
            (began.elapsed(), pages)
        });
        gate.pace();
        let did = work(
            (db, history),
            &gate,
            step,
            &mut jobs,
            cleaner_state,
            &mut made,
            &mut errors,
        );
        gate.rest();
        gate.let_in(None);
        (did, beside.join().expect("the burst is made"))
    });
    errors.extend(burst_errors.into_inner().expect("the errors"));

    // To rest. The reclaimings of the step are taken to their end, with
    // commits between the tries: a page that the latest commit wrote is
    // freed after the next one.
    drop(pinned);
    let unpaced = reclaimer(&db, db.shared.steps());
    for round in 0u32.. {
        if reclaim_jobs(&db, &unpaced, &mut jobs, &mut errors) == 0 {
            break;
        }
        if round == 400 || jobs.iter().any(|job| job.failed >= 3) {
            errors.push("a reclaiming of the step does not end".into());
            break;
        }
        tick(&db, &history, round);
    }
    settle(&db, &history, &mut errors);
    if let Err(error) = db.shared.write_back().pass() {
        errors.push(format!("a pass of write-back at the end: {error}"));
    }
    let stats = db.stats();
    let background = &stats.background;
    if background.write_back_errors + background.reclaim_errors + stats.reclaimed.given_up > 0 {
        errors.push(format!("{background:?}, {:?}", stats.reclaimed));
    }

    // The checker decides.
    let report = verify(&db, Scope::Still, db.version());
    errors.extend(
        report
            .findings
            .iter()
            .map(|finding| format!("the checker: {finding}")),
    );
    if report.more_findings + report.unreached.cells > 0 {
        errors.push(format!(
            "the checker: {} more findings, {} cells that nothing reaches",
            report.more_findings, report.unreached.cells
        ));
    }
    compare_all(&db, &history);
    if let Some(version) = made {
        // The checkpoint is a database of exactly its version.
        let opened = Db::open(CHECKPOINT, &options).expect("the checkpoint is opened");
        assert_eq!(opened.version(), version);
        compare_all(&opened, &history);
        opened.close().expect("the checkpoint is closed");
        checkpoint::delete(&*vfs, &PathBuf::from(CHECKPOINT)).expect("the checkpoint is deleted");
    }
    close(db, &cleaner_state, &mut errors);
    let kinds = std::mem::take(&mut *lock(&gate.kinds));
    Outcome {
        kinds,
        landed: gate.landed.load(Ordering::Relaxed),
        did,
        burst: took,
        pages,
        errors,
    }
}

/// The points of a step that a sweep tries: all of them, or with
/// `LIGNIN_SCHEDULE_POINTS=N` at most `N`, evenly spread.
fn chosen(points: u64) -> Vec<u64> {
    let most = std::env::var("LIGNIN_SCHEDULE_POINTS").ok();
    let most = most
        .and_then(|most| most.parse::<u64>().ok())
        .unwrap_or(u64::MAX);
    if points <= most {
        return (1..=points).collect();
    }
    (0..most).map(|index| 1 + index * points / most).collect()
}

/// Tries `burst` at every point of `step`, each schedule from one state,
/// and fails with every schedule that did not end sound.
fn sweep(step: Step, burst: Burst) {
    let base = base(0x5eed + step as u64 * 16 + burst as u64);
    // With the burst after the step: what the step does alone, and how
    // long the burst takes.
    let calm = run(&base, step, burst, 0, Duration::ZERO);
    assert!(
        calm.errors.is_empty(),
        "with the burst after the step ({}): {:#?}",
        calm.did,
        calm.errors
    );
    let points = calm.kinds.len() as u64;
    assert!(points > 0, "the step has no point: {}", calm.did);
    let patience = (calm.burst * 4).max(Duration::from_millis(200));
    let (mut failures, mut landed, mut tried) = (Vec::new(), 0u64, 0u64);
    for at in chosen(points) {
        let kind = calm.kinds[at as usize - 1];
        let schedule = format!("the burst at point {at} of {points} ({kind})");
        let outcome = catch_unwind(AssertUnwindSafe(|| run(&base, step, burst, at, patience)));
        tried += 1;
        match outcome {
            Ok(outcome) => {
                landed += u64::from(outcome.landed);
                if !outcome.errors.is_empty() {
                    failures.push(format!("{schedule}: {:?}", outcome.errors));
                }
            }
            Err(panic) => {
                let said = (panic.downcast_ref::<String>().cloned())
                    .or_else(|| panic.downcast_ref::<&str>().map(|said| (*said).to_owned()));
                failures.push(format!("{schedule}: {}", said.unwrap_or_default()));
            }
        }
    }
    let mut kinds = std::collections::BTreeMap::new();
    for kind in &calm.kinds {
        *kinds.entry(*kind).or_insert(0u64) += 1;
    }
    println!(
        "{step:?} against {burst:?}: {points} points {kinds:?}; {tried} schedules, the burst \
         landed inside the step in {landed}; {} failed. The burst took {:?}, and its commit made \
         {} pages and freed {}. The step alone: {}",
        failures.len(),
        calm.burst,
        calm.pages.0,
        calm.pages.1,
        calm.did
    );
    assert!(
        failures.is_empty(),
        "{step:?} against {burst:?}: {failures:#?}"
    );
    assert!(
        landed > 0,
        "the burst landed inside the step in no schedule"
    );
}

/// What an operation reads of a database of a crowd of small Merks, when
/// most of the crowd is dropped by a commit after the operation began, and
/// with `pass` write-back has its turn after that commit. The operation
/// pins its version with `pinned`, and holds its guard alone without.
///
/// Returns what the operation read, which is compared with the model, or
/// the panic of a read that failed.
fn read_under_a_commit(pass: bool, pinned: bool) -> Result<Read, String> {
    let fs = SimVfs::new(SimConfig::new(7));
    let options = options(Arc::new(fs.clone()));
    let history = History::new(1);
    let mut writer = Writer::new(1, 7);
    {
        let db = Db::open("/db", &options).expect("a new database");
        halt_background(&db);
        let crowd = writer.crowd(CROWD);
        commit(&db, &history, &mut writer, vec![crowd]);
        (db.shared.write_back().tick()).expect("a turn of write-back");
        close(db, &CleanerState::new(cleaner_config()), &mut Vec::new());
    }
    // Opened again: no page of the database is in the pool.
    let db = Db::open("/db", &options).expect("the database is opened again");
    halt_background(&db);
    let mut lease = LatestLease::new();
    let snapshot = pinned.then(|| db.snapshot());
    let operation = match &snapshot {
        Some(snapshot) => snapshot.operation(),
        None => db.read(&mut lease),
    };
    let at = operation.at().get();
    let state = history.through(at);
    let expected: Vec<Expected> = (state.subtrees().into_iter())
        .map(|subtree| Expected::of(&state, subtree, at))
        .collect();
    drop(state);

    let thinned = writer.thin(CROWD as usize - 6).expect("the crowd");
    commit(&db, &history, &mut writer, vec![thinned]);
    if pass {
        (db.shared.write_back().tick()).expect("a turn of write-back");
    }
    let read = catch_unwind(AssertUnwindSafe(|| {
        let mut read = Read::default();
        for expected in &expected {
            read += expected.check(&db, &operation);
        }
        read
    }));
    read.map_err(|panic| {
        let said = (panic.downcast_ref::<String>().cloned())
            .or_else(|| panic.downcast_ref::<&str>().map(|said| (*said).to_owned()));
        said.unwrap_or_default()
    })
}

/// **An operation that pins nothing reads the version of its beginning
/// under its guard** (`transactions.md` RD4, PN5), also when a commit
/// supersedes pages it has not read yet and write-back then writes their
/// new versions as base images and trims their chains: the link of the
/// version it reads is one that its guard keeps.
///
/// This is the smallest form of what the schedules of [`Step::Read`] and
/// [`Step::Reclaim`] sweep. The two tests after it are its controls.
#[test]
fn an_operation_that_pins_nothing_reads_its_version_after_a_commit_and_a_turn_of_write_back() {
    let read = read_under_a_commit(true, false).unwrap_or_else(|failed| panic!("{failed}"));
    assert_eq!((read.nodes, read.absent), (CROWD, 0));
}

#[test]
fn an_operation_that_pins_nothing_reads_its_version_after_a_commit() {
    let read = read_under_a_commit(false, false).unwrap_or_else(|failed| panic!("{failed}"));
    assert_eq!((read.nodes, read.absent), (CROWD, 0));
}

#[test]
fn a_snapshot_reads_its_version_after_a_commit_and_a_turn_of_write_back() {
    let read = read_under_a_commit(true, true).unwrap_or_else(|failed| panic!("{failed}"));
    assert_eq!((read.nodes, read.absent), (CROWD, 0));
}

/// **A checkpoint that is sampled right after the log turned to a new file
/// opens** (CK1, CK2): the log's newest file holds no item then, and the
/// log is durable through the version, so the checkpoint's log ends where
/// the durable log ends.
///
/// With the small log files of these tests the log turns every few
/// commits, and the stress test meets this by chance.
#[test]
fn a_checkpoint_sampled_right_after_the_log_turned_to_a_new_file_opens() {
    let fs = SimVfs::new(SimConfig::new(9));
    let options = options(Arc::new(fs));
    let db = Db::open("/db", &options).expect("a new database");
    halt_background(&db);
    let history = History::new(1);
    let mut writer = Writer::new(1, 9);
    let write_back = db.shared.write_back();
    let mut turned = false;
    for round in 0..200 {
        let crowd = writer.crowd(4);
        commit(&db, &history, &mut writer, vec![crowd]);
        if write_back.roll_log().expect("the log is turned") {
            turned = true;
            println!("the log turned after {round} commits");
            break;
        }
    }
    assert!(turned, "the log never turned to a new file");
    let committer = db.shared.committer();
    let created = Checkpointer::new(&committer).create(Path::new(CHECKPOINT));
    let created = created.unwrap_or_else(|error| panic!("the checkpoint: {error}"));
    let opened = Db::open(CHECKPOINT, &options).unwrap_or_else(|error| {
        let manifest = lignin_store::segment::read_manifest(&*options.vfs, Path::new(CHECKPOINT));
        panic!("the checkpoint is opened: {error}\n{created:?}\n{manifest:#?}")
    });
    assert_eq!(opened.version(), created.version);
    let read = compare_all(&opened, &history);
    assert_eq!(read.nodes, writer.crowded() as u64);
    opened.close().expect("the checkpoint is closed");
}

macro_rules! schedules {
    ($($name:ident: $step:ident, $burst:ident;)*) => {
        $(
            #[test]
            fn $name() {
                sweep(Step::$step, Burst::$burst);
            }
        )*
    };
}

schedules! {
    a_reclaiming_against_commits_that_merge_the_leaves_of_the_directory: Reclaim, Merge;
    a_reclaiming_against_commits_that_split_the_leaves_of_the_directory: Reclaim, Split;
    a_reclaiming_against_blocks: Reclaim, Blocks;
    a_turn_of_write_back_against_commits_that_merge_the_leaves_of_the_directory: WriteBack, Merge;
    a_turn_of_write_back_against_commits_that_split_the_leaves_of_the_directory: WriteBack, Split;
    a_turn_of_write_back_against_blocks_that_drop_reclaim_and_write_again: WriteBack, Blocks;
    a_pass_over_the_segments_against_commits_that_merge_the_leaves_of_the_directory: CleanSegments, Merge;
    a_pass_over_the_segments_against_commits_that_split_the_leaves_of_the_directory: CleanSegments, Split;
    a_pass_over_the_segments_against_blocks: CleanSegments, Blocks;
    a_pass_over_the_value_log_against_commits_that_merge_the_leaves_of_the_directory: CleanValues, Merge;
    a_pass_over_the_value_log_against_commits_that_split_the_leaves_of_the_directory: CleanValues, Split;
    a_pass_over_the_value_log_against_blocks: CleanValues, Blocks;
    a_checkpoint_against_commits_that_merge_the_leaves_of_the_directory: Checkpoint, Merge;
    a_checkpoint_against_commits_that_split_the_leaves_of_the_directory: Checkpoint, Split;
    a_checkpoint_against_blocks: Checkpoint, Blocks;
    an_operation_that_pins_nothing_against_commits_that_merge_the_leaves_of_the_directory: Read, Merge;
    an_operation_that_pins_nothing_against_commits_that_split_the_leaves_of_the_directory: Read, Split;
    an_operation_that_pins_nothing_against_blocks: Read, Blocks;
    a_snapshot_against_commits_that_merge_the_leaves_of_the_directory: ReadPinned, Merge;
    a_snapshot_against_commits_that_split_the_leaves_of_the_directory: ReadPinned, Split;
    a_snapshot_against_blocks: ReadPinned, Blocks;
}
