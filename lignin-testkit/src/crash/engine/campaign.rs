//! The campaign: runs of random workloads on the engine, each crashed at
//! every write boundary.
//!
//! A **run** is everything one seed gives: a grove version, streams of
//! GroveDB operations ([`Plan`]), the faults it injects, and a tree of
//! **lineages**. A lineage is one database and what it goes through, one
//! **session** after another: it is opened with its three threads, takes
//! the steps of its workload ([`Step`]), is closed or killed, and is opened
//! again. Beside each session a second database, which is never crashed,
//! takes the same operations and says what each block publishes
//! ([`super`], "What a committed block is").
//!
//! The steps of a session come in **bursts**. During a burst the three
//! threads of the database work beside the workload, and the [`Watcher`]
//! takes a crash image after every change that any of the four threads
//! makes to the files. After a burst the watcher is paused, so the
//! background threads wait at their next change, and the images are gone
//! through: each gives the states its crash plans leave, each state that
//! was not seen before is recovered and held to the model
//! ([`check_state`]), and a few are kept. Each state that was kept begins a
//! lineage of its own when its lineage has ended: the database that the
//! crash left goes on, and is crashed again.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, PoisonError,
    },
    time::{Duration, Instant},
};

use grovedb::engine::lignin::LigninEngine;
use grovedb_version::version::GroveVersion;
use lignin_format::PageSize;
use lignin_io::{
    sim::{CrashImage, CrashPlan, Fault, FaultRule, OpSet, SimConfig, SimVfs},
    Create, EntryKind, OpenMode, Vfs,
};
use lignin_store::wal::SyncPolicy;

use super::{
    check::{
        behind_in_a_flush, check_again, check_state, not_the_block, Held, Recovered, Rules, Wrong,
    },
    grove::{self, checkpoint_dir, db_dir, Grove, Tuning, BESIDE_KEY, CHECKPOINTS},
    model::{
        Block, Checkpoint, Doing, Expect, Hash, History, Landing, Made, Places, Position, Standing,
        State,
    },
    observe::{engine_panics, Actor, Taken, Watcher},
    workload::{self, Plan, Situation, Step, Steps},
};
use crate::{
    crash::{
        campaign::{plans_of, unknown_of_seed, Faults, Tally},
        check::{no_panic, panic_text, Kind, Violation},
        observe::Sabotage,
    },
    golden::run::STACK_BYTES,
    workload::{apply::Outcome as Applied, Op, Rng, TransactionOp, Transactions},
};

/// A defect planted for a run that must find it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Planted {
    /// The file layer under the database reports a success it did not
    /// deliver.
    File(Sabotage),
    /// The database that is not crashed does not take the first write of
    /// each block: what the campaign holds a block to is wrong by one
    /// operation.
    ForgottenWrite,
    /// A wipe is made as two commits: a write with no transaction, and the
    /// wipe after it. The model is told of one block, the empty grove. A
    /// crash between the two leaves a database that is neither as it was
    /// nor empty.
    SplitWipe,
    /// A thread with the name of the engine's cleaner panics while a
    /// session is open, once in the run. It is not among the defects of
    /// `lignin-crash engine planted`: the panics of the engine's threads
    /// are counted for the process ([`engine_panics`]), so this one is
    /// planted where no other campaign runs beside
    /// (`tests/engine_thread_panic.rs`).
    ThreadPanic,
}

/// What a lineage does between its streams' operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Script {
    /// The steps are drawn from the seed ([`Steps::next`]).
    Drawn,
    /// The lineage wipes its database again and again ([`WIPES`]).
    Wipes,
    /// The lineage takes these steps, again and again: none of them is
    /// drawn.
    Listed(&'static [Step]),
}

/// How one run goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    /// How many steps a lineage has.
    pub ops: usize,
    /// The most operations taken of one stream.
    pub stream_ops: usize,
    /// The most lineages of one run: the first, and those that go on from
    /// a state a crash left.
    pub lineages: usize,
    /// How many crashes deep a lineage can be: 0 for the first.
    pub depth: usize,
    /// How many of the states of a lineage begin a lineage of their own.
    pub kept: usize,
    /// The most steps of a burst: the steps between two pauses in which
    /// the crash images are gone through.
    pub burst: u64,
    /// Every plan of an image is taken if there are at most this many.
    pub every_plan_limit: usize,
    /// An image with more plans gets the two ends, the plans of a file
    /// system that writes in order with every tear of the first lost
    /// write, each change lost alone, each change applied late, and this
    /// many plans drawn from the seed.
    pub samples: u64,
    /// The most plans taken of one image, or 0 for all of them
    /// ([`chosen`]). The two ends are taken whatever it says.
    pub max_plans: usize,
    /// One state in this many goes on after it was recovered
    /// ([`check_again`]). 0 for none.
    pub again_one_in: u64,
    /// The faults of the run, or `None` for those of its seed
    /// ([`Faults::of_seed`]).
    pub faults: Option<Faults>,
    /// What a lineage does between the operations of its streams.
    pub script: Script,
    /// A defect to find.
    pub planted: Option<Planted>,
}

impl Default for Shape {
    fn default() -> Self {
        Self {
            ops: 60,
            stream_ops: 60,
            lineages: 4,
            depth: 2,
            kept: 2,
            burst: 6,
            every_plan_limit: 48,
            samples: 3,
            max_plans: 24,
            again_one_in: 16,
            faults: None,
            script: Script::Drawn,
            planted: None,
        }
    }
}

/// A violation, with where in its run it was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The seed of the run.
    pub seed: u64,
    /// The lineage of the run, from 0.
    pub lineage: u64,
    /// How many steps the lineage had made.
    pub op: usize,
    /// The session of the lineage, from 1, and the write boundary of the
    /// crash in the file system of the lineage, if a crash state was at
    /// fault.
    pub boundary: Option<(u64, u64)>,
    /// The number of the image that the state is of, among those taken of
    /// the lineage's file system ([`Taken::number`]).
    pub image: Option<u64>,
    /// The version that the database of the state opened at, where what
    /// was found is of the database and not of a checkpoint of it.
    pub version: Option<u64>,
    /// The number of the block that was open, among the blocks that the
    /// lineage began: a block that is made again after its commit was
    /// refused, after a fault or in another session has another number,
    /// and one that stays open over a wipe keeps its own.
    pub attempt: u64,
    /// Whether the state is the one of its image in which every unsynced
    /// change is kept ([`leaves_all`]): what a process leaves that is
    /// killed, in which no sync shows that was not made.
    pub whole: bool,
    /// Whether what was found is one of these differences between the
    /// database and the one that is never crashed: an operation of a
    /// stream that one made and the other refused; the root hash before a
    /// commit; a commit that one of the two refused for a conflict and the
    /// other did not; a commit that the database answered `Ok` to and the
    /// other failed; a commit that left the two at different versions.
    /// What is found of a step beside the stream (a write beside the
    /// block, a wipe, a flush) is not among them.
    pub differs: bool,
    /// The crash plan that left the state.
    pub plan: Option<String>,
    /// What was expected of the database when the image was taken, and the
    /// thread that made the change.
    pub at: Option<String>,
    /// What the workload was doing when the image was taken.
    pub doing: Option<Doing>,
    /// What was found.
    pub violation: Violation,
    /// Whether what was found is that a checkpoint is not as the call left
    /// it that returned: one that was made holds no database, or one that
    /// was deleted is there.
    pub checkpoint: bool,
    /// The last steps of the lineage, the newest last.
    pub trail: Vec<String>,
    /// The directory the files of the state were written to.
    pub saved: Option<PathBuf>,
    /// Why the files of the state were not written to the directory that
    /// was given for them.
    pub not_saved: Option<String>,
}

impl Found {
    /// A violation found in lineage `lineage` of the run of `seed` when
    /// the lineage had made `op` steps, of no crash state and with nothing
    /// more said of it.
    pub fn of(seed: u64, lineage: u64, op: usize, violation: Violation) -> Self {
        Self {
            seed,
            lineage,
            op,
            boundary: None,
            image: None,
            version: None,
            attempt: 0,
            whole: false,
            differs: false,
            plan: None,
            at: None,
            doing: None,
            violation,
            checkpoint: false,
            trail: Vec::new(),
            saved: None,
            not_saved: None,
        }
    }
}

impl fmt::Display for Found {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "seed {}, lineage {}, after step {}",
            self.seed, self.lineage, self.op
        )?;
        if let Some((session, boundary)) = self.boundary {
            write!(
                f,
                ", a crash at write boundary {boundary} of session {session}"
            )?;
        }
        writeln!(f, ": {}", self.violation)?;
        if let Some(at) = &self.at {
            writeln!(f, "  when the image was taken: {at}")?;
        }
        if let Some(plan) = &self.plan {
            writeln!(f, "  the crash kept: {plan}")?;
        }
        if let Some(saved) = &self.saved {
            writeln!(f, "  the files of the state: {}", saved.display())?;
        }
        if let Some(why) = &self.not_saved {
            writeln!(f, "  the files of the state were not written: {why}")?;
        }
        write!(f, "  the last steps: {}", self.trail.join(", "))
    }
}

/// What one run came to.
#[derive(Clone, Debug, Default)]
pub struct RunReport {
    /// The seed.
    pub seed: u64,
    /// What the seed gave the run: its grove version, the size of its
    /// pages, its faults, and the workloads of the streams it took
    /// operations from.
    pub recipe: String,
    /// The counts.
    pub tally: Tally,
    /// The fingerprint of every state a crash of the run left that was
    /// recovered: each once.
    pub fingerprints: Vec<u64>,
    /// What was found.
    pub violations: Vec<Found>,
    /// A number that two runs of one seed have in common where the run
    /// injects no fault and no violation was found: the blocks of the
    /// first lineage, each with its version, its root hash and its
    /// records. What the later lineages commit is not in it: they begin
    /// with states that a crash left, and which states the crashes of a
    /// run leave depends on where the changes of the background threads
    /// fell. Nor need it be the same twice in a run with faults, where a
    /// fault fails whichever thread makes the next call of its kind.
    pub digest: u64,
    /// Where the planted defect of the run did what it is planted to do
    /// ([`Acted`]): a lie of the file layer once in a lineage, the first;
    /// a write that was kept from the model once for each time a block
    /// was made; a wipe in two commits once for each such wipe; a thread
    /// that panicked once in the run.
    pub acts: Vec<Act>,
}

/// A place in a run at which its planted defect did what it is planted to
/// do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Act {
    /// The seed of the run.
    pub seed: u64,
    /// The lineage, from 0.
    pub lineage: u64,
    /// What it did.
    pub what: Acted,
}

/// What a planted defect did, with what a violation has to be to fit
/// that act ([`Act::shows_in`]). That a violation fits is evidence that
/// the defect made it, and no proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Acted {
    /// The file layer answered a sync that it did not make, for the first
    /// time in the lineage, when this many images of the lineage's file
    /// system had been taken. A violation fits it that is of a crash
    /// state of a later image, other than the state of that image in
    /// which every unsynced change is kept ([`Found::whole`]).
    Lied {
        /// How many images had been taken.
        images: u64,
    },
    /// A write of this block of the lineage was kept from the database
    /// that is never crashed ([`Found::attempt`]). A violation fits it
    /// that is one of the differences between the two databases
    /// ([`Found::differs`]) while that block is open.
    Forgot {
        /// The number of the block.
        attempt: u64,
    },
    /// A wipe was made as two commits, of which the first made this
    /// version. A violation fits it that is of a crash state, of an image
    /// taken during a wipe, whose database is at that version.
    Split {
        /// The version that the write before the wipe made.
        version: u64,
    },
    /// A thread with the name of one of the engine's panicked, when the
    /// lineage had made this many steps. A violation fits it that is a
    /// panic reported of a session at or after that step.
    Panicked {
        /// How many steps the lineage had made.
        op: usize,
    },
}

impl Act {
    /// Whether `found` fits this act: in the same lineage, and as
    /// [`Acted`] says for it.
    pub fn shows_in(&self, found: &Found) -> bool {
        (self.seed, self.lineage) == (found.seed, found.lineage)
            && match self.what {
                Acted::Lied { images } => {
                    !found.whole && found.image.is_some_and(|image| image > images)
                }
                Acted::Forgot { attempt } => {
                    found.boundary.is_none() && found.differs && found.attempt == attempt
                }
                Acted::Split { version } => {
                    found.image.is_some()
                        && found.doing == Some(Doing::Wipe)
                        && found.version == Some(version)
                }
                Acted::Panicked { op } => {
                    found.boundary.is_none()
                        && found.violation.kind == Kind::Panicked
                        && op <= found.op
                }
            }
    }
}

/// What is remembered of a state that was recovered.
#[derive(Clone, Debug)]
struct Seen {
    version: u64,
    /// What the database held.
    state: Arc<State>,
    /// How many subtrees the lineage had reached when the state was
    /// walked.
    places: usize,
    checkpoints: Vec<(PathBuf, Held)>,
}

/// A state that was recovered and will begin a lineage of its own.
struct Kept {
    image: CrashImage,
    plan: CrashPlan,
    history: History,
    checkpoints: Vec<Checkpoint>,
    next_checkpoint: u64,
    places: Places,
    depth: usize,
}

/// One file system, and the database on it from session to session.
struct Lineage {
    index: u64,
    depth: usize,
    watcher: Watcher,
    history: History,
    /// The subtrees that the walks of the lineage have reached.
    places: Places,
    steps: Steps,
    script: usize,
    /// What the campaign decides by chance for this lineage, apart from
    /// its steps: which states are kept.
    chance: Rng,
    /// Where the stream goes on, and where the block that is open began.
    position: Position,
    block_start: Position,
    /// The number of the block that is open, among those the lineage
    /// began: the next number when a commit of the block is done with,
    /// when the block is rolled back, abandoned or given up, and when a
    /// session is opened.
    attempt: u64,
    /// How often the block that is open was refused.
    refused: u32,
    checkpoints: Vec<Checkpoint>,
    next_checkpoint: u64,
    beside: u64,
    /// How many faults had been injected into the file system of the
    /// session when its open began.
    faults_before: u64,
    /// The version that the durable part of an image of this file system
    /// last recovered to: no state of a later crash is below it.
    durable: u64,
    /// Whether the last session was killed: the database is opened once
    /// more, whatever steps the lineage has left.
    killed: bool,
    /// Whether the place is noted at which the planted defect of the file
    /// layer first acted.
    acted: bool,
    seen: HashMap<u64, Seen>,
    /// What the states that were recovered held, each kept once.
    held: HashMap<(Hash, Hash, bool), Arc<State>>,
    candidates: u64,
    kept: Vec<Kept>,
    ops: usize,
    sessions: u64,
    trail: Vec<String>,
}

impl Lineage {
    /// The block `attempt` is published: it is the last block from here
    /// on, and durable if the commit that made it synced the log. What a
    /// crash leaves from this moment on is held to that.
    fn landed(&mut self, attempt: Arc<Block>, durable: bool) {
        self.history.push(attempt);
        let latest = self.history.latest();
        self.watcher.acknowledge(|expect| {
            expect.attempt = None;
            expect.latest = latest;
            if durable {
                expect.floor = latest;
            }
            expect.doing = Doing::Nothing;
        });
    }

    /// Nothing is being made any more: what was being made is not in the
    /// log, and no crash from this moment on leaves the database at it.
    fn idle(&self) {
        self.watcher.acknowledge(|expect| {
            expect.attempt = None;
            expect.doing = Doing::Nothing;
        });
    }

