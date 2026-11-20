//! Crashes while the cleaner works, on the simulated file system (tests
//! T-VL3 and T-DU4 of `transactions.md`, with the cleaner among what
//! runs).
//!
//! A script of commits, passes of write-back, table checkpoints and passes
//! of the cleaner over the segments and over the value log is run once for
//! every change it makes to what a crash can leave, with the power going
//! off after that change. Every state the crash can leave is recovered,
//! by the recovery of write-back's tests and by the store's own, and must
//! be one committed version, whole: every page that is in use there is
//! read from blocks that are there, with every value, and the counts of
//! live values are the pointers the pages hold. So no crash finds a
//! segment or a value-log file gone that the recovered state points into.
//!
//! Some of those states are then opened and cleaned by a second session.

use std::panic::{catch_unwind, AssertUnwindSafe};

use lignin_io::sim::{CrashPoint, SimConfig, SimVfs};

use super::*;
use crate::{
    flush::tests::{
        crash::{check, commit, free, log_options, Ran, Session, Step, Tally, PLANS},
        recover::options,
        Model,
    },
    flush::FlushError,
    segment::tests::sim::{crash_states, db_dir, shared, sim_fs, Plans},
    values::ValueStore,
};

/// One step of a script.
#[derive(Debug, Clone)]
pub(super) enum Act {
    /// A step of write-back's scripts: a commit, a pass, a checkpoint, a
    /// transaction that holds a copy.
    Store(Step),
    /// A pass of the cleaner over the segments.
    Clean,
    /// A pass of the cleaner over the value log.
    CleanValues,
}

fn set(at: (u32, u16), value: Val) -> Op {
    Op::Value {
        page: page(at.0),
        slot: slot(at.1),
        value,
    }
}

/// The life of a store with a cleaner: pages written again and again in
/// small segments, a value-log file that is half dead and is cleaned, a
/// move that a private copy puts off, a freed page whose chain is
/// trimmed and whose id is used again, and table checkpoints, of the
/// script's and of the cleaner's asking, between which segments wait.
pub(super) fn life() -> Vec<Act> {
    use Act::{Clean, CleanValues, Store};
    vec![
        Store(commit(&[
            insert((1, 1), b"a", long(1, 400)),
            insert((1, 2), b"b", long(2, 400)),
            insert((2, 1), b"c", inline(&noise(2, 150))),
        ])),
        Store(Step::Pass),
        Store(commit(&[
            insert((3, 1), b"d", long(3, 400)),
            insert((3, 2), b"e", long(4, 400)),
        ])),
        Store(Step::Pass),
        Store(Step::Checkpoint),
        Store(commit(&[hash((1, 1), 1), hash((2, 1), 1)])),
        Store(Step::Pass),
        Clean,
        // Half of a value-log file dies, and its live value is moved.
        Store(commit(&[set((1, 2), long(5, 300)), hash((2, 1), 2)])),
        Store(Step::Pass),
        CleanValues,
        Store(commit(&[hash((2, 1), 3), hash((3, 1), 3)])),
        Store(Step::Pass),
        Clean,
        // The file that the move emptied goes.
        CleanValues,
        // A page is freed: its chain is trimmed, and what held it goes.
        Store(commit(&[free(2), hash((1, 1), 4)])),
        Store(Step::Pass),
        Clean,
        // A move that a private copy puts off, and that is made later.
        Store(commit(&[set((3, 2), long(6, 300))])),
        Store(Step::Pass),
        Store(Step::Hold(3)),
        CleanValues,
        Store(Step::Release),
        CleanValues,
        Store(commit(&[hash((3, 1), 5)])),
        Store(Step::Pass),
        Clean,
        CleanValues,
        // The id of the freed page names another page.
        Store(commit(&[insert((2, 1), b"again", inline(&noise(9, 100)))])),
        Store(Step::Pass),
        Store(Step::Checkpoint),
        Clean,
        Store(commit(&[hash((2, 1), 6), hash((1, 1), 6)])),
        Store(Step::Pass),
        Clean,
        CleanValues,
    ]
}

