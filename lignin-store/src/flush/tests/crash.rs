//! Crashes, on the simulated file system (tests T-DU4 and T-PN7, and the
//! part of T-VL5 that is about a crash).
//!
//! A script of commits, passes, table checkpoints, value moves and write
//! transactions that hold copies is run once for every change to what a
//! crash can leave, with the power going off after that change. Each state
//! the crash can leave is then recovered, and what the recovery lands on
//! must be one committed version, whole, at or after the last version that
//! was known durable.

use std::panic::{catch_unwind, AssertUnwindSafe};

use lignin_io::sim::{CrashImage, CrashPoint, SimConfig, SimVfs};

use super::{
    recover::{recover, the_store_recovers_the_same},
    *,
};
use crate::segment::tests::sim::{crash_states, db_dir, shared, sim_fs, Plans};

/// One step of a script.
#[derive(Debug, Clone)]
pub(crate) enum Step {
    Commit(Vec<Op>),
    Pass,
    Checkpoint,
    /// The value of a cell is moved to another place in the value log.
    Move(u32, u16),
    SyncLog,
    /// A write transaction copies a page and holds the copy.
    Hold(u32),
    /// The transaction that holds a copy commits these changes, and the
    /// values that waited for it are placed.
    HeldCommit(Vec<Op>),
    /// The transaction that holds a copy rolls back.
    Release,
    /// A commit writes a flat leaf whole: its entries, in key order. The
    /// values it had in the value log are released.
    Flat(u32, Vec<(Vec<u8>, Val)>),
}

pub(crate) fn config() -> FlushConfig {
    FlushConfig {
        max_chain_deltas: 2,
        ..FlushConfig::new(Db::SIZE)
    }
}

/// Small log files, so that the log turns often and checkpoints have
/// files to take out of it.
pub(crate) fn log_options() -> LogOptions {
    LogOptions {
        target_file_len: 600,
        ..LogOptions::default()
    }
}

/// How one run of a script ended.
pub(crate) struct Ran {
    pub(crate) fs: SimVfs,
    pub(crate) model: Model,
    /// The version the log was known to be durable through.
    pub(crate) durable: CommitVersion,
    pub(crate) latest: CommitVersion,
    pub(crate) checkpoints: u64,
    pub(crate) log_files_dropped: u64,
    pub(crate) put_off: u64,
}

/// Runs `script` on a new store, with the power going off after `crash`
/// changes to what a crash can leave, if that many are made.
pub(crate) fn run(script: &[Step], crash: Option<u64>) -> Ran {
    let fs = sim_fs(SimConfig::new(11));
    if let Some(changes) = crash {
        fs.crash_at(CrashPoint::AfterChange(changes));
    }
    let made = catch_unwind(AssertUnwindSafe(|| {
        Db::create(shared(&fs), db_dir(), config(), log_options())
    }));
    let Ok(db) = made else {
        assert!(fs.is_powered_off(), "the store was not made");
        return Ran {
            fs,
            model: Model::default(),
            durable: CommitVersion::EMPTY,
            latest: CommitVersion::EMPTY,
            checkpoints: 0,
            log_files_dropped: 0,
            put_off: 0,
        };
    };
    let steps = catch_unwind(AssertUnwindSafe(|| {
        let mut session = Session::new(&db);
        for step in script {
            session.commits(step);
            if let Err(error) = session.writes_back(step) {
                assert!(
                    fs.is_powered_off(),
                    "{step:?} failed with the power on: {error}"
                );
                return;
            }
        }
    }));
    assert!(
        steps.is_ok() || fs.is_powered_off(),
        "a step failed with the power on"
    );
    Ran::of(&fs, &db)
}

impl Ran {
    /// How things stand in `db`, which is on `fs`.
    pub(crate) fn of(fs: &SimVfs, db: &Db) -> Self {
        let model = db
            .model
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let stats = db.stats();
        Self {
            durable: db.log.durable().version,
            latest: db.latest(),
            checkpoints: stats.checkpoints,
            log_files_dropped: stats.log_files_dropped,
            put_off: stats.checkpoints_put_off,
            model,
            fs: fs.clone(),
        }
    }
}

/// A store that a script is run on, step by step.
pub(crate) struct Session<'d> {
    db: &'d Db,
    held: Option<Txn<'d>>,
}

impl<'d> Session<'d> {
    pub(crate) fn new(db: &'d Db) -> Self {
        Self { db, held: None }
    }

    /// The part of `step` that is a committer's or the cleaner's.
    pub(crate) fn commits(&mut self, step: &Step) {
        let db = self.db;
        match step {
            Step::Commit(ops) => drop(db.commit(ops)),
            Step::Move(page_id, slot_id) => db.move_value(page(*page_id), slot(*slot_id), false),
            Step::Hold(page_id) => {
                let mut txn = db.begin();
                txn.copy(page(*page_id));
                self.held = Some(txn);
            }
            Step::HeldCommit(ops) => drop(self.take_held().commit(ops)),
            Step::Flat(page_id, entries) => {
                let entries: Vec<(&[u8], Val)> = entries
                    .iter()
                    .map(|(key, value)| (key.as_slice(), value.clone()))
                    .collect();
                db.commit_flat(page(*page_id), &entries, &BTreeMap::new());
            }
            Step::Release => self.take_held().roll_back(),
            Step::Pass | Step::Checkpoint | Step::SyncLog => {}
        }
    }