    /// The block that was being made may be in the log or not, and the
    /// session that could say has ended: the next open says.
    fn in_doubt(&self) {
        self.watcher
            .expecting(|expect| expect.doing = Doing::Nothing);
    }

    /// Whether a fault was injected into the file system of the session
    /// since it was opened.
    fn faulted(&self) -> bool {
        self.watcher.fs().stats().faults > self.faults_before
    }
}

/// Lets the threads of a database go on if the campaign panics while it
/// holds them at the watcher: the database that the panic drops waits for
/// its threads to end.
struct ResumeOnPanic(Watcher);

impl Drop for ResumeOnPanic {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.resume();
        }
    }
}

/// One session of a lineage: the database, the one that is not crashed,
/// and the transactions the stream runs in on each.
struct Session<'s> {
    db: &'s Grove,
    twin: &'s Grove,
    /// The file system of the database that is not crashed.
    twin_files: &'s SimVfs,
    live: Transactions<'s, LigninEngine>,
    shadow: Transactions<'s, LigninEngine>,
    /// Whether the block that is open has written.
    dirty: bool,
    /// Whether the planted defect has kept a write of this block from the
    /// database that is not crashed.
    forgotten: bool,
    tuning: Tuning,
}

impl Session<'_> {
    /// Whether a commit of this session syncs the log: a block is durable
    /// once its commit has returned `Ok`.
    fn syncs(&self) -> bool {
        self.tuning.sync == SyncPolicy::EveryCommit
    }

    /// Whether the session takes no write any more.
    fn ended(&self) -> bool {
        self.db.engine().database().reopen().is_some()
    }
}

/// How a session goes on after a step.
enum Flow {
    On,
    /// The database is closed and opened again.
    Close,
    /// The process is killed, and the database is opened again.
    Kill,
    /// The lineage ends: something was found that leaves nothing to go on
    /// with.
    End,
}

/// What became of an operation that is one commit.
enum Outcome {
    /// It is published: the database is at its version.
    Published,
    /// It is not in the log, and the session goes on.
    Nothing,
    /// It is not in the log, and the session has ended: the call changed no
    /// file.
    Refused,
    /// The session has ended, and whether the log holds the commit is for
    /// the next open to say.
    InDoubt,
}

/// Whether the state that `plan` leaves of `image` is the one in which
/// every unsynced change is kept: the two have one fingerprint
/// ([`SimVfs::fingerprint`]), by which the campaign tells one state from
/// another everywhere.
pub fn leaves_all(image: &CrashImage, plan: &CrashPlan) -> bool {
    let of = |plan: &CrashPlan| image.recover(plan).map(|state| state.fingerprint());
    matches!((of(plan), of(&image.keep_all())), (Ok(left), Ok(all)) if left == all)
}

/// The count of how often the planted defect of a run did what it is
/// planted to do: no violation of a run where it never did fits it.
pub const ACTED: &str = "times the planted defect acted";

/// The counts by which a campaign says what the crashes during a wipe
/// left, of every crash plan and not only of the states that were new: as
/// the database was before the wipe, at a block before the wipe that was
/// not durable, and empty. They are of the wipes of a grove that holds
/// something: of an empty one a crash leaves an empty one whatever it
/// keeps, which shows nothing.
pub const WIPE_UNCHANGED: &str =
    "crash plans of a crash during a wipe that leave the database as it was";
/// See [`WIPE_UNCHANGED`].
pub const WIPE_EARLIER: &str =
    "crash plans of a crash during a wipe that leave a block before it that was not durable";
/// See [`WIPE_UNCHANGED`].
pub const WIPE_EMPTY: &str = "crash plans of a crash during a wipe that leave the database empty";

/// The count of the value hashes that the engine's checker named as left
/// by a delete, in the checks of a run on a grove version that leaves
/// them (`grove::leaves_hashes`): one for each check that finds a
/// database sound and names one in it, so a hash that stays is counted
/// in every state it is recovered in. They are no violations.
pub const LEFT_HASHES: &str = "value hashes a delete left, as the checks named them";

/// The most violations a run reports: one defect shows in many states.
const MAX_VIOLATIONS: usize = 4;

/// How many steps the trail of a violation shows.
const TRAIL: usize = 24;

const CHANCE: u64 = 0x7665_7230_3420_6b70;
const TWIN: u64 = 0x7665_7230_3420_7477;

/// One run: its seed, its shape, and what it has come to so far.
struct Run {
    seed: u64,
    shape: Shape,
    faults: Faults,
    plan: Plan,
    page_size: PageSize,
    /// Where the states of violations are written, if anywhere.
    save: Option<PathBuf>,
    /// What an empty grove holds: the state of a database that was just
    /// made.
    empty: Option<State>,
    tally: Tally,
    fingerprints: HashSet<u64>,
    order: Vec<u64>,
    violations: Vec<Found>,
    digest: u64,
    waiting: Vec<Kept>,
    lineages: u64,
    /// How many streams of the plan the lineages took operations from.
    streams: usize,
    /// Where the planted defect acted.
    acts: Vec<Act>,
}

impl Run {
    fn new(seed: u64, shape: Shape, save: Option<PathBuf>) -> Self {
        Self {
            seed,
            shape,
            faults: shape.faults.unwrap_or(Faults::of_seed(seed)),
            plan: Plan::of_seed(seed, shape.stream_ops),
            // Small pages, so that a grove of a few hundred nodes has many
            // of them; one run in eight has the engine's own size.
            page_size: match seed % 8 {
                7 => PageSize::DEFAULT,
                _ => PageSize::MIN,
            },
            save,
            empty: None,
            tally: Tally::default(),
            fingerprints: HashSet::new(),
            order: Vec::new(),
            violations: Vec::new(),
            digest: 0,
            waiting: Vec::new(),
            lineages: 0,
            streams: 0,
            acts: Vec::new(),
        }
    }

    /// What the seed gave the run, in a line.
    fn recipe(&mut self) -> String {
        let workloads: Vec<&str> = (0..self.streams)
            .map(|index| self.plan.stream(index).workload)
            .collect();
        format!(
            "grove version {}, pages of {} bytes, faults {}, streams {}",
            self.plan.grove_version.protocol_version,
            self.page_size.get(),
            self.faults.name(),
            workloads.join(" ")
        )
    }

    fn done(&self) -> bool {
        self.violations.len() >= MAX_VIOLATIONS
    }

    fn sabotage(&self) -> Option<Sabotage> {
        match self.shape.planted {
            Some(Planted::File(sabotage)) => Some(sabotage),
            _ => None,
        }
    }

    fn found(
        &mut self,
        lineage: &Lineage,
        wrong: impl Into<Wrong>,
        crash: Option<(&Taken, &CrashPlan)>,
    ) {
        self.note(lineage, wrong.into(), crash, false);
    }

    /// Notes a violation. `differs` says whether it is that the database
    /// answered other than the one that is never crashed.
    fn note(
        &mut self,
        lineage: &Lineage,
        wrong: Wrong,
        crash: Option<(&Taken, &CrashPlan)>,
        differs: bool,
    ) {
        self.tally.count("violations", 1);
        if self.done() {
            return;
        }
        let at = |taken: &Taken| {
            let when = match taken.acknowledged {
                true => "at no change, after one",
                false => "at a change",
            };
            format!("{}, {when} by {}", taken.expect, taken.actor.name())
        };
        let mut found = Found {
            boundary: crash.map(|(taken, _)| (lineage.sessions, taken.boundary)),
            image: crash.map(|(taken, _)| taken.number),
            version: wrong.version,
            attempt: lineage.attempt,
            whole: crash.is_some_and(|(taken, plan)| leaves_all(&taken.image, plan)),
            differs,
            plan: crash.map(|(_, plan)| format!("{plan:?}")),
            at: crash.map(|(taken, _)| at(taken)),
            doing: crash.map(|(taken, _)| taken.expect.doing),
            checkpoint: wrong.checkpoint,
            trail: lineage.trail.clone(),
            ..Found::of(self.seed, lineage.index, lineage.ops, wrong.violation)
        };
        // The files of the state, with what is said of it here.
        if let (Some(to), Some((taken, plan))) = (&self.save, crash) {
            let named = format!(
                "seed-{}-lineage-{}-session-{}-boundary-{}-violation-{}",
                self.seed,
                lineage.index,
                lineage.sessions,
                taken.boundary,
                self.violations.len()
            );
            let dir = to.join(named);
            let state = (taken.image.recover(plan)).expect("the plan was recovered from before");
            // What is said of the state names the directory it is in.
            let said = |dir: &Path| {
                let saved = Found {
                    saved: Some(dir.to_path_buf()),
                    ..found.clone()
                };
                format!("{saved}\n")
            };
            match save(&state, &dir, said) {
                Ok(dir) => found.saved = Some(dir),
                Err(error) => found.not_saved = Some(format!("{}: {error}", dir.display())),
            }
        }
        self.violations.push(found);
    }

    /// A violation of a database that was not crashed.
    fn session_fault(&mut self, lineage: &Lineage, said: impl Into<String>) {
        self.found(lineage, Violation::new(Kind::Session, said), None);
    }

    /// Ends the lineage with a violation that is one of the differences
    /// between the two databases ([`Found::differs`]).
    fn differing(&mut self, lineage: &Lineage, said: impl Into<String>) -> Flow {
        let wrong = Wrong::from(Violation::new(Kind::Session, said));
        self.note(lineage, wrong, None, true);
        lineage.idle();
        Flow::End
    }

    /// The first lineage of the run: a file system that holds the
    /// directory the checkpoints are made in, whose name is durable, and
    /// no database. Its model begins with the block that the open of a new
    /// database publishes: what a database on a file system of its own
    /// holds that was just made.
    fn first(&mut self) -> Result<Lineage, String> {
        let config = SimConfig {
            sector_size: [512, 512, 4096, 128][((self.seed / 12) % 4) as usize],
            unknown: unknown_of_seed(self.seed),
            ..SimConfig::new(self.seed)
        };
        let fs = SimVfs::new(config);
        fs.create_dir(Path::new(CHECKPOINTS))
            .map_err(|error| error.to_string())?;
        fs.sync_dir(Path::new("/"))
            .map_err(|error| error.to_string())?;
        let made: Arc<dyn Vfs> = Arc::new(SimVfs::new(SimConfig::new(self.seed ^ TWIN)));
        let tuning = Tuning::at_rest(self.page_size);
        let new = grove::open(made, db_dir(), &tuning, true)
            .map_err(|error| format!("a new database: {error}"))?;
        let version = grove::version_of(&new);
        let (state, places) = grove::read_committed(&new, self.plan.grove_version, &Places::new())?;
        drop(new);
        // What a wipe has to leave is an empty grove by what it holds, and
        // not only by being what a new database holds.
        if let Some(said) = not_empty(&state) {
            return Err(format!("a new database is not an empty grove: {said}"));
        }
        self.empty = Some(state.clone());
        let history = History::new(Block {
            first: version,
            version,
            state,
            resume: Position::default(),
            made: Made::Open,
            inside: Vec::new(),
        });
        Ok(self.lineage(fs, history, places, Vec::new(), 0, 0))
    }

    fn lineage(
        &mut self,
        fs: SimVfs,
        history: History,
        places: Places,
        checkpoints: Vec<Checkpoint>,
        next_checkpoint: u64,
        depth: usize,
    ) -> Lineage {
        let index = self.lineages;
        self.lineages += 1;
        self.tally.count("lineages", 1);
        let latest = history.latest();
        let expect = Expect {
            checkpoints: checkpoints.clone(),
            doing: Doing::Open,
            ..Expect::exactly(latest)
        };
        let position = history.last().resume;
        // The file system of a lineage begins with every change of the
        // state it was made from durable.
        let durable = history.last().version;
        Lineage {
            index,
            depth,
            watcher: Watcher::new(fs, expect, self.sabotage()),
            history,
            places,
            steps: Steps::new(self.seed, index),
            script: 0,
            chance: Rng::derive(self.seed, CHANCE, index),
            position,
            block_start: position,
            attempt: 0,
            refused: 0,
            checkpoints,
            next_checkpoint,
            beside: 0,
            faults_before: 0,
            durable,
            killed: false,
            acted: false,
            seen: HashMap::new(),
            held: HashMap::new(),
            candidates: 0,
            kept: Vec::new(),
            ops: 0,
            sessions: 0,
            trail: Vec::new(),
        }
    }

    /// Runs the whole run.
    fn run(mut self) -> RunReport {
        match self.first() {
            Ok(first) => self.live(first, true),
            Err(error) => {
                let said = format!("the run did not begin: {error}");
                self.tally.count("violations", 1);
                let violation = Violation::new(Kind::Session, said);
                self.violations.push(Found::of(self.seed, 0, 0, violation));
            }
        }
        while let Some(kept) = self.waiting.pop() {
            if self.done() || self.lineages >= self.shape.lineages as u64 {
                break;
            }
            let fs = (kept.image.recover(&kept.plan)).expect("the plan was recovered from before");
            let lineage = self.lineage(
                fs,
                kept.history,
                kept.places,
                kept.checkpoints,
                kept.next_checkpoint,
                kept.depth,
            );
            self.live(lineage, false);
        }
        self.tally.count("runs", 1);
        self.tally.count(faults_of(self.faults), 1);
        RunReport {
            seed: self.seed,
            recipe: self.recipe(),
            tally: self.tally,
            fingerprints: self.order,
            violations: self.violations,
            digest: self.digest,
            acts: self.acts,
        }
    }

    /// The life of one lineage: its database is opened, and made if the
    /// directory has none, and put through the steps of its workload, one
    /// session after another.
    fn live(&mut self, mut lineage: Lineage, first: bool) {
        let lived = catch_unwind(AssertUnwindSafe(|| {
            // A session that was killed is followed by an open, also when
            // the lineage has no step left: the open is where a killed
            // process is held to have lost nothing.
            while (lineage.ops < self.shape.ops || lineage.killed) && !self.done() {
                if !self.session(&mut lineage) {
                    break;
                }
            }
        }));
        if let Err(panic) = lived {
            // The threads of a database that the panic left open wait for
            // nothing.
            lineage.watcher.resume();
            let said = format!("the lineage panicked: {}", panic_text(&*panic));
            self.found(&lineage, Violation::new(Kind::Panicked, said), None);
        }
        let watcher = lineage.watcher.clone();
        self.count_watcher(&mut lineage, &watcher);
        if first {
            self.digest = lineage.history.digest();
        }
        if lineage.depth < self.shape.depth {
            self.waiting.append(&mut lineage.kept);
        }
    }

    /// What the file layer of a session counted, when the lineage leaves
    /// it.
    fn count_watcher(&mut self, lineage: &mut Lineage, watcher: &Watcher) {
        self.tally.count(ACTED, watcher.lies());
        self.note_lie(lineage);
        self.tally
            .count("faults injected", watcher.fs().stats().faults);
        for (actor, met) in Actor::ALL.into_iter().zip(watcher.faults_met()) {
            self.tally.count(faults_by(actor), met);
        }
    }

    /// Notes where the defect that is planted in the file layer first
    /// acted in the lineage, once it has: by the images that had been
    /// taken of the lineage's file system.
    fn note_lie(&mut self, lineage: &mut Lineage) {
        if lineage.acted {
            return;
        }
        if let Some(images) = lineage.watcher.first_lie() {
            lineage.acted = true;
            self.acts.push(Act {
                seed: self.seed,
                lineage: lineage.index,
                what: Acted::Lied { images },
            });
        }
    }

