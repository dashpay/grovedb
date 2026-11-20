//! The campaign: runs of random workloads, each crashed at every write
//! boundary.
//!
//! A **run** is everything one seed gives. It begins with an empty
//! directory on a simulated file system and is a tree of **lineages**. A
//! lineage is one file system and what the store on it goes through, one
//! session after another: the session is killed or closed and the store is
//! opened again, as often as the workload says. After every operation of a
//! lineage the crash images that the [`Observer`] took meanwhile are gone
//! through. Each image gives the states its crash plans leave, each state
//! that was not seen before is recovered and held to the model
//! ([`check_state`]), and a few of them are kept. Each state that was kept
//! begins a lineage of its own when its lineage has ended: the store that
//! the crash left goes on, and is crashed again.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, PoisonError,
    },
    time::{Duration, Instant},
};

use lignin_format::PageId;
use lignin_io::{
    sim::{
        Chance, CrashImage, CrashPlan, Fault, FaultOdds, FaultRule, OpSet, SimConfig, SimVfs,
        UnknownFate,
    },
    Vfs,
};
use lignin_store::flush::FlushConfig;

use super::{
    check::{check_again, check_state, db_dir, expected_ids, Kind, Recovered, Violation},
    model::{self, apply, Change, Expect, History, Landing, Point},
    observe::{Observer, Sabotage, Taken},
    store::{Committed, OpenError, Reclaim, Session, Stepped, Tuning},
    workload::{Op, Shape, Situation, Workload},
};
use crate::workload::rng::Rng;

/// The faults a run injects into the file layer under its sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Faults {
    /// None: the store meets crashes only.
    None,
    /// Syncs fail with `EIO`, of files and of directories.
    Syncs,
    /// Every operation can fail with `EIO`, and those that need space with
    /// `ENOSPC`; an append that fails can leave its bytes; and the
    /// workload fills the device for a while.
    All,
}

impl Faults {
    /// The faults of the run of `seed`: none for two seeds in four.
    pub fn of_seed(seed: u64) -> Self {
        match seed % 4 {
            0 | 1 => Self::None,
            2 => Self::Syncs,
            _ => Self::All,
        }
    }

    fn odds(self) -> FaultOdds {
        match self {
            Self::None => FaultOdds::NONE,
            Self::Syncs => FaultOdds {
                eio: Chance::one_in(40),
                eio_ops: OpSet::SYNCS,
                ..FaultOdds::NONE
            },
            Self::All => FaultOdds {
                eio: Chance::one_in(500),
                eio_ops: OpSet::ALL,
                enospc: Chance::one_in(400),
                enospc_ops: OpSet::ALL,
                torn_append: Chance::one_in(3),
            },
        }
    }

    /// The name the command line knows the faults by.
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Syncs => "syncs",
            Self::All => "all",
        }
    }
}

/// What becomes of the changes a failed sync left unknown, in the run of
/// `seed`: lost, kept, or drawn, by turns.
pub fn unknown_of_seed(seed: u64) -> UnknownFate {
    match (seed / 4) % 3 {
        0 => UnknownFate::Lost,
        1 => UnknownFate::Kept,
        _ => UnknownFate::Drawn(seed),
    }
}

/// A defect planted for a run that must find it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Planted {
    /// The file layer reports a success it did not deliver.
    File(Sabotage),
    /// The model does not learn of the first change of a key-value hash
    /// that each commit makes: what the campaign compares with is wrong by
    /// one hash.
    ForgottenHash,
}

/// How one run goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunShape {
    /// How many operations a lineage has.
    pub ops: usize,
    /// The most lineages of one run: the first, and those that go on from
    /// a state a crash left.
    pub lineages: usize,
    /// How many crashes deep a lineage can be: 0 for the first.
    pub depth: usize,
    /// How many of the states of a lineage begin a lineage of their own.
    pub kept: usize,
    /// Every plan of an image is taken if there are at most this many.
    pub every_plan_limit: usize,
    /// An image with more plans gets the two ends, the plans of a file
    /// system that writes in order with every tear of the first lost
    /// write, each change lost alone, each change applied late, and this
    /// many plans drawn from the seed.
    pub samples: u64,
    /// One state in this many is recovered a second time, goes on, is
    /// closed and is opened again ([`check_again`]). 0 for none.
    pub again_one_in: u64,
    /// The faults of the run, or `None` for those of its seed
    /// ([`Faults::of_seed`]).
    pub faults: Option<Faults>,
    /// How large the stores get.
    pub shape: Shape,
    /// A defect to find.
    pub planted: Option<Planted>,
}

impl Default for RunShape {
    fn default() -> Self {
        Self {
            ops: 60,
            lineages: 5,
            depth: 2,
            kept: 2,
            every_plan_limit: 48,
            samples: 3,
            again_one_in: 8,
            faults: None,
            shape: Shape::default(),
            planted: None,
        }
    }
}

/// Counts of what a campaign did, by name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tally(BTreeMap<&'static str, u64>);

impl Tally {
    /// Adds `by` to the count `name`.
    pub fn count(&mut self, name: &'static str, by: u64) {
        if by > 0 {
            *self.0.entry(name).or_default() += by;
        }
    }

    /// The count `name`.
    pub fn get(&self, name: &str) -> u64 {
        self.0.get(name).copied().unwrap_or(0)
    }

