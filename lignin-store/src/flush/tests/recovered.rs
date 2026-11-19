//! Write-back on a store that was recovered (STORE-07, with tests T-DU3,
//! T-DU4, T-ID1 and T-VL6 of `transactions.md` across two sessions).
//!
//! A session is crashed, the store's recovery opens what it left, and a
//! second session goes on from there: commits, passes, value moves and
//! table checkpoints. That session is crashed too, at several points, and
//! every state it can leave is recovered. What the second recovery lands
//! on must be one committed version of the two sessions together.
//!
//! What this holds, that no test of one session does:
//!
//! - **A page that the first recovery rebuilt holds the changes of several
//!   replayed commits, and the pool lists it under the newest.** A table
//!   checkpoint of the second session must cover none of those commits
//!   while the page is in no block, or the next recovery starts after
//!   changes that only the log had.
//! - **The commits that the first crash took give their versions to the
//!   commits of the second session** (DU3), and no later recovery replays
//!   a group of the commits that were lost.
//! - **The log files of the first session are dropped by the checkpoints
//!   of the second**, once it covers what they hold, and not before.

use lignin_io::sim::{CrashPoint, SimVfs};

use super::{
    crash::{check, commit, flat, free, life, run, Ran, Session, Step, Tally},
    recover::options,
    *,
};
use crate::segment::tests::sim::{crash_states, db_dir, shared, Plans};

/// What the second session does: new pages, changes to the pages that
/// the first session left, if the version the store landed on has them,
/// a freed page whose id is used again, a flat leaf, a value move, and
/// table checkpoints in between.
fn sequel(model: &Model, landed: CommitVersion) -> Vec<Step> {
    let mut steps = vec![commit(&[
        insert((30, 1), b"n", long(21, 700)),
        insert((31, 1), b"o", inline(b"small")),
    ])];
    for id in [1, 2, 3] {
        let cells = model.at(page(id), landed).map(|(_, cells)| cells);
        if let Some(cell) = cells.and_then(<[Cell]>::first) {
            steps.push(commit(&[hash((id, cell.slot), 0x70 + id as u8)]));
        }
    }
    steps.extend([
        Step::Pass,
        Step::Checkpoint,
        commit(&[hash((30, 1), 0x33), free(31)]),
        flat(41, &[(b"k", long(22, 600))]),
        Step::Checkpoint,
        commit(&[insert((31, 2), b"p", long(23, 500))]),
        Step::Pass,
        Step::Move(30, 1),
        Step::Checkpoint,
        commit(&[hash((30, 1), 0x34)]),
        Step::SyncLog,
    ]);
    steps
}

/// A few states of each crash that the second session can leave.
const SECOND: Plans = Plans {
    every_plan_limit: 6,
    samples: 2,
};

/// What two sessions came to.
#[derive(Debug, Default)]
struct Twice {
    /// Stores that were recovered and went on.
    recovered: u64,
    /// Of them, those whose recovery replayed commits.
    replayed: u64,
    /// Table checkpoints the second sessions installed.
    checkpoints: u64,
    /// Log files the second sessions dropped.
    log_files_dropped: u64,
    /// Commits of the first session that the first crash took.
    lost: u64,
}