    /// The database of a session and the one beside it that is not
    /// crashed, both opened on what the last session left: the second on a
    /// copy of the files with every change of the last session in it, as a
    /// process that ended leaves them.
    ///
    /// An open with no crash before it loses nothing. So the database is at
    /// the last block; or, where the last session ended with a commit of
    /// which it could not say whether the log holds it, at the block of
    /// that commit, which is then the last. A recovery makes the log
    /// durable to its end before it returns (`lignin_store::recovery`, step
    /// 9), so nothing it landed on is lost from here on.
    fn open(
        &mut self,
        lineage: &mut Lineage,
        tuning: &Tuning,
    ) -> Option<(Grove, Grove, Arc<SimVfs>)> {
        let watcher = lineage.watcher.clone();
        let image = watcher.fs().crash_image();
        let files = image
            .recover(&image.keep_all())
            .expect("a plan of the image");
        let files = Arc::new(files);
        let at_rest = Tuning::at_rest(self.page_size);
        let twin = match grove::open(files.clone(), db_dir(), &at_rest, true) {
            Ok(twin) => twin,
            Err(error) => {
                let said =
                    format!("the files of a session did not open with no thread beside: {error}");
                self.session_fault(lineage, said);
                return None;
            }
        };
        watcher.expecting(|expect| expect.doing = Doing::Open);
        watcher.resume();
        // The faults of a session are counted from before its open: a
        // thread that the open starts can meet one before the open has
        // returned.
        let before = watcher.fs().stats().faults;
        lineage.faults_before = before;
        let open = || {
            catch_unwind(AssertUnwindSafe(|| {
                grove::open(watcher.vfs(), db_dir(), tuning, true)
            }))
        };
        let mut opened = open();
        if matches!(opened, Ok(Err(_))) && watcher.fs().stats().faults > before {
            // A recovery that a fault failed says that the file layer
            // failed, and is made again (`lignin_store::recovery`,
            // "Twice").
            self.tally.count("opens that a fault failed", 1);
            watcher.fs().clear_rules();
            lineage.faults_before = watcher.fs().stats().faults;
            opened = open();
        }
        let db = match opened {
            Ok(Ok(db)) => db,
            Ok(Err(error)) => {
                watcher.pause();
                self.session_fault(lineage, format!("the open of a session: {error}"));
                self.check_images(lineage);
                return None;
            }
            Err(panic) => {
                watcher.pause();
                let said = format!("the open of a session panicked: {}", panic_text(&*panic));
                self.found(lineage, Violation::new(Kind::Panicked, said), None);
                self.check_images(lineage);
                return None;
            }
        };
        let (at, beside) = (grove::version_of(&db), grove::version_of(&twin));
        let last = lineage.history.last().clone();
        let expected = watcher.expect();
        let in_doubt = (expected.attempt.clone())
            .filter(|attempt| attempt.first <= at && at <= attempt.version);
        let landed = match in_doubt {
            _ if at != beside => Err(format!("its files open at version {beside} with no thread")),
            _ if at == last.version => Ok(None),
            Some(attempt) if at == attempt.version => Ok(Some(attempt)),
            Some(_) => Err("the commit that was in doubt has no version inside it".to_string()),
            None => Err("no commit of the last session was in doubt".to_string()),
        };
        match landed {
            Ok(None) => {}
            Ok(Some(block)) => {
                self.tally
                    .count("commits in doubt that the next open found", 1);
                lineage.history.push(block);
            }
            Err(said) => {
                let said = format!(
                    "a session opened at version {at} with no crash before it, and the last \
                     block is of version {}: {said}",
                    last.version
                );
                self.session_fault(lineage, said);
                end(&watcher, db);
                self.check_images(lineage);
                return None;
            }
        }
        // The open has made the log durable to its end: what a crash
        // leaves from this moment on is held to that.
        let latest = lineage.history.latest();
        watcher.acknowledge(|expect| {
            expect.attempt = None;
            expect.latest = latest;
            expect.floor = latest;
            expect.syncs = tuning.sync == SyncPolicy::EveryCommit;
            expect.doing = Doing::Nothing;
        });
        // The stream goes on after the last block: what a block wrote that
        // was open when the last session ended is lost.
        lineage.position = lineage.history.last().resume;
        lineage.block_start = lineage.position;
        lineage.attempt += 1;
        lineage.refused = 0;
        lineage.killed = false;
        Some((db, twin, files))
    }

    /// One session of a lineage: the database is opened where the last
    /// session left it, takes steps until one of them ends the session,
    /// and is closed or killed. Returns whether the lineage goes on.
    fn session(&mut self, lineage: &mut Lineage) -> bool {
        let panics = engine_panics();
        let goes_on = self.session_of(lineage);
        // A thread of a database that panics ends, and the drop of the
        // handle, which joins it, says nothing of it.
        let panicked = engine_panics() - panics;
        if panicked > 0 {
            self.tally
                .count("panics of the threads of the engine", panicked);
            let said = format!(
                "{panicked} panics of a background thread of the engine while the session was \
                 open, in this process: with more than one run at a time, in a database of this \
                 run or of another"
            );
            self.found(lineage, Violation::new(Kind::Panicked, said), None);
        }
        goes_on
    }

    fn session_of(&mut self, lineage: &mut Lineage) -> bool {
        lineage.sessions += 1;
        self.tally.count("sessions", 1);
        let turn = (self.seed.wrapping_mul(7))
            .wrapping_add(lineage.index * 5)
            .wrapping_add(lineage.sessions * 13);
        let tuning = Tuning::of_turn(self.page_size, turn);
        let Some((db, twin, twin_files)) = self.open(lineage, &tuning) else {
            return false;
        };
        let watcher = lineage.watcher.clone();
        // The transactions of the session end with this block, before the
        // databases do: what the block that is open wrote is lost.
        let flow = {
            let mut session = Session {
                db: &db,
                twin: &twin,
                twin_files: &twin_files,
                live: Transactions::new(&db),
                shadow: Transactions::new(&twin),
                dirty: false,
                forgotten: false,
                tuning,
            };
            self.bursts(lineage, &mut session, &watcher)
        };
        self.count_session(&db);
        drop(twin);
        self.end_session(lineage, &watcher, db, flow)
    }

    /// The steps of a session, in bursts, until one of them ends it.
    fn bursts(
        &mut self,
        lineage: &mut Lineage,
        session: &mut Session<'_>,
        watcher: &Watcher,
    ) -> Flow {
        let db = session.db;
        loop {
            // A burst: the steps are taken with the three threads of the
            // database at work beside them.
            watcher.resume();
            let burst = 1 + lineage.steps.below(self.shape.burst.max(1));
            let mut flow = Flow::On;
            for _ in 0..burst {
                if lineage.ops >= self.shape.ops || self.done() {
                    flow = Flow::Close;
                    break;
                }
                let stepped = catch_unwind(AssertUnwindSafe(|| self.step(lineage, session)));
                self.note_lie(lineage);
                flow = stepped.unwrap_or_else(|panic| {
                    let said = format!("a step panicked: {}", panic_text(&*panic));
                    self.found(lineage, Violation::new(Kind::Panicked, said), None);
                    Flow::End
                });
                if !matches!(flow, Flow::On) {
                    break;
                }
            }
            if !matches!(flow, Flow::On) {
                return flow;
            }
            // The counts of the engine are read while its threads run: a
            // thread that waits at the watcher can hold a lock of the
            // engine.
            self.background(lineage, db);
            watcher.pause();
            let held = ResumeOnPanic(watcher.clone());
            self.check_images(lineage);
            self.check_now(lineage);
            drop(held);
        }
    }

    /// Ends a session as `flow` says. Returns whether the lineage goes on.
    fn end_session(
        &mut self,
        lineage: &mut Lineage,
        watcher: &Watcher,
        db: Grove,
        flow: Flow,
    ) -> bool {
        match flow {
            Flow::Kill => {
                // The process of the session is killed: nothing more is
                // written to its file system, and the lineage goes on in
                // what a process that is killed leaves. Every change it
                // made is there for the next process to read, so the
                // database opens at the last block, or at the one of a
                // commit that the session left in doubt. And what it had
                // not synced is as little durable as before: a crash of
                // the machine before the next open has made the log
                // durable can lose it still.
                //
                // The handle of the killed process is dropped to end its
                // threads, and for nothing else: a process that is killed
                // closes nothing. Its close is taken out of the check of
                // a debug build, which would read through a file layer
                // that answers nothing any more. A panic of the drop is
                // then a panic of the engine over a call that failed.
                //
                // What the background work of the session could not do is
                // looked at before, while the process is alive: what its
                // threads count once their calls answer nothing is the
                // kill's doing.
                self.background(lineage, &db);
                db.engine().database().unchecked_at_close();
                watcher.kill();
                let dropped = catch_unwind(AssertUnwindSafe(|| drop(db)));
                watcher.pause();
                if let Err(panic) = dropped {
                    let said = format!(
                        "the handle of a killed session, dropped: {}",
                        panic_text(&*panic)
                    );
                    self.found(lineage, Violation::new(Kind::Panicked, said), None);
                }
                self.check_images(lineage);
                // What a crash at the moment of the kill leaves, and what
                // the killed process leaves, held to what is expected now.
                self.check_now(lineage);
                // The faults that were planted and not met go with the
                // process.
                watcher.fs().clear_rules();
                let checkpoints = lineage.checkpoints.clone();
                watcher.expecting(|expect| {
                    expect.checkpoints = checkpoints;
                    expect.doing = Doing::Open;
                    expect.wiped = false;
                });
                lineage.killed = true;
                true
            }
            Flow::Close | Flow::On => {
                self.background(lineage, &db);
                // A close makes the log durable, in a session that takes
                // writes still and that no fault was injected into. The
                // database is closed as a node closes it, by the drop of
                // GroveDB's handle, which answers nothing: so a close that
                // a fault can have failed is taken to have made nothing
                // durable, and one that no fault can have failed is held
                // to have made the log durable by every crash state after
                // it.
                let durable = !lineage.faulted() && db.engine().database().reopen().is_none();
                watcher.expecting(|expect| expect.doing = Doing::Close);
                watcher.resume();
                let closed = catch_unwind(AssertUnwindSafe(|| drop(db)));
                watcher.pause();
                if let Err(panic) = closed {
                    let said = format!("the close of a session: {}", panic_text(&*panic));
                    self.found(lineage, Violation::new(Kind::Panicked, said), None);
                }
                // What a crash leaves from the moment the close has
                // returned is held to what the close made durable.
                let latest = lineage.history.latest();
                let faulted = lineage.faulted();
                watcher.acknowledge(|expect| {
                    if durable && !faulted {
                        expect.floor = latest;
                    }
                    expect.doing = Doing::Nothing;
                    expect.wiped = false;
                });
                self.check_images(lineage);
                true
            }
            Flow::End => {
                watcher.pause();
                let held = ResumeOnPanic(watcher.clone());
                self.check_images(lineage);
                drop(held);
                end(watcher, db);
                false
            }
        }
    }

    /// What the background work of a session could not do: nothing, in a
    /// session that no fault was injected into.
    fn background(&mut self, lineage: &Lineage, db: &Grove) {
        // The counts are read before the faults are: an error that a fault
        // made is then counted with a fault that was injected before it.
        let stats = db.engine().database().stats();
        if lineage.faulted() {
            return;
        }
        let background = stats.background;
        let errors =
            background.write_back_errors + background.reclaim_errors + background.cleaner_errors;
        if errors > 0 {
            let said = format!(
                "the background work of a session failed with no fault injected ({} turns of \
                 write-back, {} of the reclaimer, {} of the cleaner): {}",
                background.write_back_errors,
                background.reclaim_errors,
                background.cleaner_errors,
                background.last_error.unwrap_or_default()
            );
            self.session_fault(lineage, said);
        }
        if let Some(reopen) = stats.reopen {
            let said =
                format!("a session takes no write any more with no fault injected: {reopen}");
            self.session_fault(lineage, said);
        }
    }

    /// What a session did, by the counts of its engine.
    fn count_session(&mut self, db: &Grove) {
        let stats = db.engine().database().stats();
        let tally = &mut self.tally;
        tally.count("commits published", stats.commits.published);
        tally.count("commits refused for a conflict", stats.commits.conflicts);
        tally.count("commits that failed", stats.commits.failed);
        tally.count("table checkpoints", stats.store.flush.checkpoints);
        tally.count("log files dropped", stats.store.flush.log_files_dropped);
        tally.count(
            "values placed by write-back",
            stats.store.flush.values_placed,
        );
        tally.count("passes of the cleaner", stats.cleaner.passes);
        tally.count("blocks moved by the cleaner", stats.cleaner.blocks_moved);
        tally.count(
            "segments deleted by the cleaner",
            stats.cleaner.segments_dropped,
        );
        tally.count("values moved by the cleaner", stats.cleaner.values_moved);
        tally.count(
            "value-log files deleted by the cleaner",
            stats.cleaner.value_files_dropped,
        );
        tally.count("dropped subtrees reclaimed", stats.reclaimed.merks);
        tally.count("pages freed by the reclaimer", stats.reclaimed.pages_freed);
        let background = stats.background;
        tally.count(
            "turns of the background work that failed",
            background.write_back_errors + background.reclaim_errors + background.cleaner_errors,
        );
        tally.count(
            "sessions that took no write any more",
            u64::from(stats.reopen.is_some()),
        );
    }

    /// One step of a session.
    fn step(&mut self, lineage: &mut Lineage, session: &mut Session<'_>) -> Flow {
        if session.ended() {
            // The session takes no write any more: the caller closes the
            // database and opens it again (`lignindb::db`, "A session that
            // takes no write any more"). With no fault injected it is a
            // violation, which `background` reports.
            if !lineage.faulted() {
                return self.fatal(lineage, "a session takes no write any more");
            }
            trail(lineage, "the session ended");
            return Flow::Kill;
        }
        lineage.ops += 1;
        if self.shape.planted == Some(Planted::ThreadPanic) && self.acts.is_empty() {
            let cleaner = std::thread::Builder::new().name("lignindb-cleaner".to_string());
            let panicked = cleaner.spawn(|| panic!("the planted defect: a thread panics"));
            let _ = panicked.expect("a thread").join();
            self.tally.count(ACTED, 1);
            self.acts.push(Act {
                seed: self.seed,
                lineage: lineage.index,
                what: Acted::Panicked { op: lineage.ops },
            });
        }
        let step = match self.shape.script {
            Script::Drawn => {
                let made = |checkpoint: &&Checkpoint| checkpoint.standing == Standing::Made;
                lineage.steps.next(Situation {
                    dirty: session.dirty,
                    checkpoints: lineage.checkpoints.iter().filter(made).count(),
                    calm: lineage.refused >= 2,
                    faults: self.faults,
                })
            }
            Script::Wipes | Script::Listed(_) => {
                let steps = match self.shape.script {
                    Script::Listed(steps) => steps,
                    _ => &WIPES,
                };
                let step = steps.get(lineage.script % steps.len().max(1));
                lineage.script += 1;
                step.copied().unwrap_or(Step::Stream)
            }
        };
        self.tally.count(step.name(), 1);
        match step {
            Step::Stream => self.stream(lineage, session),
            Step::Flush => self.flush(lineage, session),
            Step::Checkpoint => self.checkpoint(lineage, session),
            Step::DeleteCheckpoint => self.delete_checkpoint(lineage),
            Step::Wipe => self.wipe(lineage, session),
            Step::Beside => self.beside(lineage, session),
            Step::Settle => {
                trail(lineage, "pause");
                let moments = 1 + lineage.steps.below(4);
                std::thread::sleep(Duration::from_millis(moments));
                Flow::On
            }
            Step::Close => {
                trail(lineage, "close");
                Flow::Close
            }
            Step::Kill => {
                trail(lineage, "kill");
                Flow::Kill
            }
            Step::Fault(planted) => {
                trail(lineage, &format!("fault {planted:?}"));
                let rule = match planted {
                    workload::Planted::Sync => FaultRule::new(Fault::Eio).on(OpSet::SYNCS),
                    workload::Planted::Write => FaultRule::new(Fault::Eio).on(OpSet::WRITES),
                    workload::Planted::Name => FaultRule::new(Fault::Eio).on(OpSet::NAMES),
                    workload::Planted::Full => {
                        (FaultRule::new(Fault::Enospc).on(OpSet::WRITES)).times(3)
                    }
                };
                lineage.watcher.fs().add_rule(rule);
                Flow::On
            }
        }
    }

