//! Faults of the file layer, on the simulated file system.
//!
//! The script of the crash tests is run up to one of its steps, and the
//! file layer fails one operation of the part of that step that is
//! write-back's: every operation of every such part in turn, and an append
//! both ways, taken back and left torn in its file. Whatever fails:
//!
//! - no committed page version is lost to a reader, at once and afterwards;
//! - the session goes on, with another commit, and either takes a table
//!   checkpoint that covers everything, or reports one of the failures
//!   that last until the store is opened again;
//! - a crash afterwards recovers one committed version, whole, at or after
//!   the last one that was known durable.

use std::{
    collections::BTreeMap,
    panic::{catch_unwind, AssertUnwindSafe},
};

use lignin_io::sim::{Event, Fault, FaultRule, OpKind, SimConfig, SimVfs};

use super::{
    crash::{check, config, life, log_options, Ran, Session, Step, Tally, PLANS},
    *,
};
use crate::{
    segment::tests::sim::{db_dir, shared, sim_fs},
    wal::LogError,
};

/// One operation of the part of a step that is write-back's.
#[derive(Debug, Clone, Copy)]
struct Target {
    /// The step of the script.
    step: usize,
    /// How many operations into that part.
    op: u64,
    /// Whether a failed append is left in its file.
    torn: bool,
}

/// How a session ended after its fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum End {
    /// A table checkpoint covers every commit.
    Healed,
    /// A sync of a segment failed: no checkpoint in this session.
    SegmentsNotDurable,
    /// A sync of the log failed: nothing more is durable in this session.
    LogNotDurable,
    /// The log could not find out what became of an append.
    LogUnsettled,
}

fn sticky(error: &FlushError) -> Option<End> {
    match error {
        FlushError::SegmentsNotDurable => Some(End::SegmentsNotDurable),
        FlushError::Log(LogError::SyncFailed { .. }) => Some(End::LogNotDurable),
        FlushError::Log(LogError::Unsettled { .. }) => Some(End::LogUnsettled),
        _ => None,
    }
}

fn new_store(trace: bool) -> (SimVfs, Db) {
    let fs = sim_fs(SimConfig {
        trace_ops: trace,
        ..SimConfig::new(11)
    });
    let db = Db::create(shared(&fs), db_dir(), config(), log_options());
    (fs, db)
}

/// The kinds of the operations that the write-back part of each step of
/// `script` makes when nothing fails, in order.
fn operations(script: &[Step]) -> Vec<Vec<OpKind>> {
    let (fs, db) = new_store(true);
    let mut session = Session::new(&db);
    let mut spans = Vec::new();
    for step in script {
        session.commits(step);
        let from = fs.stats().ops;
        session.writes_back(step).expect("nothing fails");
        spans.push((from, fs.stats().ops));
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
        .map(|(from, to)| (from + 1..=to).map(|op| kinds[&op]).collect())
        .collect()
}

/// Runs `script` up to the step of `target`, fails the operation it names,
/// and then lets the session go on. Returns how it ended and whether the
/// step of the fault reported it.
fn run(script: &[Step], target: Target) -> (End, bool) {
    let (fs, db) = new_store(false);
    let wb = db.wb();
    let mut session = Session::new(&db);
    for step in &script[..target.step] {
        session.commits(step);
        session.writes_back(step).expect("nothing fails yet");
    }
    let step = &script[target.step];
    session.commits(step);
    let rule = FaultRule::new(Fault::Eio)
        .from_op(fs.stats().ops + 1 + target.op)
        .torn_append(target.torn);
    fs.add_rule(rule);
    let reported = session.writes_back(step).is_err();
    fs.clear_rules();
    assert_eq!(fs.stats().faults, 1, "the operation was failed");

    // A reader finds every version, whatever failed.
    db.check_warm();

    // The session goes on. The transaction that held a copy ends, and
    // what waited for it is placed.
    session.release();
    let mut end = None;
    let mut note = |result: Result<(), FlushError>| {
        if let Err(error) = result {
            let found = sticky(&error).unwrap_or_else(|| panic!("no fault is left: {error}"));
            end.get_or_insert(found);
        }
    };
    note(wb.roll_log().map(drop));
    if db.log.next_group().is_ok() && !db.log.is_due() {
        db.commit(&[
            insert((9, 1), b"z", long(9, 500)),
            insert((8, 1), b"y", inline(b"after")),
        ]);
        note(wb.place_ready().map(drop));
        db.commit(&[hash((9, 1), 1)]);
    }
    note(wb.tick().map(drop));
    note(wb.tick().map(drop));
    match wb.checkpoint() {
        Ok(Checkpointed::Installed { covers, .. }) => {
            assert_eq!(covers, db.latest());
            assert_eq!(end, None, "a failure that lasts was reported before");
            db.check_cold();
            end = Some(End::Healed);
        }
        Ok(put_off) => panic!("nothing holds a copy: {put_off:?}"),
        Err(error) => note(Err(error)),
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

/// **Every operation of write-back fails once.** See the module
/// documentation for what holds whichever it is.
#[test]
fn a_fault_at_any_operation_of_write_back_loses_nothing() {
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
    assert!(targets.len() > 250, "{} operations", targets.len());

    let mut ends: BTreeMap<End, u64> = BTreeMap::new();
    let mut silent = 0;
    for target in &targets {
        let ran = catch_unwind(AssertUnwindSafe(|| run(&script, *target)));
        let (end, reported) = ran.unwrap_or_else(|_| {
            let kind = operations[target.step][target.op as usize];
            panic!(
                "{target:?}: {kind:?} of {:?} failed, and the session did not go on as it should",
                script[target.step]
            )
        });
        *ends.entry(end).or_default() += 1;
        silent += u64::from(!reported);
    }
    // Most faults are got over in the same session.
    let healed = ends.get(&End::Healed).copied().unwrap_or(0);
    assert!(healed * 2 > targets.len() as u64, "{ends:?}");
    // And each of the failures that last is met.
    assert!(ends.contains_key(&End::SegmentsNotDurable), "{ends:?}");
    assert!(ends.contains_key(&End::LogNotDurable), "{ends:?}");
    // A fault that its step does not report is one that the step got over
    // by itself: a record that is appended again, a file that is deleted
    // later.
    assert!(
        silent > 0 && silent * 4 < targets.len() as u64,
        "{silent} of {}",
        targets.len()
    );
}