    /// The part of `step` that is write-back's.
    pub(crate) fn writes_back(&mut self, step: &Step) -> Result<(), FlushError> {
        let wb = self.db.wb();
        match step {
            Step::Pass => wb.pass().map(drop),
            Step::Checkpoint => wb.checkpoint().map(drop),
            Step::SyncLog => wb.sync_log().map(drop),
            Step::HeldCommit(_) | Step::Release => wb.place_ready().map(drop),
            Step::Commit(_) | Step::Move(..) | Step::Hold(_) | Step::Flat(..) => Ok(()),
        }
    }

    /// The transaction that holds a copy gives it up, if there is one.
    pub(crate) fn release(&mut self) {
        if let Some(txn) = self.held.take() {
            txn.roll_back();
        }
    }

    fn take_held(&mut self) -> Txn<'d> {
        self.held.take().expect("a transaction holds a copy")
    }
}

/// What the recoveries of one script came to.
#[derive(Debug, Default)]
pub(crate) struct Tally {
    pub(crate) crashes: u64,
    pub(crate) states: u64,
    /// Recoveries that landed above the version known durable: commits
    /// that a crash kept though nobody had synced them.
    pub(crate) ahead: u64,
    /// Recoveries that replayed commits on a checkpoint.
    pub(crate) replayed: u64,
    from_checkpoint: u64,
    /// Pages that the store's recovery wrote to a segment while it read
    /// the log, when it was made to keep one page in memory.
    spilled: u64,
}

/// Recovers every state that a crash of `ran` can leave, as `plans` picks
/// them, and checks each against the model.
pub(crate) fn check(ran: &Ran, plans: &Plans, tally: &mut Tally) {
    check_image(&ran.fs.crash_image(), ran, plans, tally);
}

/// Recovers every state that `image` can leave, as `plans` picks them, and
/// checks each ([`check_state`]).
pub(crate) fn check_image(image: &CrashImage, ran: &Ran, plans: &Plans, tally: &mut Tally) {
    tally.crashes += 1;
    for (plan, state) in crash_states(image, plans).states {
        check_state(&state, ran, &plan, tally);
    }
}

/// Recovers `state`, which the plan `plan` of a crash left. A recovery
/// lands at or after the version `ran` knew durable when the power went,
/// and what it lands on is what the model of `ran` says. Returns the
/// version, or `None` if the power went before the store had its first
/// manifest.
pub(crate) fn check_state(
    state: &SimVfs,
    ran: &Ran,
    plan: &dyn std::fmt::Debug,
    tally: &mut Tally,
) -> Option<CommitVersion> {
    let ids: Vec<PageId> = ran.model.pages.keys().copied().collect();
    let manifest = db_dir().join(ManifestStore::FILE_NAME);
    if state.open(&manifest, lignin_io::OpenMode::Read).is_err() {
        assert_eq!(
            ran.latest,
            CommitVersion::EMPTY,
            "the manifest is gone: {plan:?}"
        );
        return None;
    }
    let recovered = recover(shared(state), db_dir(), &ids);
    let landed = recovered.landed;
    assert!(
        ran.durable <= landed && landed <= ran.latest,
        "landed on {landed}, with {} durable and {} the latest: {plan:?}",
        ran.durable,
        ran.latest
    );
    for &id in &ids {
        let expected = ran.model.at(id, landed).map(|(_, cells)| cells);
        match (&recovered.pages[&id], expected) {
            (Some(found), Some(cells)) => assert!(
                same(found, cells),
                "page {id} at version {landed}: {found:?}, not {cells:?}: {plan:?}"
            ),
            (None, None) => {}
            (found, expected) => {
                panic!("page {id} at version {landed}: {found:?}, not {expected:?}: {plan:?}")
            }
        }
    }
    // The store's own recovery lands on the same state, twice.
    tally.spilled += the_store_recovers_the_same(shared(state), db_dir(), &ids, &recovered);
    tally.states += 1;
    tally.ahead += u64::from(landed > ran.durable);
    tally.replayed += u64::from(landed > recovered.covers);
    tally.from_checkpoint += u64::from(recovered.checkpoint);
    Some(landed)
}

pub(crate) fn commit(ops: &[Op]) -> Step {
    Step::Commit(ops.to_vec())
}

pub(crate) fn free(id: u32) -> Op {
    Op::Free { page: page(id) }
}

pub(crate) fn flat(id: u32, entries: &[(&[u8], Val)]) -> Step {
    let entries = entries
        .iter()
        .map(|(key, value)| (key.to_vec(), value.clone()));
    Step::Flat(id, entries.collect())
}

