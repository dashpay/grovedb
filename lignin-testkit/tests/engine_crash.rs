//! The engine crash campaign (VER-04): its short form, and its own
//! acceptance test.
//!
//! The long form is the command `lignin-crash engine`
//! (`lignin-ci.sh crash-engine`): these are the runs a debug build gets
//! through beside the other tests.
//!
//! A database of the campaign has three threads beside the one that calls
//! it, and where their changes to the files fall is the machine's to say.
//! So these tests assert what the thread of the workload does by itself:
//! the blocks it commits, the changes its own calls make to the files, and
//! what the crash plans of those changes leave. What the three background
//! threads did is printed with the report of each campaign, and held by
//! the campaign itself: every state they left is recovered like any other.

// The engine the golden corpus is pinned to has no LigninDB under GroveDB
// (`build.rs`).
#![cfg(grovedb_engine_seam)]

use std::path::PathBuf;

use lignin_testkit::{
    crash::{
        campaign::Faults,
        check::Kind,
        engine::{
            campaign::{
                campaign, Planted, Report, Script, Settings, Shape, ACTED, WIPE_EARLIER,
                WIPE_EMPTY, WIPE_UNCHANGED,
            },
            cli::DEFECTS,
            model::Position,
            workload::{Plan, Planted as Fault, Step},
        },
    },
    workload::Op,
};

/// A run that a debug build gets through in a quarter of a minute: two
/// lineages of fourteen steps, the first and one that goes on from a state
/// a crash left, with three crash plans of every image.
fn short() -> Shape {
    Shape {
        ops: 14,
        stream_ops: 40,
        lineages: 2,
        depth: 1,
        burst: 4,
        max_plans: 3,
        again_one_in: 8,
        ..Shape::default()
    }
}

fn settings(seed: u64, runs: u64, shape: Shape) -> Settings {
    Settings {
        seed,
        runs: Some(runs),
        shape,
        ..Settings::default()
    }
}

fn run(seed: u64, runs: u64, shape: Shape) -> Report {
    run_saving(seed, runs, shape, None)
}

fn run_saving(seed: u64, runs: u64, shape: Shape, save: Option<PathBuf>) -> Report {
    let settings = Settings {
        save,
        ..settings(seed, runs, shape)
    };
    let report = campaign(&settings, &mut |_, _| {});
    println!("{report}");
    report
}

/// **The short form of the campaign.** Four runs, which between them have
/// every kind of faults: every state that the campaign's crash plans leave
/// at each of their write boundaries opens at a committed block, with that
/// block's root hash and records.
#[test]
fn every_crash_state_of_a_short_campaign_is_a_committed_block() {
    let kinds: Vec<Faults> = (1..=4).map(Faults::of_seed).collect();
    assert_eq!(
        kinds,
        [Faults::None, Faults::Syncs, Faults::All, Faults::None]
    );
    let report = run(1, 4, short());
    assert!(report.clean(), "{report}");
    assert_eq!(report.runs, 4);
    let tally = &report.tally;
    assert_eq!(tally.get("violations"), 0);
    // A state is counted once in its run, and recovered once in each
    // lineage that meets it.
    assert!(tally.get("distinct crash states") <= tally.get("recoveries"));
    assert!(report.points > 0 && report.points <= tally.get("distinct crash states"));
    // What the thread of the workload did by itself, so that a campaign
    // which crashes nothing worth crashing does not pass: every run opens
    // its database, which is made by changes of that thread, each of which
    // is a boundary with a state of its own; every lineage has a session;
    // and the plan that keeps every change is taken of every image.
    for (name, least) in [
        ("runs", 4),
        ("lineages", 4),
        ("sessions", 4),
        ("write boundaries", 4),
        ("write boundaries made by the caller's thread", 4),
        ("write boundaries during an open", 4),
        ("states of a crash during an open", 4),
        ("states of a crash at a change of the caller's thread", 4),
        ("states that a killed process leaves", 4),
        ("crash plans", 8),
    ] {
        assert!(tally.get(name) >= least, "{name}: {}", tally.get(name));
    }
}