    /// Ends the lineage with a violation of the session.
    fn fatal(&mut self, lineage: &Lineage, said: impl Into<String>) -> Flow {
        self.session_fault(lineage, said);
        lineage.idle();
        Flow::End
    }

    /// What a call of the session that failed comes to: with a fault
    /// injected into the file system of the session since it was opened,
    /// it is the fault's, and is counted; with none, it is a violation,
    /// and the lineage ends.
    fn failed(&mut self, lineage: &Lineage, what: &str, error: impl fmt::Display) -> Option<Flow> {
        match lineage.faulted() {
            true => {
                self.tally.count("errors after a fault", 1);
                None
            }
            false => Some(self.fatal(lineage, format!("{what} failed: {error}"))),
        }
    }

    /// The block that is open is given up, in the database and in the one
    /// that is not crashed, and is made again from its beginning.
    fn give_up(&mut self, lineage: &mut Lineage, session: &mut Session<'_>) {
        let abandon = Op::Transaction(TransactionOp::Abandon);
        let version = self.plan.grove_version;
        let _ = session.live.apply(&abandon, version).unwrap();
        let _ = session.shadow.apply(&abandon, version).unwrap();
        session.dirty = false;
        session.forgotten = false;
        lineage.attempt += 1;
        lineage.position = lineage.block_start;
    }

    /// What became of an operation that is one commit, which answered `ok`
    /// and left the database `made` versions on. `wrote` says whether the
    /// call changed a file. `Err` is what no engine that keeps its rules
    /// answers.
    ///
    /// A commit is put into the log by the thread that makes it: the
    /// three threads of a database commit nothing for a caller
    /// (`lignindb::db`, "Background work"). So a call that failed and
    /// changed no file has left nothing in the log, whatever ended the
    /// session, and only one that changed a file can be in doubt.
    fn outcome(
        &self,
        lineage: &Lineage,
        session: &Session<'_>,
        ok: bool,
        made: u64,
        wrote: bool,
    ) -> Result<Outcome, String> {
        match (ok, made) {
            (true, 1) => Ok(Outcome::Published),
            (true, 0) => Ok(Outcome::Nothing),
            // A commit that is answered with an error and published: its
            // sync failed.
            (false, 1) if lineage.faulted() => Ok(Outcome::Published),
            (false, 0) if !session.ended() => Ok(Outcome::Nothing),
            (false, 0) if !lineage.faulted() => {
                Err("the session takes no write any more, with no fault injected".to_string())
            }
            (false, 0) if !wrote => Ok(Outcome::Refused),
            (false, 0) => Ok(Outcome::InDoubt),
            (ok, made) => Err(format!(
                "it answered {} and made {made} versions",
                if ok { "`Ok`" } else { "an error" }
            )),
        }
    }

    /// A commit that a session refused which had ended: nothing of it is
    /// in the log, and the session is left.
    fn refused(&mut self, lineage: &Lineage) -> Flow {
        lineage.idle();
        self.tally
            .count("commits refused by a session that had ended", 1);
        Flow::Kill
    }

    /// The next operation of the stream, on the database and on the one
    /// that is not crashed.
    fn stream(&mut self, lineage: &mut Lineage, session: &mut Session<'_>) -> Flow {
        self.streams = self.streams.max(lineage.position.stream + 1);
        let Some(op) = self.plan.op(lineage.position) else {
            // The stream has ended: the next one begins in an empty grove.
            self.tally.count("streams that ended", 1);
            return self.wipe(lineage, session);
        };
        match &op {
            Op::Commit => self.commit(lineage, session),
            Op::FlushPendingDrops => self.flush_drops(lineage, session, &op),
            _ => self.plain(lineage, session, &op),
        }
    }

    /// An operation that commits nothing by itself.
    fn plain(&mut self, lineage: &mut Lineage, session: &mut Session<'_>, op: &Op) -> Flow {
        trail(lineage, &name_of(op));
        let version = self.plan.grove_version;
        let before = grove::version_of(session.db);
        let writes = !op.keeps_transaction() && !op.ends_transaction();
        let live = session.live.apply(op, version).unwrap();
        // The planted defect keeps a write from the model: an operation
        // that writes, which a step of a transaction does not.
        let forgets = self.shape.planted == Some(Planted::ForgottenWrite)
            && writes
            && !matches!(op, Op::Transaction(_));
        let twin = if forgets && live.is_ok() && !session.forgotten {
            session.forgotten = true;
            self.tally.count(ACTED, 1);
            self.acts.push(Act {
                seed: self.seed,
                lineage: lineage.index,
                what: Acted::Forgot {
                    attempt: lineage.attempt,
                },
            });
            Ok(())
        } else {
            (session.shadow.apply(op, version).unwrap()).map(|_| ())
        };
        if live.is_ok() != twin.is_ok() {
            if live.is_err() && lineage.faulted() {
                // A fault failed the operation, and what it left of the
                // block is not known: the block is given up.
                self.tally.count("operations that a fault failed", 1);
                self.give_up(lineage, session);
                return Flow::On;
            }
            let said = |result: Result<(), String>| match result {
                Ok(()) => "was made".to_string(),
                Err(error) => format!("failed ({error})"),
            };
            let said = format!(
                "the operation `{}` {} in the database and {} in the one that is not crashed",
                name_of(op),
                said(live.map(|_| ()).map_err(|error| error.to_string())),
                said(twin.map_err(|error| error.to_string())),
            );
            return self.differing(lineage, said);
        }
        if grove::version_of(session.db) != before {
            let said = format!(
                "the operation `{}` is no commit and made a version",
                name_of(op)
            );
            return self.fatal(lineage, said);
        }
        self.tally.count(
            "operations that both databases refused",
            u64::from(live.is_err()),
        );
        lineage.position.op += 1;
        if op.ends_transaction() {
            session.dirty = false;
            session.forgotten = false;
            lineage.attempt += 1;
            lineage.block_start = lineage.position;
            lineage.refused = 0;
        } else if writes {
            session.dirty = true;
        }
        Flow::On
    }

    /// The commit of the block that is open.
    ///
    /// The database that is not crashed says, through its own block, what
    /// the commit publishes: the block that a crash from here on may leave
    /// the database at. Then the database commits, and then the other.
    fn commit(&mut self, lineage: &mut Lineage, session: &mut Session<'_>) -> Flow {
        // What is found of the commit is found of the block that it
        // commits: the number of the block goes on, by one, when the
        // commit is done with.
        let open = lineage.attempt;
        let flow = self.commit_block(lineage, session);
        lineage.attempt = open + 1;
        flow
    }