    /// Adds every count of `other`.
    pub fn add(&mut self, other: &Self) {
        for (name, count) in &other.0 {
            self.count(name, *count);
        }
    }

    /// The counts, by name.
    pub fn counts(&self) -> impl Iterator<Item = (&'static str, u64)> + '_ {
        self.0.iter().map(|(name, count)| (*name, *count))
    }
}

/// A violation, with where in its run it was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// The seed of the run.
    pub seed: u64,
    /// The lineage of the run, from 0.
    pub lineage: u64,
    /// How many operations the lineage had made.
    pub op: usize,
    /// The write boundary of the crash, if a crash state was at fault.
    pub boundary: Option<u64>,
    /// The crash plan that left the state.
    pub plan: Option<String>,
    /// What was found.
    pub violation: Violation,
    /// The last operations of the lineage, the newest last.
    pub trail: Vec<String>,
}

impl fmt::Display for Found {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "seed {}, lineage {}, after operation {}",
            self.seed, self.lineage, self.op
        )?;
        if let Some(boundary) = self.boundary {
            write!(f, ", a crash at write boundary {boundary}")?;
        }
        writeln!(f, ": {}", self.violation)?;
        if let Some(plan) = &self.plan {
            writeln!(f, "  the crash kept: {plan}")?;
        }
        write!(f, "  the last operations: {}", self.trail.join(", "))
    }
}

/// What one run came to.
#[derive(Clone, Debug, Default)]
pub struct RunReport {
    /// The seed.
    pub seed: u64,
    /// The counts.
    pub tally: Tally,
    /// The fingerprint of every state a crash of the run left that was
    /// recovered: each once.
    pub fingerprints: Vec<u64>,
    /// What was found.
    pub violations: Vec<Found>,
    /// A number that two runs which did the same have in common: the
    /// states in the order they were recovered, where each landed, and the
    /// schedule of each file system.
    pub digest: u64,
}

struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn u64(&mut self, value: u64) {
        for byte in value.to_le_bytes() {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

/// A state that was recovered and will begin a lineage of its own.
struct Kept {
    image: CrashImage,
    plan: CrashPlan,
    history: History,
    created: bool,
    depth: usize,
}

/// The states one crash can leave, as the campaign picks them.
fn plans_of(image: &CrashImage, shape: &RunShape) -> Vec<CrashPlan> {
    if let Some(plans) = image.every_plan(shape.every_plan_limit) {
        return plans;
    }
    let mut plans = vec![image.lose_all(), image.keep_all()];
    plans.extend(image.prefixes());
    plans.extend(image.holes());
    plans.extend(image.reorders());
    plans.extend((0..shape.samples).map(|draw| image.sample(draw)));
    plans
}

/// One run: its seed, its shape, and what it has come to so far.
struct Run {
    seed: u64,
    shape: RunShape,
    faults: Faults,
    tally: Tally,
    fingerprints: HashSet<u64>,
    order: Vec<u64>,
    violations: Vec<Found>,
    digest: Fnv,
    waiting: Vec<Kept>,
    lineages: u64,
}

/// One file system, and the store on it from session to session.
struct Lineage {
    index: u64,
    depth: usize,
    observer: Observer,
    history: History,
    workload: Workload,
    /// What the campaign decides by chance for this lineage, apart from
    /// the workload's own draws.
    chance: Rng,
    /// How many sessions the lineage has had: each has a tuning of its
    /// own.
    sessions: u64,
    session: Option<Session>,
    held: Option<BTreeSet<PageId>>,
    pinned: bool,
    full: bool,
    /// Where each state of this file system that was recovered landed.
    seen: HashMap<u64, Landing>,
    /// The items that were in doubt and are points now, with their points:
    /// a state that was found at such an item is at that point.
    settled: Vec<(Arc<Point>, usize)>,
    /// How many states were candidates for being kept.
    candidates: u64,
    kept: Vec<Kept>,
    ops: usize,
    trail: Vec<String>,
}

/// The most violations a run reports: one defect shows in many states.
const MAX_VIOLATIONS: usize = 4;

/// How many operations the trail of a violation shows.
const TRAIL: usize = 24;

impl Run {
    fn new(seed: u64, shape: RunShape) -> Self {
        Self {
            seed,
            shape,
            faults: shape.faults.unwrap_or(Faults::of_seed(seed)),
            tally: Tally::default(),
            fingerprints: HashSet::new(),
            order: Vec::new(),
            violations: Vec::new(),
            digest: Fnv::new(),
            waiting: Vec::new(),
            lineages: 0,
        }
    }

    fn done(&self) -> bool {
        self.violations.len() >= MAX_VIOLATIONS
    }

    fn found(&mut self, lineage: &Lineage, violation: Violation, crash: Option<(u64, &CrashPlan)>) {
        self.tally.count("violations", 1);
        if self.done() {
            return;
        }
        self.violations.push(Found {
            seed: self.seed,
            lineage: lineage.index,
            op: lineage.ops,
            boundary: crash.map(|(boundary, _)| boundary),
            plan: crash.map(|(_, plan)| format!("{plan:?}")),
            violation,
            trail: lineage.trail.clone(),
        });
    }

    /// The tuning of the stores of one lineage: by turns the pages a
    /// replay keeps, the window a recovery reads the log through, the pool,
    /// and the runs a chain may have, so that one session finds chains
    /// that another's thresholds allowed.
    fn tuning(&self, turn: u64) -> Tuning {
        let small = Tuning::small();
        let max_chain_deltas = [2, 0, 5][((turn / 12) % 3) as usize];
        Tuning {
            max_replay_pages: [None, Some(1), Some(3)][(turn % 3) as usize],
            read_window: [small.read_window, 4 << 10][((turn / 3) % 2) as usize],
            pool_capacity: [small.pool_capacity, 96 << 10][((turn / 6) % 2) as usize],
            flush: FlushConfig {
                max_chain_deltas,
                ..small.flush
            },
            ..small
        }
    }

    /// The first lineage of the run: an empty directory, whose name is
    /// durable.
    fn first(&mut self) -> Lineage {
        let config = SimConfig {
            sector_size: [512, 512, 4096, 128][((self.seed / 12) % 4) as usize],
            unknown: unknown_of_seed(self.seed),
            ..SimConfig::new(self.seed)
        };
        let fs = SimVfs::new(config);
        fs.create_dir(db_dir()).expect("a directory");
        fs.sync_dir(std::path::Path::new("/"))
            .expect("the directory's name is durable");
        self.lineage(fs, History::new(), Expect::of_new_store(), 0)
    }

    fn lineage(&mut self, fs: SimVfs, history: History, expect: Expect, depth: usize) -> Lineage {
        let index = self.lineages;
        self.lineages += 1;
        self.tally.count("lineages", 1);
        let observer = match self.shape.planted {
            Some(Planted::File(sabotage)) => Observer::sabotaged(fs, expect, sabotage),
            _ => Observer::new(fs, expect),
        };
        Lineage {
            index,
            depth,
            observer,
            history,
            workload: Workload::new(self.seed, index, self.shape.shape),
            chance: Rng::derive(self.seed, 0x6b65_7074, index),
            sessions: 0,
            session: None,
            held: None,
            pinned: false,
            full: false,
            seen: HashMap::new(),
            settled: Vec::new(),
            candidates: 0,
            kept: Vec::new(),
            ops: 0,
            trail: Vec::new(),
        }
    }

    /// Runs the whole run.
    fn run(mut self) -> RunReport {
        let first = self.first();
        self.live(first);
        while let Some(kept) = self.waiting.pop() {
            if self.done() || self.lineages >= self.shape.lineages as u64 {
                break;
            }
            let fs = kept
                .image
                .recover(&kept.plan)
                .expect("the plan was recovered from before");
            let expect = match kept.created {
                true => Expect::exactly(kept.history.latest()),
                false => Expect::of_new_store(),
            };
            let lineage = self.lineage(fs, kept.history, expect, kept.depth);
            self.live(lineage);
        }
        self.tally.count("runs", 1);
        self.tally.count(self.faults.name(), 1);
        RunReport {
            seed: self.seed,
            tally: self.tally,
            fingerprints: self.order,
            violations: self.violations,
            digest: self.digest.0,
        }
    }

    /// The life of one lineage: the store is made if the directory has
    /// none, opened, and put through the operations of its workload.
    fn live(&mut self, mut lineage: Lineage) {
        let fs = lineage.observer.fs().clone();
        if !lineage.observer.expect().created {
            // The store is made with no fault injected: a session that
            // cannot make its store has nothing to crash.
            fs.set_faults(FaultOdds::NONE);
            if let Err(error) = Session::create(lineage.observer.vfs(), db_dir()) {
                let violation =
                    Violation::new(Kind::Session, format!("no store was made: {error}"));
                self.found(&lineage, violation, None);
                return;
            }
            lineage.observer.expecting(|expect| expect.created = true);
            self.check_images(&mut lineage);
        }
        fs.set_faults(self.faults.odds());
        if !self.open(&mut lineage, "open") {
            return;
        }
        while lineage.ops < self.shape.ops && !self.done() {
            let situation = Situation {
                held: lineage.held.as_ref(),
                pinned: lineage.pinned,
                faulty: self.faults == Faults::All,
                full: lineage.full,
            };
            let pages = lineage.history.last().pages.clone();
            let op = lineage.workload.next(&pages, situation);
            lineage.ops += 1;
            self.tally.count("operations", 1);
            if !self.step(&mut lineage, op) {
                break;
            }
        }
        self.end_session(&mut lineage);
        self.tally.count("faults injected", fs.stats().faults);
        self.digest.u64(fs.trace_digest());
        if lineage.depth < self.shape.depth {
            self.waiting.append(&mut lineage.kept);
        }
    }

    fn faulted(&self, lineage: &Lineage) -> bool {
        lineage.observer.fs().stats().faults > 0
    }

    /// What a session's call came to: an error is a violation unless a
    /// fault was injected into this file system, and is then counted.
    fn tolerate<T>(
        &mut self,
        lineage: &Lineage,
        what: &str,
        result: Result<T, String>,
    ) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) if self.faulted(lineage) => {
                self.tally.count("errors after a fault", 1);
                let _ = error;
                None
            }
            Err(error) => {
                let said = format!("{what} failed with no fault injected: {error}");
                self.found(lineage, Violation::new(Kind::Session, said), None);
                None
            }
        }
    }

    fn end_session(&mut self, lineage: &mut Lineage) {
        let Some(session) = lineage.session.take() else {
            return;
        };
        let stats = session.flush_stats();
        let tally = &mut self.tally;
        tally.count("table checkpoints", stats.checkpoints);
        tally.count("table checkpoints put off", stats.checkpoints_put_off);
        tally.count("log files dropped", stats.log_files_dropped);
        tally.count("base images written", stats.base_blocks);
        tally.count("delta runs written", stats.delta_blocks);
        tally.count("chains consolidated", stats.consolidations);
        tally.count("runs refused", stats.runs_refused);
        tally.count("values placed", stats.values_placed);
        tally.count("values given up", stats.values_given_up);
        tally.count("values regrouped", stats.values_regrouped);
        tally.count("page steps put off", stats.steps_put_off);
        tally.count("page versions held for readers", stats.held_for_readers);
        tally.count("links refused", stats.links_refused);
        lineage.held = None;
        lineage.pinned = false;
    }

    /// Opens the store of the lineage where its last session left it, with
    /// no crash in between: nothing is lost, so the store is at the last
    /// point, or at the item whose append was in doubt. Returns whether
    /// the lineage goes on.
    fn open(&mut self, lineage: &mut Lineage, why: &'static str) -> bool {
        self.end_session(lineage);
        let fs = lineage.observer.fs().clone();
        let vfs = lineage.observer.vfs();
        let turn = (self.seed.wrapping_mul(7))
            .wrapping_add(lineage.index * 5)
            .wrapping_add(lineage.sessions * 13);
        let tuning = self.tuning(turn);
        lineage.sessions += 1;
        let mut opened = Session::open(vfs.clone(), db_dir(), &tuning);
        if opened.is_err() && self.faulted(lineage) {
            // A recovery that a fault failed is made again
            // (`lignin_store::recovery`, "Twice").
            self.tally.count("recoveries failed by a fault", 1);
            fs.set_faults(FaultOdds::NONE);
            fs.clear_rules();
            lineage.full = false;
            opened = Session::open(vfs, db_dir(), &tuning);
            fs.set_faults(self.faults.odds());
        }
        self.tally.count("sessions", 1);
        let session = match opened {
            Ok(session) => session,
            Err(error) => {
                let said = format!("{why}: {error}");
                self.found(lineage, Violation::new(Kind::Session, said), None);
                self.check_images(lineage);
                return false;
            }
        };
        let expect = lineage.observer.expect();
        let exact = Expect {
            floor: expect.latest,
            ..expect.clone()
        };
        fs.set_faults(FaultOdds::NONE);
        let read = session.read(&expected_ids(&lineage.history, &exact));
        fs.set_faults(self.faults.odds());
        let landing =
            read.map_err(|error| Violation::new(Kind::Session, format!("{why}: {error}")));
        let landing = landing.and_then(|pages| {
            let landed = session.latest().get();
            (lineage.history.find(&exact, landed, &pages)).map_err(|mismatch| {
                let said = format!("{why}, with no crash: {mismatch}");
                Violation::new(Kind::Session, said)
            })
        });
        match landing {
            Ok(Landing::Attempt(point)) => {
                self.tally.count("items in doubt that were in the log", 1);
                lineage.history.push(Point {
                    end: session.durable(),
                    ..(*point).clone()
                });
                lineage.settled.push((point, lineage.history.latest()));
            }
            Ok(_) => {}
            Err(violation) => {
                self.found(lineage, violation, None);
                self.check_images(lineage);
                return false;
            }
        }
        let report = session.report();
        self.tally
            .count("commits replayed at an open", report.commits);
        let (latest, floor) = (
            lineage.history.latest(),
            lineage.history.floor(session.durable()),
        );
        lineage.observer.expecting(|expect| {
            expect.attempt = None;
            expect.latest = latest;
            expect.floor = expect.floor.max(floor);
        });
        lineage.session = Some(session);
        self.check_images(lineage);
        true
    }

    /// One operation of a lineage, and everything that is checked after
    /// it. Returns whether the lineage goes on.
    fn step(&mut self, lineage: &mut Lineage, op: Op) -> bool {
        lineage.trail.push(describe(&op));
        if lineage.trail.len() > TRAIL {
            lineage.trail.remove(0);
        }
        self.tally.count(op.name(), 1);
        let reopen = self.make(lineage, &op);
        let mut goes_on = true;
        if let Some(session) = &lineage.session {
            for complaint in session.complaints() {
                self.tolerate::<()>(lineage, "write-back", Err(complaint));
            }
        }
        let stopped = (lineage.session.as_ref()).is_some_and(|session| session.stopped());
        if let Some(why) = reopen.or(stopped.then_some("the log takes nothing more")) {
            goes_on = self.open(lineage, why);
        }
        if goes_on {
            self.after(lineage);
        }
        goes_on
    }

    /// What is checked after an operation: the store says what the model
    /// says, and the crash images taken meanwhile recover.
    fn after(&mut self, lineage: &mut Lineage) {
        let Some(session) = &lineage.session else {
            return;
        };
        let fs = lineage.observer.fs().clone();
        let floor = lineage.history.floor(session.durable());
        lineage
            .observer
            .expecting(|expect| expect.floor = expect.floor.max(floor));

        fs.set_faults(FaultOdds::NONE);
        let expected = lineage.history.last().pages.clone();
        let ids: BTreeSet<PageId> = expected.keys().copied().collect();
        let read = session.read(&ids);
        // The checker's scope asks for a store that is still: no write
        // transaction holds a page.
        let sound = match lineage.held {
            Some(_) => Ok(()),
            None => session.verify(),
        };
        fs.set_faults(self.faults.odds());
        let violation = match read {
            Ok(pages) if pages == *expected => sound.err(),
            Ok(pages) => Some(model::difference(&pages, &expected)),
            Err(error) => Some(error),
        };
        if let Some(said) = violation {
            let said = format!("the store that was not crashed: {said}");
            self.found(lineage, Violation::new(Kind::Session, said), None);
        }
        self.check_images(lineage);

        // What a sync made durable is not lost by a crash after it: the
        // state with no unsynced change is held to the floor as it is now.
        let now = lineage.observer.now();
        let plan = now.image.lose_all();
        self.check_plan(lineage, &now, plan);
    }

    /// Makes `op`. Returns why the store has to be opened again, if it
    /// has.
    fn make(&mut self, lineage: &mut Lineage, op: &Op) -> Option<&'static str> {
        let session = lineage.session.take()?;
        let reopen = self.make_with(lineage, &session, op);
        lineage.session = Some(session);
        if reopen.is_some() {
            self.end_session(lineage);
        }
        reopen
    }

    fn make_with(
        &mut self,
        lineage: &mut Lineage,
        session: &Session,
        op: &Op,
    ) -> Option<&'static str> {
        let fs = lineage.observer.fs().clone();
        match op {
            Op::Commit { changes, durable } => {
                let (committed, reopen) = self.commit(lineage, session, changes, false);
                if committed && *durable {
                    self.tolerate(lineage, "the sync of a commit", session.sync_log());
                }
                return reopen;
            }
            Op::CommitHeld(changes) => {
                lineage.held = None;
                return self.commit(lineage, session, changes, true).1;
            }
            Op::Hold(pages) => {
                let held = self.tolerate(lineage, "copying pages", session.hold(pages));
                if held == Some(true) {
                    lineage.held = Some(pages.iter().copied().collect());
                }
            }
            Op::Release => {
                session.release();
                lineage.held = None;
            }
            Op::Reclaim { page, what } => return self.reclaim(lineage, session, *page, what),
            Op::Pass => drop(self.tolerate(lineage, "a pass", session.pass())),
            Op::Tick => drop(self.tolerate(lineage, "a turn of write-back", session.tick())),
            Op::Checkpoint => {
                let covers = self.tolerate(lineage, "a table checkpoint", session.checkpoint());
                self.tally
                    .count("checkpoints asked for", u64::from(covers.is_some()));
            }
            Op::SyncLog => drop(self.tolerate(lineage, "a sync of the log", session.sync_log())),
            Op::RollLog => drop(self.tolerate(lineage, "a roll of the log", session.roll_log())),
            Op::Consolidate => {
                let done = self.tolerate(lineage, "consolidating", session.consolidate());
                self.tally
                    .count("long chains consolidated", done.unwrap_or(0) as u64);
            }
            Op::Clean => {
                let moved = self.tolerate(lineage, "cleaning a value-log file", session.clean(20));
                if let Some(Some(moved)) = moved {
                    self.tally.count("value-log files cleaned", 1);
                    self.tally.count("values moved by the cleaner", moved);
                }
            }
            Op::DropValueFiles => {
                let dropped = session.drop_empty_value_files();
                let dropped = self.tolerate(lineage, "dropping value-log files", dropped);
                self.tally
                    .count("value-log files dropped", dropped.unwrap_or(0) as u64);
            }
            Op::TrimFreed => {
                let trimmed = session.trim_freed() as u64;
                self.tally.count("chains of freed pages trimmed", trimmed);
            }
            Op::Pin => lineage.pinned |= session.pin(),
            Op::Unpin => {
                session.unpin();
                lineage.pinned = false;
            }
            Op::Kill => return Some("the store was killed and opened again"),
            Op::Close => {
                session.release();
                let ended = self.tolerate(lineage, "closing the store", session.finish());
                let latest = session.latest().get();
                if ended.is_some_and(|covers| covers != Some(latest)) && !self.faulted(lineage) {
                    let said = format!(
                        "the store was closed at version {latest}, and its last table \
                         checkpoint covers {ended:?}"
                    );
                    self.found(lineage, Violation::new(Kind::Session, said), None);
                }
                return Some("the store was closed and opened again");
            }
            Op::FailNext => fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::WRITES)),
            Op::DiskFull => {
                fs.add_rule(FaultRule::new(Fault::Enospc).forever());
                lineage.full = true;
            }
            Op::DiskFree => {
                fs.clear_rules();
                lineage.full = false;
            }
        }
        None
    }

    /// A commit of `changes`. The model is told what the commit makes of
    /// the pages before its group is appended, and takes it for done when
    /// the store says it is. Returns whether it is, and why the store has
    /// to be opened again, if it has.
    fn commit(
        &mut self,
        lineage: &mut Lineage,
        session: &Session,
        changes: &[Change],
        held: bool,
    ) -> (bool, Option<&'static str>) {
        let before = lineage.history.last().clone();
        let observer = lineage.observer.clone();
        let forgets = self.shape.planted == Some(Planted::ForgottenHash);
        let mut attempt = None;
        let mut misfit = None;
        let committed = session.commit(changes, held, &mut |made| {
            let mut known: Vec<Change> = changes.to_vec();
            if forgets {
                let hash = |change: &Change| matches!(change, Change::Hash { kv: true, .. });
                if let Some(at) = known.iter().position(hash) {
                    known.remove(at);
                }
            }
            match apply(&before.pages, &known, made) {
                Ok(pages) => {
                    let point = Arc::new(Point {
                        version: before.version + 1,
                        end: None,
                        pages: Arc::new(pages),
                    });
                    observer.expecting(|expect| expect.attempt = Some(point.clone()));
                    attempt = Some(point);
                }
                Err(error) => misfit = Some(error),
            }
        });
        if let Some(misfit) = misfit {
            self.found(
                lineage,
                Violation::new(Kind::Session, misfit.to_string()),
                None,
            );
            return (false, Some("the model did not take a commit"));
        }
        match committed {
            Committed::Yes { version, end, .. } => {
                let point = attempt.expect("the group was appended");
                if version != point.version {
                    let said = format!("a commit made version {version}, not {}", point.version);
                    self.found(lineage, Violation::new(Kind::Session, said), None);
                }
                lineage.history.push(Point {
                    version,
                    end: Some(end),
                    pages: point.pages.clone(),
                });
                let latest = lineage.history.latest();
                lineage.settled.push((point, latest));
                observer.expecting(|expect| {
                    expect.attempt = None;
                    expect.latest = latest;
                });
                self.tally.count("commits made", 1);
                (true, None)
            }
            Committed::No(reason) => {
                observer.expecting(|expect| expect.attempt = None);
                self.tally.count("commits that failed", 1);
                self.tolerate::<()>(lineage, "a commit", Err(reason));
                (false, None)
            }
            Committed::InDoubt(_) => {
                self.tally.count("commits left in doubt", 1);
                (false, Some("a commit was left in doubt"))
            }
            Committed::Broken(reason) => {
                self.found(lineage, Violation::new(Kind::Session, reason), None);
                (false, Some("a commit was not installed"))
            }
        }
    }

    fn reclaim(
        &mut self,
        lineage: &mut Lineage,
        session: &Session,
        page: PageId,
        what: &Reclaim,
    ) -> Option<&'static str> {
        let before = lineage.history.last().clone();
        let observer = lineage.observer.clone();
        let slots = match what {
            Reclaim::Cells(slots) => slots.clone(),
            Reclaim::Page => Vec::new(),
        };
        let mut attempt = None;
        let mut misfit = None;
        let stepped = session.reclaim_step(page, what, &mut || match model::reclaim(
            &before.pages,
            page,
            &slots,
        ) {
            Ok(pages) => {
                let point = Arc::new(Point {
                    version: before.version,
                    end: None,
                    pages: Arc::new(pages),
                });
                observer.expecting(|expect| expect.attempt = Some(point.clone()));
                attempt = Some(point);
            }
            Err(error) => misfit = Some(error),
        });
        if let Some(misfit) = misfit {
            self.found(
                lineage,
                Violation::new(Kind::Session, misfit.to_string()),
                None,
            );
            return Some("the model did not take a step");
        }
        match stepped {
            Stepped::Yes { end } => {
                let point = attempt.expect("the record was appended");
                lineage.history.push(Point {
                    end: Some(end),
                    ..(*point).clone()
                });
                let latest = lineage.history.latest();
                lineage.settled.push((point, latest));
                observer.expecting(|expect| {
                    expect.attempt = None;
                    expect.latest = latest;
                });
                self.tally.count("reclaiming steps made", 1);
                None
            }
            Stepped::Refused(_) => {
                observer.expecting(|expect| expect.attempt = None);
                self.tally.count("reclaiming steps refused", 1);
                None
            }
            Stepped::No(reason) => {
                observer.expecting(|expect| expect.attempt = None);
                self.tolerate::<()>(lineage, "a reclaiming step", Err(reason));
                None
            }
            Stepped::InDoubt(_) => {
                self.tally.count("steps left in doubt", 1);
                Some("a reclaiming step was left in doubt")
            }
            Stepped::Broken(reason) => {
                self.found(lineage, Violation::new(Kind::Session, reason), None);
                Some("a reclaiming step was not made")
            }
        }
    }

    /// Goes through the crash images taken since the last call: every
    /// state that their plans leave.
    fn check_images(&mut self, lineage: &mut Lineage) {
        for taken in lineage.observer.drain() {
            self.tally.count("write boundaries", 1);
            for plan in plans_of(&taken.image, &self.shape) {
                if self.done() {
                    return;
                }
                self.check_plan(lineage, &taken, plan);
            }
        }
    }

    /// One state: the one `plan` leaves of the crash `taken`.
    fn check_plan(&mut self, lineage: &mut Lineage, taken: &Taken, plan: CrashPlan) {
        self.tally.count("crash plans", 1);
        let state = (taken.image.recover(&plan)).expect("a plan of the image fits it");
        let fingerprint = state.fingerprint();
        if let Some(landing) = lineage.seen.get(&fingerprint) {
            self.tally.count("states seen before", 1);
            // A state that was found at an item whose append had not
            // returned is at the point that item became, once it has.
            let settled = |point: &Arc<Point>| {
                let same = |(item, _): &&(Arc<Point>, usize)| Arc::ptr_eq(item, point);
                lineage.settled.iter().find(same).map(|(_, at)| *at)
            };
            let landing = match landing {
                Landing::Attempt(point) if !landing.allowed(&taken.expect) => {
                    settled(point).map_or(landing.clone(), Landing::Point)
                }
                other => other.clone(),
            };
            if !landing.allowed(&taken.expect) {
                let said = format!(
                    "a state that recovers to {} is left where points {} to {} are expected",
                    at(&landing),
                    taken.expect.floor,
                    taken.expect.latest
                );
                let violation = Violation::new(Kind::NotCommitted, said);
                self.found(lineage, violation, Some((taken.boundary, &plan)));
            }
            return;
        }
        if self.fingerprints.insert(fingerprint) {
            self.order.push(fingerprint);
            self.tally.count("distinct crash states", 1);
        }
        self.tally.count("recoveries", 1);
        self.digest.u64(fingerprint);
        // The tuning of a recovery goes by turns as well.
        let tuning = self.tuning(fingerprint >> 7);
        if self.faults != Faults::None && (fingerprint >> 19) % 4 == 0 {
            self.recover_with_faults(lineage, taken, &plan, &state, &tuning);
        }
        let recovered = match check_state(&state, &lineage.history, &taken.expect, &tuning) {
            Ok(recovered) => recovered,
            Err(violation) => {
                self.digest.u64(u64::MAX);
                self.found(lineage, violation, Some((taken.boundary, &plan)));
                return;
            }
        };
        self.record(lineage, taken, &recovered);
        let Recovered { landing, pages, .. } = recovered;
        lineage.seen.insert(fingerprint, landing.clone());

        let again = self.shape.again_one_in;
        let landed = match &landing {
            Landing::NoStore => None,
            Landing::Point(at) => Some(lineage.history.point(*at).version),
            Landing::Attempt(point) => Some(point.version),
        };
        if let Some(landed) = landed.filter(|_| again > 0 && (fingerprint >> 11) % again == 0) {
            self.tally.count("states recovered again", 1);
            let other = self.tuning((fingerprint >> 7) + 1);
            if let Err(violation) = check_again(&state, &other, landed, &pages) {
                self.found(lineage, violation, Some((taken.boundary, &plan)));
            }
        }
        self.keep(lineage, taken, plan, &landing);
    }

    /// A recovery of `state` that the run's faults are injected into. If
    /// it returns a store, the store is at a point it may be at. If it
    /// fails, it says that the file layer failed and not that the store
    /// is damaged, and it leaves what the next recovery starts from: the
    /// caller recovers `state` again with no fault.
    fn recover_with_faults(
        &mut self,
        lineage: &Lineage,
        taken: &Taken,
        plan: &CrashPlan,
        state: &SimVfs,
        tuning: &Tuning,
    ) {
        state.set_faults(self.faults.odds());
        let vfs: Arc<dyn Vfs> = Arc::new(state.clone());
        let opened = Session::open(vfs, db_dir(), tuning);
        state.set_faults(FaultOdds::NONE);
        let violation = match opened {
            Ok(session) => {
                self.tally
                    .count("recoveries with faults that returned a store", 1);
                let ids = expected_ids(&lineage.history, &taken.expect);
                let landed = session.latest().get();
                match session.read(&ids) {
                    Ok(pages) => (lineage.history.find(&taken.expect, landed, &pages))
                        .err()
                        .map(|mismatch| Violation::new(Kind::NotCommitted, mismatch.to_string())),
                    Err(error) => Some(Violation::new(Kind::Inconsistent, error)),
                }
            }
            Err(OpenError::NoStore) => None,
            Err(OpenError::Failed { damage: false, .. }) => {
                self.tally.count("recoveries with faults that failed", 1);
                None
            }
            Err(error @ OpenError::Failed { damage: true, .. }) => {
                let said = format!("a recovery that only faults were injected into: {error}");
                Some(Violation::new(Kind::NotRecovered, said))
            }
        };
        if let Some(violation) = violation {
            self.found(lineage, violation, Some((taken.boundary, plan)));
        }
    }

    fn record(&mut self, lineage: &Lineage, taken: &Taken, recovered: &Recovered) {
        let tally = &mut self.tally;
        let expect = &taken.expect;
        match &recovered.landing {
            Landing::NoStore => {
                self.digest.u64(u64::MAX - 1);
                tally.count("states with no store yet", 1);
            }
            Landing::Attempt(_) => {
                self.digest.u64(u64::MAX - 2);
                tally.count("states at an item whose append had not returned", 1);
            }
            Landing::Point(at) => {
                self.digest.u64(*at as u64);
                tally.count(
                    "states above the durable floor",
                    u64::from(*at > expect.floor),
                );
                tally.count("states that lost commits", u64::from(*at < expect.latest));
                let step = lineage.history.point(*at).version
                    == lineage.history.point(at.saturating_sub(1)).version;
                tally.count("states at a reclaiming step", u64::from(step && *at > 0));
            }
        }
        if let Some(report) = &recovered.report {
            tally.count(
                "states from a table checkpoint",
                u64::from(report.covers.get() > 0),
            );
            tally.count(
                "states that replayed commits",
                u64::from(report.commits > 0),
            );
            tally.count("commits replayed", report.commits);
            tally.count("background changes replayed", report.steps);
            tally.count("changes dropped for a lost page", report.changes_dropped);
            tally.count("pages freed in replay", report.pages_freed);
            tally.count("pages spilled in replay", report.pages_spilled);
            tally.count("pages rebuilt", report.pages_rebuilt);
            tally.count("pending values recovered", report.pending_values);
        }
    }

    /// Keeps a state, perhaps, to begin a lineage of its own: each state
    /// of a lineage has the same chance of being one of those kept.
    fn keep(&mut self, lineage: &mut Lineage, taken: &Taken, plan: CrashPlan, landing: &Landing) {
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
        let history = match landing {
            Landing::NoStore => History::new(),
            Landing::Point(at) => lineage.history.up_to(*at),
            Landing::Attempt(point) => {
                let mut history = lineage.history.up_to(taken.expect.latest);
                history.push((**point).clone());
                history
            }
        };
        let kept = Kept {
            image: taken.image.clone(),
            plan,
            history,
            created: !matches!(landing, Landing::NoStore),
            depth: lineage.depth + 1,
        };
        match at < lineage.kept.len() {
            true => lineage.kept[at] = kept,
            false => lineage.kept.push(kept),
        }
    }
}