/// The blocks that the first lineage of a run commits are a function of
/// its seed, whatever the threads of its database do beside it: two
/// campaigns of one seed, in a run with no faults, have one digest, and a
/// campaign of another seed has another.
#[test]
fn the_blocks_of_a_run_are_a_function_of_its_seed() {
    assert_eq!(
        (Faults::of_seed(5), Faults::of_seed(1)),
        (Faults::None, Faults::None)
    );
    let shape = Shape {
        ops: 30,
        lineages: 1,
        max_plans: 2,
        again_one_in: 0,
        ..short()
    };
    let (one, again, other) = (run(5, 1, shape), run(5, 1, shape), run(1, 1, shape));
    assert!(one.clean() && again.clean() && other.clean());
    assert_eq!(one.digest, again.digest);
    assert_ne!(one.digest, other.digest);
    // The steps were the same too.
    for name in ["operations of the streams", "blocks committed", "sessions"] {
        assert_eq!(one.tally.get(name), again.tally.get(name), "{name}");
    }
    assert!(one.tally.get("blocks committed") > 0);
}

/// **A crash at any point during a wipe leaves a database that opens,
/// either unchanged or empty.** A lineage that wipes again and again: of
/// every image that is taken while a wipe is being made, every crash plan
/// leaves the database at the block before the wipe, at a block before
/// that which was not durable, or empty.
///
/// Every wipe of the lineage follows a write with no transaction, so the
/// grove it wipes holds something, and the first follows a flush after
/// that. So the state with no unsynced change of the image that the first
/// wipe's own commit makes is the database as it was, and of each wipe the
/// state with every change is the empty one: both counts are of what the
/// thread of the workload does by itself.
#[test]
fn a_crash_during_a_wipe_leaves_the_database_unchanged_or_empty() {
    let shape = Shape {
        ops: 26,
        lineages: 1,
        max_plans: 4,
        faults: Some(Faults::None),
        script: Script::Wipes,
        ..short()
    };
    let report = run(1, 2, shape);
    assert!(report.clean(), "{report}");
    let tally = &report.tally;
    // Each lineage makes the first three wipes of the script, each of a
    // grove that a write with no transaction has put an item into.
    assert_eq!(tally.get("wipes"), 6);
    assert_eq!(tally.get("wipes made"), 6);
    assert_eq!(tally.get("writes beside a block made"), 6);
    assert_eq!(tally.get("flushes"), 4);
    assert!(tally.get("write boundaries during a wipe") >= 6);
    assert!(tally.get(WIPE_UNCHANGED) >= 2, "{report}");
    assert!(tally.get(WIPE_EMPTY) >= 6, "{report}");
    println!(
        "{} as it was, {} at an earlier block, {} empty",
        tally.get(WIPE_UNCHANGED),
        tally.get(WIPE_EARLIER),
        tally.get(WIPE_EMPTY)
    );
}

/// **The campaign's own acceptance test**, in its short form: three of the
/// planted defects, each found by the first violation that fits it, where
/// the run of that violation's seed with no defect planted finds none.
/// `lignin-crash engine planted` plants all of them.
///
/// Each is found by what the thread of the workload does by itself. A
/// write that the model misses shows at the latest at the commit of its
/// block, where the two root hashes differ. A wipe that is two commits
/// leaves, of the image of its first commit, the state with every change,
/// which is at the version that commit made: neither the database as it
/// was nor empty. And where no sync of a log file does anything, the
/// first log file of the database is empty in the state with no unsynced
/// change of every image from the one after its header was to be synced:
/// no thread makes a byte of it durable, and the manifest of the new
/// database names the file with its header.
#[test]
fn the_campaign_finds_what_is_planted() {
    let wipes = Shape {
        ops: 26,
        lineages: 1,
        max_plans: 4,
        faults: Some(Faults::None),
        script: Script::Wipes,
        ..short()
    };
    let drawn = Shape {
        lineages: 1,
        faults: Some(Faults::None),
        ..short()
    };
    for (name, shape) in [
        ("forgotten-write", drawn),
        ("split-wipe", wipes),
        ("new-log-sync", wipes),
    ] {
        let defect = (DEFECTS.into_iter())
            .find(|defect| defect.name == name)
            .expect("a defect of the command");
        let control = run(1, 1, shape);
        assert!(control.clean(), "{name}: {control}");
        assert_eq!(control.tally.get(ACTED), 0);
        let planted = Shape {
            planted: Some(defect.planted),
            ..shape
        };
        let states = tempfile::tempdir().expect("a directory");
        let report = run_saving(1, 1, planted, Some(states.path().to_path_buf()));
        assert!(report.tally.get(ACTED) > 0, "{name}: {report}");
        assert!(!report.acts.is_empty(), "{name}: {report}");
        let own = (report.violations.iter()).find(|found| defect.found_by(found, &report.acts));
        let own = own.unwrap_or_else(|| panic!("{name}: {report}"));
        // A violation of a crash state is reported with the files of the
        // state, in a directory of its own, and one of a database that
        // was not crashed with none.
        assert_eq!(own.not_saved, None, "{name}");
        match &own.saved {
            Some(dir) => {
                assert!(own.boundary.is_some(), "{name}");
                assert!(dir.starts_with(states.path()), "{name}");
                assert!(dir.join("db").is_dir(), "{name}");
                let said = std::fs::read_to_string(dir.join("VIOLATION.txt")).expect("read");
                assert_eq!(said, format!("{own}\n"), "{name}");
            }
            None => assert!(own.boundary.is_none(), "{name}"),
        }
    }
    assert!(matches!(DEFECTS[8].planted, Planted::SplitWipe));
    assert_eq!(DEFECTS[7].kinds, [Kind::Session]);
}