    fn commit_block(&mut self, lineage: &mut Lineage, session: &mut Session<'_>) -> Flow {
        trail(lineage, "commit");
        let version = self.plan.grove_version;
        let before = grove::version_of(session.db);
        let previewed = grove::read(
            session.twin,
            session.shadow.block(),
            version,
            &lineage.places,
        );
        let (state, places) = match previewed {
            Ok(previewed) => previewed,
            Err(error) => return self.fatal(lineage, format!("before a commit: {error}")),
        };
        let root = match grove::root_of(session.db, session.live.block(), version) {
            Ok(root) => root,
            Err(error) => {
                if let Some(flow) = self.failed(lineage, "the read of a root hash", error) {
                    return flow;
                }
                self.give_up(lineage, session);
                return Flow::On;
            }
        };
        if root != state.root {
            let said = format!(
                "before the commit of a block the root hash of the database is {} and that of \
                 the one that is not crashed {}",
                hex::encode(root),
                hex::encode(state.root)
            );
            return self.differing(lineage, said);
        }
        lineage.places.extend(places);
        let next = Position {
            op: lineage.position.op + 1,
            ..lineage.position
        };
        let attempt = Arc::new(Block {
            first: before + 1,
            version: before + 1,
            state,
            resume: next,
            made: Made::Commit,
            inside: Vec::new(),
        });
        lineage.watcher.expecting(|expect| {
            expect.attempt = Some(attempt.clone());
            expect.doing = Doing::Commit;
        });
        let wrote = lineage.watcher.boundaries_of_the_caller();
        let committed = session.live.apply(&Op::Commit, version).unwrap();
        let wrote = lineage.watcher.boundaries_of_the_caller() > wrote;
        let after = grove::version_of(session.db);
        // The block of a commit that failed is made again from its
        // beginning.
        let again = |lineage: &mut Lineage| {
            lineage.position = lineage.block_start;
            lineage.refused += 1;
        };
        session.dirty = false;
        session.forgotten = false;
        // A commit that is refused for a conflict is what a block asks for
        // that wrote what a write beside it met: no failure, with a fault
        // or without. The answer says what was done: nothing is published,
        // and nothing of the block is there for a crash to bring back,
        // whatever became of the session. The other database refuses the
        // block the same way.
        let conflict = |answer: &Result<Applied, grovedb::Error>| {
            (answer.as_ref()).is_err_and(grovedb::Error::is_commit_conflict)
        };
        if conflict(&committed) {
            if after != before {
                let said = format!(
                    "the commit of a block was refused for a conflict, and the database went \
                     from version {before} to version {after}"
                );
                return self.fatal(lineage, said);
            }
            lineage.idle();
            self.tally.count("blocks whose commit was refused", 1);
            again(lineage);
            let twin_before = grove::version_of(session.twin);
            let twin = session.shadow.apply(&Op::Commit, version).unwrap();
            let twin_at = grove::version_of(session.twin);
            // The other database made a version with its refusal: a
            // violation by itself. And the two differ where the other did
            // not refuse for a conflict, or is at another version.
            let moved = conflict(&twin) && twin_at != twin_before;
            let differs = !conflict(&twin) || twin_at != before;
            if !moved && !differs {
                return Flow::On;
            }
            let said = format!(
                "the commit of a block was refused for a conflict in the database, which is at \
                 version {before}, and answered {:?} in the one that is not crashed, which went \
                 from version {twin_before} to version {twin_at}",
                twin.map(|_| ()).map_err(|error| error.to_string())
            );
            return match differs {
                true => self.differing(lineage, said),
                false => self.fatal(lineage, said),
            };
        }
        let outcome = self.outcome(lineage, session, committed.is_ok(), after - before, wrote);
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(said) => return self.fatal(lineage, format!("the commit of a block: {said}")),
        };
        let twin = match outcome {
            Outcome::Published => {
                lineage.landed(attempt.clone(), session.syncs() && committed.is_ok());
                self.tally.count("blocks committed", 1);
                lineage.position = next;
                lineage.block_start = next;
                lineage.refused = 0;
                session.shadow.apply(&Op::Commit, version).unwrap()
            }
            Outcome::Nothing if committed.is_ok() => {
                lineage.idle();
                self.tally.count("blocks that wrote nothing", 1);
                lineage.position = next;
                lineage.block_start = next;
                session.shadow.apply(&Op::Commit, version).unwrap()
            }
            // A failure of a commit is a fault's, or a violation.
            Outcome::Nothing => {
                lineage.idle();
                let error = committed.err().map(|error| error.to_string());
                let error = error.unwrap_or_default();
                if let Some(flow) = self.failed(lineage, "the commit of a block", error) {
                    return flow;
                }
                self.tally.count("commits that a fault failed", 1);
                again(lineage);
                let abandon = Op::Transaction(TransactionOp::Abandon);
                return match session.shadow.apply(&abandon, version).unwrap() {
                    Ok(_) => Flow::On,
                    Err(error) => self.fatal(lineage, format!("a block was not given up: {error}")),
                };
            }
            Outcome::Refused => return self.refused(lineage),
            Outcome::InDoubt => {
                lineage.in_doubt();
                self.tally.count("commits left in doubt", 1);
                return Flow::Kill;
            }
        };
        let twin_at = grove::version_of(session.twin);
        if twin.is_err() || twin_at != after {
            // The two differ where they are at different versions, where
            // the database made the commit and the other failed, and where
            // the other alone answered a conflict (the database did not:
            // its conflict is dealt with above). A commit that both
            // published and both answered with an error that is no
            // conflict is a failure of the other database, which no fault
            // met.
            let differs = twin_at != after || committed.is_ok() || conflict(&twin);
            let said = format!(
                "the commit of a block left the database at version {after}, and answered {:?} \
                 in the one that is not crashed, which is at version {twin_at}",
                twin.map(|_| ()).map_err(|error| error.to_string()),
            );
            return match differs {
                true => self.differing(lineage, said),
                false => self.fatal(lineage, said),
            };
        }
        match after > before {
            true => self.published_as_shown(lineage, session, &attempt),
            false => Flow::On,
        }
    }

    /// The database that is not crashed holds, after its commit, what it
    /// showed before it: the block the campaign expects is the block that
    /// was published.
    fn published_as_shown(
        &mut self,
        lineage: &mut Lineage,
        session: &Session<'_>,
        attempt: &Block,
    ) -> Flow {
        let version = self.plan.grove_version;
        let committed = grove::read_committed(session.twin, version, &lineage.places);
        let difference = match &committed {
            Ok((state, _)) => state.difference(&attempt.state),
            Err(error) => Some(error.clone()),
        };
        match difference {
            None => Flow::On,
            Some(difference) => {
                let said = format!(
                    "the database that is not crashed published another block than it showed \
                     before its commit: {difference}"
                );
                self.fatal(lineage, said)
            }
        }
    }

    /// `GroveDb::flush_pending_prefix_drops`, which commits by itself and
    /// more than once. A copy of the files of the database that is not
    /// crashed makes it first, under a watcher, and says what the database
    /// holds after each of its commits ([`flushed_on_a_copy`]). Then the
    /// database that is not crashed makes it, and has to make the versions
    /// that the copy made and come to what the copy came to; and then the
    /// database, which has to report what the other reported and be at
    /// the version the other is at.
    ///
    /// **It is made where no fault can meet it.** What a flush leaves that
    /// a fault cuts short, the model does not say: a look at a record that
    /// fails has the flush pass over that record. So the faults that were
    /// planted and not met are taken back first, and a session into which
    /// a fault was injected is left in the place of the flush. The next
    /// session begins with no fault, and its stream goes on at the flush,
    /// where the same holds. A flush that fails, or does not do what the
    /// other database's did, is a violation.
    fn flush_drops(&mut self, lineage: &mut Lineage, session: &mut Session<'_>, op: &Op) -> Flow {
        lineage.watcher.fs().clear_rules();
        if lineage.faulted() {
            trail(lineage, "no flush of drops after a fault");
            self.tally.count(
                "sessions left before a flush of pending drops, after a fault",
                1,
            );
            return Flow::Kill;
        }
        trail(lineage, "flush drops");
        let version = self.plan.grove_version;
        let before = grove::version_of(session.db);
        // What a flush answered: how many records it reclaimed, passed
        // over as live again, and left for a look that failed.
        let answered = |applied: &Result<Applied, grovedb::Error>| match applied {
            Ok(Applied::Reclaimed(report)) => Some((
                report.reclaimed_records,
                report.skipped_live,
                report.unresolved_records,
            )),
            _ => None,
        };
        let copied = flushed_on_a_copy(
            session.twin_files,
            &Tuning::at_rest(self.page_size),
            version,
            &lineage.places,
        );
        let mut on_the_copy = match copied {
            Ok(states) => states,
            Err(said) => {
                let said = format!("a flush of the pending drops on a copy: {said}");
                return self.fatal(lineage, said);
            }
        };
        let twin = session.shadow.apply(op, version).unwrap();
        let twin_at = grove::version_of(session.twin);
        let next = Position {
            op: lineage.position.op + 1,
            ..lineage.position
        };
        // The copy made the versions that the other database made, a flush
        // with nothing to do among them, which makes none.
        let made_by_both = on_the_copy.keys().copied().eq(before + 1..=twin_at);
        let mut attempt = None;
        if made_by_both && twin_at > before {
            let (state, places) =
                match grove::read_committed(session.twin, version, &lineage.places) {
                    Ok(read) => read,
                    Err(error) => return self.fatal(lineage, format!("after a flush: {error}")),
                };
            let last = on_the_copy.remove(&twin_at).expect("a version of both");
            if last.difference(&state).is_none() {
                lineage.places.extend(places);
                let block = Arc::new(Block {
                    first: before + 1,
                    version: twin_at,
                    state,
                    resume: next,
                    made: Made::DropsFlushed,
                    inside: on_the_copy.into_values().collect(),
                });
                lineage.watcher.expecting(|expect| {
                    expect.attempt = Some(block.clone());
                    expect.doing = Doing::DropsFlushed;
                });
                attempt = Some(block);
            }
        }
        if attempt.is_none() && (!made_by_both || twin_at > before) {
            let said = format!(
                "a flush of the pending drops took the database that is not crashed from \
                 version {before} to version {twin_at}, and a copy of its files not to the same \
                 by the same versions"
            );
            return self.fatal(lineage, said);
        }
        let live = session.live.apply(op, version).unwrap();
        let after = grove::version_of(session.db);
        let same = answered(&live).is_some() && answered(&live) == answered(&twin);
        if !same || after != twin_at {
            let said = format!(
                "a flush of the pending drops answered {:?} in the database, which is at \
                 version {after}, and {:?} in the one that is not crashed, which is at version \
                 {twin_at}, with no fault injected",
                live.map(|_| ()).map_err(|error| error.to_string()),
                twin.map(|_| ()).map_err(|error| error.to_string()),
            );
            return self.fatal(lineage, said);
        }
        if let Some(block) = attempt {
            self.tally
                .count("flushes of pending drops that committed", 1);
            self.tally
                .count("commits of the flushes of pending drops", after - before);
            lineage.landed(block, session.syncs());
        }
        lineage.position = next;
        if !session.dirty {
            lineage.block_start = next;
        }
        Flow::On
    }

    /// `GroveDb::flush`: every block that was committed when it was called
    /// is durable when it has returned.
    fn flush(&mut self, lineage: &mut Lineage, session: &mut Session<'_>) -> Flow {
        trail(lineage, "flush");
        let target = lineage.history.latest();
        lineage
            .watcher
            .expecting(|expect| expect.doing = Doing::Flush);
        let flushed = session.db.flush();
        // What a crash leaves from the moment the flush has answered is
        // held to what it made durable.
        lineage.watcher.acknowledge(|expect| {
            if flushed.is_ok() {
                expect.floor = expect.floor.max(target);
            }
            expect.doing = Doing::Nothing;
        });
        match flushed {
            Ok(()) => Flow::On,
            Err(error) => (self.failed(lineage, "a flush", error)).unwrap_or(Flow::On),
        }
    }

    /// A checkpoint of the database is made. From the moment its creation
    /// has returned `Ok` it is there after any crash, with the block it
    /// was made at, and so is that block in the database. One whose
    /// creation failed holds nothing or the whole checkpoint.
    fn checkpoint(&mut self, lineage: &mut Lineage, session: &mut Session<'_>) -> Flow {
        let number = lineage.next_checkpoint;
        lineage.next_checkpoint += 1;
        let dir = checkpoint_dir(number);
        trail(lineage, &format!("checkpoint {number}"));
        let point = lineage.history.latest();
        lineage.checkpoints.push(Checkpoint {
            dir: dir.clone(),
            point,
            standing: Standing::Building,
        });
        let checkpoints = lineage.checkpoints.clone();
        lineage.watcher.expecting(|expect| {
            expect.checkpoints = checkpoints;
            expect.doing = Doing::Checkpoint;
        });
        // Through the facade of the engine, with write-back catching up
        // before the sample always or never, or through GroveDB's own
        // call, which leaves that to the engine's default and names no
        // version: what it made is found in the states that hold it.
        let expected = lineage.history.last().version;
        let made = match lineage.steps.below(3) {
            0 => grove::checkpoint(session.db, &dir, Some(0)),
            1 => grove::checkpoint(session.db, &dir, None),
            _ => grove::create_checkpoint(session.db, &dir).map(|()| expected),
        };
        if made == Ok(expected) {
            lineage.checkpoints.last_mut().expect("pushed").standing = Standing::Made;
            self.tally.count("checkpoints made", 1);
        }
        let checkpoints = lineage.checkpoints.clone();
        lineage.watcher.acknowledge(|expect| {
            expect.checkpoints = checkpoints;
            if made == Ok(expected) {
                expect.floor = expect.floor.max(point);
            }
            expect.doing = Doing::Nothing;
        });
        match made {
            Ok(version) if version == expected => Flow::On,
            Ok(version) => {
                let said = format!(
                    "a checkpoint was made of version {version} while the last block is of \
                     version {expected}"
                );
                self.fatal(lineage, said)
            }
            Err(error) => {
                (self.failed(lineage, "the making of a checkpoint", error)).unwrap_or(Flow::On)
            }
        }
    }

    /// The oldest checkpoint is removed. While the removal is under way,
    /// and after one that failed, it is whole, marked as being removed, or
    /// holds no database. A removal that returned `Ok` is durable
    /// (`lignindb::checkpoint::delete`): from then on its directory is
    /// gone after any crash.
    fn delete_checkpoint(&mut self, lineage: &mut Lineage) -> Flow {
        let made = |checkpoint: &Checkpoint| checkpoint.standing == Standing::Made;
        let Some(at) = lineage.checkpoints.iter().position(made) else {
            return Flow::On;
        };
        let dir = lineage.checkpoints[at].dir.clone();
        trail(lineage, &format!("delete {}", dir.display()));
        lineage.checkpoints[at].standing = Standing::Deleting;
        let checkpoints = lineage.checkpoints.clone();
        lineage.watcher.expecting(|expect| {
            expect.checkpoints = checkpoints;
            expect.doing = Doing::DeleteCheckpoint;
        });
        let deleted = grove::delete_checkpoint(&*lineage.watcher.vfs(), &dir);
        if deleted.is_ok() {
            lineage.checkpoints[at].standing = Standing::Deleted;
            self.tally
                .count("deletions of a checkpoint that returned", 1);
        }
        let checkpoints = lineage.checkpoints.clone();
        lineage.watcher.acknowledge(|expect| {
            expect.checkpoints = checkpoints;
            expect.doing = Doing::Nothing;
        });
        match deleted {
            Ok(()) => Flow::On,
            Err(error) => {
                (self.failed(lineage, "the deletion of a checkpoint", error)).unwrap_or(Flow::On)
            }
        }
    }

    /// `GroveDb::wipe`: one commit, after which the grove is empty: it
    /// holds what a database holds that was just made. The database that
    /// is not crashed is wiped after it, and is held to that.
    fn wipe(&mut self, lineage: &mut Lineage, session: &mut Session<'_>) -> Flow {
        trail(lineage, "wipe");
        let version = self.plan.grove_version;
        let before = grove::version_of(session.db);
        let Some(empty) = self.empty.clone() else {
            return self.fatal(lineage, "the run has no empty grove");
        };
        // The planted defect: a write with no transaction is made under the
        // name of the wipe, and the wipe after it. The model is told of one
        // block, the empty grove.
        let split = self.shape.planted == Some(Planted::SplitWipe);
        let next = Position {
            stream: lineage.position.stream + 1,
            op: 0,
        };
        let attempt = Arc::new(Block {
            first: before + 1,
            version: before + 1 + u64::from(split),
            state: empty.clone(),
            resume: next,
            made: Made::Wipe,
            inside: Vec::new(),
        });
        // What the wipe drops waits to be reclaimed from here on: the
        // counts of the boundaries after a wipe begin with its own.
        let was_wiped = lineage.watcher.expect().wiped;
        lineage.watcher.expecting(|expect| {
            expect.attempt = Some(attempt.clone());
            expect.doing = Doing::Wipe;
            expect.wiped = true;
        });
        let wipe = |db: &Grove, written: &mut dyn FnMut()| {
            let mut wiped = Ok(());
            if split {
                wiped = grove::put_item(db, BESIDE_KEY, u64::MAX, None, version);
                if wiped.is_ok() {
                    written();
                }
            }
            wiped.and_then(|()| db.wipe().map_err(|error| error.to_string()))
        };
        // The planted defect has acted once its write is made, which
        // makes the version after the one the database was at.
        let mut acted = false;
        let wrote = lineage.watcher.boundaries_of_the_caller();
        let wiped = wipe(session.db, &mut || acted = true);
        let wrote = lineage.watcher.boundaries_of_the_caller() > wrote;
        if acted {
            self.tally.count(ACTED, 1);
            self.acts.push(Act {
                seed: self.seed,
                lineage: lineage.index,
                what: Acted::Split {
                    version: before + 1,
                },
            });
        }
        let made = grove::version_of(session.db) - before;
        let outcome = match split {
            true if wiped.is_ok() && made == 2 => Ok(Outcome::Published),
            true => Err("the planted defect could not be planted".to_string()),
            false => self.outcome(lineage, session, wiped.is_ok(), made, wrote),
        };
        let not_wiped = |lineage: &Lineage| {
            lineage.watcher.expecting(|expect| expect.wiped = was_wiped);
        };
        match (outcome, &wiped) {
            (Ok(Outcome::Published), _) => {}
            (Ok(Outcome::Nothing), Err(error)) => {
                // The wipe was not made, and the stream goes on.
                not_wiped(lineage);
                lineage.idle();
                return (self.failed(lineage, "a wipe", error)).unwrap_or(Flow::On);
            }
            // A wipe is a commit also where there is nothing to drop.
            (Ok(Outcome::Nothing), Ok(())) => {
                return self.fatal(lineage, "a wipe answered `Ok` and made no version");
            }
            (Ok(Outcome::Refused), _) => {
                not_wiped(lineage);
                return self.refused(lineage);
            }
            (Ok(Outcome::InDoubt), _) => {
                lineage.in_doubt();
                self.tally.count("commits left in doubt", 1);
                return Flow::Kill;
            }
            (Err(said), _) => return self.fatal(lineage, format!("a wipe: {said}")),
        }
        lineage.landed(attempt, session.syncs() && wiped.is_ok());
        self.tally.count("wipes made", 1);
        let beside = wipe(session.twin, &mut || {})
            .and_then(|()| grove::read_committed(session.twin, version, &lineage.places));
        let difference = match beside {
            Ok((state, _)) => state.difference(&empty),
            Err(error) => Some(error),
        };
        if let Some(difference) = difference {
            let said = format!("a grove that was wiped is not an empty one: {difference}");
            return self.fatal(lineage, said);
        }
        lineage.position = next;
        lineage.block_start = next;
        // The block that was open is given up, or stays open with what it
        // wrote before the wipe: its commit then meets the wipe.
        if session.dirty && lineage.steps.below(2) == 0 {
            self.tally.count("blocks left open over a wipe", 1);
        } else {
            self.give_up(lineage, session);
            lineage.position = next;
            lineage.refused = 0;
        }
        Flow::On
    }

    /// A write with no transaction beside the block that is open: one item
    /// of the root tree, which commits at once. The database that is not
    /// crashed makes it in a transaction of its own first, and says through
    /// it what the write publishes.
    fn beside(&mut self, lineage: &mut Lineage, session: &mut Session<'_>) -> Flow {
        trail(lineage, "write beside");
        let version = self.plan.grove_version;
        let before = grove::version_of(session.db);
        lineage.beside += 1;
        let value = lineage.beside;
        let own = session.twin.start_transaction();
        let prepared = grove::put_item(session.twin, BESIDE_KEY, value, Some(&own), version)
            .and_then(|()| grove::read(session.twin, &own, version, &lineage.places));
        let (state, places) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return self.fatal(lineage, format!("a write beside a block: {error}")),
        };
        lineage.places.extend(places);
        // A crash that leaves the database at this block has lost what the
        // open block wrote: the stream goes on where that block began.
        let attempt = Arc::new(Block {
            first: before + 1,
            version: before + 1,
            state,
            resume: lineage.block_start,
            made: Made::Beside,
            inside: Vec::new(),
        });
        lineage.watcher.expecting(|expect| {
            expect.attempt = Some(attempt.clone());
            expect.doing = Doing::Beside;
        });
        let wrote = lineage.watcher.boundaries_of_the_caller();
        let written = grove::put_item(session.db, BESIDE_KEY, value, None, version);
        let wrote = lineage.watcher.boundaries_of_the_caller() > wrote;
        let after = grove::version_of(session.db);
        match self.outcome(lineage, session, written.is_ok(), after - before, wrote) {
            Ok(Outcome::Published) => {}
            Ok(Outcome::Nothing) => {
                lineage.idle();
                let failed = written.err().unwrap_or_default();
                return (self.failed(lineage, "a write with no transaction", failed))
                    .unwrap_or(Flow::On);
            }
            Ok(Outcome::Refused) => return self.refused(lineage),
            Ok(Outcome::InDoubt) => {
                lineage.in_doubt();
                self.tally.count("commits left in doubt", 1);
                return Flow::Kill;
            }
            Err(said) => {
                return self.fatal(lineage, format!("a write with no transaction: {said}"));
            }
        }
        lineage.landed(attempt.clone(), session.syncs() && written.is_ok());
        self.tally.count("writes beside a block made", 1);
        let committed = session.twin.commit_transaction(own).unwrap();
        if committed.is_err() || grove::version_of(session.twin) != after {
            let said = format!(
                "the write beside a block answered {:?} in the database that is not crashed",
                committed.map_err(|error| error.to_string())
            );
            return self.fatal(lineage, said);
        }
        self.published_as_shown(lineage, session, &attempt)
    }

    /// Goes through the crash images taken since the last call: every
    /// state that their plans leave.
    fn check_images(&mut self, lineage: &mut Lineage) {
        for taken in lineage.watcher.drain() {
            let tally = &mut self.tally;
            if taken.acknowledged {
                tally.count("images taken when a call of the workload had answered", 1);
                // Such an image can hold a change that another thread
                // made after the call returned.
                tally.count(
                    "images of an answer that follow a change of another thread",
                    u64::from(taken.actor != Actor::Caller),
                );
            } else {
                tally.count("write boundaries", 1);
                tally.count(boundaries_by(taken.actor), 1);
                tally.count(boundaries_during(taken.expect.doing), 1);
                let after_a_wipe = taken.expect.wiped && taken.expect.doing != Doing::Wipe;
                tally.count(
                    "write boundaries after a wipe, in the session that made it",
                    u64::from(after_a_wipe),
                );
            }
            self.check_image(lineage, &taken);
        }
    }

    /// What a step made of the floor and of the block it committed holds
    /// from here on, whether or not a file changed after the campaign
    /// learned it: the states that a crash now can leave are held to what
    /// is expected now. The watcher is paused, so the file system is still.
    fn check_now(&mut self, lineage: &mut Lineage) {
        let now = lineage.watcher.now();
        self.check_image(lineage, &now);
    }

    /// The states that the plans leave of the crash `taken`, each held to
    /// what was expected of the database when the image was taken, and two
    /// of them to more:
    ///
    /// - The first plan is the one with no unsynced change. The version it
    ///   recovers to is one that the durable part of the file system
    ///   holds, so no state of this crash or of a later one of the same
    ///   file system is below it, whatever the engine has reported: not
    ///   the states of the other plans of this image, and not the one
    ///   with every change.
    /// - The plan that keeps every change whole and in order leaves what a
    ///   process that is killed leaves. The database is then where an open
    ///   with no crash finds it: at the last block or at the one that is
    ///   being made, and at no block before.
    fn check_image(&mut self, lineage: &mut Lineage, taken: &Taken) {
        let mut expect = (*taken.expect).clone();
        expect.durable = expect.durable.max(lineage.durable);
        let reported = lineage.history.block(expect.floor).version;
        // A crash during the wipe of a grove that holds something says
        // what the wipe leaves: of an empty one, every state is empty.
        let before_wipe = &lineage.history.block(expect.latest).state;
        let holds = |empty: &State| before_wipe.difference(empty).is_some();
        let wipe_of_something =
            expect.doing == Doing::Wipe && self.empty.as_ref().is_some_and(holds);
        let whole = taken.image.keep_all();
        // The versions that the state with no unsynced change and the one
        // with every change are at.
        let (mut with_none, mut with_all) = (None, None);
        for (turn, plan) in chosen(&taken.image, &self.shape).into_iter().enumerate() {
            if self.done() {
                return;
            }
            let is_whole = plan == whole;
            let killed = expect.killed();
            let held_to = match is_whole {
                true => &killed,
                false => &expect,
            };
            let recovered = self.check_plan(lineage, taken, held_to, plan, is_whole);
            if let (true, Some((landing, _))) = (wipe_of_something, recovered) {
                let name = match landing {
                    Landing::Point(at) if at == expect.latest => WIPE_UNCHANGED,
                    Landing::Point(_) | Landing::Inside(_) => WIPE_EARLIER,
                    Landing::Attempt | Landing::InsideAttempt => WIPE_EMPTY,
                };
                self.tally.count(name, 1);
            }
            if let (true, Some((_, version))) = (is_whole, recovered) {
                with_all = Some(version);
            }
            if let (0, Some((_, version))) = (turn, recovered) {
                with_none = Some(version);
                self.tally.count(
                    "floors raised by a durable state",
                    u64::from(version > reported.max(lineage.durable)),
                );
                lineage.durable = lineage.durable.max(version);
                expect.durable = expect.durable.max(version);
            }
        }
        if let (Some(durable), Some(reached)) = (with_none, with_all)
            && let Some(said) = behind_in_a_flush(&expect, durable, reached)
        {
            let lost = Wrong {
                version: Some(durable),
                ..Wrong::from(Violation::new(Kind::NotCommitted, said))
            };
            self.found(lineage, lost, Some((taken, &taken.image.lose_all())));
        }
    }

    /// One state: the one `plan` leaves of the crash `taken`, held to
    /// `expect`. Returns where it landed and at which version, if that is
    /// where it may be.
    fn check_plan(
        &mut self,
        lineage: &mut Lineage,
        taken: &Taken,
        expect: &Expect,
        plan: CrashPlan,
        killed: bool,
    ) -> Option<(Landing, u64)> {
        self.tally.count("crash plans", 1);
        let state = (taken.image.recover(&plan)).expect("a plan of the image fits it");
        let fingerprint = state.fingerprint();
        let left_hashes = AtomicU64::new(0);
        let rules = Rules {
            history: &lineage.history,
            places: &lineage.places,
            grove_version: self.plan.grove_version,
            page_size: self.page_size,
            left_hashes: &left_hashes,
        };
        // A state that was recovered before is not opened again: what it
        // held is held to what is expected now. One that was walked when
        // the lineage had reached fewer subtrees is, so that the walk
        // lists what it holds of the others.
        let first = !lineage.seen.contains_key(&fingerprint);
        match lineage.seen.get(&fingerprint) {
            Some(seen) if seen.places == lineage.places.len() => {
                self.tally.count("states seen before", 1);
                return match again_allowed(rules, expect, seen) {
                    Ok(landing) => Some((landing, seen.version)),
                    Err(wrong) => {
                        self.found(lineage, wrong, Some((taken, &plan)));
                        None
                    }
                };
            }
            Some(_) => self
                .tally
                .count("states recovered again for the subtrees reached since", 1),
            None => {}
        }
        if self.fingerprints.insert(fingerprint) {
            self.order.push(fingerprint);
            self.tally.count("distinct crash states", 1);
        }
        self.tally.count("recoveries", 1);
        let turn = fingerprint >> 7;
        let checked = no_panic("the recovery of a crash state", || {
            Ok(check_state(&state, rules, expect, turn))
        });
        self.tally
            .count(LEFT_HASHES, left_hashes.swap(0, Ordering::Relaxed));
        let recovered = match checked.map_err(Wrong::from).and_then(|checked| checked) {
            Ok(recovered) => recovered,
            Err(wrong) => {
                self.found(lineage, wrong, Some((taken, &plan)));
                return None;
            }
        };
        if first {
            self.record(taken, expect, &recovered, killed);
        }
        let held = &recovered.state;
        let key = (held.root, held.digest, held.incomplete_restore);
        let state = (lineage.held.entry(key))
            .or_insert_with(|| Arc::new(held.clone()))
            .clone();
        let seen = Seen {
            version: recovered.version,
            state,
            places: lineage.places.len(),
            checkpoints: recovered.checkpoints.clone(),
        };
        lineage.seen.insert(fingerprint, seen);
        if !first {
            // It was counted, kept or not, and sent on or not when it was
            // first recovered.
            return Some((recovered.landing, recovered.version));
        }
        let again = self.shape.again_one_in;
        if again > 0 && (fingerprint >> 11).is_multiple_of(again) {
            self.tally.count("states that went on", 1);
            let other = (taken.image.recover(&plan)).expect("a plan of the image fits it");
            let went_on = no_panic("a recovered database that goes on", || {
                check_again(&other, rules, expect, &recovered, turn)
            });
            self.tally
                .count(LEFT_HASHES, left_hashes.swap(0, Ordering::Relaxed));
            if let Err(violation) = went_on {
                self.found(lineage, violation, Some((taken, &plan)));
            }
        }
        self.keep(lineage, taken, plan, &recovered);
        Some((recovered.landing, recovered.version))
    }

    /// Counts a state that was recovered for the first time.
    fn record(&mut self, taken: &Taken, expect: &Expect, recovered: &Recovered, killed: bool) {
        let tally = &mut self.tally;
        tally.count(states_by(taken.actor), 1);
        tally.count("states that a killed process leaves", u64::from(killed));
        match recovered.landing {
            Landing::Point(at) => {
                tally.count(
                    "states above the durable floor",
                    u64::from(at > expect.floor),
                );
                tally.count("states that lost blocks", u64::from(at < expect.latest));
            }
            Landing::Inside(_) => {
                tally.count("states inside an operation that commits more than once", 1);
            }
            Landing::Attempt => tally.count("states at the block that was being made", 1),
            Landing::InsideAttempt => {
                tally.count("states inside an operation that commits more than once", 1);
                tally.count("states at the block that was being made", 1);
            }
        }
        tally.count(
            "states of a crash after a wipe, in the session that made it",
            u64::from(taken.expect.wiped && taken.expect.doing != Doing::Wipe),
        );
        tally.count(states_during(taken.expect.doing), 1);
        for (checkpoint, (_, held)) in expect.checkpoints.iter().zip(&recovered.checkpoints) {
            let name = match (checkpoint.standing, held) {
                (Standing::Made, _) => "checkpoints found whole that were made",
                (Standing::Building, Held::Whole) => {
                    "checkpoints found whole whose creation had not returned"
                }
                (Standing::Building, _) => "checkpoints not found whose creation had not returned",
                (Standing::Deleting, Held::Whole) => {
                    "checkpoints found whole that were being deleted"
                }
                (Standing::Deleting, Held::Marked) => {
                    "checkpoints found marked that were being deleted"
                }
                (Standing::Deleting, Held::Nothing | Held::Gone) => {
                    "checkpoints not found that were being deleted"
                }
                (Standing::Deleted, _) => "checkpoints gone whose deletion had returned",
                (Standing::Left(_), _) => {
                    "checkpoints found as the state had them that the lineage began with"
                }
            };
            tally.count(name, 1);
        }
    }

    /// Keeps a state, perhaps, to begin a lineage of its own: each state
    /// of a lineage has the same chance of being one of those kept.
    fn keep(&mut self, lineage: &mut Lineage, taken: &Taken, plan: CrashPlan, found: &Recovered) {
        if lineage.depth >= self.shape.depth || self.shape.kept == 0 {
            return;
        }
        lineage.candidates += 1;
        let at = match lineage.kept.len() < self.shape.kept {
            true => lineage.kept.len(),
            false => lineage.chance.below(lineage.candidates) as usize,
        };
        if at >= self.shape.kept {
            return;
        }
        // The lineage of the state begins with the blocks up to the one it
        // is at. A state inside the making of a block is a block of its
        // own there: the model knows what it holds from the state itself.
        let inside = |before: &Arc<Block>| {
            Arc::new(Block {
                first: found.version,
                version: found.version,
                state: found.state.clone(),
                resume: before.resume,
                made: Made::Inside,
                inside: Vec::new(),
            })
        };
        let latest = taken.expect.latest;
        let mut history;
        match found.landing {
            Landing::Point(point) => history = lineage.history.up_to(point),
            Landing::Inside(point) => {
                history = lineage.history.up_to(point - 1);
                history.push(inside(lineage.history.block(point - 1)));
            }
            Landing::Attempt => {
                history = lineage.history.up_to(latest);
                let attempt = taken.expect.attempt.clone();
                let attempt = attempt.expect("a landing at the attempt");
                history.push(attempt);
            }
            Landing::InsideAttempt => {
                history = lineage.history.up_to(latest);
                history.push(inside(lineage.history.block(latest)));
            }
        }
        // Its checkpoints are those of the image, each as the state has
        // it: one that is whole is one that was made; one whose deletion
        // had returned stays gone; and what the crash left of one that it
        // cut short stays what it is.
        let had = taken.expect.checkpoints.iter().zip(&found.checkpoints);
        let checkpoints = had
            .map(|(checkpoint, (_, held))| Checkpoint {
                standing: match (checkpoint.standing, *held) {
                    (_, Held::Whole) => Standing::Made,
                    (Standing::Deleted, _) => Standing::Deleted,
                    (Standing::Left(left), _) => Standing::Left(left),
                    (_, held) => Standing::Left(held),
                },
                ..checkpoint.clone()
            })
            .collect();
        let kept = Kept {
            image: taken.image.clone(),
            plan,
            history,
            checkpoints,
            next_checkpoint: lineage.next_checkpoint,
            places: lineage.places.clone(),
            depth: lineage.depth + 1,
        };
        match at < lineage.kept.len() {
            true => lineage.kept[at] = kept,
            false => lineage.kept.push(kept),
        }
    }
}