/// A new store on `fs` whose value-log files are full after two values.
pub(super) fn new_store(fs: &SimVfs) -> Db {
    new_store_with(fs, small_segments())
}

/// The same, with a write-back that works as `config` says.
pub(super) fn new_store_with(fs: &SimVfs, config: FlushConfig) -> Db {
    let mut db = Db::create(shared(fs), db_dir(), config, log_options());
    let values = {
        let manifest = db.manifest.lock().expect("the manifest");
        ValueStore::open(&manifest, &[]).expect("the value log")
    };
    db.values = values.with_target_file_len(700);
    db
}

/// Why a step of a script failed.
#[derive(Debug)]
pub(super) enum Failed {
    Store(FlushError),
    Cleaner(CleanerError),
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => error.fmt(f),
            Self::Cleaner(error) => error.fmt(f),
        }
    }
}

/// The part of `act` that is a committer's: what is done before the part
/// that writes files.
pub(super) fn commits(session: &mut Session<'_>, act: &Act) {
    if let Act::Store(step) = act {
        session.commits(step);
    }
}

/// The part of `act` that is write-back's or the cleaner's.
pub(super) fn works(
    session: &mut Session<'_>,
    cleaner: &Cleaner<'_>,
    db: &Db,
    act: &Act,
) -> Result<(), Failed> {
    match act {
        Act::Store(step) => session.writes_back(step).map_err(Failed::Store),
        Act::Clean => cleaner.pass().map(drop).map_err(Failed::Cleaner),
        Act::CleanValues => {
            let pass = cleaner.pass_values(&Cells(db));
            pass.map(drop).map_err(Failed::Cleaner)
        }
    }
}

/// Makes the steps of `script` on `db`, with a cleaner of the state
/// `state`, until one fails. Whatever fails, fails because the power went
/// off.
fn act(fs: &SimVfs, db: &Db, state: &CleanerState, script: &[Act]) {
    let mut session = Session::new(db);
    let cleaner = Cleaner::new(db.wb(), state);
    for act in script {
        commits(&mut session, act);
        if let Err(error) = works(&mut session, &cleaner, db, act) {
            assert!(
                fs.is_powered_off(),
                "{act:?} failed with the power on: {error}"
            );
            return;
        }
    }
}

/// Runs `script` on a new store, with the power going off after `crash`
/// changes to what a crash can leave, if that many are made.
fn run(script: &[Act], crash: Option<u64>) -> (Ran, CleanerStats) {
    let fs = sim_fs(SimConfig::new(17));
    if let Some(changes) = crash {
        fs.crash_at(CrashPoint::AfterChange(changes));
    }
    let Ok(db) = catch_unwind(AssertUnwindSafe(|| new_store(&fs))) else {
        assert!(fs.is_powered_off(), "the store was not made");
        let ran = Ran {
            fs,
            model: Model::default(),
            durable: CommitVersion::EMPTY,
            latest: CommitVersion::EMPTY,
            checkpoints: 0,
            log_files_dropped: 0,
            put_off: 0,
        };
        return (ran, CleanerStats::default());
    };
    let state = CleanerState::new(eager());
    let steps = catch_unwind(AssertUnwindSafe(|| act(&fs, &db, &state, script)));
    assert!(
        steps.is_ok() || fs.is_powered_off(),
        "a step failed with the power on"
    );
    (Ran::of(&fs, &db), state.stats())
}

/// What the script is meant to go through, on a run with no crash.
#[test]
fn the_script_trims_moves_and_deletes() {
    let (ran, stats) = run(&life(), None);
    assert!(!ran.fs.is_powered_off());
    assert_eq!(ran.latest, version(10));
    assert!(stats.links_trimmed >= 1, "{stats:?}");
    assert!(stats.blocks_moved >= 3, "{stats:?}");
    assert!(stats.segments_dropped >= 4, "{stats:?}");
    assert_eq!(stats.segments_retired, stats.segments_dropped, "{stats:?}");
    assert!(stats.checkpoints >= 1, "{stats:?}");
    assert!(stats.segments_sealed >= 1, "{stats:?}");
    assert!(stats.value_files_closed >= 1, "{stats:?}");
    assert!(stats.values_moved >= 2, "{stats:?}");
    assert!(stats.values_left >= 1, "{stats:?}");
    assert!(stats.value_files_dropped >= 2, "{stats:?}");
    assert_eq!((stats.errors, stats.blocks_lost), (0, 0), "{stats:?}");
    let mut tally = Tally::default();
    check(&ran, &PLANS, &mut tally);
    assert!(tally.states > 0);
}