/// The steps of a lineage that takes every kind of step by a list, with
/// nothing drawn: two checkpoints are made and deleted, each with a write
/// of the workload's own after it, the session is killed and one is
/// closed, and the grove is wiped.
const TOUR: [Step; 27] = [
    Step::Stream,
    Step::Stream,
    Step::Stream,
    Step::Stream,
    Step::Beside,
    Step::Checkpoint,
    Step::Beside,
    Step::Flush,
    Step::Checkpoint,
    Step::Stream,
    Step::Stream,
    Step::DeleteCheckpoint,
    Step::Beside,
    Step::Settle,
    Step::Kill,
    Step::Stream,
    Step::Stream,
    Step::Beside,
    Step::DeleteCheckpoint,
    Step::Beside,
    Step::Close,
    Step::Stream,
    Step::Stream,
    Step::Beside,
    Step::Wipe,
    Step::Stream,
    Step::Stream,
];

/// **Checkpoints, a kill and a close**, by a list of steps
/// ([`TOUR`]). What the thread of the workload does by itself is counted:
/// the three sessions that the kill and the close make, the checkpoints
/// that were made and deleted, and the writes beside a block. Each write
/// beside a block is an append to the log of the caller's own, so the
/// state that keeps every change of its image is one that was not
/// recovered before: after the making of a checkpoint that state holds
/// the checkpoint whole, and after its deletion it holds no directory of
/// it.
#[test]
fn a_lineage_of_listed_steps_makes_and_deletes_checkpoints_and_is_killed() {
    let shape = Shape {
        ops: TOUR.len(),
        lineages: 1,
        max_plans: 4,
        again_one_in: 4,
        faults: Some(Faults::None),
        script: Script::Listed(&TOUR),
        ..short()
    };
    let report = run(1, 1, shape);
    assert!(report.clean(), "{report}");
    let tally = &report.tally;
    assert_eq!(tally.get("sessions"), 3, "{report}");
    assert_eq!(tally.get("kills"), 1);
    assert_eq!(tally.get("closes"), 1);
    assert_eq!(tally.get("checkpoints made"), 2, "{report}");
    assert_eq!(
        tally.get("deletions of a checkpoint that returned"),
        2,
        "{report}"
    );
    assert_eq!(tally.get("writes beside a block made"), 6, "{report}");
    assert!(tally.get("wipes made") >= 1, "{report}");
    assert!(
        tally.get("checkpoints found whole that were made") >= 1,
        "{report}"
    );
    assert!(
        tally.get("checkpoints gone whose deletion had returned") >= 1,
        "{report}"
    );
    assert!(
        tally.get("states that a killed process leaves") >= 6,
        "{report}"
    );
    assert!(tally.get("images taken when a call of the workload had answered") >= 12);
}