/// The steps of a lineage that wipes again and again ([`Script::Wipes`]),
/// round after round: a few operations of a stream, a write with no
/// transaction, so that the grove holds something whatever the stream has
/// committed, a flush, a wipe, which finds every block before it durable,
/// a pause, a few operations more, such a write again, a wipe that no
/// flush comes before, and a pause. The third round ends in a close, so
/// that the next session opens what the wipes left to reclaim.
pub const WIPES: [Step; 44] = {
    let mut steps = [Step::Stream; 44];
    steps[6] = Step::Beside;
    steps[7] = Step::Flush;
    steps[8] = Step::Wipe;
    steps[9] = Step::Settle;
    steps[14] = Step::Beside;
    steps[15] = Step::Wipe;
    steps[16] = Step::Settle;
    steps[22] = Step::Beside;
    steps[23] = Step::Flush;
    steps[24] = Step::Wipe;
    steps[25] = Step::Settle;
    steps[30] = Step::Beside;
    steps[31] = Step::Wipe;
    steps[32] = Step::Settle;
    steps[39] = Step::Beside;
    steps[40] = Step::Flush;
    steps[41] = Step::Wipe;
    steps[42] = Step::Settle;
    steps[43] = Step::Close;
    steps
};

/// How many of the plans nearest to the one that keeps every change are
/// taken of an image whose plans are more than a run takes
/// ([`Shape::max_plans`]).
pub const NEWEST: usize = 8;

/// The plans the campaign takes of `image`. The first is the one with no
/// unsynced change, and the one that keeps every change is among them.
///
/// The store campaign's choice (`crash::campaign::plans_of`) is taken whole
/// where it is at most [`Shape::max_plans`] plans, or where that is 0. An
/// image of the engine can have hundreds of unsynced changes, of its three
/// threads and of the caller, and that choice then grows with the square
/// of their number. Of a longer choice the campaign takes:
///
/// 1. the two ends: no unsynced change, and every change;
/// 2. the last [`NEWEST`] of the plans of a file system that writes in
///    order, which are those nearest to the image as it stands: its newest
///    change lost, or cut short at its last sectors. The plans before
///    them leave what the images before this one left with every change;
/// 3. of the rest (the other plans in order, each change lost alone, each
///    change applied late, the plans drawn from the seed) as many as there
///    is room for, at even distances.
pub fn chosen(image: &CrashImage, shape: &Shape) -> Vec<CrashPlan> {
    let all = plans_of(image, shape.every_plan_limit, shape.samples);
    if shape.max_plans == 0 || all.len() <= shape.max_plans {
        return all;
    }
    let (durable, whole) = (image.lose_all(), image.keep_all());
    let in_order: Vec<CrashPlan> = image.prefixes().collect();
    let newest: Vec<CrashPlan> = (in_order.into_iter().rev())
        .filter(|plan| *plan != durable && *plan != whole)
        .take(NEWEST.min(shape.max_plans.saturating_sub(2)))
        .collect();
    let mut plans = vec![durable, whole];
    plans.extend(newest);
    let rest: Vec<CrashPlan> = (all.into_iter())
        .filter(|plan| !plans.contains(plan))
        .collect();
    let room = (shape.max_plans.saturating_sub(plans.len())).min(rest.len());
    for pick in 0..room {
        plans.push(rest[pick * rest.len() / room].clone());
    }
    plans
}

