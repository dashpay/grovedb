//! The migrator from RocksDB (`lignin migrate`, LigninDB task OPS-02) on
//! the groves of the golden corpus.
//!
//! A node that is migrated goes on from where its RocksDB database stood,
//! so a migrated store has to answer every later operation as the RocksDB
//! database would have. Each test here takes the operation stream of a
//! corpus trace, applies its first part to a grove on RocksDB, closes that
//! grove and migrates it. Then:
//!
//! 1. **Every record reads the same.** The migrator's own comparison has
//!    passed, or it gives the target no name: every record of every
//!    keyspace of every prefix of the source, read through the engine. The
//!    test lists what both groves store through the seam once more
//!    (`diff::state::stored`) and holds the two listings equal.
//! 2. **The checks say the same.** `verify_grovedb` reports the same
//!    issues on both, and the migrator has verified the target with the
//!    physical invariant checker and Merk's hash rules.
//! 3. **The history goes on the same.** The rest of the stream is applied
//!    to the RocksDB grove and to the migrated one, and every step is
//!    compared as the pair runner compares it: the answer, the error, the
//!    cost, the root hash and the proof. At the end what both store is
//!    listed and compared again, and the migrated grove is closed, which
//!    in a debug build checks its store with Merk's rules.
//!
//! A cut is made where the stream has committed and holds no snapshot: a
//! migration is of a database at rest.

// The engines are compared through the `Engine` seam, which the engine
// the golden corpus is pinned to does not have (`build.rs`).
#![cfg(grovedb_engine_seam)]

use std::{
    collections::BTreeSet,
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};

use grovedb::{GroveDb, LigninEngine, RocksDbEngine};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use lignin_testkit::{
    diff::{
        pair::differences,
        state::{first_difference, stored},
    },
    golden::{
        plan,
        run::{Runner, STACK_BYTES},
    },
    workload::{Op, StreamEngine},
};
use lignin_tools::{
    migrate::{migrate, work_dir, Migration},
    Outcome,
};

/// One trace of the corpus: its grove version, its name and its
/// operations.
struct Trace {
    version: &'static GroveVersion,
    name: String,
    ops: Vec<Op>,
}

fn corpus() -> Vec<Trace> {
    let planned = GROVE_VERSIONS.iter().flat_map(plan::standard);
    let traces = planned.map(|planned| {
        let spec = planned.spec;
        let ops = (spec.segments.iter())
            .flat_map(|segment| segment.source.ops(spec.seed).collect::<Vec<_>>())
            .collect();
        Trace {
            version: spec.grove_version,
            name: spec.name,
            ops,
        }
    });
    traces.collect()
}

/// The places of a stream where it can be cut: after an operation that
/// commits. Whether a snapshot is open there is known when the stream is
/// run.
fn boundaries(ops: &[Op]) -> Vec<usize> {
    let commits = ops.iter().enumerate();
    let commits = commits.filter(|(_, op)| matches!(op, Op::Commit));
    commits.map(|(at, _)| at + 1).collect()
}

/// What a migration of `source` into `target` printed, if it gave the
/// target its name.
fn migrated(source: &Path, target: &Path, batch_bytes: usize) -> Result<String, String> {
    let migration = Migration {
        source: source.to_path_buf(),
        target: Some(target.to_path_buf()),
        options: lignindb::Options::default(),
        batch_bytes,
        leave_unreachable: true,
    };
    let mut out = Vec::new();
    let outcome = migrate(&migration, &mut out);
    let out = String::from_utf8_lossy(&out).into_owned();
    match outcome {
        Ok(Outcome::Done) => Ok(out),
        Ok(other) => Err(format!("ended as {other:?}:\n{out}")),
        Err(error) => Err(format!("failed: {error}\n{out}")),
    }
}

/// The value of the word `name=` on the line of `out` that begins with
/// `line`.
fn word(out: &str, line: &str, name: &str) -> u64 {
    let line = out.lines().find(|found| found.starts_with(line));
    let line = line.unwrap_or_else(|| panic!("no line {line:?} in:\n{out}"));
    let word = line.split_whitespace().find_map(|word| {
        let value = word.strip_prefix(name)?.strip_prefix('=')?;
        value.parse().ok()
    });
    word.unwrap_or_else(|| panic!("no word {name} in {line:?}"))
}

