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

use lignin_io::sim::{CrashPoint, SimConfig, SimVfs};

use super::{recover::recover, *};
use crate::segment::tests::sim::{crash_states, db_dir, shared, sim_fs, Plans};

/// One step of a script.
#[derive(Debug, Clone)]
enum Step {
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
}

fn config() -> FlushConfig {
    FlushConfig {
        max_chain_deltas: 2,
        ..FlushConfig::new(Db::SIZE)
    }
}

/// Small log files, so that the log turns often and checkpoints have
/// files to take out of it.
fn log_options() -> LogOptions {
    LogOptions {
        target_file_len: 600,
        ..LogOptions::default()
    }
}

/// How one run of a script ended.
struct Ran {
    fs: SimVfs,
    model: Model,
    /// The version the log was known to be durable through.
    durable: CommitVersion,
    latest: CommitVersion,
    checkpoints: u64,
    log_files_dropped: u64,
    put_off: u64,
}

/// Runs `script` on a new store, with the power going off after `crash`
/// changes to what a crash can leave, if that many are made.
fn run(script: &[Step], crash: Option<u64>) -> Ran {
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
        let wb = db.wb();
        let mut held = None;
        for step in script {
            let done = match step {
                Step::Commit(ops) => Ok(db.commit(ops)).map(drop),
                Step::Pass => wb.pass().map(drop),
                Step::Checkpoint => wb.checkpoint().map(drop),
                Step::Move(page_id, slot_id) => {
                    Ok(db.move_value(page(*page_id), slot(*slot_id), false))
                }
                Step::SyncLog => wb.sync_log().map(drop),
                Step::Hold(page_id) => {
                    let mut txn = db.begin();
                    txn.copy(page(*page_id));
                    held = Some(txn);
                    Ok(())
                }
                Step::HeldCommit(ops) => {
                    held.take().expect("a transaction holds a copy").commit(ops);
                    wb.place_ready().map(drop)
                }
                Step::Release => {
                    held.take().expect("a transaction holds a copy").roll_back();
                    wb.place_ready().map(drop)
                }
            };
            if let Err(error) = done {
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
    let model = db
        .model
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    let stats = db.stats();
    Ran {
        durable: db.log.durable().version,
        latest: db.latest(),
        checkpoints: stats.checkpoints,
        log_files_dropped: stats.log_files_dropped,
        put_off: stats.checkpoints_put_off,
        model,
        fs: fs.clone(),
    }
}

/// What the recoveries of one script came to.
#[derive(Debug, Default)]
struct Tally {
    crashes: u64,
    states: u64,
    /// Recoveries that landed above the version known durable: commits
    /// that a crash kept though nobody had synced them.
    ahead: u64,
    /// Recoveries that replayed commits on a checkpoint.
    replayed: u64,
    from_checkpoint: u64,
}

/// Recovers every state that a crash of `ran` can leave, as `plans` picks
/// them, and checks each against the model.
fn check(ran: &Ran, plans: &Plans, tally: &mut Tally) {
    let ids: Vec<PageId> = ran.model.pages.keys().copied().collect();
    let states = crash_states(&ran.fs.crash_image(), plans);
    tally.crashes += 1;
    for (plan, state) in states.states {
        if state
            .open(
                &db_dir().join(ManifestStore::FILE_NAME),
                lignin_io::OpenMode::Read,
            )
            .is_err()
        {
            // The power went before the store had its first manifest.
            assert_eq!(
                ran.latest,
                CommitVersion::EMPTY,
                "the manifest is gone: {plan:?}"
            );
            continue;
        }
        let recovered = recover(shared(&state), db_dir(), &ids);
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
        tally.states += 1;
        tally.ahead += u64::from(landed > ran.durable);
        tally.replayed += u64::from(landed > recovered.covers);
        tally.from_checkpoint += u64::from(recovered.checkpoint);
    }
}

fn commit(ops: &[Op]) -> Step {
    Step::Commit(ops.to_vec())
}

fn free(id: u32) -> Op {
    Op::Free { page: page(id) }
}

fn set(at: (u32, u16), value: Val) -> Op {
    Op::Value {
        page: page(at.0),
        slot: slot(at.1),
        value,
    }
}

/// The life of a store with write-back: pages with values in and out of
/// their cells, runs and base images, a consolidation, a value that waits
/// for a private copy and is placed after the commit that carried it on,
/// values moved under a checkpoint, a page freed with a change that was
/// never written back, its id used again, and table checkpoints in
/// between that take log files away.
fn life() -> Vec<Step> {
    vec![
        commit(&[
            insert((1, 1), b"a", inline(b"one")),
            insert((2, 1), b"b", long(1, 900)),
        ]),
        Step::Pass,
        Step::Checkpoint,
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
        Step::SyncLog,
        commit(&[hash((1, 1), 5), free(3)]),
        commit(&[insert((3, 2), b"e", long(4, 600))]),
        Step::Checkpoint,
        commit(&[set((1, 2), long(5, 650)), hash((2, 1), 6)]),
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
    assert_eq!(ran.latest, version(11));
    assert!(ran.checkpoints >= 5, "{} checkpoints", ran.checkpoints);
    assert!(
        ran.log_files_dropped >= 3,
        "{} log files dropped",
        ran.log_files_dropped
    );
    assert_eq!(ran.put_off, 2);
    let mut tally = Tally::default();
    check(&ran, &PLANS, &mut tally);
    assert!(tally.states > 0);
}

const PLANS: Plans = Plans {
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
}
