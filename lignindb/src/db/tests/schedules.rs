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
//! thread, which makes the burst, and waits for it. Where the step holds
//! the commit lock or the lock on the counts of values at its point, no
//! commit can come in: the step goes on after a while and the burst lands
//! behind the lock, which is a schedule as well. Where it holds neither,
//! the step waits until a commit of the burst is published
//! ([`Gate::await_a_commit`]). [`sweep`] tries the points of a step, each
//! from the same state: a database that was written by both writers of
//! the stress test, closed, opened again with nothing in memory, and
//! written a little more with no write-back ([`prelude`]), so that page
//! versions are owed, values are pending and drops wait.
//!
//! With the crate's tests a sweep tries a sample of the points of its
//! step, evenly spread. `LIGNIN_SCHEDULE_POINTS=all` has it try every
//! point, which the nightly run does (`lignin-ci.sh schedules`), and
//! `LIGNIN_SCHEDULE_POINTS=N` at most `N`. The sweep of
//! [`Burst::Empties`] is apart in both: its state is made for it
//! ([`pages_to_empty`]), and it tries every point of the step, which has
//! few, whatever is asked.
//!
//! # The oracle
//!
//! **The physical invariant checker decides** (`crate::verify`,
//! [`Scope::Still`]), because the damaging form of D-145 counted no error:
//! after the step and the burst, everything that was dropped is reclaimed,
//! write-back is made to pass, and the checker reads the store at rest:
//! no finding, and no cell that nothing reaches. Beside it: no error of
//! the step, of write-back or of a reclaiming; every subtree read through a
//! snapshot is what the model of the stress test has; a checkpoint that
//! the step made or opened holds exactly its version.
//!
//! # What a schedule does not reach
//!
//! **A point is a seam, not an instruction.** Between two reads of pages
//! that are both in the buffer pool there is no point unless the step asks
//! about versions, pins, guards or epochs between them, so a step that
//! reads two levels of
//! a tree at one version it asked for once is swept before and after the
//! walk and not inside it. That is the form such a walk has to have
//! (D-145), and what makes it safe there is that it reads one version; the
//! races inside the pool's own calls are the loom models' of
//! `lignin-store` (`cache::tests::races`). One burst is let in at one
//! point: two bursts at two points of one step are the stress test's
//! (`super::stress`), by chance.
//!
//! # A schedule inside a commit
//!
//! In a schedule of the kind above no step stands inside a commit: at its
//! point it waits for the publication of the commit it let in. A second
//! kind has steps there on purpose ([`inside_a_commit`]). A commit is
//! held between its count and its publication, where it has installed
//! its pages and given the lock on the counts of values back and the
//! published version is still the one before it
//! (`CommitState::run_before_publication`). A turn of write-back and the
//! passes of the cleaner run there, each to its end, from a state in
//! which they meet what the commit did: the values that write-back places
//! are those of the commit's pages, and the file that the passes over the
//! value log empty is one that the commit has released values of. A
//! snapshot reads the version before the commit afterwards, and the
//! oracle below decides.
//!
//! A round of the engine's reclaimer is no step of this kind: one that
//! ends a reclaiming asks for the commit lock, which the commit holds.
//! [`a_version_pinned_inside_a_commit_checks_sound_with_the_reclaimer_held_back`]
//! has a round inside a commit that ends none. Neither is the creation of
//! a checkpoint of the database ([`Db::checkpoint`]), which asks for that
//! lock too.
//!
//! # What was found by chance, and which schedule finds it on purpose
//!
//! | Defect | Schedule |
//! | --- | --- |
//! | D-145: the reclaimer walked to a record's leaf with each level at the then-current version of its page; a commit that freed or split the leaf between two levels gave an error, or left a root record with no entry | a reclaiming against a burst that merges, and one that splits, the leaves of the directory |
//! | D-116: a page that was made under an id used before, and freed while a pass of write-back had its block appended and not yet linked, was linked | a turn of write-back against a commit that empties pages it has not written ([`Burst::Empties`]) |
//! | D-172: an open of a checkpoint refused a log file that the database it was made of had appended to since the open first read it (`WrongLogEnd`) | `super::beside`, which commits before every look of the open at its log; the opens of a checkpoint here commit at one point each |
//! | D-142: a value-log file that a page step emptied was stamped empty at the version the step read, and was taken out under a snapshot of that version, which still read a cell that a commit had counted against the file and not yet published | passes over the value log inside a commit |
//! | D-142: the owners of values looked a page up at the published version, and a value that write-back had placed in a page of a commit not yet published was counted live with no owner found | a turn of write-back and passes over the value log inside a commit |

use std::{
    collections::BTreeMap,
    io,
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, ThreadId},
};

use lignin_format::{CommitVersion, FileId};
use lignin_io::{
    sim::{SimConfig, SimVfs},
    DirEntry, OpenMode, Vfs, VfsFile, VfsLock,
};
use lignin_store::{
    alloc::{Epoch, Pins},
    cleaner::{BothOwners, Cleaner, CleanerConfig, CleanerState, ValuePass},
    flush::{Published, Stores, WriteBack},
    values::{Cleaned, OwnersError},
};
use lignin_tree::{
    drop::{Progress, Reclaim, Reclaimer},
    flat::{FlatOwners, StepLog},
    value::TreeOwners,
};

use super::{
    stress::{
        compare_all, compare_at,
        history::History,
        record, stress_options, tick, verify,
        work::{joined, shared, write, Piece, Writer},
    },
    *,
};
use crate::{
    checkpoint::{self, CheckpointOptions},
    db::store::{lock, Shared, Steps},
    snapshot::{LatestLease, Snapshot, Versions},
    txn::{
        tests::model::{OwnedBatch, OwnedSubtree, Val, Write},
        Unit,
    },
    verify::{Rule, Scope},
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
    /// The database, for what the gate asks it at its point.
    asks: Database,
    /// Whether a commit could come in at the point: nobody held the commit
    /// lock or the lock on the counts of values there. `None` before the
    /// point, and for a schedule with none.
    open: Mutex<Option<bool>>,
    /// Whether a commit of the burst was published before the step went
    /// on from its point.
    committed: AtomicBool,
}

