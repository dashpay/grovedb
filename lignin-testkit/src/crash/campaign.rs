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
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, PoisonError,
    },
    time::{Duration, Instant},
};

use lignin_format::{wal::LogPlace, PageId};
use lignin_io::{
    sim::{
        Chance, CrashImage, CrashPlan, Fault, FaultOdds, FaultRule, OpSet, SimConfig, SimVfs,
        UnknownFate,
    },
    Vfs,
};
use lignin_store::{cleaner::CleanerConfig, flush::FlushConfig};

use super::{
    check::{
        check_again, check_state, check_with_faults, db_dir, expected_ids, no_panic, panic_text,
        Faulted, Kind, Recovered, Violation,
    },
    model::{self, apply, Change, Expect, History, Landing, Pages, Point},
    observe::{Observer, Sabotage, Taken},
    store::{Committed, Item, OpenError, Reclaim, Session, Stepped, Tuning},
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

/// The states one crash can leave, as the campaign picks them. The first
/// is the one with no unsynced change.
fn plans_of(image: &CrashImage, shape: &RunShape) -> Vec<CrashPlan> {
    let durable = image.lose_all();
    if let Some(plans) = image.every_plan(shape.every_plan_limit) {
        return first_of(plans, durable);
    }
    let mut plans = vec![durable, image.keep_all()];
    plans.extend(image.prefixes());
    plans.extend(image.holes());
    plans.extend(image.reorders());
    plans.extend((0..shape.samples).map(|draw| image.sample(draw)));
    plans
}

/// `plans` with `first` before the others, which keep their order.
fn first_of(mut plans: Vec<CrashPlan>, first: CrashPlan) -> Vec<CrashPlan> {
    match plans.iter().position(|plan| *plan == first) {
        Some(at) => plans[..=at].rotate_right(1),
        None => plans.insert(0, first),
    }
    plans
}

/// The tuning of a store, by turns: the pages a replay keeps, the window a
/// recovery reads the log through, the pool, the runs a chain may have,
/// so that one session finds chains that another's thresholds allowed, and
/// the dead space the cleaner leaves.
fn tuning_of(turn: u64) -> Tuning {
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
        cleaner: CleanerConfig {
            target_dead_percent: [10, 0, 40][((turn / 5) % 3) as usize],
            value_dead_percent: [20, 0][((turn / 7) % 2) as usize],
            ..small.cleaner
        },
        ..small
    }
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
    /// How many faults had been injected into the file system when the
    /// operation that is being made began.
    op_before: u64,
    /// How many faults had been injected, and how many of them last
    /// ([`Observer::lasting_faults`]), when the session was opened.
    faults_before: u64,
    lasting_before: u64,
    /// Whether the log took no item when the operation that is being made
    /// began ([`Session::log_takes_items`]).
    rolling: bool,
    /// The point that the durable part of an image of this file system
    /// last recovered to: no state of a later crash is before it.
    durable: usize,
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

impl Lineage {
    /// Whether an error of the session is a fault's:
    ///
    /// - a fault was injected into the operation that is being made
    /// - since the store was opened, a sync failed that lasts for the
    ///   session ([`Observer::lasting_faults`])
    /// - an append to the log failed earlier in the session, and the log
    ///   had not been rolled when the operation began: until it is, it
    ///   takes no item
    ///
    /// A store that a recovery returned owes nothing to the faults of the
    /// session before, and every other fault fails the operation it was
    /// injected into and no later one.
    fn excused(&self) -> bool {
        let faults = self.observer.fs().stats().faults;
        faults > self.op_before
            || self.observer.lasting_faults() > self.lasting_before
            || (self.rolling && faults > self.faults_before)
    }

    /// What the session knows to be durable is the floor from here on,
    /// whatever becomes of the session.
    fn floor_of(&self, session: &Session) {
        let floor = self.history.floor(session.durable());
        self.observer
            .expecting(|expect| expect.floor = expect.floor.max(floor));
    }

    /// An item that makes `pages` of the store at `version` is about to be
    /// appended: from here a crash can leave it.
    fn attempting(&self, version: u64, pages: Pages) -> Arc<Point> {
        let point = Arc::new(Point {
            version,
            end: None,
            pages: Arc::new(pages),
        });
        self.observer
            .expecting(|expect| expect.attempt = Some(point.clone()));
        point
    }

    /// The append of the item `attempt` returned `Ok`: it is the last
    /// point of the history from here on, and ends at `end` in the log.
    fn landed(&mut self, attempt: Arc<Point>, end: LogPlace) {
        self.history.push(Point {
            end: Some(end),
            ..(*attempt).clone()
        });
        let latest = self.history.latest();
        self.settled.push((attempt, latest));
        self.observer.expecting(|expect| {
            expect.attempt = None;
            expect.latest = latest;
        });
    }

    /// The item that was being appended is out of the log for good.
    fn gone(&self) {
        self.observer.expecting(|expect| expect.attempt = None);
    }

    /// A sync of the log returned `Ok`, or a recovery did: everything in
    /// the log is durable, whatever place the log reports.
    fn synced(&self) {
        let latest = self.history.latest();
        self.observer.expecting(|expect| expect.floor = latest);
    }

    /// What the log decided of the item `attempt`, as the session tells
    /// it.
    fn told(&mut self, attempt: &mut Option<Arc<Point>>, item: Item<'_>) {
        match item {
            Item::Appending(_) => {}
            Item::Appended(end) => {
                if let Some(point) = attempt.take() {
                    self.landed(point, end);
                }
            }
            Item::Gone => self.gone(),
            Item::Synced => self.synced(),
        }
    }
}

/// The count of how often the planted defect of a run did what it is
/// planted to do: a violation of a run where it never did is not its
/// finding.
pub const ACTED: &str = "times the planted defect acted";

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
        let durable = expect.floor;
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
            op_before: 0,
            faults_before: 0,
            lasting_before: 0,
            rolling: false,
            durable,
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
        let lived = catch_unwind(AssertUnwindSafe(|| self.go_through(&mut lineage)));
        if let Err(panic) = lived {
            // What the panic left of the session is asked nothing more.
            lineage.session = None;
            let said = format!("the lineage panicked: {}", panic_text(&*panic));
            self.found(&lineage, Violation::new(Kind::Panicked, said), None);
        }
        self.end_session(&mut lineage);
        self.tally.count(ACTED, lineage.observer.lies());
        self.tally.count("faults injected", fs.stats().faults);
        self.digest.u64(fs.trace_digest());
        if lineage.depth < self.shape.depth {
            self.waiting.append(&mut lineage.kept);
        }
    }

    /// The operations of a lineage, from the making of its store on.
    fn go_through(&mut self, lineage: &mut Lineage) {
        if !self.begin(lineage) {
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
            if !self.step(lineage, op) {
                break;
            }
        }
    }

    /// Makes the store of a lineage if its directory has none, and opens
    /// it. Returns whether the lineage goes on.
    fn begin(&mut self, lineage: &mut Lineage) -> bool {
        let fs = lineage.observer.fs().clone();
        if !lineage.observer.expect().created {
            // The store is made with no fault injected: a session that
            // cannot make its store has nothing to crash.
            fs.set_faults(FaultOdds::NONE);
            if let Err(error) = Session::create(lineage.observer.vfs(), db_dir()) {
                self.session_fault(lineage, format!("no store was made: {error}"));
                return false;
            }
            lineage.observer.expecting(|expect| expect.created = true);
            self.check_images(lineage);
        }
        fs.set_faults(self.faults.odds());
        self.open(lineage, "open")
    }

    /// A violation of a store that was not crashed.
    fn session_fault(&mut self, lineage: &Lineage, said: impl Into<String>) {
        self.found(lineage, Violation::new(Kind::Session, said), None);
    }

    /// Runs `work` with no fault injected into `fs`.
    fn quietly<T>(&self, fs: &SimVfs, work: impl FnOnce() -> T) -> T {
        fs.set_faults(FaultOdds::NONE);
        let done = work();
        fs.set_faults(self.faults.odds());
        done
    }

    /// What a session's call came to: an error is a violation unless a
    /// fault was injected into this file system, and is then counted.
    fn tolerate<T>(
        &mut self,
        lineage: &Lineage,
        what: &str,
        result: Result<T, String>,
    ) -> Option<T> {
        result
            .map_err(|error| self.tolerate_error(lineage, what, error))
            .ok()
    }

    fn tolerate_error(&mut self, lineage: &Lineage, what: &str, error: String) {
        match lineage.excused() {
            true => self.tally.count("errors after a fault", 1),
            false => {
                let said = format!("{what} failed with no fault injected: {error}");
                self.session_fault(lineage, said);
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
        let tuning = tuning_of(turn);
        lineage.sessions += 1;
        let before = fs.stats().faults;
        let mut opened = Session::open(vfs.clone(), db_dir(), &tuning);
        if let Err(error) = &opened
            && fs.stats().faults > before
        {
            // A recovery that a fault failed says that the file layer
            // failed, and is made again (`lignin_store::recovery`,
            // "Twice").
            if matches!(error, OpenError::Failed { damage: true, .. }) {
                let said = format!("{why}, with nothing but a fault injected: {error}");
                self.session_fault(lineage, said);
            }
            self.tally.count("recoveries failed by a fault", 1);
            lineage.full = false;
            opened = self.quietly(&fs, || {
                fs.clear_rules();
                Session::open(vfs, db_dir(), &tuning)
            });
        }
        self.tally.count("sessions", 1);
        let session = match opened {
            Ok(session) => session,
            Err(error) => {
                self.session_fault(lineage, format!("{why}: {error}"));
                self.check_images(lineage);
                return false;
            }
        };
        let expect = lineage.observer.expect();
        let exact = Expect {
            floor: expect.latest,
            ..expect.clone()
        };
        let ids = expected_ids(&lineage.history, &exact);
        let read = self.quietly(&fs, || session.read(&ids));
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
        // A recovery makes the log durable to its end before it returns
        // (`lignin_store::recovery`, step 9): nothing it landed on is lost
        // from here on.
        let latest = lineage.history.latest();
        lineage.observer.expecting(|expect| {
            expect.attempt = None;
            expect.latest = latest;
            expect.floor = latest;
        });
        lineage.op_before = fs.stats().faults;
        lineage.faults_before = lineage.op_before;
        lineage.lasting_before = lineage.observer.lasting_faults();
        lineage.rolling = false;
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
        let stopped = (lineage.session.as_ref()).is_some_and(|session| session.stopped());
        if let Some(why) = reopen.or(stopped.then_some("the log takes nothing more"))
            && !self.open(lineage, why)
        {
            return false;
        }
        self.after(lineage);
        true
    }

    /// What is checked after an operation: the store says what the model
    /// says, and the crash images taken meanwhile recover.
    fn after(&mut self, lineage: &mut Lineage) {
        let Some(session) = &lineage.session else {
            return;
        };
        let fs = lineage.observer.fs().clone();
        let expected = lineage.history.last().pages.clone();
        let ids: BTreeSet<PageId> = expected.keys().copied().collect();
        // The checker's scope asks for a store that is still: no write
        // transaction holds a page.
        let held = lineage.held.is_some();
        let (read, sound) = self.quietly(&fs, || {
            let read = session.read(&ids);
            let sound = match held {
                true => Ok(()),
                false => session.verify(),
            };
            (read, sound)
        });
        let violation = match read {
            Ok(pages) if pages == *expected => sound.err(),
            Ok(pages) => Some(model::difference(&pages, &expected)),
            Err(error) => Some(error),
        };
        if let Some(said) = violation {
            self.session_fault(lineage, format!("the store that was not crashed: {said}"));
        }
        self.check_images(lineage);

        // What the operation made of the floor and of the item it
        // appended holds from here on, whether or not a file changed
        // after the session learned it: the states that a crash now can
        // leave are held to what is expected now.
        let now = lineage.observer.now();
        let plans = plans_of(&now.image, &self.shape);
        self.check_image(lineage, now, plans);
    }

    /// Makes `op`. Returns why the store has to be opened again, if it
    /// has.
    fn make(&mut self, lineage: &mut Lineage, op: &Op) -> Option<&'static str> {
        let session = lineage.session.take()?;
        lineage.op_before = lineage.observer.fs().stats().faults;
        lineage.rolling = !session.log_takes_items();
        let reopen = self.make_with(lineage, &session, op);
        for complaint in session.complaints() {
            self.tolerate_error(lineage, "write-back", complaint);
        }
        lineage.floor_of(&session);
        if !session.holds() {
            lineage.held = None;
        }
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
                return self.commit(lineage, session, changes, false, *durable);
            }
            Op::CommitHeld(changes) => return self.commit(lineage, session, changes, true, false),
            Op::Hold(pages) => {
                let held = self.tolerate(lineage, "copying pages", session.hold(pages));
                if held == Some(true) {
                    lineage.held = Some(pages.iter().copied().collect());
                }
            }
            Op::Release => drop(session.release()),
            Op::Reclaim { page, what } => return self.reclaim(lineage, session, *page, what),
            Op::Pass => drop(self.tolerate(lineage, "a pass", session.pass())),
            Op::Tick => drop(self.tolerate(lineage, "a turn of write-back", session.tick())),
            Op::Checkpoint => {
                let covers = self.tolerate(lineage, "a table checkpoint", session.checkpoint());
                self.tally
                    .count("checkpoints asked for", u64::from(covers.is_some()));
            }
            Op::SyncLog => {
                let synced = self.tolerate(lineage, "a sync of the log", session.sync_log());
                if synced.is_some() {
                    lineage.synced();
                }
            }
            Op::RollLog => drop(self.tolerate(lineage, "a roll of the log", session.roll_log())),
            Op::Consolidate => {
                let done = self.tolerate(lineage, "consolidating", session.consolidate());
                self.tally
                    .count("long chains consolidated", done.unwrap_or(0) as u64);
            }
            Op::Clean => {
                let cleaned = session.clean_values();
                let cleaned =
                    self.tolerate(lineage, "a pass of the cleaner over the value log", cleaned);
                if let Some(pass) = cleaned {
                    let tally = &mut self.tally;
                    if let Some((_, cleaned)) = pass.cleaned {
                        tally.count("value-log files cleaned", 1);
                        tally.count("values moved by the cleaner", cleaned.moved);
                    }
                    tally.count("value-log files dropped", pass.dropped.len() as u64);
                }
            }
            Op::CleanSegments => {
                let cleaned = session.clean_segments();
                let cleaned =
                    self.tolerate(lineage, "a pass of the cleaner over the segments", cleaned);
                if let Some(pass) = cleaned {
                    let tally = &mut self.tally;
                    tally.count("links trimmed by the cleaner", pass.links_trimmed);
                    tally.count("blocks moved by the cleaner", pass.blocks_moved);
                    tally.count("segments sealed for the cleaner", pass.sealed.len() as u64);
                    tally.count("segments emptied by the cleaner", pass.retired as u64);
                    tally.count("segments deleted by the cleaner", pass.dropped as u64);
                    tally.count("table checkpoints the cleaner asked for", pass.checkpoints);
                }
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
                if ended.is_some_and(|covers| covers != Some(latest)) && !lineage.excused() {
                    let said = format!(
                        "the store was closed at version {latest}, and its last table \
                         checkpoint covers {ended:?}"
                    );
                    self.session_fault(lineage, said);
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

    /// A commit of `changes`, with a sync of the log if it is to be
    /// `durable`. The model is told what the log decides of the group when
    /// the session learns it: what the commit makes of the pages before
    /// the group is appended, that it is a point once the append has
    /// returned, and that the point is durable once the sync has. Returns
    /// why the store has to be opened again, if it has.
    fn commit(
        &mut self,
        lineage: &mut Lineage,
        session: &Session,
        changes: &[Change],
        held: bool,
        durable: bool,
    ) -> Option<&'static str> {
        let before = lineage.history.last().clone();
        let made = before.version + 1;
        let forgets = self.shape.planted == Some(Planted::ForgottenHash);
        let (mut attempt, mut misfit, mut forgotten) = (None, None, 0);
        let committed = session.commit(changes, held, durable, &mut |item| {
            if let Item::Appending(ids) = item {
                let mut known: Vec<Change> = changes.to_vec();
                if forgets {
                    let hash = |change: &Change| matches!(change, Change::Hash { kv: true, .. });
                    if let Some(at) = known.iter().position(hash) {
                        known.remove(at);
                        forgotten += 1;
                    }
                }
                match apply(&before.pages, &known, ids, made) {
                    Ok(pages) => attempt = Some(lineage.attempting(made, pages)),
                    Err(error) => misfit = Some(error),
                }
            }
            lineage.told(&mut attempt, item);
        });
        self.tally.count(ACTED, forgotten);
        if let Some(misfit) = misfit {
            self.session_fault(lineage, misfit.to_string());
            return Some("the model did not take a commit");
        }
        match committed {
            Committed::Yes {
                version, synced, ..
            } => {
                if version != made {
                    let said = format!("a commit made version {version}, not {made}");
                    self.session_fault(lineage, said);
                }
                self.tally.count("commits made", 1);
                match synced {
                    Some(Ok(())) => self.tally.count("commits synced", 1),
                    Some(Err(error)) => self.tolerate_error(lineage, "the sync of a commit", error),
                    None => {}
                }
                None
            }
            Committed::No(reason) => {
                lineage.gone();
                self.tally.count("commits that failed", 1);
                self.tolerate_error(lineage, "a commit", reason);
                None
            }
            Committed::InDoubt(reason) => {
                self.tally.count("commits left in doubt", 1);
                self.in_doubt(lineage, "a commit", reason);
                Some("a commit was left in doubt")
            }
            Committed::Broken(reason) => {
                self.session_fault(lineage, reason);
                Some("a commit was not installed")
            }
        }
    }

    /// The log cannot say whether an item is in it: the file layer's
    /// doing, and so a violation where no fault was injected.
    fn in_doubt(&mut self, lineage: &Lineage, what: &str, reason: String) {
        if !lineage.excused() {
            let said = format!("{what} was left in doubt with no fault injected: {reason}");
            self.session_fault(lineage, said);
        }
    }

    /// A reclaiming step, with the model told as for a commit: the step
    /// makes a point of the version the store is at.
    fn reclaim(
        &mut self,
        lineage: &mut Lineage,
        session: &Session,
        page: PageId,
        what: &Reclaim,
    ) -> Option<&'static str> {
        let before = lineage.history.last().clone();
        let (mut attempt, mut misfit) = (None, None);
        let stepped = session.reclaim_step(page, what, &mut |item| {
            if let Item::Appending(_) = item {
                match model::reclaim(&before.pages, page, what.slots()) {
                    Ok(pages) => attempt = Some(lineage.attempting(before.version, pages)),
                    Err(error) => misfit = Some(error),
                }
            }
            lineage.told(&mut attempt, item);
        });
        if let Some(misfit) = misfit {
            self.session_fault(lineage, misfit.to_string());
            return Some("the model did not take a step");
        }
        match stepped {
            Stepped::Yes { .. } => {
                self.tally.count("reclaiming steps made", 1);
                None
            }
            Stepped::Refused(_) => {
                self.tally.count("reclaiming steps refused", 1);
                None
            }
            Stepped::No(reason) => {
                lineage.gone();
                self.tolerate_error(lineage, "a reclaiming step", reason);
                None
            }
            Stepped::InDoubt(reason) => {
                self.tally.count("steps left in doubt", 1);
                self.in_doubt(lineage, "a reclaiming step", reason);
                Some("a reclaiming step was left in doubt")
            }
            Stepped::Broken(reason) => {
                self.session_fault(lineage, reason);
                Some("a reclaiming step was not made")
            }
        }
    }

    /// Goes through the crash images taken since the last call: every
    /// state that their plans leave.
    fn check_images(&mut self, lineage: &mut Lineage) {
        for taken in lineage.observer.drain() {
            self.tally.count("write boundaries", 1);
            let plans = plans_of(&taken.image, &self.shape);
            self.check_image(lineage, taken, plans);
        }
    }

    /// The states that `plans` leave of the crash `taken`, each held to
    /// what was expected of the store when the image was taken, and two of
    /// them to more:
    ///
    /// - The first plan is the one with no unsynced change. The point it
    ///   recovers to is one that the durable part of the file system
    ///   holds, so no state of this crash or of a later one is before it,
    ///   whatever the log has reported.
    /// - The plan that keeps every change whole and in order leaves what a
    ///   process that is killed leaves: the files as the session sees
    ///   them. The store is then where an open with no crash finds it, at
    ///   the last point or at the item that is being appended, and at no
    ///   point before.
    fn check_image(&mut self, lineage: &mut Lineage, mut taken: Taken, plans: Vec<CrashPlan>) {
        taken.expect.floor = taken.expect.floor.max(lineage.durable);
        let whole = taken.image.keep_all();
        let killed = Expect {
            floor: taken.expect.latest,
            ..taken.expect.clone()
        };
        for (turn, plan) in plans.into_iter().enumerate() {
            if self.done() {
                return;
            }
            let expect = match plan == whole {
                true => &killed,
                false => &taken.expect,
            };
            self.tally.count(
                "states that a killed process leaves",
                u64::from(plan == whole),
            );
            let landing = self.check_plan(lineage, &taken, expect, plan);
            if let (0, Some(Landing::Point(at))) = (turn, landing) {
                self.tally.count(
                    "floors raised by a durable state",
                    u64::from(at > taken.expect.floor),
                );
                lineage.durable = at;
                taken.expect.floor = at;
            }
        }
    }

    /// One state: the one `plan` leaves of the crash `taken`, held to
    /// `expect`. Returns where it landed, if that is where it may be.
    fn check_plan(
        &mut self,
        lineage: &mut Lineage,
        taken: &Taken,
        expect: &Expect,
        plan: CrashPlan,
    ) -> Option<Landing> {
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
                Landing::Attempt(point) if !landing.allowed(expect) => {
                    settled(point).map_or(landing.clone(), Landing::Point)
                }
                other => other.clone(),
            };
            if !landing.allowed(expect) {
                let said = format!(
                    "a state that recovers to {} is left where points {} to {} are expected",
                    where_landed(&landing),
                    expect.floor,
                    expect.latest
                );
                let violation = Violation::new(Kind::NotCommitted, said);
                self.found(lineage, violation, Some((taken.boundary, &plan)));
                return None;
            }
            return Some(landing);
        }
        if self.fingerprints.insert(fingerprint) {
            self.order.push(fingerprint);
            self.tally.count("distinct crash states", 1);
        }
        self.tally.count("recoveries", 1);
        self.digest.u64(fingerprint);
        // The tuning of a recovery goes by turns as well.
        let tuning = tuning_of(fingerprint >> 7);
        let checked = no_panic("the recovery of a crash state", || {
            check_state(&state, &lineage.history, expect, &tuning)
        });
        let recovered = match checked {
            Ok(recovered) => recovered,
            Err(violation) => {
                self.digest.u64(u64::MAX);
                self.found(lineage, violation, Some((taken.boundary, &plan)));
                return None;
            }
        };
        self.record(lineage, expect, &recovered);
        let Recovered { landing, pages, .. } = recovered;
        lineage.seen.insert(fingerprint, landing.clone());

        let landed = match &landing {
            Landing::NoStore => None,
            Landing::Point(at) => Some(lineage.history.point(*at).version),
            Landing::Attempt(point) => Some(point.version),
        };
        // The same state once more, as the crash left it, recovered with
        // the run's faults.
        if self.faults != Faults::None && (fingerprint >> 19).is_multiple_of(4) {
            let other = (taken.image.recover(&plan)).expect("a plan of the image fits it");
            let odds = self.faults.odds();
            let faulted = no_panic("a recovery with faults injected", || {
                check_with_faults(&other, odds, &tuning, landed, &pages)
            });
            match faulted {
                Ok(Faulted::Returned) => {
                    let name = "recoveries with faults that returned a store";
                    self.tally.count(name, 1);
                }
                Ok(Faulted::Failed) => self.tally.count("recoveries with faults that failed", 1),
                Ok(Faulted::NoStore) => {}
                Err(violation) => self.found(lineage, violation, Some((taken.boundary, &plan))),
            }
        }
        let again = self.shape.again_one_in;
        let chosen = |_: &u64| again > 0 && (fingerprint >> 11).is_multiple_of(again);
        if let Some(landed) = landed.filter(chosen) {
            self.tally.count("states recovered again", 1);
            let other = tuning_of((fingerprint >> 7) + 1);
            let again = no_panic("the second recovery of a crash state", || {
                check_again(&state, &other, landed, &pages)
            });
            if let Err(violation) = again {
                self.found(lineage, violation, Some((taken.boundary, &plan)));
            }
        }
        self.keep(lineage, taken, plan, &landing);
        Some(landing)
    }

    fn record(&mut self, lineage: &Lineage, expect: &Expect, recovered: &Recovered) {
        let tally = &mut self.tally;
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

fn where_landed(landing: &Landing) -> String {
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
    caught(seed, || Run::new(seed, shape).run())
}

/// What `work` came to, or a report of the panic that ended it: a run that
/// panicked has found something, and it is reported with its seed like any
/// other violation.
fn caught(seed: u64, work: impl FnOnce() -> RunReport) -> RunReport {
    catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|panic| {
        let said = format!("the run panicked: {}", panic_text(&*panic));
        let mut tally = Tally::default();
        tally.count("runs", 1);
        tally.count("violations", 1);
        RunReport {
            seed,
            tally,
            violations: vec![Found {
                seed,
                lineage: 0,
                op: 0,
                boundary: None,
                plan: None,
                violation: Violation::new(Kind::Panicked, said),
                trail: Vec::new(),
            }],
            ..RunReport::default()
        }
    })
}