/// What both groves store, listed through the seam and compared.
fn same_stored<A: StreamEngine, B: StreamEngine>(
    rocks: &GroveDb<A>,
    lignin: &GroveDb<B>,
    version: &GroveVersion,
    when: &str,
) -> Result<u64, String> {
    let before = BTreeSet::new();
    let (was, is) = (rocks.start_transaction(), lignin.start_transaction());
    let reference = stored(rocks, &was, version, &before)?;
    let candidate = stored(lignin, &is, version, &before)?;
    match first_difference(&reference.records, &candidate.records) {
        None => Ok(reference.records.len() as u64),
        Some((reference, candidate)) => Err(format!(
            "{when}, the groves store otherwise:\n  RocksDB:  {reference:?}\n  LigninDB: \
             {candidate:?}"
        )),
    }
}

/// What a trace that was cut, migrated and gone on with came to.
#[derive(Debug, Default)]
struct Went {
    /// The steps after the cut that both engines answered alike.
    steps: u64,
    /// The records the migrator compared.
    compared: u64,
    /// The records that the listings of the two groves held, right after
    /// the migration and at the end.
    listed: u64,
    /// What the migrator counted of the source, by the words of its
    /// survey line.
    unlinked: u64,
    left_behind: u64,
    marks: u64,
    orphans: u64,
    root_keys_where_no_tree: u64,
    keyed_children: u64,
    stale_roots: u64,
    pending_drops: u64,
    nodes: u64,
    flat: u64,
}