/// Recovers the state `state` that the first session `first` left, runs
/// the second session on it, and recovers what that leaves after each of
/// its steps and, if `power_off` says so, with the power going off after
/// that many changes of the second session. Returns how many changes the
/// steps of the second session made to what a crash can leave.
fn go_on(
    state: &SimVfs,
    first: &Ran,
    kept: Option<usize>,
    power_off: Option<u64>,
    twice: &mut Twice,
    tally: &mut Tally,
) -> u64 {
    let db = Db::recover(shared(state), db_dir(), &options(kept), first.model.clone());
    let landed = db.latest();
    assert!(first.durable <= landed && landed <= first.latest);
    twice.recovered += 1;
    twice.replayed += u64::from(db.stats().log_files_dropped == 0 && db.wb().log_held() > 0);
    twice.lost += first.latest.get() - landed.get();
    // What the first session committed is read through the pool the
    // recovery filled, with every value.
    db.check_warm();

    let script = sequel(&db.model.lock().expect("the model"), landed);
    let before = state.stats().changes;
    if let Some(changes) = power_off {
        state.crash_at(CrashPoint::AfterChange(before + changes));
    }
    let steps = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut session = Session::new(&db);
        for step in &script {
            session.commits(step);
            if let Err(error) = session.writes_back(step) {
                assert!(
                    state.is_powered_off(),
                    "{step:?} failed with the power on: {error}"
                );
                return;
            }
            if power_off.is_none() {
                db.check_warm();
                check(&Ran::of(state, &db), &SECOND, tally);
            }
        }
    }));
    assert!(
        steps.is_ok() || state.is_powered_off(),
        "a step of the second session failed with the power on"
    );
    assert_eq!(state.is_powered_off(), power_off.is_some());
    let made = state.stats().changes - before;
    let stats = db.stats();
    twice.checkpoints += stats.checkpoints;
    twice.log_files_dropped += stats.log_files_dropped;
    if power_off.is_some() {
        check(&Ran::of(state, &db), &SECOND, tally);
    } else {
        // With the power on, the session ends as a store ends it, and the
        // next one opens what it left with nothing to replay.
        let ended = db.latest();
        assert!(stats.checkpoints >= 3, "{stats:?}");
        let model = db.model.lock().expect("the model").clone();
        db.wb().finish().expect("the session ends");
        drop(db);
        let again = Db::recover(shared(state), db_dir(), &options(kept), model);
        assert_eq!(again.latest(), ended);
        assert_eq!(again.wb().log_held(), 0);
        again.check_warm();
        again.check_cold();
    }
    made
}

/// **A store that was recovered goes on, and what it leaves recovers.**
/// The first session is cut at every seventh change it makes to what a
/// crash can leave. Two of the states of each cut are recovered by the
/// store's recovery, one with every rebuilt page kept in memory and one
/// with a single page kept, and a second session runs on each. Every
/// state that the second session leaves after each of its steps is
/// recovered again, and is one committed version of the two sessions,
/// whole.
#[test]
fn a_store_that_was_recovered_goes_on_and_what_it_leaves_recovers() {
    let script = life();
    let whole = run(&script, None);
    let changes = whole.fs.stats().changes;
    let (mut twice, mut tally) = (Twice::default(), Tally::default());
    for crash in (1..=changes).step_by(7).chain([changes]) {
        let first = run(&script, Some(crash));
        let image = first.fs.crash_image();
        let states = crash_states(&image, &SECOND).states;
        for (at, (_, state)) in states.into_iter().enumerate().take(2) {
            if state
                .open(
                    &db_dir().join(ManifestStore::FILE_NAME),
                    lignin_io::OpenMode::Read,
                )
                .is_err()
            {
                continue;
            }
            let kept = (at == 1).then_some(1);
            go_on(&state, &first, kept, None, &mut twice, &mut tally);
        }
    }
    println!("{twice:?}, {tally:?}");
    assert!(twice.recovered > 60, "{twice:?}");
    assert!(twice.replayed > 20, "{twice:?}");
    assert!(twice.lost > 0, "{twice:?}");
    assert!(twice.checkpoints >= 3 * twice.recovered, "{twice:?}");
    assert!(twice.log_files_dropped > twice.recovered, "{twice:?}");
    assert!(tally.states > 10 * twice.recovered, "{tally:?}");
}

/// The same, with the power going off in the second session: after each
/// third change it makes, on stores that the first session left at four
/// points of its life.
#[test]
fn a_crash_of_the_session_after_a_recovery_recovers_too() {
    let script = life();
    let whole = run(&script, None);
    let changes = whole.fs.stats().changes;
    let (mut twice, mut tally) = (Twice::default(), Tally::default());
    for crash in [changes / 5, changes / 2, 3 * changes / 4, changes] {
        let first = run(&script, Some(crash));
        let image = first.fs.crash_image();
        // How many changes the second session makes on this store.
        let made = {
            let state = crash_states(&image, &SECOND).states.swap_remove(0).1;
            go_on(&state, &first, None, None, &mut twice, &mut tally)
        };
        for power_off in (1..=made).step_by(3) {
            let state = crash_states(&image, &SECOND).states.swap_remove(0).1;
            go_on(
                &state,
                &first,
                None,
                Some(power_off),
                &mut twice,
                &mut tally,
            );
        }
    }
    println!("{twice:?}, {tally:?}");
    assert!(twice.recovered > 100, "{twice:?}");
    assert!(tally.states > 2 * twice.recovered, "{tally:?}");
}