/// The database a gate asks, while a schedule has one open.
#[derive(Default)]
struct Database(Mutex<Option<Arc<Shared>>>);

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the database of the schedule")
    }
}

/// How long a commit that neither lock keeps out is waited for at a
/// point. A schedule in which none is published in that time fails: a
/// commit waits there for something the harness does not ask about.
const COMMIT_LANDS_WITHIN: Duration = Duration::from_secs(60);

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
            asks: Database::default(),
            open: Mutex::new(None),
            committed: AtomicBool::new(false),
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
        if passed != self.at {
            return;
        }
        self.await_a_commit();
        if self.let_in(Some(self.patience)) {
            self.landed.store(true, Ordering::Relaxed);
        }
    }

    /// Lets the burst in at the point and, where a commit can come in,
    /// waits until one of the burst is published, up to
    /// [`COMMIT_LANDS_WITHIN`]. Whether one can is asked of the two locks
    /// that every commit takes, with no wait: the paced thread, which
    /// asks, holds them here or nobody does. So the step goes on from
    /// such a point when a commit has landed there, and not when a time
    /// that a fast machine needs for one has passed.
    fn await_a_commit(&self) {
        let Some(shared) = lock(&self.asks.0).clone() else {
            return;
        };
        let store = &shared.store;
        let counts = store.values.try_step().expect("the value log");
        let open = counts.is_some() && store.committing.try_lock().is_some();
        drop(counts);
        *lock(&self.open) = Some(open);
        if !open {
            return;
        }
        let before = shared.versions.latest();
        self.go();
        let began = Instant::now();
        while shared.versions.latest() == before && began.elapsed() < COMMIT_LANDS_WITHIN {
            match lock(&self.made).try_recv() {
                Ok(()) => {
                    self.finished.store(true, Ordering::Relaxed);
                    break;
                }
                Err(mpsc::TryRecvError::Disconnected) => break,
                Err(mpsc::TryRecvError::Empty) => thread::sleep(Duration::from_micros(200)),
            }
        }
        let committed = shared.versions.latest() != before;
        self.committed.store(committed, Ordering::Relaxed);
    }

    /// Wakes the thread of the burst, once.
    fn go(&self) {
        if let Some(go) = lock(&self.go).take() {
            let _ = go.send(());
        }
    }

    /// Lets the burst in, if it was not, and waits until it is made: for
    /// `patience`, or for as long as it takes. Returns whether it is made.
    fn let_in(&self, patience: Option<Duration>) -> bool {
        self.go();
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
    /// The open of a checkpoint of the database, which was created with
    /// the database at rest. Its points are the operations of the open on
    /// the files of the checkpoint, most of which it shares with the
    /// database that goes on beside it (CK5, CK7).
    OpenCheckpoint,
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
    /// A commit that takes the one node out of each of a few Merks which
    /// the state has just entered, under page ids that named other pages
    /// before, and of which nothing is written back: the page of each is
    /// freed with no free link of its own (`page-table.md` T18, D-116).
    /// The state of this burst has those pages and little else that is
    /// unwritten ([`pages_to_empty`]), and its sweep tries every point.
    Empties,
}

/// How many Merks the state of [`Burst::Empties`] has just entered, each
/// with its node in a page of its own.
const FRESH: usize = 8;

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

/// What a commit did to the pages of the store: how many it made, how
/// many it freed, and how many of the freed ones no segment ever held
/// under their id's present life (`Published::freed_unlinked`).
#[derive(Debug, Default, Clone, Copy)]
struct Pages {
    made: usize,
    freed: usize,
    unlinked: usize,
}

/// Commits `pieces` of `writer` in one batch, through a transaction of its
/// own, and tells the model. Returns what it did to the pages.
fn commit(db: &Db, history: &History, writer: &mut Writer, pieces: Vec<Piece>) -> Pages {
    let batch = joined(&pieces);
    let committed = write(db, &batch);
    let Ok(Committed::Published(published)) = committed else {
        panic!("a commit of a schedule: {committed:?}");
    };
    assert!(published.miscounted.is_none(), "{published:?}");
    let writes = pieces.iter().flat_map(|piece| piece.writes.iter().cloned());
    history.commit(published.version.get(), writes.collect());
    writer.committed(pieces);
    Pages {
        made: published.created.len(),
        freed: published.freed.len(),
        unlinked: published.freed_unlinked,
    }
}

/// One block of `writer`: documents, a new generation every other block,
/// and with `drops` the drop of the oldest generation beyond those kept.
fn block(db: &Db, history: &History, writer: &mut Writer, number: u64, drops: bool) {
    let mut pieces = vec![writer.documents(number as usize % Writer::MERKS)];
    if number.is_multiple_of(2) {
        pieces.push(writer.generation());
    }
    if drops && writer.generations() > GENERATIONS {
        pieces.extend(writer.drop_oldest());
    }
    commit(db, history, writer, pieces);
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
    value_pass(db, cleaner, log).map(|pass| format!("{pass:?}"))
}

