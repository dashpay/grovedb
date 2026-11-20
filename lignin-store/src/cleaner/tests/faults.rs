//! Faults of the file layer under the cleaner, on the simulated file
//! system.
//!
//! The script of the crash tests is run up to one of the cleaner's steps,
//! and the file layer fails one operation of that step: every operation
//! of every such step in turn, and an append both ways, taken back and
//! left torn in its file. Whatever fails:
//!
//! - no committed page version is lost to a reader, at once and
//!   afterwards;
//! - the session goes on, with more commits and more passes, and either
//!   takes a table checkpoint that covers everything, after which the
//!   cleaner leaves nothing dead in a segment that takes no more blocks,
//!   or reports one of the failures that last until the store is opened
//!   again;
//! - a crash afterwards recovers one committed version, whole.

use std::{
    collections::BTreeMap,
    panic::{catch_unwind, AssertUnwindSafe},
};

use lignin_io::sim::{Event, Fault, FaultRule, OpKind, SimConfig, SimVfs};

use super::{
    crash::{commits, life, new_store, works, Act, Failed},
    *,
};
use crate::{
    flush::{
        tests::crash::{check, Ran, Session, Tally, PLANS},
        FlushError,
    },
    segment::tests::sim::sim_fs,
    wal::LogError,
};

/// One operation of a step of the cleaner.
#[derive(Debug, Clone, Copy)]
struct Target {
    /// The step of the script.
    step: usize,
    /// How many operations into it.
    op: u64,
    /// Whether a failed append is left in its file.
    torn: bool,
}

/// How a session ended after its fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum End {
    /// A table checkpoint covers every commit, and nothing dead is left
    /// where the cleaner can reach it.
    Healed,
    /// A sync of a segment failed: no checkpoint in this session.
    SegmentsNotDurable,
    /// A sync of the log failed: nothing more is durable in this session.
    LogNotDurable,
    /// The log could not find out what became of an append.
    LogUnsettled,
}

fn sticky_flush(error: &FlushError) -> Option<End> {
    match error {
        FlushError::SegmentsNotDurable => Some(End::SegmentsNotDurable),
        FlushError::Segments(crate::segment::SegmentStoreError::SegmentFailed { .. }) => {
            Some(End::SegmentsNotDurable)
        }
        FlushError::Table(crate::table::PageTableError::Store(
            crate::segment::SegmentStoreError::SegmentFailed { .. },
        )) => Some(End::SegmentsNotDurable),
        FlushError::Log(LogError::SyncFailed { .. }) => Some(End::LogNotDurable),
        FlushError::Log(LogError::Unsettled { .. }) => Some(End::LogUnsettled),
        _ => None,
    }
}

fn sticky(error: &Failed) -> Option<End> {
    match error {
        Failed::Store(error) | Failed::Cleaner(CleanerError::Flush(error)) => sticky_flush(error),
        Failed::Cleaner(CleanerError::SegmentNotDurable { .. }) => Some(End::SegmentsNotDurable),
        Failed::Cleaner(_) => None,
    }
}

fn store(trace: bool) -> (SimVfs, Db) {
    let fs = sim_fs(SimConfig {
        trace_ops: trace,
        ..SimConfig::new(17)
    });
    let db = new_store(&fs);
    (fs, db)
}

/// The kinds of the operations that each step of the cleaner in `script`
/// makes when nothing fails, in order. A step of write-back has none
/// here: its faults are write-back's to test.
fn operations(script: &[Act]) -> Vec<Vec<OpKind>> {
    let (fs, db) = store(true);
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(db.wb(), &state);
    let mut session = Session::new(&db);
    let mut spans = Vec::new();
    for act in script {
        commits(&mut session, act);
        let from = fs.stats().ops;
        works(&mut session, &cleaner, &db, act).expect("nothing fails");
        let cleaners = !matches!(act, Act::Store(_));
        spans.push(cleaners.then_some((from, fs.stats().ops)));
    }
    let kinds: BTreeMap<u64, OpKind> = fs
        .trace()
        .into_iter()
        .filter_map(|event| match event {
            Event::Op(record) => Some((record.op, record.kind)),
            _ => None,
        })
        .collect();
    spans
        .into_iter()
        .map(|span| match span {
            Some((from, to)) => (from + 1..=to).map(|op| kinds[&op]).collect(),
            None => Vec::new(),
        })
        .collect()
}