/// Tells the runs that are under way to be the last, when it goes: so a
/// campaign that a panic ends does not wait for runs with no end.
struct StopAtEnd<'s>(&'s AtomicBool);

impl Drop for StopAtEnd<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
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

    std::thread::scope(|scope| {
        let work = || {
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
        };
        let _stop = StopAtEnd(&stop);
        let workers: Vec<_> = (0..settings.jobs.max(1))
            .map(|_| scope.spawn(work))
            .collect();
        // Runs are taken in in the order of their seeds. A run that ended
        // after the campaign did, or whose seed comes after a seed that
        // was never begun, is not part of it.
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

#[cfg(test)]
mod tests {
    use lignin_store::wal::LogError;

    use super::*;

    fn quiet() -> RunShape {
        RunShape {
            faults: Some(Faults::None),
            ..RunShape::default()
        }
    }

    /// A run with its first lineage, the store made and opened.
    fn begun() -> (Run, Lineage) {
        let mut run = Run::new(1, quiet());
        let mut lineage = run.first();
        assert!(run.begin(&mut lineage));
        (run, lineage)
    }

    /// The next commit of the lineage's workload.
    fn commit(lineage: &mut Lineage, durable: bool) -> Op {
        let pages = lineage.history.last().pages.clone();
        loop {
            let situation = Situation {
                held: None,
                pinned: false,
                faulty: false,
                full: false,
            };
            if let Op::Commit { changes, .. } = lineage.workload.next(&pages, situation) {
                return Op::Commit { changes, durable };
            }
        }
    }

    #[test]
    fn a_run_that_panics_is_reported_with_its_seed() {
        let report = caught(7, || panic!("the store gave up"));
        let found = report.violations.first().expect("a violation");
        assert_eq!(
            (report.seed, found.seed, found.violation.kind),
            (7, 7, Kind::Panicked)
        );
        assert_eq!(
            found.violation.detail,
            "the run panicked: the store gave up"
        );
        assert_eq!(report.tally.get("violations"), 1);
        let clean = caught(7, RunReport::default);
        assert!(clean.violations.is_empty());
    }

    #[test]
    fn a_lineage_that_panics_is_reported_with_its_place() {
        let mut run = Run::new(1, quiet());
        let mut lineage = run.first();
        Session::create(lineage.observer.vfs(), db_dir()).expect("a store");
        // The store is expected at a point that the model does not have:
        // the campaign's own mistake, which ends in a panic.
        lineage
            .observer
            .expecting(|expect| *expect = Expect::exactly(5));
        lineage.trail.push("pass".to_string());
        run.live(lineage);
        let found = run.violations.first().expect("a violation");
        assert_eq!(found.violation.kind, Kind::Panicked);
        assert!(
            found.violation.detail.starts_with("the lineage panicked: "),
            "{found}"
        );
        assert_eq!((found.lineage, found.trail.len()), (0, 1));
        // The run goes on to its end: what was counted is kept.
        assert_eq!(run.tally.get("lineages"), 1);
        assert_eq!(run.tally.get("violations"), 1);
    }

    #[test]
    fn a_campaign_that_a_panic_ends_does_not_wait_for_its_runs() {
        // No end is named that the runs reach: only the panic ends them.
        let settings = Settings {
            runs: None,
            points: Some(u64::MAX),
            jobs: 2,
            shape: RunShape {
                ops: 4,
                lineages: 1,
                again_one_in: 0,
                ..quiet()
            },
            ..Settings::default()
        };
        let (ended, wait) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                campaign(&settings, &mut |_, _| panic!("nobody listens"))
            }));
            let _ = ended.send(outcome.is_err());
        });
        let panicked = wait
            .recv_timeout(Duration::from_secs(600))
            .expect("the campaign ended");
        assert!(panicked);
    }

    #[test]
    fn the_durable_plan_is_put_before_the_others_which_keep_their_order() {
        let plan = |name: usize| CrashPlan {
            names: vec![name],
            changes: Vec::new(),
        };
        let durable = CrashPlan::default();
        let (one, two) = (plan(1), plan(2));
        let first = |plans: &[&CrashPlan]| {
            let plans = plans.iter().map(|plan| (*plan).clone()).collect();
            first_of(plans, durable.clone())
        };
        let expected = vec![durable.clone(), one.clone(), two.clone()];
        assert_eq!(first(&[&one, &two, &durable]), expected);
        assert_eq!(first(&[&one, &durable, &two]), expected);
        assert_eq!(first(&[&durable, &one, &two]), expected);
        assert_eq!(first(&[&one, &two]), expected);
        assert_eq!(first(&[]), vec![durable.clone()]);
    }

    #[test]
    fn the_first_plan_of_an_image_is_the_one_with_no_unsynced_change() {
        let (mut run, mut lineage) = begun();
        let mut images = Vec::new();
        for durable in [false, true, false, true] {
            let op = commit(&mut lineage, durable);
            assert_eq!(run.make(&mut lineage, &op), None);
            images.append(&mut lineage.observer.drain());
        }
        assert!(images.len() > 4);
        let mut every_plan = 0;
        for taken in &images {
            let durable = taken.image.lose_all();
            let listed = RunShape {
                every_plan_limit: 0,
                ..quiet()
            };
            let mut all = vec![plans_of(&taken.image, &listed)];
            if let Some(every) = taken.image.every_plan(quiet().every_plan_limit) {
                let plans = plans_of(&taken.image, &quiet());
                assert_eq!(plans.len(), every.len());
                assert!(every.iter().all(|plan| plans.contains(plan)));
                every_plan += usize::from(every.len() > 1);
                all.push(plans);
            }
            // The first is the durable state, and what a killed process
            // leaves is among them.
            let whole = taken.image.keep_all();
            for plans in all {
                assert_eq!(plans[0], durable, "boundary {}", taken.boundary);
                assert!(plans.contains(&whole), "boundary {}", taken.boundary);
            }
        }
        assert!(every_plan > 0);
    }

    /// A lineage of two commits, with an image from between them: the
    /// first commit is in the log and not synced then, and the second is
    /// synced.
    fn two_commits() -> (Run, Lineage, Taken) {
        let (mut run, mut lineage) = begun();
        let op = commit(&mut lineage, false);
        assert!(run.step(&mut lineage, op));
        let early = lineage.observer.now();
        assert_eq!((lineage.history.latest(), lineage.durable), (1, 0));
        assert_eq!((early.expect.floor, early.expect.latest), (0, 1));
        let op = commit(&mut lineage, true);
        assert!(run.step(&mut lineage, op));
        assert!(run.violations.is_empty(), "{:?}", run.violations);
        // The second commit was synced, and the state with no unsynced
        // change recovered to it.
        assert_eq!((lineage.history.latest(), lineage.durable), (2, 2));
        assert_eq!(run.tally.get("commits synced"), 1);
        (run, lineage, early)
    }

    /// The image `early` as a crash after the second commit would leave
    /// it, if the store took back what it had written. The log is taken to
    /// have reported nothing.
    fn later(early: &Taken) -> Taken {
        Taken {
            image: early.image.clone(),
            expect: Expect {
                latest: 2,
                ..early.expect.clone()
            },
            boundary: early.boundary,
        }
    }

    #[test]
    fn no_state_is_before_the_point_a_durable_state_recovered_to() {
        let (mut run, mut lineage, early) = two_commits();
        let plan = early.image.lose_all();
        assert_ne!(plan, early.image.keep_all());
        lineage.durable = 0;
        run.check_image(&mut lineage, later(&early), vec![plan.clone()]);
        assert!(run.violations.is_empty(), "{:?}", run.violations);
        assert_eq!(lineage.durable, 0);

        lineage.durable = 2;
        run.check_image(&mut lineage, later(&early), vec![plan.clone()]);
        let found = run.violations.first().expect("a violation");
        assert_eq!(found.violation.kind, Kind::NotCommitted);
        assert_eq!(found.boundary, Some(early.boundary));
        assert!(
            found.violation.detail.contains("point 0") && found.violation.detail.contains("2 to 2"),
            "{found}"
        );

        // The same of a state that is recovered for the first time.
        lineage.seen.clear();
        run.check_image(&mut lineage, later(&early), vec![plan]);
        assert_eq!(run.violations.len(), 2);
        let found = &run.violations[1];
        assert_eq!(found.violation.kind, Kind::NotCommitted);
        assert!(
            found.violation.detail.contains("which was durable"),
            "{found}"
        );
    }

    #[test]
    fn a_crash_that_loses_nothing_leaves_the_store_at_its_last_point() {
        let (mut run, mut lineage, early) = two_commits();
        lineage.durable = 0;
        let plans = || vec![early.image.lose_all(), early.image.keep_all()];
        // As the image was taken, one commit was made: the state with
        // every change is at it.
        let then = Taken {
            image: early.image.clone(),
            expect: early.expect.clone(),
            boundary: early.boundary,
        };
        run.check_image(&mut lineage, then, plans());
        assert!(run.violations.is_empty(), "{:?}", run.violations);
        assert_eq!(lineage.durable, 0);
        // With two commits made it is a point short, although the log has
        // reported nothing durable.
        run.check_image(&mut lineage, later(&early), plans());
        assert_eq!(run.violations.len(), 1, "{:?}", run.violations);
        let found = &run.violations[0];
        assert_eq!(found.violation.kind, Kind::NotCommitted);
        assert!(
            found.violation.detail.contains("point 1") && found.violation.detail.contains("2 to 2"),
            "{found}"
        );
        assert!(run.tally.get("states that a killed process leaves") >= 2);

        // The same of a state that is recovered for the first time.
        lineage.seen.clear();
        lineage.durable = 0;
        run.check_image(&mut lineage, later(&early), plans());
        assert_eq!(run.violations.len(), 2, "{:?}", run.violations);
        let found = &run.violations[1];
        assert_eq!(found.violation.kind, Kind::NotCommitted);
        assert!(
            found.violation.detail.contains("which was durable"),
            "{found}"
        );
    }

    #[test]
    fn an_error_is_a_faults_in_its_own_operation_or_after_one_that_lasts() {
        use lignin_io::{Create, OpenMode};

        let (mut run, mut lineage) = begun();
        let fs = lineage.observer.fs().clone();
        let vfs = lineage.observer.vfs();
        vfs.create_dir(std::path::Path::new("/seg")).expect("made");
        let open = |name: &str| {
            let path = std::path::Path::new(name);
            vfs.open(path, OpenMode::Append(Create::New))
                .expect("opened")
        };
        // What `Run::make` does when an operation begins.
        let begin = |lineage: &mut Lineage| lineage.op_before = fs.stats().faults;
        let failed = |run: &mut Run, lineage: &Lineage| {
            run.tolerate_error(lineage, "a call", "it failed".to_string());
            (run.tally.get("errors after a fault"), run.violations.len())
        };
        assert!(!lineage.excused());

        // A write that fails: an error of the same operation is the
        // fault's, and one of the next operation is a violation.
        let (written, synced) = (open("/written"), open("/seg/synced"));
        fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::WRITES));
        assert!(written.append(b"block").is_err());
        assert_eq!(failed(&mut run, &lineage), (1, 0));
        begin(&mut lineage);
        assert!(!lineage.excused());
        assert_eq!(failed(&mut run, &lineage), (1, 1));
        let found = &run.violations[0];
        assert_eq!(found.violation.kind, Kind::Session);
        assert_eq!(
            found.violation.detail,
            "a call failed with no fault injected: it failed"
        );

        // While the log takes no item after an append that failed, the
        // errors of the operations are that fault's.
        lineage.rolling = true;
        assert_eq!(failed(&mut run, &lineage), (2, 1));
        lineage.rolling = false;
        assert!(!lineage.excused());

        // The sync of a segment that fails lasts: the errors of the
        // operations after it are the fault's, until the store is opened
        // again.
        fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::SYNCS));
        assert!(synced.sync_data().is_err());
        begin(&mut lineage);
        assert!(lineage.excused());
        assert_eq!(failed(&mut run, &lineage), (3, 1));
        assert!(run.step(&mut lineage, Op::Kill));
        assert!(!lineage.excused());
        assert_eq!(failed(&mut run, &lineage), (3, 2));
    }

    #[test]
    fn the_log_takes_no_item_after_an_append_that_failed_until_it_is_rolled() {
        let (mut run, mut lineage) = begun();
        let fs = lineage.observer.fs().clone();
        let page = || Op::Commit {
            changes: vec![Change::NewFlat { entries: vec![] }],
            durable: false,
        };
        let counts = |run: &Run| {
            let made = run.tally.get("commits made");
            (
                made,
                run.tally.get("errors after a fault"),
                run.violations.len(),
            )
        };
        let takes = |lineage: &Lineage| {
            let session = lineage.session.as_ref().expect("a session");
            session.log_takes_items()
        };
        // Two commits, each of a page: a step can free the first page once
        // the second commit has made a later version.
        assert!(run.step(&mut lineage, page()));
        assert!(run.step(&mut lineage, page()));
        let first = *(lineage.history.point(1).pages.keys().next()).expect("a page");
        assert_eq!(counts(&run), (2, 0, 0));

        // The append of a third commit fails. Its error is its own
        // fault's, and the log takes no item from here on.
        fs.add_rule(FaultRule::new(Fault::Eio).on(OpSet::WRITES));
        assert!(run.step(&mut lineage, page()));
        assert_eq!((fs.stats().faults, counts(&run)), (1, (2, 1, 0)));
        assert_eq!(lineage.observer.lasting_faults(), 0);
        assert!(!takes(&lineage));

        // A step whose record the log does not take, for that reason and
        // no other: the error is that fault's still, with no fault
        // injected into the step.
        let refused = {
            let session = lineage.session.as_ref().expect("a session");
            session.reclaim_step(first, &Reclaim::Page, &mut |_| {})
        };
        let Stepped::No(reason) = refused else {
            panic!("the log took the record of the step: {refused:?}");
        };
        assert_eq!(reason, LogError::NoActiveFile.to_string());
        assert!(!takes(&lineage));
        let free = Op::Reclaim {
            page: first,
            what: Reclaim::Page,
        };
        assert!(run.step(&mut lineage, free));
        assert!(lineage.rolling && lineage.excused());
        assert_eq!((fs.stats().faults, counts(&run)), (1, (2, 2, 0)));
        assert_eq!(run.tally.get("reclaiming steps made"), 0);
        assert_eq!(run.tally.get("reclaiming steps refused"), 0);
        assert_eq!(lineage.history.latest(), 2);

        // The log still takes no item: the step did not roll it. A commit
        // rolls the log before it asks for its place, and is made. From
        // the next operation on nothing is excused: an error is a
        // violation.
        assert!(!takes(&lineage));
        assert!(run.step(&mut lineage, page()));
        assert!(takes(&lineage));
        assert_eq!(counts(&run), (3, 2, 0));
        assert!(run.step(&mut lineage, Op::Pass));
        assert!(!lineage.rolling && !lineage.excused());
        run.tolerate_error(&lineage, "a call", "it failed".to_string());
        assert_eq!(counts(&run), (3, 2, 1));
        assert_eq!(run.violations[0].violation.kind, Kind::Session);
    }
}