fn set(at: (u32, u16), value: Val) -> Op {
    Op::Value {
        page: page(at.0),
        slot: slot(at.1),
        value,
    }
}

/// The life of a store with write-back: tree pages and a flat leaf with
/// values in and out of their cells, runs and base images, a
/// consolidation, a value that waits
/// for a private copy and is placed after the commit that carried it on,
/// values moved under a checkpoint, a checkpoint that is put off because
/// a page that a move changed cannot be written, a page freed with a
/// change that was never written back, its id used again, and table
/// checkpoints in between that take log files away.
pub(crate) fn life() -> Vec<Step> {
    vec![
        commit(&[
            insert((1, 1), b"a", inline(b"one")),
            insert((2, 1), b"b", long(1, 900)),
        ]),
        Step::Pass,
        Step::Checkpoint,
        // A flat leaf, with a value that is placed by an entry's place.
        flat(20, &[(b"a", inline(b"flat")), (b"b", long(11, 600))]),
        commit(&[hash((1, 1), 1)]),
        commit(&[insert((1, 2), b"c", long(2, 700))]),
        Step::Checkpoint,
        Step::Move(2, 1),
        Step::Checkpoint,
        commit(&[hash((2, 1), 2), insert((3, 1), b"d", long(3, 800))]),
        Step::Hold(3),
        Step::Pass,
        Step::Checkpoint,
        Step::HeldCommit(vec![hash((3, 1), 3)]),
        Step::Move(2, 1),
        Step::Pass,
        commit(&[hash((1, 1), 4)]),
        // The leaf is written again: the value it had is released, the new
        // one waits for a transaction that holds a copy of the leaf, and a
        // checkpoint is put off for it.
        flat(20, &[(b"a", inline(b"flat again")), (b"c", long(12, 500))]),
        Step::Hold(20),
        Step::Checkpoint,
        Step::Release,
        Step::SyncLog,
        commit(&[hash((1, 1), 5), free(3)]),
        commit(&[insert((3, 2), b"e", long(4, 600))]),
        Step::Checkpoint,
        commit(&[set((1, 2), long(5, 650)), hash((2, 1), 6)]),
        Step::Hold(1),
        Step::Checkpoint,
        Step::Release,
        Step::Checkpoint,
        // A value of page 1 is moved, and the page then cannot be written:
        // a checkpoint taken now would lack what the move did to the page,
        // with the move's record before its sample, where no replay looks.
        Step::Move(1, 2),
        commit(&[insert((1, 3), b"f", long(6, 500))]),
        Step::Hold(1),
        Step::Checkpoint,
        Step::Release,
        Step::Checkpoint,
        commit(&[free(2), hash((1, 1), 7)]),
        Step::Pass,
        commit(&[hash((3, 2), 8)]),
    ]
}

/// What the script is meant to go through, on a run with no crash.
#[test]
fn the_script_takes_checkpoints_drops_log_files_and_puts_a_checkpoint_off() {
    let ran = run(&life(), None);
    assert!(!ran.fs.is_powered_off());
    assert_eq!(ran.latest, version(14));
    assert!(ran.checkpoints >= 5, "{} checkpoints", ran.checkpoints);
    assert!(
        ran.log_files_dropped >= 3,
        "{} log files dropped",
        ran.log_files_dropped
    );
    assert_eq!(ran.put_off, 4);
    let mut tally = Tally::default();
    check(&ran, &PLANS, &mut tally);
    assert!(tally.states > 0);
}

pub(crate) const PLANS: Plans = Plans {
    every_plan_limit: 48,
    samples: 4,
};

/// **Tests T-DU4, T-PN7 and T-VL5, at every crash point.** The power goes
/// off after each change that the script makes to what a crash can leave.
/// Whatever the crash keeps, a recovery lands on one committed version at
/// or after the last one that was known durable, every page that is in
/// use there says what its commits left, with every value, no link is
/// above that version, and the counts of live values are the pointers the
/// pages hold.
#[test]
fn a_crash_at_any_point_recovers_one_committed_version_whole() {
    let script = life();
    let whole = run(&script, None);
    let changes = whole.fs.stats().changes;
    let mut tally = Tally::default();
    for crash in 1..=changes {
        let ran = run(&script, Some(crash));
        assert!(
            ran.fs.is_powered_off(),
            "the power goes off at change {crash}"
        );
        check(&ran, &PLANS, &mut tally);
    }
    assert_eq!(tally.crashes, changes);
    // The recoveries are not all of one kind.
    assert!(tally.states > 2 * changes, "{tally:?}");
    assert!(tally.ahead > 100, "{tally:?}");
    assert!(tally.replayed > 100, "{tally:?}");
    assert!(tally.from_checkpoint > tally.states / 2, "{tally:?}");
    assert!(tally.spilled > tally.replayed, "{tally:?}");
    println!("{tally:?}");
}