fn at(landing: &Landing) -> String {
    match landing {
        Landing::NoStore => "no store".to_string(),
        Landing::Point(at) => format!("point {at}"),
        Landing::Attempt(point) => format!("the item in doubt of version {}", point.version),
    }
}

fn describe(op: &Op) -> String {
    match op {
        Op::Commit { changes, .. } | Op::CommitHeld(changes) => {
            let pages: BTreeSet<String> = changes
                .iter()
                .map(|change| match change.page() {
                    Some(page) => page.to_string(),
                    None => "new".to_string(),
                })
                .collect();
            let pages: Vec<String> = pages.into_iter().collect();
            format!("{} ({})", op.name(), pages.join(" "))
        }
        Op::Hold(pages) => format!("hold {pages:?}"),
        Op::Reclaim { page, what } => format!("reclaim {what:?} of {page}"),
        other => other.name().to_string(),
    }
}

/// Runs the run of `seed`.
pub fn run(seed: u64, shape: RunShape) -> RunReport {
    Run::new(seed, shape).run()
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
    pub shape: RunShape,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            seed: 1,
            runs: Some(1),
            points: None,
            duration: None,
            jobs: 1,
            shape: RunShape::default(),
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
    /// A number that two campaigns which did the same have in common.
    pub digest: u64,
}