/// Applies `ops[..cut]` to a grove on RocksDB, migrates it, and applies
/// the rest to both. `Ok(None)` where the stream has a snapshot open at
/// `cut`.
fn gone_on(
    trace: &Trace,
    cut: usize,
    scratch: &Path,
    batch: usize,
) -> Result<Option<Went>, String> {
    let dir = tempfile::Builder::new()
        .prefix("lignin-migrate-")
        .tempdir_in(scratch)
        .map_err(|error| error.to_string())?;
    let (rocks_dir, lignin_dir) = (dir.path().join("rocksdb"), dir.path().join("lignindb"));
    let version = trace.version;
    {
        let rocks = GroveDb::<RocksDbEngine>::open_with_engine(&rocks_dir)
            .map_err(|error| format!("opening RocksDB: {error}"))?;
        let mut runner = Runner::new(&rocks, version).map_err(|error| error.to_string())?;
        for (step, op) in trace.ops[..cut].iter().enumerate() {
            (runner.step(op)).map_err(|error| format!("step {step} on RocksDB: {error}"))?;
        }
        if runner.transactions().snapshot().is_some() {
            return Ok(None);
        }
    }

    let out = migrated(&rocks_dir, &lignin_dir, batch)?;
    assert!(
        !work_dir(&lignin_dir).unwrap().exists(),
        "the work directory is renamed"
    );
    let survey = |name| word(&out, "migrate: survey", name);
    let mut went = Went {
        compared: ["data", "aux", "meta", "root_keys", "marks"]
            .iter()
            .map(|name| word(&out, "migrate: compared", name))
            .sum(),
        unlinked: survey("unlinked"),
        left_behind: survey("left_behind"),
        marks: survey("marks"),
        orphans: survey("orphan_records"),
        root_keys_where_no_tree: survey("root_keys_where_no_tree"),
        keyed_children: survey("keyed_children"),
        stale_roots: survey("stale_roots"),
        pending_drops: survey("pending_drops"),
        nodes: survey("nodes"),
        flat: survey("flat"),
        ..Went::default()
    };
    assert_eq!(word(&out, "migrate: compared", "differing"), 0, "{out}");

    let rocks = GroveDb::<RocksDbEngine>::open_with_engine(&rocks_dir)
        .map_err(|error| format!("opening RocksDB again: {error}"))?;
    let lignin = GroveDb::<LigninEngine>::open_with_engine(&lignin_dir)
        .map_err(|error| format!("opening the migrated grove: {error}"))?;
    went.listed += same_stored(&rocks, &lignin, version, "after the migration")?;
    let checked = |issues: Result<_, grovedb::Error>| issues.map_err(|error| error.to_string());
    let was = checked(rocks.verify_grovedb(None, true, false, version));
    let is = checked(lignin.verify_grovedb(None, true, false, version));
    if was != is {
        return Err(format!(
            "verify_grovedb reports otherwise:\n  RocksDB:  {was:?}\n  LigninDB: {is:?}"
        ));
    }

    let mut reference = Runner::new(&rocks, version).map_err(|error| error.to_string())?;
    let mut candidate = Runner::new(&lignin, version).map_err(|error| error.to_string())?;
    for (step, op) in trace.ops.iter().enumerate().skip(cut) {
        let (was, is) = (reference.step(op), candidate.step(op));
        let (was, is) = match (was, is) {
            (Ok(was), Ok(is)) => (was, is),
            (was, is) => {
                let text = |seen: Result<_, lignin_testkit::golden::Error>| match seen {
                    Ok(_) => "answered".to_owned(),
                    Err(error) => error.to_string(),
                };
                let (was, is) = (text(was), text(is));
                // Engines that cannot be observed in the same way did the
                // same, and the stream ends there, as in the pair runner.
                match was == is {
                    true => break,
                    false => {
                        return Err(format!(
                            "step {step} ({op:?}) was not observed alike:\n  RocksDB:  {was}\n  \
                             LigninDB: {is}"
                        ));
                    }
                }
            }
        };
        let parts = differences(&was.step, &is.step);
        if !parts.is_empty() {
            let shown = |text: String| text.chars().take(1_500).collect::<String>();
            return Err(format!(
                "step {step} of {} differs in {parts:?}: {}\n  RocksDB:  {}\n  LigninDB: {}",
                trace.ops.len(),
                shown(format!("{op:?}")),
                shown(format!("{:?} {:?}", was.step, was.remarks)),
                shown(format!("{:?} {:?}", is.step, is.remarks)),
            ));
        }
        went.steps += 1;
    }
    drop((reference, candidate));
    went.listed += same_stored(&rocks, &lignin, version, "at the end of the stream")?;
    Ok(Some(went))
}

/// [`gone_on`] at the first place at or after `wanted` where the stream
/// can be cut, on a thread with the stack GroveDB's recursion takes. A
/// panic is what the cut came to.
fn cut_at(
    trace: &Trace,
    wanted: usize,
    scratch: &Path,
    batch: usize,
) -> Result<(usize, Went), String> {
    let places = boundaries(&trace.ops);
    let places = places.into_iter().filter(|place| *place >= wanted);
    for cut in places {
        let run = std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(STACK_BYTES)
                .spawn_scoped(scope, || gone_on(trace, cut, scratch, batch))
                .expect("a thread")
                .join()
        });
        match run {
            Ok(Ok(Some(went))) => return Ok((cut, went)),
            Ok(Ok(None)) => continue,
            Ok(Err(what)) => return Err(format!("cut at {cut}: {what}")),
            Err(panic) => {
                let message = (panic.downcast_ref::<String>().cloned())
                    .or_else(|| panic.downcast_ref::<&str>().map(ToString::to_string));
                return Err(format!(
                    "cut at {cut}: panicked: {}",
                    message.unwrap_or_default()
                ));
            }
        }
    }
    Err(format!(
        "the stream has no place to cut at or after {wanted}"
    ))
}

/// The places a trace is cut at: `spread` places over its length, and
/// right after the first operation of each of `kinds` (by the text of the
/// operation).
fn cuts(trace: &Trace, spread: usize, kinds: &[&str]) -> Vec<usize> {
    let len = trace.ops.len();
    let mut wanted: BTreeSet<usize> = (1..=spread).map(|part| len * part / (spread + 1)).collect();
    for kind in kinds {
        let found = trace
            .ops
            .iter()
            .position(|op| format!("{op:?}").contains(kind));
        wanted.extend(found.map(|at| at + 1));
    }
    wanted.into_iter().collect()
}