/// **A flush of the pending drops**, which commits more than once. The
/// run is of the first seed whose first stream drops subtrees flat,
/// commits the block and flushes the drops, and its lineage takes the
/// operations of that stream and nothing else, to the one after the
/// flush. Each commit of the flush is an append to the log of the
/// caller's own, so the state that keeps every change of its image is at
/// that commit: inside the flush, for every commit but the last. Each is
/// held to what a copy of the files held at that version of its flush.
#[test]
fn the_states_inside_a_flush_of_pending_drops_are_those_of_its_commits() {
    let stream_ops = 40;
    let flushes = |seed: u64| {
        let mut plan = Plan::of_seed(seed, stream_ops);
        let op = |op: usize| plan.op(Position { stream: 0, op });
        let ops: Vec<Op> = (0..stream_ops).map_while(op).collect();
        let flush = ops.iter().position(|op| *op == Op::FlushPendingDrops)?;
        let drops = |op: &Op| format!("{op:?}").contains("DropFlat");
        let dropped = flush > 0 && ops[flush - 1] == Op::Commit && ops[..flush].iter().any(drops);
        dropped.then_some((seed, flush))
    };
    let (seed, flush) = (1..500)
        .find_map(flushes)
        .expect("a seed whose stream flushes");
    println!("seed {seed}: operation {flush} of the first stream flushes the drops");
    let shape = Shape {
        ops: flush + 2,
        stream_ops,
        lineages: 1,
        max_plans: 4,
        again_one_in: 4,
        faults: Some(Faults::None),
        script: Script::Listed(&[Step::Stream]),
        ..short()
    };
    let report = run(seed, 1, shape);
    assert!(report.clean(), "{report}");
    let tally = &report.tally;
    assert_eq!(
        tally.get("flushes of pending drops that committed"),
        1,
        "{report}"
    );
    let commits = tally.get("commits of the flushes of pending drops");
    assert!(commits >= 2, "{report}");
    let inside = tally.get("states inside an operation that commits more than once");
    assert!(inside >= commits - 1, "{report}");
    assert!(tally.get("states of a crash during a flush of pending drops") >= commits);
}

/// **A panic of the campaign while it holds the threads of a database**
/// ends the run with a violation, and the run does end: the threads are
/// let go before the database is dropped, which waits for them. The panic
/// is made by asking for more crash plans of an image than a list can
/// hold, which fails at the first image that is gone through.
#[test]
fn a_panic_while_the_threads_of_a_database_are_held_ends_the_run() {
    let shape = Shape {
        ops: 4,
        lineages: 1,
        every_plan_limit: 0,
        samples: u64::MAX,
        max_plans: 0,
        faults: Some(Faults::None),
        ..short()
    };
    let report = run(1, 1, shape);
    assert_eq!(report.violations.len(), 1, "{report}");
    let found = &report.violations[0];
    assert_eq!(found.violation.kind, Kind::Panicked, "{report}");
    assert!(
        found.violation.to_string().contains("the lineage panicked"),
        "{report}"
    );
}

/// The steps of a lineage that plants a fault of each kind by a list,
/// with nothing drawn, among operations of its stream, writes beside a
/// block, flushes, checkpoints, a kill and a wipe.
const FAULTED: [Step; 40] = [
    Step::Stream,
    Step::Stream,
    Step::Stream,
    Step::Fault(Fault::Sync),
    Step::Stream,
    Step::Stream,
    Step::Beside,
    Step::Flush,
    Step::Fault(Fault::Write),
    Step::Stream,
    Step::Stream,
    Step::Beside,
    Step::Checkpoint,
    Step::Fault(Fault::Name),
    Step::Stream,
    Step::Stream,
    Step::Checkpoint,
    Step::DeleteCheckpoint,
    Step::Fault(Fault::Full),
    Step::Stream,
    Step::Stream,
    Step::Beside,
    Step::Flush,
    Step::Fault(Fault::Sync),
    Step::Beside,
    Step::Settle,
    Step::Stream,
    Step::Kill,
    Step::Stream,
    Step::Stream,
    Step::Fault(Fault::Write),
    Step::Beside,
    Step::Wipe,
    Step::Stream,
    Step::Fault(Fault::Sync),
    Step::Checkpoint,
    Step::Beside,
    Step::Close,
    Step::Stream,
    Step::Stream,
];

/// **Faults of every kind**, planted by a list of steps ([`FAULTED`]). A
/// fault fails the next call of its kind, whichever thread makes it, so
/// what a lineage does after its first fault is the machine's to say, and
/// this asserts only that every step of the list was taken and that the
/// campaign finds nothing: a session that a fault ends is killed, opened
/// again, and held to what it had committed.
#[test]
fn a_lineage_with_faults_of_every_kind_is_clean() {
    for seed in [1, 2, 3] {
        let shape = Shape {
            ops: FAULTED.len(),
            lineages: 2,
            faults: Some(Faults::All),
            script: Script::Listed(&FAULTED),
            ..short()
        };
        let report = run(seed, 1, shape);
        assert!(report.clean(), "{report}");
        let planted = FAULTED.iter().filter(|step| matches!(step, Step::Fault(_)));
        assert!(
            report.tally.get("faults planted") >= planted.count() as u64,
            "{report}"
        );
    }
}