/// What a flush of the pending drops leaves after each of its commits, by
/// the version: `files` are those of a database, which stays as it is. A
/// copy of them is opened under a watcher of its own and flushed, and
/// after every change of the copy's files the state that a process leaves
/// that is killed then is opened and read. Each commit of the flush is
/// such a change.
pub fn flushed_on_a_copy(
    files: &SimVfs,
    tuning: &Tuning,
    grove_version: &GroveVersion,
    places: &Places,
) -> Result<BTreeMap<u64, State>, String> {
    let image = files.crash_image();
    let copy = (image.recover(&image.keep_all())).map_err(|error| format!("{error:?}"))?;
    let watcher = Watcher::new(copy, Expect::exactly(0), None);
    let db =
        grove::open(watcher.vfs(), db_dir(), tuning, false).map_err(|error| error.to_string())?;
    let before = grove::version_of(&db);
    watcher.drain();
    db.flush_pending_prefix_drops(grove_version)
        .map_err(|error| error.to_string())?;
    let taken = watcher.drain();
    drop(db);
    let mut states = BTreeMap::new();
    for taken in taken {
        let killed =
            (taken.image.recover(&taken.image.keep_all())).map_err(|error| format!("{error:?}"))?;
        let db = grove::open(Arc::new(killed), db_dir(), tuning, false)
            .map_err(|error| error.to_string())?;
        let at = grove::version_of(&db);
        if at > before && !states.contains_key(&at) {
            let (state, _) = grove::read_committed(&db, grove_version, places)?;
            states.insert(at, state);
        }
    }
    Ok(states)
}

/// The root hash of a grove with no element: that of a Merk with no node.
pub const EMPTY_ROOT: Hash = [0; 32];

/// How `state` is not that of an empty grove: the root hash of a grove
/// with no element, no record that a walk lists, and no mark of a restore
/// that did not finish.
pub fn not_empty(state: &State) -> Option<String> {
    if state.root != EMPTY_ROOT {
        return Some(format!("its root hash is {}", hex::encode(state.root)));
    }
    if let Some(record) = state.records.first() {
        return Some(format!(
            "it holds {} records, the first {}",
            state.records.len(),
            record.text()
        ));
    }
    match state.incomplete_restore {
        true => Some("it has the mark of a restore that did not finish".to_string()),
        false => None,
    }
}

/// Ends a session whose database is not to be closed in good order: its
/// threads are let go, and the handle is dropped with whatever that
/// answers.
fn end(watcher: &Watcher, db: Grove) {
    watcher.resume();
    let _ = catch_unwind(AssertUnwindSafe(|| drop(db)));
    watcher.pause();
}

fn trail(lineage: &mut Lineage, step: &str) {
    lineage.trail.push(step.to_string());
    if lineage.trail.len() > TRAIL {
        lineage.trail.remove(0);
    }
}

/// The kind of an operation of a stream, in a word or two.
fn name_of(op: &Op) -> String {
    let text = format!("{op:?}");
    let end = text
        .find(|c: char| !(c.is_alphanumeric() || c == '(' || c == '_'))
        .unwrap_or(text.len());
    let name = text[..end].trim_end_matches('(');
    match name.contains('(') {
        true => format!("{name})"),
        false => name.to_string(),
    }
}

fn boundaries_by(actor: Actor) -> &'static str {
    match actor {
        Actor::Caller => "write boundaries made by the caller's thread",
        Actor::WriteBack => "write boundaries made by write-back's thread",
        Actor::Reclaimer => "write boundaries made by the reclaimer's thread",
        Actor::Cleaner => "write boundaries made by the cleaner's thread",
    }
}

fn states_by(actor: Actor) -> &'static str {
    match actor {
        Actor::Caller => "states of a crash at a change of the caller's thread",
        Actor::WriteBack => "states of a crash at a change of write-back's thread",
        Actor::Reclaimer => "states of a crash at a change of the reclaimer's thread",
        Actor::Cleaner => "states of a crash at a change of the cleaner's thread",
    }
}

fn faults_by(actor: Actor) -> &'static str {
    match actor {
        Actor::Caller => "faults that met the caller's thread",
        Actor::WriteBack => "faults that met write-back's thread",
        Actor::Reclaimer => "faults that met the reclaimer's thread",
        Actor::Cleaner => "faults that met the cleaner's thread",
    }
}

fn faults_of(faults: Faults) -> &'static str {
    match faults {
        Faults::None => "runs with no fault",
        Faults::Syncs => "runs with failing syncs",
        Faults::All => "runs with faults of every kind",
    }
}

fn boundaries_during(doing: Doing) -> &'static str {
    match doing {
        Doing::Nothing => "write boundaries during nothing that commits",
        Doing::Open => "write boundaries during an open",
        Doing::Commit => "write boundaries during the commit of a block",
        Doing::Beside => "write boundaries during a write with no transaction",
        Doing::DropsFlushed => "write boundaries during a flush of pending drops",
        Doing::Flush => "write boundaries during a flush",
        Doing::Checkpoint => "write boundaries during the making of a checkpoint",
        Doing::DeleteCheckpoint => "write boundaries during the deletion of a checkpoint",
        Doing::Wipe => "write boundaries during a wipe",
        Doing::Close => "write boundaries during a close",
    }
}

fn states_during(doing: Doing) -> &'static str {
    match doing {
        Doing::Nothing => "states of a crash during nothing that commits",
        Doing::Open => "states of a crash during an open",
        Doing::Commit => "states of a crash during the commit of a block",
        Doing::Beside => "states of a crash during a write with no transaction",
        Doing::DropsFlushed => "states of a crash during a flush of pending drops",
        Doing::Flush => "states of a crash during a flush",
        Doing::Checkpoint => "states of a crash during the making of a checkpoint",
        Doing::DeleteCheckpoint => "states of a crash during the deletion of a checkpoint",
        Doing::Wipe => "states of a crash during a wipe",
        Doing::Close => "states of a crash during a close",
    }
}

/// A state that was recovered before, held to what is expected now: the
/// version it opened at is one that is allowed now, what it held is what
/// the model has at that version now, and its checkpoints are as they
/// were found.
fn again_allowed(rules: Rules<'_>, expect: &Expect, seen: &Seen) -> Result<Landing, Wrong> {
    let again = |said: String| {
        let said = format!("a state that was recovered before is left again, and {said}");
        Violation::new(Kind::NotCommitted, said)
    };
    // What is found of the database is of one at the version it opened
    // at.
    let of_the_database = |said: String| Wrong {
        version: Some(seen.version),
        ..Wrong::from(again(said))
    };
    let landing = (rules.history.find(expect, seen.version)).map_err(of_the_database)?;
    if let Some(said) = not_the_block(rules, expect, landing, seen.version, &seen.state) {
        return Err(of_the_database(said));
    }
    for checkpoint in &expect.checkpoints {
        let found = (seen.checkpoints.iter()).find(|(dir, _)| *dir == checkpoint.dir);
        // A state that was recovered before the checkpoint was begun has
        // no directory of it.
        let held = found.map_or(Held::Gone, |(_, held)| *held);
        let kind = match (checkpoint.standing, held) {
            (Standing::Deleted, Held::Gone) => None,
            (Standing::Deleted, _) => Some(Kind::NotCommitted),
            (Standing::Left(left), held) => (held != left).then_some(Kind::NotCommitted),
            (_, Held::Whole) => None,
            (Standing::Made, _) => Some(Kind::NotRecovered),
            (Standing::Building, Held::Nothing | Held::Gone) => None,
            (Standing::Building, Held::Marked) => Some(Kind::NotRecovered),
            (Standing::Deleting, _) => None,
        };
        if let Some(kind) = kind {
            let said = format!(
                "a state that was recovered before is left again, and the checkpoint in {} is \
                 {:?} in it, while it is {:?}",
                checkpoint.dir.display(),
                held,
                checkpoint.standing
            );
            // A checkpoint that was made and is not whole, or one that was
            // deleted and is there.
            let of_a_call_that_returned =
                matches!(checkpoint.standing, Standing::Made | Standing::Deleted);
            return Err(Wrong {
                checkpoint: of_a_call_that_returned,
                ..Wrong::from(Violation::new(kind, said))
            });
        }
    }
    Ok(landing)
}

/// Writes the files of `state` to a new directory of the machine, `to` or
/// the first of `to-2`, `to-3` and so on that is not there, with what
/// `text` says of that directory beside them, in `VIOLATION.txt`: what a
/// violation is looked into with. Returns the directory.
///
/// Two names of one file, which a checkpoint and its database can have,
/// are two names of one file there too. `state` is changed by this: it is
/// a copy that the caller keeps for nothing else.
fn save(state: &SimVfs, to: &Path, text: impl FnOnce(&Path) -> String) -> std::io::Result<PathBuf> {
    if let Some(above) = to.parent() {
        std::fs::create_dir_all(above)?;
    }
    let mut dir = to.to_path_buf();
    let mut number = 1;
    while let Err(error) = std::fs::create_dir(&dir) {
        if error.kind() != std::io::ErrorKind::AlreadyExists || number >= 1000 {
            return Err(error);
        }
        number += 1;
        dir = PathBuf::from(format!("{}-{number}", to.display()));
    }
    std::fs::write(dir.join("VIOLATION.txt"), text(&dir))?;
    let mut files = Vec::new();
    let mut open = vec![PathBuf::from("/")];
    while let Some(from) = open.pop() {
        let below = dir.join(from.strip_prefix("/").unwrap_or(&from));
        std::fs::create_dir_all(&below)?;
        for entry in state.read_dir(&from)? {
            let path = from.join(&entry.name);
            match entry.kind {
                EntryKind::Dir => open.push(path),
                EntryKind::File => files.push(path),
                EntryKind::Other => {}
            }
        }
    }
    files.sort();
    let on_disk = |path: &Path| dir.join(path.strip_prefix("/").unwrap_or(path));
    let len = |path: &Path| state.open(path, OpenMode::Read)?.len();
    let mut written: HashSet<PathBuf> = HashSet::new();
    for (at, path) in files.iter().enumerate() {
        if written.contains(path) {
            continue;
        }
        let file = state.open(path, OpenMode::Read)?;
        let mut bytes = vec![0; file.len()? as usize];
        file.read_exact_at(&mut bytes, 0)?;
        drop(file);
        std::fs::write(on_disk(path), bytes)?;
        // The other names of the file: those whose file grows when this
        // one does.
        let others: Vec<&PathBuf> = (files[at + 1..].iter())
            .filter(|other| !written.contains(*other))
            .collect();
        let before = (others.iter().map(|other| len(other))).collect::<Result<Vec<_>, _>>()?;
        state
            .open(path, OpenMode::Append(Create::Never))?
            .append(&[0])?;
        for (other, before) in others.into_iter().zip(before) {
            if len(other)? != before {
                std::fs::hard_link(on_disk(path), on_disk(other))?;
                written.insert(other.clone());
            }
        }
    }
    Ok(dir)
}

/// Runs the run of `seed`.
pub fn run(seed: u64, shape: Shape, save: Option<PathBuf>) -> RunReport {
    catch_unwind(AssertUnwindSafe(|| Run::new(seed, shape, save).run())).unwrap_or_else(|panic| {
        let said = format!("the run panicked: {}", panic_text(&*panic));
        let mut tally = Tally::default();
        tally.count("runs", 1);
        tally.count("violations", 1);
        RunReport {
            seed,
            tally,
            violations: vec![Found::of(seed, 0, 0, Violation::new(Kind::Panicked, said))],
            ..RunReport::default()
        }
    })
}

/// How a campaign is run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The seed of the first run. Run *n* has the seed after that of run
    /// *n* - 1.
    pub seed: u64,
    /// How many runs the campaign has at most.
    pub runs: Option<u64>,
    /// The campaign ends with the run that brings the distinct crash
    /// states to this many.
    pub points: Option<u64>,
    /// No run is begun once the campaign has lasted this long.
    pub duration: Option<Duration>,
    /// How many runs are made at once.
    pub jobs: usize,
    /// How each run goes.
    pub shape: Shape,
    /// The directory of the machine that the files of a state are written
    /// to in which a violation was found.
    pub save: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            seed: 1,
            runs: Some(1),
            points: None,
            duration: None,
            jobs: 1,
            shape: Shape::default(),
            save: None,
        }
    }
}

/// What a campaign came to.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// The seed of the first run.
    pub first_seed: u64,
    /// How many runs were made: the seeds from the first on.
    pub runs: u64,
    /// The distinct crash states that were recovered, over all runs: by
    /// their fingerprints.
    pub points: u64,
    /// The counts of every run, added up.
    pub tally: Tally,
    /// What was found.
    pub violations: Vec<Found>,
    /// How long the campaign took.
    pub elapsed: Duration,
    /// The digests of the runs, hashed in the order of their seeds
    /// ([`RunReport::digest`]).
    pub digest: u64,
    /// Where the planted defect acted, in every run ([`RunReport::acts`]).
    pub acts: Vec<Act>,
    /// How many runs found a violation that were begun before the
    /// campaign reached its end and ended after it. Their violations are
    /// among [`Self::violations`], and nothing else of them is counted.
    pub beyond: u64,
}

impl Report {
    /// Whether nothing was found.
    pub fn clean(&self) -> bool {
        self.violations.is_empty()
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let last = (self.first_seed).wrapping_add(self.runs.saturating_sub(1));
        writeln!(
            f,
            "{} runs (seeds {} to {last}), {} distinct crash states recovered, {} violations, \
             {:.1} s",
            self.runs,
            self.first_seed,
            self.points,
            self.violations.len(),
            self.elapsed.as_secs_f64()
        )?;
        for (name, count) in self.tally.counts() {
            writeln!(f, "  {count:>12}  {name}")?;
        }
        for found in &self.violations {
            writeln!(f, "VIOLATION: {found}")?;
        }
        if self.beyond > 0 {
            writeln!(
                f,
                "{} of the runs that found a violation are after the last run that is counted",
                self.beyond
            )?;
        }
        write!(f, "digest of the blocks {:016x}", self.digest)
    }
}

/// Tells the runs that are under way to be the last, when it goes.
struct StopAtEnd<'s>(&'s AtomicBool);

impl Drop for StopAtEnd<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Runs a campaign: the runs of the seeds from [`Settings::seed`] on, until
/// one of the ends that `settings` names is reached. `progress` is told of
/// each run as it is taken in, in the order of the seeds.
///
/// Runs are taken in in the order of their seeds whatever the order they
/// end in, and the campaign ends with the first run that reaches an end. A
/// run that was begun before that and ends after it is not counted, and a
/// violation that it found is the campaign's ([`Report::beyond`]).
///
/// The streams of each run are a function of its seed and of the shape.
/// Where the changes of the background threads fall among the caller's is
/// not, nor which thread a fault meets, so two campaigns of one seed can
/// recover other crash states, count other numbers of them, and, where
/// faults are injected or a lineage begins with a state that a crash
/// left, make other operations.
pub fn campaign(settings: &Settings, progress: &mut dyn FnMut(&RunReport, u64)) -> Report {
    let began = Instant::now();
    let next = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let finished: Mutex<BTreeMap<u64, RunReport>> = Mutex::default();
    let mut taking = Taking::new(settings.seed);
    let over = |began: &Instant| {
        settings
            .duration
            .is_some_and(|limit| began.elapsed() >= limit)
    };

    std::thread::scope(|scope| {
        let work = || {
            while !stop.load(Ordering::SeqCst) && !over(&began) {
                let index = next.fetch_add(1, Ordering::SeqCst);
                if settings.runs.is_some_and(|runs| index >= runs) {
                    break;
                }
                let seed = settings.seed.wrapping_add(index);
                let done = run(seed, settings.shape, settings.save.clone());
                finished
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(index, done);
            }
        };
        let _stop = StopAtEnd(&stop);
        // GroveDB recurses once for each level of a grove.
        let workers: Vec<_> = (0..settings.jobs.max(1))
            .map(|_| {
                std::thread::Builder::new()
                    .stack_size(STACK_BYTES)
                    .spawn_scoped(scope, work)
                    .expect("a thread for the runs")
            })
            .collect();
        let mut wanted = 0;
        loop {
            let idle = workers.iter().all(|worker| worker.is_finished());
            let done = finished
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&wanted);
            let Some(done) = done else {
                if idle {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
                continue;
            };
            wanted += 1;
            let counted = !stop.load(Ordering::SeqCst);
            if taking.take(&done, counted) {
                progress(&done, taking.report.points);
            }
            let points = taking.report.points;
            let reached = settings.points.is_some_and(|goal| points >= goal);
            if counted && (reached || !done.violations.is_empty()) {
                stop.store(true, Ordering::SeqCst);
            }
        }
    });
    let mut report = taking.report;
    let bytes = taking.digest.finalize();
    report.digest = u64::from_le_bytes(bytes.as_bytes()[..8].try_into().expect("eight bytes"));
    report.elapsed = began.elapsed();
    report
}