/// One pass of `cleaner` over the value log, with the owners of values of
/// both kinds.
fn value_pass(db: &Db, cleaner: &Cleaner<'_>, log: &dyn StepLog) -> Result<ValuePass, String> {
    let store = &*db.shared.store;
    let owners = BothOwners(
        TreeOwners::new(store.loader(), &store.pool, store.page_size, &log),
        FlatOwners::new(store.loader(), &store.pool, store.config(), &log),
    );
    (cleaner.pass_values(&owners))
        .map_err(|error| format!("the cleaner's pass over the value log: {error}"))
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
    let db = open_at_rest("/db", &options(Arc::new(fs.clone()))).expect("a new database");
    let history = History::new(1);
    let mut writers = [Writer::new(1, seed), Writer::new(2, seed)];
    let mut first = OwnedBatch::default();
    first.subtrees.push(OwnedSubtree::Enter(shared(), None));
    first.subtrees.extend(writers[0].enters());
    first.subtrees.extend(writers[1].enters());
    record(&db, &history, &first);
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
    /// Whether a commit could come in at the point, and whether one of
    /// the burst was published there.
    open: Option<bool>,
    committed: bool,
    /// What the step did, in its own words.
    did: String,
    /// How many links of blocks write-back was refused while the step
    /// ran.
    refused: u64,
    /// How long the burst took, and what its first commit did to the
    /// pages.
    burst: Duration,
    pages: Pages,
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
) -> Pages {
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
        Burst::Empties => {
            let emptied = writers[0].empty_newest(FRESH);
            commit(db, history, &mut writers[0], vec![emptied])
        }
    };
    let mut failed = Vec::new();
    let write_back = db.shared.write_back();
    if step.reads() || matches!(step, Step::Checkpoint | Step::OpenCheckpoint) {
        // Everything of the background work has its turns under a reader,
        // under the creation of a checkpoint and beside the open of one,
        // more than once: a reclaiming that waits for the step is left
        // waiting.
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
        if step != Step::WriteBack
            && !alone
            && let Err(error) = write_back.pass()
        {
            failed.push(format!("a pass of write-back beside the step: {error}"));
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

/// The checkpoint a step made: its version, and the checkpoint itself if
/// the step opened it.
#[derive(Default)]
struct Made {
    version: Option<CommitVersion>,
    opened: Option<Db>,
}

/// The step, on the calling thread, which the gate paces.
fn work(
    (db, history, options): (&Db, &History, &Options),
    gate: &Gate,
    step: Step,
    jobs: &mut [Job],
    cleaner_state: &CleanerState,
    made: &mut Made,
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
            match db.checkpoint(Path::new(CHECKPOINT), CheckpointOptions::default()) {
                Ok(created) => {
                    made.version = Some(created.version);
                    Ok(format!("{created:?}"))
                }
                Err(error) => Err(format!("the checkpoint: {error}")),
            }
        }
        Step::OpenCheckpoint => {
            // The creation is no part of the step, and it is over, with
            // what it held, before the first point.
            gate.rest();
            let created = db.checkpoint(Path::new(CHECKPOINT), CheckpointOptions::default());
            gate.pace();
            match created {
                Ok(created) => {
                    made.version = Some(created.version);
                    match open_at_rest(CHECKPOINT, options) {
                        Ok(opened) => {
                            made.opened = Some(opened);
                            Ok(format!("opened, {created:?}"))
                        }
                        Err(error) => Err(format!(
                            "the checkpoint is opened beside the database it was made of: {error}"
                        )),
                    }
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
            let read = compare_at(db, history, &operation);
            let at = operation.at().get();
            Ok(format!("{read:?} at version {at}"))
        }
    };
    did.unwrap_or_else(|error| {
        errors.push(error);
        String::new()
    })
}

const CHECKPOINT: &str = "/checkpoint";

/// What every schedule but those of [`Burst::Empties`] begins with,
/// written with no write-back: page versions are owed, values are pending,
/// and what is dropped waits. A reader pins a version after the drops, and
/// the blocks after it supersede what it reads. Returns the reader's pin.
fn prelude<'d>(db: &'d Db, history: &History, writers: &mut [Writer; 2]) -> Snapshot<'d> {
    for number in 0..PRELUDE {
        block(
            db,
            history,
            &mut writers[number as usize % 2],
            1_000 + number,
            true,
        );
    }
    let thinned = writers[0].thin(3).expect("the crowd of the state");
    commit(db, history, &mut writers[0], vec![thinned]);
    let pinned = db.snapshot();
    for number in PRELUDE..PRELUDE + PINNED {
        block(
            db,
            history,
            &mut writers[number as usize % 2],
            1_000 + number,
            false,
        );
    }
    pinned
}