/// What the second sessions came to.
#[derive(Debug, Default)]
struct Sequels {
    sessions: u64,
    /// Segments that held nothing a link points to when the session began
    /// and that it deleted: what the session before left with no link.
    dropped: u64,
    moved: u64,
    trimmed: u64,
}

/// Opens `state`, which a crash of `ran` left, as a session opens it, and
/// cleans it: what the recovery landed on is read whole before and after,
/// nothing dead is left in a segment that takes no more blocks, and what
/// the session leaves recovers.
fn clean_on(state: &SimVfs, ran: &Ran, sequels: &mut Sequels, tally: &mut Tally) {
    let db = Db::recover(shared(state), db_dir(), &options(None), ran.model.clone());
    db.check_warm();
    let cleaner_state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &cleaner_state);
    settle(&cleaner);
    cleaner.pass_values(&Cells(&db)).expect("a pass");
    db.check_warm();
    // The pages that the recovery rebuilt and those whose pointers a
    // cleaning rewrote are written, a checkpoint covers them, and the
    // cleaner takes what that sets free.
    rest(&db, &cleaner);
    db.check_warm();
    db.check_cold();
    let space = cleaner.space().expect("the space");
    check_space(&db, &space);
    for segment in &space.segments {
        assert!(
            segment.is_written() || segment.dead_bytes() == 0,
            "{}",
            describe(&db, &space)
        );
    }
    let stats = cleaner_state.stats();
    sequels.sessions += 1;
    sequels.dropped += stats.segments_dropped;
    sequels.moved += stats.blocks_moved;
    sequels.trimmed += stats.links_trimmed;
    check(&Ran::of(state, &db), &SECOND, tally);
}

/// A few states of each crash, for a second session.
const SECOND: Plans = Plans {
    every_plan_limit: 4,
    samples: 2,
};

/// **Tests T-VL3 and T-DU4 with the cleaner at work, at every crash
/// point.** The power goes off after each change that the script makes to
/// what a crash can leave. Whatever the crash keeps, a recovery lands on
/// one committed version at or after the last one that was known durable,
/// and every page that is in use there says what its commits left, with
/// every value: no block and no value that it points to is gone.
#[test]
fn a_crash_at_any_point_of_the_cleaner_recovers_one_committed_version_whole() {
    let script = life();
    let (whole, _) = run(&script, None);
    let changes = whole.fs.stats().changes;
    let (mut tally, mut after, mut sequels) =
        (Tally::default(), Tally::default(), Sequels::default());
    for crash in 1..=changes {
        let (ran, _) = run(&script, Some(crash));
        assert!(
            ran.fs.is_powered_off(),
            "the power goes off at change {crash}"
        );
        check(&ran, &PLANS, &mut tally);
        // Every seventh crash, a session goes on from some of its states.
        if crash % 7 == 0 && ran.latest > CommitVersion::EMPTY {
            for (_, state) in crash_states(&ran.fs.crash_image(), &SECOND).states {
                clean_on(&state, &ran, &mut sequels, &mut after);
            }
        }
    }
    assert_eq!(tally.crashes, changes);
    assert!(tally.states > 2 * changes, "{tally:?}");
    assert!(tally.ahead > 100 && tally.replayed > 100, "{tally:?}");
    assert!(sequels.sessions > changes / 7, "{sequels:?}");
    assert!(sequels.dropped > sequels.sessions, "{sequels:?}");
    assert!(sequels.moved > 0 && sequels.trimmed > 0, "{sequels:?}");
    assert!(after.states > sequels.sessions, "{after:?}");
    println!("{changes} crash points: {tally:?}; {sequels:?}; afterwards {after:?}");
}