/// Runs `script` up to the step of `target`, fails the operation it names,
/// and then lets the session go on. Returns how it ended and whether the
/// step of the fault reported it.
fn run(script: &[Act], target: Target) -> (End, bool) {
    let (fs, db) = store(false);
    let wb = db.wb();
    let state = CleanerState::new(eager());
    let cleaner = Cleaner::new(wb, &state);
    let mut session = Session::new(&db);
    for act in &script[..target.step] {
        commits(&mut session, act);
        works(&mut session, &cleaner, &db, act).expect("nothing fails yet");
    }
    let act = &script[target.step];
    commits(&mut session, act);
    let rule = FaultRule::new(Fault::Eio)
        .from_op(fs.stats().ops + 1 + target.op)
        .torn_append(target.torn);
    fs.add_rule(rule);
    let reported = works(&mut session, &cleaner, &db, act).is_err();
    fs.clear_rules();
    assert_eq!(fs.stats().faults, 1, "the operation was failed");

    // A reader finds every version, whatever failed.
    db.check_warm();

    // The session goes on. The transaction that held a copy ends, and
    // what waited for it is placed.
    session.release();
    let mut end = None;
    let mut note = |result: Result<(), Failed>| {
        if let Err(error) = result {
            let found = sticky(&error).unwrap_or_else(|| panic!("no fault is left: {error}"));
            end.get_or_insert(found);
        }
    };
    let store = |result: Result<(), FlushError>| result.map_err(Failed::Store);
    let clean = |result: Result<(), CleanerError>| result.map_err(Failed::Cleaner);
    note(store(wb.roll_log().map(drop)));
    if db.log.next_group().is_ok() && !db.log.is_due() {
        db.commit(&[
            insert((9, 1), b"z", long(9, 500)),
            insert((8, 1), b"y", inline(b"after")),
        ]);
        note(store(wb.place_ready().map(drop)));
        db.commit(&[hash((9, 1), 1), hash((1, 1), 77)]);
    }
    note(store(wb.tick().map(drop)));
    note(store(wb.tick().map(drop)));
    for _ in 0..3 {
        note(clean(cleaner.pass().map(drop)));
        note(clean(cleaner.pass_values(&Cells(&db)).map(drop)));
    }
    db.check_warm();
    match wb.checkpoint() {
        Ok(Checkpointed::Installed { covers, .. }) => {
            assert_eq!(covers, db.latest());
            assert_eq!(end, None, "a failure that lasts was reported before");
            // With the checkpoint in force the cleaner finishes its work.
            rest(&db, &cleaner);
            db.check_cold();
            let space = cleaner.space().expect("the space");
            check_space(&db, &space);
            for segment in &space.segments {
                assert!(
                    segment.has_writer || segment.dead_bytes() == 0,
                    "{}",
                    describe(&db, &space)
                );
            }
            assert_eq!(state.stats().segments_unreadable, 0);
            end = Some(End::Healed);
        }
        Ok(put_off) => panic!("nothing holds a copy: {put_off:?}"),
        Err(error) => note(Err(Failed::Store(error))),
    }
    db.check_warm();

    // And whatever a crash keeps now is a committed version.
    let mut tally = Tally::default();
    check(&Ran::of(&fs, &db), &PLANS, &mut tally);
    assert!(tally.states > 0);
    (
        end.expect("the session ended one way or the other"),
        reported,
    )
}

/// **Every operation of the cleaner fails once.** See the module
/// documentation for what holds whichever it is.
#[test]
fn a_fault_at_any_operation_of_the_cleaner_loses_nothing() {
    let script = life();
    let operations = operations(&script);
    let mut targets = Vec::new();
    for (step, kinds) in operations.iter().enumerate() {
        for (op, kind) in kinds.iter().enumerate() {
            let op = op as u64;
            targets.push(Target {
                step,
                op,
                torn: false,
            });
            if *kind == OpKind::Append {
                targets.push(Target {
                    step,
                    op,
                    torn: true,
                });
            }
        }
    }
    assert!(targets.len() > 120, "{} operations", targets.len());

    let mut ends: BTreeMap<End, u64> = BTreeMap::new();
    let mut by_kind: BTreeMap<String, u64> = BTreeMap::new();
    let mut silent = 0;
    for target in &targets {
        let kind = operations[target.step][target.op as usize];
        let ran = catch_unwind(AssertUnwindSafe(|| run(&script, *target)));
        let (end, reported) = ran.unwrap_or_else(|_| {
            panic!(
                "{target:?}: {kind:?} of {:?} failed, and the session did not go on as it should",
                script[target.step]
            )
        });
        *ends.entry(end).or_default() += 1;
        *by_kind.entry(format!("{kind:?}")).or_default() += 1;
        silent += u64::from(!reported);
    }
    println!(
        "{} operations failed: {ends:?}, by kind {by_kind:?}, {silent} not reported by their step",
        targets.len()
    );
    // Most faults leave a session that heals, and the ones that do not
    // are of the kinds that cannot: a sync that failed.
    let healed = ends.get(&End::Healed).copied().unwrap_or(0);
    assert!(healed * 2 > targets.len() as u64, "{ends:?}");
    assert!(ends.len() >= 2, "{ends:?}");
    // The file layer was failed in every kind of operation the cleaner
    // makes.
    for kind in ["Append", "SyncData", "Read", "Rename", "Open", "RemoveFile"] {
        assert!(
            by_kind.keys().any(|found| found.contains(kind)),
            "no {kind} was failed: {by_kind:?}"
        );
    }
}