/// What a schedule of [`Burst::Empties`] begins with. [`FRESH`] members of
/// the crowd are dropped and reclaimed, which frees their pages and
/// releases the ids. As many members are entered then, with no write-back,
/// so that their pages can take ids that were used before. Whether they
/// did, the commit of the burst says when it frees them
/// ([`Pages::unlinked`]).
fn pages_to_empty(db: &Db, history: &History, writer: &mut Writer, errors: &mut Vec<String>) {
    let leaving = writer.thin(FRESH).expect("the crowd of the state");
    commit(db, history, writer, vec![leaving]);
    settle(db, history, errors);
    // A retired id is given out again a round or two after its page went.
    for round in 0..3 {
        db.reclaim();
        tick(db, history, 2_000 + round);
    }
    let crowd = writer.crowd(FRESH as u64);
    commit(db, history, writer, vec![crowd]);
}

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
    let db = open_at_rest("/db", &options).expect("the state is opened");
    *lock(&gate.asks.0) = Some(db.shared.clone());
    let history = base.world.history.copy();
    let mut writers = base.world.writers.clone();
    assert_eq!(db.version().get(), history.version());
    let mut errors = Vec::new();

    let pinned = match burst {
        Burst::Empties => {
            pages_to_empty(&db, &history, &mut writers[0], &mut errors);
            None
        }
        _ => Some(prelude(&db, &history, &mut writers)),
    };
    let pinned = Mutex::new(pinned);
    let refused = || db.shared.write_back().state().stats().links_refused;
    let refused_before = refused();

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
    let mut made = Made::default();
    let (did, (took, pages)) = thread::scope(|threads| {
        let (db, history, writers) = (&db, &history, &mut writers);
        let (pinned, burst_errors, cleaner_state) = (&pinned, &burst_errors, &cleaner_state);
        // If the step fails, the thread of the burst is not left waiting.
        let _released = Released(&gate);
        let beside = threads.spawn(move || {
            if gone.recv().is_err() {
                return (Duration::ZERO, Pages::default());
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
            (db, history, &options),
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
    drop(lock(&gate.asks.0).take());
    let refused = refused() - refused_before;

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
    if let Some(version) = made.version {
        // The checkpoint is a database of exactly its version, whatever
        // was committed beside its creation or its open.
        let opened = match made.opened.take() {
            Some(opened) => opened,
            None => Db::open(CHECKPOINT, &options).expect("the checkpoint is opened"),
        };
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
        open: *lock(&gate.open),
        committed: gate.committed.load(Ordering::Relaxed),
        did,
        refused,
        burst: took,
        pages,
        errors,
    }
}

/// How many points of a step a sweep tries where nobody says.
const SAMPLE: u64 = 12;

/// The points of a step that a sweep tries, evenly spread: at most
/// [`SAMPLE`] of them, at most `N` with `LIGNIN_SCHEDULE_POINTS=N`, and
/// every point with `LIGNIN_SCHEDULE_POINTS=all`.
fn chosen(points: u64) -> Vec<u64> {
    let most = match std::env::var("LIGNIN_SCHEDULE_POINTS") {
        Err(_) => SAMPLE,
        Ok(all) if all == "all" => u64::MAX,
        Ok(most) => most.parse().unwrap_or_else(|_| {
            panic!("LIGNIN_SCHEDULE_POINTS={most} is neither a number nor `all`")
        }),
    };
    if points <= most {
        return (1..=points).collect();
    }
    (0..most).map(|index| 1 + index * points / most).collect()
}

/// Tries `burst` at the [`chosen`] points of `step`, each schedule from
/// one state, and fails with every schedule that did not end sound.
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
    let every = burst == Burst::Empties;
    let patience = (calm.burst * 4).max(Duration::from_millis(200));
    let (mut failures, mut landed, mut tried) = (Vec::new(), 0u64, 0u64);
    let (mut open, mut committed, mut in_hand) = (0u64, 0u64, 0u64);
    let tried_at = match every {
        true => (1..=points).collect(),
        false => chosen(points),
    };
    for at in tried_at {
        let kind = calm.kinds[at as usize - 1];
        let schedule = format!("the burst at point {at} of {points} ({kind})");
        let outcome = catch_unwind(AssertUnwindSafe(|| run(&base, step, burst, at, patience)));
        tried += 1;
        match outcome {
            Ok(outcome) => {
                landed += u64::from(outcome.landed);
                open += u64::from(outcome.open == Some(true));
                committed += u64::from(outcome.committed);
                let unlinked = outcome.pages.unlinked > 0;
                in_hand += u64::from(unlinked && outcome.refused > calm.refused);
                if outcome.open == Some(true) && !outcome.committed {
                    failures.push(format!(
                        "{schedule}: no commit of the burst was published within \
                         {COMMIT_LANDS_WITHIN:?}, where neither the commit lock nor the lock on \
                         the counts of values was held"
                    ));
                }
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
        "{step:?} against {burst:?}: {points} points {kinds:?}; {tried} schedules, a commit \
         could come in at the point of {open} and was published there in {committed}, and the \
         whole burst landed inside the step in {landed}; {} failed. The burst took {:?}, and its \
         commit made {} pages and freed {}. The step alone: {}",
        failures.len(),
        calm.burst,
        calm.pages.made,
        calm.pages.freed,
        calm.did
    );
    if every {
        println!(
            "{step:?} against {burst:?}: the burst freed a page that no segment held, and \
             write-back was refused a link, in {in_hand} schedules"
        );
    }
    assert!(
        failures.is_empty(),
        "{step:?} against {burst:?}: {failures:#?}"
    );
    // A commit was waited for wherever one could come in, and each of
    // those schedules that has not failed had one published at its point.
    assert!(open > 0, "a commit could come in at no point of the step");
    if every {
        // By construction: in some schedule the commit of the burst freed
        // a page that no segment held under an id used before, and the
        // turn was refused a link that it is given with the burst behind
        // it. The turn had the block of such a page appended, and the page
        // was freed before the link was pushed.
        assert!(
            in_hand > 0,
            "in no schedule did the burst free a page that write-back had in hand"
        );
    }
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
    let db = open_at_rest("/db", &options).expect("a new database");
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
    let created = db.checkpoint(Path::new(CHECKPOINT), CheckpointOptions::default());
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

/// What a commit writes behind a pinned version, in
/// [`a_check_of_a_pinned_version_wants_the_reclaimer_held_back_from_the_pin_on`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Behind {
    /// No commit is made.
    Nothing,
    /// One record of a column, in the leaf that holds the flat record of
    /// the Merk below the dropped one.
    Column,
    /// One entry of the subtree directory.
    Directory,
}

/// A database in which a Merk that hosts another, which has a flat record,
/// was dropped by the latest commit, with its model and nothing reclaimed.
fn dropped_just_now() -> (Db, History) {
    dropped_on(SimVfs::new(SimConfig::new(11)))
}

/// [`dropped_just_now`], on the file system `fs`.
pub(super) fn dropped_on(fs: SimVfs) -> (Db, History) {
    let db = open_at_rest("/db", &options(Arc::new(fs))).expect("a new database");
    let history = History::new(1);
    let mut writer = Writer::new(1, 11);
    let mut first = OwnedBatch::default();
    first.subtrees.push(OwnedSubtree::Enter(shared(), None));
    first.subtrees.extend(writer.enters());
    record(&db, &history, &first);
    for _ in 0..2 {
        let generation = writer.generation();
        commit(&db, &history, &mut writer, vec![generation]);
    }
    let dropped = writer.drop_oldest().expect("a generation is entered");
    commit(&db, &history, &mut writer, vec![dropped]);
    (db, history)
}

/// The commit of `behind`.
pub(super) fn commit_behind(db: &Db, history: &History, behind: Behind) {
    let mut batch = OwnedBatch::default();
    match behind {
        Behind::Nothing => return,
        Behind::Column => {
            let unit = Unit::new(Column::Aux, shared());
            let value = Val::Flat(vec![7; 20]);
            batch
                .records
                .push(Write::Put(unit, b"more".to_vec(), value));
        }
        Behind::Directory => {
            let empty = Subtree::new([9; 32]);
            batch.subtrees.push(OwnedSubtree::Enter(empty, None));
        }
    }
    record(db, history, &batch);
}

/// Reclaims everything that was dropped, with a commit now and then: a
/// page that the latest commit wrote is freed after the next one.
fn reclaim_all(db: &Db, history: &History) {
    for round in 1u32..400 {
        if db.reclaim().waiting == 0 {
            return;
        }
        if round.is_multiple_of(4) {
            tick(db, history, round);
        }
    }
    panic!("the drop is not reclaimed: {:?}", db.stats().reclaimed);
}

/// **The physical checker reads a pinned version soundly where no step of
/// the reclaimer was made since the version was pinned as the latest**
/// (D-183). A step of the reclaimer changes the newest version of one
/// page, and a pinned version sees that change only where no later commit
/// wrote the page. So a reclaiming behind a pinned version can show the
/// checker one half of what it took out.
///
/// The state: a Merk that hosts another was dropped by the latest commit.
/// That version is pinned, a commit writes behind it, and the drop is
/// reclaimed.
///
/// - **With the reclaimer not held back**, and a commit that wrote the
///   leaf which holds the flat record of the Merk below: the checker at
///   the pinned version finds that record under a prefix with no entry.
///   The record went from the leaf's newer version, which the pinned
///   version does not read, and the entry from the directory's page, which
///   it does. The pinned version reads as the model has it all the same,
///   and the latest version is sound.
/// - **With the reclaimer held back from the pin to the end of the
///   check**, which a thread that reclaims beside the check waits for: the
///   checker finds nothing, whatever the commit wrote, and the drop is
///   reclaimed afterwards.
#[test]
fn a_check_of_a_pinned_version_wants_the_reclaimer_held_back_from_the_pin_on() {
    let (db, history) = dropped_just_now();
    let pinned = db.snapshot();
    let before = verify(&db, Scope::Reader, pinned.version());
    assert!(before.is_sound(), "{before}");
    commit_behind(&db, &history, Behind::Column);
    reclaim_all(&db, &history);
    let report = verify(&db, Scope::Reader, pinned.version());
    assert!(
        report.findings.len() == 1 && report.has(Rule::FlatRecordWithoutEntry),
        "{report}"
    );
    compare_at(&db, &history, &pinned.operation());
    let latest = db.snapshot();
    let at_latest = verify(&db, Scope::Reader, latest.version());
    assert!(at_latest.is_sound(), "{at_latest}");
    drop((pinned, latest));

    for behind in [Behind::Nothing, Behind::Column, Behind::Directory] {
        let (db, history) = dropped_just_now();
        let held_back = lock(&db.shared.store.reclaiming);
        let pinned = db.snapshot();
        commit_behind(&db, &history, behind);
        thread::scope(|threads| {
            let beside = threads.spawn(|| reclaim_all(&db, &history));
            let report = verify(&db, Scope::Reader, pinned.version());
            assert_eq!(db.stats().reclaimed.merks, 0, "{behind:?}");
            assert!(report.is_sound(), "{behind:?}: {report}");
            drop(held_back);
            beside.join().expect("the drop is reclaimed");
        });
        assert_eq!(db.stats().reclaimed.merks, 1, "{behind:?}");
        compare_at(&db, &history, &pinned.operation());
    }
}

/// **A version that is pinned inside a commit checks sound too, with the
/// reclaimer held back from the pin on.** A commit gives the lock on the
/// counts of values back when it has counted, before it publishes, so a
/// round of the reclaimer can run between the two. It reads at the
/// published version there, and a step on a page whose newest version the
/// commit installed is refused: the page is not as the step read it.
///
/// The state of the test above, with the commit that writes the leaf of
/// the flat record held before its publication. A round of the reclaimer
/// inside it takes nothing out. The version before the commit is pinned
/// there with the reclaimer held back, the commit is published, and the
/// checker finds nothing at the pinned version.
#[test]
fn a_version_pinned_inside_a_commit_checks_sound_with_the_reclaimer_held_back() {
    let (db, history) = dropped_just_now();
    let (arrived, at_publication) = mpsc::channel();
    let (go_on, resumed) = mpsc::channel::<()>();
    let resumed = Mutex::new(resumed);
    (db.shared.store.committing).run_before_publication(move |_| {
        let _ = arrived.send(());
        let _ = lock(&resumed).recv();
    });
    thread::scope(|threads| {
        let (db, history) = (&db, &history);
        // The sender is dropped when this closure is left, which lets the
        // commit that waits at its publication go on. A commit that ends
        // before its publication is seen in the loop below; one that
        // neither ends nor comes to it is waited for with no bound.
        let go_on = go_on;
        let commit = threads.spawn(move || commit_behind(db, history, Behind::Column));
        while at_publication
            .recv_timeout(Duration::from_millis(10))
            .is_err()
        {
            assert!(
                !commit.is_finished(),
                "the commit ended and did not come to its publication"
            );
        }
        let before = db.version();
        // The round is made on a thread of its own: one that finished a
        // reclaiming would ask for the commit lock, which the commit
        // holds here, and wait for it.
        let round = threads.spawn(move || db.reclaim());
        let began = Instant::now();
        while !round.is_finished() {
            assert!(
                began.elapsed() < Duration::from_secs(30),
                "timed out polling for the round of the reclaimer, with the commit held before \
                 its publication"
            );
            thread::sleep(Duration::from_millis(1));
        }
        let round = round.join().expect("a round of the reclaimer");
        let reclaimed = db.stats().reclaimed;
        assert_eq!((round.finished, round.waiting), (0, 1), "{round:?}");
        assert_eq!(
            (reclaimed.flat_records, reclaimed.blocked_by_contention),
            (0, 1),
            "{reclaimed:?}"
        );

        let held_back = lock(&db.shared.store.reclaiming);
        let pinned = db.snapshot();
        assert_eq!(pinned.version(), before);
        go_on.send(()).expect("the commit goes on");
        commit.join().expect("the commit is made");
        assert_eq!(Some(db.version()), before.next());
        let report = verify(db, Scope::Reader, pinned.version());
        assert!(report.is_sound(), "{report}");
        drop(held_back);
        compare_at(db, history, &pinned.operation());
    });
    // The hook holds what waits for the test: it goes before the database.
    (db.shared.store.committing).run_before_publication(|_| {});
    reclaim_all(&db, &history);
    assert_eq!(db.stats().reclaimed.merks, 1);
}

/// A cleaner whose targets are that nothing is dead: a pass over the
/// value log closes a file that is being filled as soon as a byte of it
/// is dead, and cleans the closed file with the largest share of dead
/// bytes, however small that share is.
fn eager_cleaner() -> CleanerConfig {
    CleanerConfig {
        seal_dead_percent: 0,
        value_dead_percent: 0,
        min_segment_len: 1,
        ..cleaner_config()
    }
}

/// How long a step that is run inside a commit is waited for. One that
/// has not ended by then fails the schedule, and the commit is let go, so
/// that a step that waited for the commit lock ends.
const INSIDE: Duration = Duration::from_secs(300);

/// The passes over the value log that one step of a schedule inside a
/// commit makes at most. Each cleans one file at most.
const PASSES: usize = 64;

/// What a schedule inside a commit gave.
#[derive(Debug, Default)]
struct Within {
    /// The value-log file that write-back was filling when the commit was
    /// made: the values of two Merks stood in it.
    filled: Option<FileId>,
    /// How many live values that file lost by the count of the commit,
    /// before its publication: those of the Merk that the commit wrote.
    released: u64,
    /// How many it kept then: those of the other Merk.
    others: u64,
    /// How many values write-back placed inside the commit. They are
    /// values of the commit's own pages: a turn before the commit found
    /// nothing to place.
    placed: u64,
    /// How many live values that file gained by those turns of
    /// write-back.
    placed_there: u64,
    /// The files that the passes over the value log cleaned inside the
    /// commit, each with what its cleaning did.
    cleaned: Vec<(FileId, Cleaned)>,
    /// The blocks that the passes over the segments moved inside the
    /// commit, and the table checkpoints that they had installed there.
    blocks_moved: u64,
    checkpoints: u64,
    /// The files that were taken out of the store while the version
    /// before the commit was pinned, inside the commit and after it.
    dropped_under_the_pin: Vec<FileId>,
    /// Whether that file was taken out once nothing pinned the version.
    dropped_at_the_end: bool,
    /// What each step inside the commit did, in its own words.
    did: Vec<String>,
    /// Everything that is no sound end of the schedule.
    errors: Vec<String>,
}

impl Within {
    /// Asserts that the commit released values of the file that
    /// write-back was filling, that the passes inside the commit emptied
    /// the file by moving every value that was live in it then, and that
    /// the file stayed while the version before the commit was pinned and
    /// went afterwards.
    fn emptied_and_kept(&self) {
        let filled = self.filled.expect("a file");
        assert!(self.released > 0 && self.others > 0, "{self:?}");
        let cleaned = (self.cleaned.iter()).find(|(file, _)| *file == filled);
        let (_, cleaning) = cleaned.unwrap_or_else(|| panic!("{filled} is not cleaned: {self:?}"));
        assert_eq!(
            (cleaning.moved, cleaning.live),
            (self.others + self.placed, 0),
            "the passes inside the commit left values of {filled} where they were: {self:?}"
        );
        assert!(
            !self.dropped_under_the_pin.contains(&filled),
            "{filled} was taken out under the pin: {self:?}"
        );
        assert!(self.dropped_at_the_end, "{filled} stayed: {self:?}");
    }
}

/// The live values of each value-log file that holds one.
fn live_values(db: &Db) -> BTreeMap<FileId, u64> {
    let counts = db.shared.store.values.counts();
    (counts.iter())
        .filter(|count| count.values > 0)
        .map(|count| (count.file, count.values))
        .collect()
}

/// The live values of the value-log file `file`.
fn live_in(db: &Db, file: FileId) -> u64 {
    live_values(db).get(&file).copied().unwrap_or(0)
}

/// What one step of a schedule inside a commit did.
#[derive(Debug, Default)]
struct Did {
    /// In the step's own words.
    told: String,
    /// Its passes over the value log.
    values: Vec<ValuePass>,
    /// The blocks that its pass over the segments moved, and the table
    /// checkpoints that the pass had installed.
    segments: (u64, u64),
}

/// One step of the background work on the calling thread, with no pace:
/// a turn of write-back, a pass of the cleaner over the segments, or
/// passes of the cleaner over the value log until one neither moves a
/// value nor takes a file out.
fn step_at_once(db: &Db, step: Step, cleaner_state: &CleanerState) -> Result<Did, String> {
    let write_back = db.shared.write_back();
    let cleaner = Cleaner::new(write_back, cleaner_state);
    let mut did = Did::default();
    match step {
        Step::WriteBack => {
            let tick = write_back.tick();
            let tick = tick.map_err(|error| format!("write-back: {error}"))?;
            did.told = format!("{tick:?}");
        }
        Step::CleanSegments => {
            let pass = cleaner.pass();
            let pass =
                pass.map_err(|error| format!("the cleaner's pass over the segments: {error}"))?;
            did.told = format!("{pass:?}");
            did.segments = (pass.blocks_moved, pass.checkpoints);
        }
        Step::CleanValues => loop {
            let pass = value_pass(db, &cleaner, &db.shared.steps())?;
            let progressed = pass.progressed();
            did.values.push(pass);
            if !progressed {
                did.told = format!("{:?}", did.values);
                break;
            }
            if did.values.len() == PASSES {
                return Err(format!(
                    "the value log is not cleaned in {PASSES} passes: {:?}",
                    did.values
                ));
            }
        },
        other => unreachable!("{other:?} is no step that is run inside a commit"),
    }
    Ok(did)
}

/// **One schedule of the second kind: background steps inside a commit.**
/// A commit is held between its count and its publication
/// (`CommitState::run_before_publication`): its pages are installed, the
/// counts of the value log have what it released, and the published
/// version is the one before it. `steps` run there, one after another,
/// each to its end. Then the commit is published, the background work
/// has its turns, and the store is brought to rest and judged as after a
/// schedule of the first kind. A snapshot pins the version before the
/// commit from before it to after those turns, and is compared with the
/// model of that version then.
///
/// **The state is made so that the passes over the value log meet the
/// commit's pages.** A commit writes every document of two Merks again,
/// and a turn of write-back places the values that it brought: they
/// stand in one file, and a second turn finds nothing to place. The
/// commit that is held writes every document of the first of the two
/// again. It releases values of that file, in which those of the other
/// Merk stay live, and it brings pending values on pages of its own. So
/// inside the commit
///
/// - a turn of write-back places values in pages that are installed and
///   not published, in that file, and counts them at once;
/// - the passes over the value log, of a cleaner whose targets are that
///   nothing is dead ([`eager_cleaner`]), close that file and clean it.
///   They move the values of the other Merk and those that the turn
///   placed, whose owners the published version does not have, and so
///   they empty a file that the commit has counted against while a
///   snapshot still reads the cells that the commit changed.
///
/// What was reached is returned and asserted by the caller, with what
/// went wrong.
fn inside_a_commit(steps: &[Step]) -> Within {
    let base = base(23);
    let vfs: Arc<dyn Vfs> = Arc::new(base.fs.crash(0));
    let db = open_at_rest("/db", &options(vfs)).expect("the state is opened");
    let history = base.world.history.copy();
    let mut writers = base.world.writers.clone();
    assert_eq!(db.version().get(), history.version());
    let mut within = Within::default();
    let cleaner_state = CleanerState::new(eager_cleaner());
    let at_once = |step: Step| step_at_once(&db, step, &cleaner_state);

    // The state. No reader is left from before: the snapshot of this
    // schedule is the oldest pin there is.
    drop(prelude(&db, &history, &mut writers));
    at_once(Step::WriteBack).expect("a turn of write-back of the state");
    at_once(Step::CleanValues).expect("the passes of the state");
    let before = live_values(&db);
    let both = vec![writers[0].rewritten(0), writers[0].rewritten(1)];
    commit(&db, &history, &mut writers[0], both);
    at_once(Step::WriteBack).expect("a turn of write-back of the state");
    let after = live_values(&db);
    let gained: Vec<FileId> = (after.iter())
        .filter(|(file, live)| before.get(*file).copied().unwrap_or(0) < **live)
        .map(|(file, _)| *file)
        .collect();
    let [filled] = gained[..] else {
        panic!("write-back placed the values of the two Merks in {gained:?}");
    };
    within.filled = Some(filled);
    // Nothing is pending: one more turn places nothing.
    let placed = || db.shared.write_back().state().stats().values_placed;
    let placed_by_then = placed();
    at_once(Step::WriteBack).expect("a turn of write-back of the state");
    assert_eq!(placed(), placed_by_then, "a value was pending still");

    let reader = db.snapshot();
    let before_the_commit = reader.version();
    let live = live_in(&db, filled);
    let (arrived, at_publication) = mpsc::channel();
    let (go_on, resumed) = mpsc::channel::<()>();
    let resumed = Mutex::new(resumed);
    (db.shared.store.committing).run_before_publication(move |_| {
        let _ = arrived.send(());
        let _ = lock(&resumed).recv();
    });
    thread::scope(|threads| {
        let (db, history, cleaner_state) = (&db, &history, &cleaner_state);
        let writer = &mut writers[0];
        // The sender is dropped when this closure is left, which lets a
        // commit that waits at its publication go on.
        let go_on = go_on;
        let held = threads.spawn(move || {
            let written = writer.rewritten(0);
            commit(db, history, writer, vec![written])
        });
        while at_publication
            .recv_timeout(Duration::from_millis(10))
            .is_err()
        {
            assert!(
                !held.is_finished(),
                "the commit ended and did not come to its publication"
            );
        }
        assert_eq!(db.version(), before_the_commit, "the commit is published");
        within.others = live_in(db, filled);
        within.released = live - within.others;

        for &step in steps {
            let (placed_before, there_before) = (placed(), live_in(db, filled));
            // On a thread of its own, so that a step that does not end
            // fails the schedule and does not hold the test.
            let made = threads.spawn(move || step_at_once(db, step, cleaner_state));
            let began = Instant::now();
            while !made.is_finished() && began.elapsed() < INSIDE {
                thread::sleep(Duration::from_millis(1));
            }
            if !made.is_finished() {
                (within.errors).push(format!("{step:?} did not end inside the commit"));
                let _ = go_on.send(());
            }
            match made.join().expect("a step inside the commit") {
                Ok(did) => {
                    within.did.push(did.told);
                    within.blocks_moved += did.segments.0;
                    within.checkpoints += did.segments.1;
                    for pass in did.values {
                        within.cleaned.extend(pass.cleaned);
                        within.dropped_under_the_pin.extend(pass.dropped);
                    }
                }
                Err(error) => within.errors.push(format!("inside the commit: {error}")),
            }
            if step == Step::WriteBack {
                within.placed += placed() - placed_before;
                within.placed_there += live_in(db, filled) - there_before;
            }
        }
        if db.version() != before_the_commit {
            (within.errors).push("the commit was published before the steps ended".to_string());
        }
        let _ = go_on.send(());
        held.join().expect("the commit is made");
    });
    // The hook holds what waited for the test: it goes before a later
    // commit comes to it.
    (db.shared.store.committing).run_before_publication(|_| {});
    assert_eq!(Some(db.version()), before_the_commit.next());

    // The background work has its turns under the snapshot, which then
    // reads all of its version.
    for _ in 0..3 {
        for step in [Step::WriteBack, Step::CleanSegments, Step::CleanValues] {
            match at_once(step) {
                Ok(did) => {
                    let dropped = did.values.into_iter().flat_map(|pass| pass.dropped);
                    within.dropped_under_the_pin.extend(dropped);
                }
                Err(error) => within.errors.push(format!("after the commit: {error}")),
            }
        }
    }
    let read = catch_unwind(AssertUnwindSafe(|| {
        compare_at(&db, &history, &reader.operation())
    }));
    if read.is_err() {
        within.errors.push(format!(
            "the snapshot of version {before_the_commit} does not read its version"
        ));
    }
    drop(reader);
    for round in 0..6 {
        match at_once(Step::CleanValues) {
            Ok(did) => {
                let mut dropped = did.values.iter().flat_map(|pass| &pass.dropped);
                within.dropped_at_the_end |= dropped.any(|file| *file == filled);
            }
            Err(error) => within.errors.push(format!("at the end: {error}")),
        }
        tick(&db, &history, 7_000 + round);
    }

    // To rest, and the checker decides.
    let mut errors = std::mem::take(&mut within.errors);
    settle(&db, &history, &mut errors);
    if let Err(error) = db.shared.write_back().pass() {
        errors.push(format!("a pass of write-back at the end: {error}"));
    }
    let stats = db.stats();
    let background = &stats.background;
    if background.write_back_errors + background.reclaim_errors + stats.reclaimed.given_up > 0 {
        errors.push(format!("{background:?}, {:?}", stats.reclaimed));
    }
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
    close(db, &cleaner_state, &mut errors);
    within.errors = errors;
    println!("{steps:?} inside a commit: {within:#?}");
    within
}

/// A turn of write-back inside a commit places values of the commit's
/// pages, which are installed and not published.
#[test]
fn a_turn_of_write_back_inside_a_commit_places_values_of_its_pages() {
    let within = inside_a_commit(&[Step::WriteBack]);
    assert!(within.errors.is_empty(), "{:#?}", within.errors);
    assert!(
        within.placed > 0 && within.placed_there == within.placed,
        "{within:?}"
    );
}

/// Passes over the value log inside a commit empty a file that the
/// commit has counted against. The file is not taken out of the store
/// while a snapshot pins the version before the commit, whose cells
/// still point into it, and is taken out afterwards
/// (`value-log-format.md` O8).
#[test]
fn passes_over_the_value_log_inside_a_commit_keep_the_file_they_empty_for_the_version_before() {
    let within = inside_a_commit(&[Step::CleanValues]);
    assert!(within.errors.is_empty(), "{:#?}", within.errors);
    within.emptied_and_kept();
}

/// A turn of write-back and passes over the value log inside a commit:
/// the passes move the values that the turn has just placed in pages of
/// the commit, whose owners the published version does not have
/// (`transactions.md` PV7), and empty the file as in the test above.
#[test]
fn passes_over_the_value_log_inside_a_commit_find_the_owners_of_values_placed_in_its_pages() {
    let within = inside_a_commit(&[Step::WriteBack, Step::CleanValues]);
    assert!(within.errors.is_empty(), "{:#?}", within.errors);
    assert!(
        within.placed > 0 && within.placed_there == within.placed,
        "{within:?}"
    );
    within.emptied_and_kept();
}

/// A pass over the segments inside a commit, between a turn of
/// write-back and passes over the value log: it moves a block and has a
/// table checkpoint installed there, and the passes after it do what
/// they do in the test above.
#[test]
fn a_pass_over_the_segments_inside_a_commit_leaves_a_sound_store() {
    let within = inside_a_commit(&[Step::WriteBack, Step::CleanSegments, Step::CleanValues]);
    assert!(within.errors.is_empty(), "{:#?}", within.errors);
    assert!(
        within.placed > 0 && within.placed_there == within.placed,
        "{within:?}"
    );
    assert!(
        within.blocks_moved > 0 && within.checkpoints > 0,
        "{within:?}"
    );
    within.emptied_and_kept();
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
    an_open_of_a_checkpoint_against_commits_that_merge_the_leaves_of_the_directory: OpenCheckpoint, Merge;
    an_open_of_a_checkpoint_against_commits_that_split_the_leaves_of_the_directory: OpenCheckpoint, Split;
    an_open_of_a_checkpoint_against_blocks: OpenCheckpoint, Blocks;
    a_turn_of_write_back_against_a_commit_that_empties_pages_it_has_not_written: WriteBack, Empties;
}