/// What a campaign has taken in of its runs.
struct Taking {
    report: Report,
    points: HashSet<u64>,
    digest: blake3::Hasher,
}

impl Taking {
    fn new(first_seed: u64) -> Self {
        Self {
            report: Report {
                first_seed,
                ..Report::default()
            },
            points: HashSet::new(),
            digest: blake3::Hasher::new(),
        }
    }

    /// Takes in the run `done`, and says whether it took anything. A run
    /// that is `counted` is taken in whole. One that is not was begun
    /// before the campaign reached its end and ended after it: nothing of
    /// it is counted, and what it found is not lost, so the campaign is
    /// not clean.
    fn take(&mut self, done: &RunReport, counted: bool) -> bool {
        let report = &mut self.report;
        if !counted && done.violations.is_empty() {
            return false;
        }
        report.violations.extend(done.violations.iter().cloned());
        report.acts.extend(done.acts.iter().copied());
        if !counted {
            report.beyond += 1;
            return true;
        }
        report.runs += 1;
        report.tally.add(&done.tally);
        self.points.extend(done.fingerprints.iter().copied());
        self.digest.update(&done.digest.to_le_bytes());
        report.points = self.points.len() as u64;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::state::{Keyspace, Listing, Record};

    /// An image of one file with thirty unsynced writes of a few sectors.
    fn image() -> CrashImage {
        let fs = SimVfs::new(SimConfig::new(9));
        let mode = OpenMode::Write(Create::New);
        let file = fs.open(Path::new("/pages"), mode).expect("opened");
        fs.sync_dir(Path::new("/")).expect("synced");
        for write in 0..30u64 {
            let bytes = vec![write as u8; 1500];
            file.write_at(&bytes, write * 1000).expect("written");
        }
        fs.crash_image()
    }

    #[test]
    fn a_plan_leaves_all_whose_state_is_the_one_with_every_change() {
        let fs = SimVfs::new(SimConfig::new(9));
        fs.create_dir(Path::new("/db")).expect("made");
        fs.sync_dir(Path::new("/")).expect("synced");
        let nothing = fs.crash_image();
        assert!(leaves_all(&nothing, &nothing.lose_all()));
        // Two names, the second of which builds on the first, and two
        // writes of one byte.
        let mode = OpenMode::Write(Create::New);
        let file = fs.open(Path::new("/db/tmp"), mode).expect("opened");
        fs.rename(Path::new("/db/tmp"), Path::new("/db/file"))
            .expect("renamed");
        file.write_at(b"a", 0).expect("written");
        file.write_at(b"b", 0).expect("written");
        let image = fs.crash_image();
        let all = image.keep_all();
        assert_eq!((image.names().len(), all.changes.len()), (2, 2));
        assert!(leaves_all(&image, &all));
        assert!(!leaves_all(&image, &image.lose_all()));
        let with = |names: &[usize], changes: Vec<lignin_io::sim::Survivor>| CrashPlan {
            names: names.to_vec(),
            changes,
        };
        // The rename alone keeps the name it builds on, and the second
        // write alone leaves the byte that both leave: other plans, and
        // the same state.
        assert_eq!(image.names_kept(&with(&[1], Vec::new())), [0, 1]);
        assert!(leaves_all(&image, &with(&[1], all.changes.clone())));
        assert!(leaves_all(
            &image,
            &with(&[0, 1], all.changes[1..].to_vec())
        ));
        // The first name alone, the first write alone, and both writes
        // with the first applied last: other states.
        assert!(!leaves_all(&image, &with(&[0], all.changes.clone())));
        assert!(!leaves_all(
            &image,
            &with(&[0, 1], all.changes[..1].to_vec())
        ));
        let other_order = all.changes.iter().rev().cloned().collect();
        assert!(!leaves_all(&image, &with(&[0, 1], other_order)));
        // A plan that does not fit the image leaves no state.
        assert!(!leaves_all(&nothing, &all));
    }

    #[test]
    fn of_an_image_with_many_plans_the_ends_the_newest_and_a_spread_are_taken() {
        let image = image();
        let (durable, whole) = (image.lose_all(), image.keep_all());
        let every = Shape {
            max_plans: 0,
            ..Shape::default()
        };
        let all = chosen(&image, &every);
        assert_eq!(all, plans_of(&image, every.every_plan_limit, every.samples));
        assert!(all.len() > 100, "{} plans", all.len());
        assert_eq!(all[0], durable);

        let in_order: Vec<CrashPlan> = image.prefixes().collect();
        assert_eq!(in_order.last(), Some(&whole));
        for most in [2, 3, 10, 24, 40] {
            let shape = Shape {
                max_plans: most,
                ..Shape::default()
            };
            let plans = chosen(&image, &shape);
            assert_eq!(plans.len(), most);
            assert_eq!((&plans[0], &plans[1]), (&durable, &whole));
            // The plans of the newest change, from the one nearest to
            // the image as it stands.
            let newest = NEWEST.min(most - 2);
            let nearest = in_order.iter().rev().skip(1).take(newest);
            assert!(plans[2..2 + newest].iter().eq(nearest), "{most}");
            // Each is a plan of the whole choice.
            for (at, plan) in plans.iter().enumerate() {
                assert!(all.contains(plan), "{most}: plan {at}");
            }
        }
        // The rest is spread over the whole choice: with room for fourteen
        // of them, one of the changes lost alone is among them.
        let shape = Shape::default();
        assert_eq!(shape.max_plans, 24);
        let holes: Vec<CrashPlan> = image.holes().collect();
        let plans = chosen(&image, &shape);
        assert!(plans.iter().any(|plan| holes.contains(plan)));

        // An image with few plans has them all.
        let small = Shape {
            max_plans: all.len(),
            ..Shape::default()
        };
        assert_eq!(chosen(&image, &small), all);
    }

    fn state(root: u8) -> State {
        State::new([root; 32], Listing::default(), false)
    }

    fn block(version: u64, root: u8) -> Block {
        Block {
            first: version,
            version,
            state: state(root),
            resume: Position::default(),
            made: Made::Commit,
            inside: Vec::new(),
        }
    }

    /// A state that was recovered before is not opened again when a later
    /// crash leaves it, and is held to what is expected then: the block
    /// that is being made at its version can be another than the one it
    /// held, and its checkpoints have the standing of now.
    #[test]
    fn a_state_that_is_left_again_is_held_to_what_is_expected_then() {
        let history = History::new(block(2, 1));
        let places = Places::new();
        let left_hashes = AtomicU64::new(0);
        let rules = Rules {
            history: &history,
            places: &places,
            grove_version: GroveVersion::latest(),
            page_size: PageSize::MIN,
            left_hashes: &left_hashes,
        };
        // A state at version 3, which held the block of root 7.
        let seen = Seen {
            version: 3,
            state: Arc::new(state(7)),
            places: 0,
            checkpoints: Vec::new(),
        };
        let making = |root: u8| Expect {
            attempt: Some(Arc::new(block(3, root))),
            ..Expect::exactly(0)
        };
        let landed = again_allowed(rules, &making(7), &seen);
        assert_eq!(landed.expect("the block being made"), Landing::Attempt);
        let other = again_allowed(rules, &making(8), &seen).expect_err("another block");
        assert_eq!(other.violation.kind, Kind::NotCommitted);
        let said = other.violation.to_string();
        assert!(said.contains("recovered before is left again"), "{said}");
        assert!(
            said.contains("does not hold what that block published"),
            "{said}"
        );
        let none = again_allowed(rules, &Expect::exactly(0), &seen).expect_err("no block");
        assert!(none.violation.to_string().contains("which no block made"));

        // A checkpoint, by its standing now and by what the state held of
        // it: nothing where the state was recovered before it was begun.
        let dir = checkpoint_dir(0);
        let with = |standing: Standing, held: Option<Held>| {
            let expect = Expect {
                checkpoints: vec![Checkpoint {
                    dir: dir.clone(),
                    point: 0,
                    standing,
                }],
                ..Expect::exactly(0)
            };
            let seen = Seen {
                version: 2,
                state: Arc::new(state(1)),
                places: 0,
                checkpoints: held.map(|held| (dir.clone(), held)).into_iter().collect(),
            };
            again_allowed(rules, &expect, &seen)
        };
        assert!(with(Standing::Made, Some(Held::Whole)).is_ok());
        let lost = with(Standing::Made, None).expect_err("a checkpoint that was made");
        assert!(lost.checkpoint);
        assert_eq!(lost.violation.kind, Kind::NotRecovered);
        assert!(with(Standing::Building, None).is_ok());
        assert!(with(Standing::Building, Some(Held::Nothing)).is_ok());
        let marked = with(Standing::Building, Some(Held::Marked)).expect_err("marked");
        assert!(!marked.checkpoint);
        assert!(with(Standing::Deleting, Some(Held::Marked)).is_ok());
        assert!(with(Standing::Deleted, None).is_ok());
        let back = with(Standing::Deleted, Some(Held::Whole)).expect_err("it is back");
        assert!(back.checkpoint);
        assert_eq!(back.violation.kind, Kind::NotCommitted);
    }

    #[test]
    fn an_empty_grove_has_the_root_of_no_element_no_record_and_no_mark() {
        let empty = State::new(EMPTY_ROOT, Listing::default(), false);
        assert_eq!(not_empty(&empty), None);
        let rooted = not_empty(&state(1)).expect("a root hash");
        assert!(rooted.contains("its root hash is 0101"), "{rooted}");
        let record = Record {
            path: Vec::new(),
            secondary: None,
            keyspace: Keyspace::Aux,
            key: vec![1],
            value: vec![2],
            left_behind: false,
        };
        let listing = Listing {
            records: vec![record],
            ..Listing::default()
        };
        let holds = not_empty(&State::new(EMPTY_ROOT, listing, false)).expect("a record");
        assert!(holds.contains("it holds 1 records"), "{holds}");
        let marked = not_empty(&State::new(EMPTY_ROOT, Listing::default(), true));
        assert!(marked.expect("a mark").contains("the mark of a restore"));
    }

    fn found(seed: u64) -> Found {
        let violation = Violation::new(Kind::NotCommitted, "planted for the test");
        Found {
            boundary: Some((1, 9)),
            image: Some(12),
            doing: Some(Doing::Commit),
            ..Found::of(seed, 0, 3, violation)
        }
    }

    /// A run that was begun before the campaign reached its end, and ended
    /// after it, is not counted, and a violation it found is the
    /// campaign's: the campaign is not clean.
    #[test]
    fn what_a_run_found_that_ended_after_the_campaign_is_not_lost() {
        let run = |seed: u64, violations: Vec<Found>| {
            let mut tally = Tally::default();
            tally.count("runs", 1);
            RunReport {
                seed,
                tally,
                fingerprints: vec![seed, seed + 100],
                violations,
                digest: seed,
                ..RunReport::default()
            }
        };
        let mut taking = Taking::new(1);
        assert!(taking.take(&run(1, Vec::new()), true));
        assert!(taking.report.clean());
        // The campaign has reached its end: a clean run after it is
        // nothing to it.
        assert!(!taking.take(&run(2, Vec::new()), false));
        assert!(taking.take(&run(3, vec![found(3)]), false));
        let report = &taking.report;
        assert_eq!((report.runs, report.points, report.beyond), (1, 2, 1));
        assert_eq!(report.tally.get("runs"), 1);
        assert!(!report.clean());
        assert_eq!(report.violations[0].seed, 3);
        let said = report.to_string();
        assert!(
            said.contains("1 of the runs that found a violation are after"),
            "{said}"
        );
        // A run that is counted and found something is taken in whole.
        assert!(taking.take(&run(4, vec![found(4)]), true));
        assert_eq!((taking.report.runs, taking.report.points), (2, 4));
        assert_eq!(taking.report.violations.len(), 2);
    }

    /// The files of a state are saved to a directory that was not there,
    /// and two names of one file, as a checkpoint and its database have
    /// them, are two names of one file in it.
    #[cfg(unix)]
    #[test]
    fn a_saved_state_is_in_a_new_directory_with_the_names_of_one_file_as_one_file() {
        use std::os::unix::fs::MetadataExt;

        let fs = SimVfs::new(SimConfig::new(11));
        fs.create_dir(Path::new("/db")).expect("made");
        fs.create_dir(Path::new("/checkpoints")).expect("made");
        let write = |path: &str, bytes: &[u8]| {
            let mode = OpenMode::Write(Create::New);
            let file = fs.open(Path::new(path), mode).expect("opened");
            file.write_at(bytes, 0).expect("written");
        };
        write("/db/a", b"shared");
        write("/db/b", b"shared");
        write("/db/empty", b"");
        let link = |from: &str, to: &str| {
            fs.hard_link(Path::new(from), Path::new(to))
                .expect("linked");
        };
        link("/db/a", "/checkpoints/a");
        link("/db/a", "/checkpoints/a2");
        link("/db/empty", "/checkpoints/empty");
        let copy = || {
            let image = fs.crash_image();
            image
                .recover(&image.keep_all())
                .expect("a plan of the image")
        };

        let above = tempfile::tempdir().expect("a directory");
        let to = above.path().join("states").join("state");
        let said = |dir: &Path| format!("in {}\n", dir.display());
        let first = save(&copy(), &to, said).expect("saved");
        assert_eq!(first, to);
        let inode = |name: &str| std::fs::metadata(first.join(name)).expect("a file").ino();
        assert_eq!(inode("db/a"), inode("checkpoints/a"));
        assert_eq!(inode("db/a"), inode("checkpoints/a2"));
        assert_ne!(inode("db/a"), inode("db/b"));
        assert_eq!(inode("db/empty"), inode("checkpoints/empty"));
        assert_ne!(inode("db/empty"), inode("db/a"));
        let read = |name: &str| std::fs::read(first.join(name)).expect("read");
        assert_eq!(read("db/a"), b"shared");
        assert_eq!(read("checkpoints/a2"), b"shared");
        assert_eq!(read("db/b"), b"shared");
        assert_eq!(read("checkpoints/empty"), b"");
        assert_eq!(read("VIOLATION.txt"), said(&first).into_bytes());

        // The name is taken now: the next state gets another directory,
        // and what is said of it names that one.
        let second = save(&copy(), &to, said).expect("saved");
        assert_eq!(second, above.path().join("states").join("state-2"));
        assert_eq!(read("VIOLATION.txt"), said(&first).into_bytes());
        let again = std::fs::read(second.join("VIOLATION.txt")).expect("read");
        assert_eq!(again, said(&second).into_bytes());
    }
}