impl Report {
    /// Whether nothing was found.
    pub fn clean(&self) -> bool {
        self.violations.is_empty()
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let last = self.first_seed + self.runs.saturating_sub(1);
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
        write!(f, "digest {:016x}", self.digest)
    }
}

/// Runs a campaign: the runs of the seeds from [`Settings::seed`] on, until
/// one of the ends that `settings` names is reached. `progress` is told of
/// each run as it is taken in, in the order of the seeds.
///
/// What a campaign comes to is a function of its settings but for
/// [`Settings::duration`] and [`Settings::jobs`]: runs are taken in in the
/// order of their seeds whatever the order they end in, and the campaign
/// ends with the first run that reaches an end.
pub fn campaign(settings: &Settings, progress: &mut dyn FnMut(&RunReport, u64)) -> Report {
    let began = Instant::now();
    let next = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let finished: Mutex<BTreeMap<u64, RunReport>> = Mutex::default();
    let mut report = Report {
        first_seed: settings.seed,
        ..Report::default()
    };
    let mut points: HashSet<u64> = HashSet::new();
    let mut digest = Fnv::new();
    let over = |began: &Instant| {
        settings
            .duration
            .is_some_and(|limit| began.elapsed() >= limit)
    };

    let working = AtomicU64::new(settings.jobs.max(1) as u64);
    std::thread::scope(|scope| {
        for _ in 0..settings.jobs.max(1) {
            scope.spawn(|| {
                while !stop.load(Ordering::SeqCst) && !over(&began) {
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    if settings.runs.is_some_and(|runs| index >= runs) {
                        break;
                    }
                    let done = run(settings.seed.wrapping_add(index), settings.shape);
                    finished
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .insert(index, done);
                }
                working.fetch_sub(1, Ordering::SeqCst);
            });
        }
        // Runs are taken in in the order of their seeds. A run that ended
        // after the campaign did, or whose seed comes after a seed that
        // was never begun, is not part of it.
        let mut wanted = 0;
        loop {
            let idle = working.load(Ordering::SeqCst) == 0;
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
            if stop.load(Ordering::SeqCst) {
                continue;
            }
            report.runs += 1;
            report.tally.add(&done.tally);
            points.extend(done.fingerprints.iter().copied());
            digest.u64(done.digest);
            report.violations.extend(done.violations.iter().cloned());
            report.points = points.len() as u64;
            progress(&done, report.points);
            let reached = settings.points.is_some_and(|goal| report.points >= goal);
            if reached || !done.violations.is_empty() {
                stop.store(true, Ordering::SeqCst);
            }
        }
    });
    report.digest = digest.0;
    report.elapsed = began.elapsed();
    report
}