/// Runs every cut of `plan` on the traces of the corpus, in parallel, and
/// returns what each came to.
fn run(
    traces: &[Trace],
    plan: &[(usize, usize)],
    batch: usize,
) -> Vec<Result<(usize, Went), String>> {
    let scratch = tempfile::tempdir().expect("a directory");
    let next = AtomicUsize::new(0);
    let ended = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get().min(8));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let at = next.fetch_add(1, Ordering::Relaxed);
                let Some((trace, wanted)) = plan.get(at) else {
                    break;
                };
                let came_to = cut_at(&traces[*trace], *wanted, scratch.path(), batch);
                ended.lock().expect("no worker panics").push((at, came_to));
            });
        }
    });
    let mut ended = ended.into_inner().expect("no worker panics");
    ended.sort_by_key(|(at, _)| *at);
    ended.into_iter().map(|(_, came_to)| came_to).collect()
}

/// The operations whose first occurrence a trace is also cut after: a flat
/// drop (before its drain, which is a later operation), a conversion, a
/// move, and the writes of indexed trees.
const KINDS: [&str; 6] = [
    "DropFlat",
    "ConvertToBigSumTree",
    "Move",
    "IndexedDelete",
    "IndexedInsert",
    "ReconcileIndexed",
];

fn checked(traces: &[Trace], plan: &[(usize, usize)], batch: usize) {
    let ended = run(traces, plan, batch);
    let mut sum = Went::default();
    let (mut went_on, mut failed) = (0usize, Vec::new());
    for ((trace, wanted), came_to) in plan.iter().zip(ended) {
        let trace = &traces[*trace];
        match came_to {
            Ok((_, went)) => {
                went_on += 1;
                sum.steps += went.steps;
                sum.compared += went.compared;
                sum.listed += went.listed;
                sum.unlinked += went.unlinked;
                sum.left_behind += went.left_behind;
                sum.marks += went.marks;
                sum.orphans += went.orphans;
                sum.root_keys_where_no_tree += went.root_keys_where_no_tree;
                sum.keyed_children += went.keyed_children;
                sum.stale_roots += went.stale_roots;
                sum.pending_drops += went.pending_drops;
                sum.nodes += went.nodes;
                sum.flat += went.flat;
            }
            Err(what) => failed.push(format!(
                "v{} {} at {wanted}: {what}",
                trace.version.protocol_version, trace.name
            )),
        }
    }
    println!(
        "{} cuts of {} traces: {went_on} went on the same to the end, {} did not; {sum:?}",
        plan.len(),
        traces.len(),
        failed.len()
    );
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    // What the migrated sources held: a comparison of groves with nothing
    // in them would pass as well.
    assert!(
        sum.steps > 0 && sum.nodes > 0 && sum.compared > sum.nodes,
        "{sum:?}"
    );
}

/// Every trace of the corpus, cut once near its middle.
#[test]
fn the_corpus_goes_on_the_same_after_a_migration_at_its_middle() {
    let traces = corpus();
    assert!(!traces.is_empty());
    let plan: Vec<(usize, usize)> = (0..traces.len())
        .map(|trace| (trace, traces[trace].ops.len() / 2))
        .collect();
    checked(&traces, &plan, 64 << 20);
}

/// Every trace of the corpus, cut at three places over its length and
/// after the first flat drop, conversion, move and write of an indexed
/// tree it has, with commits of the load that take a megabyte each.
#[test]
#[ignore = "the long form: several migrations of every trace of the corpus"]
fn the_corpus_goes_on_the_same_after_a_migration_at_every_kind_of_place() {
    let traces = corpus();
    let mut plan = Vec::new();
    for (at, trace) in traces.iter().enumerate() {
        plan.extend(
            cuts(trace, 3, &KINDS)
                .into_iter()
                .map(|wanted| (at, wanted)),
        );
    }
    checked(&traces, &plan, 1 << 20);
}
