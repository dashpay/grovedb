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
//!    to the RocksDB grove and to the migrated one, and each step is
//!    compared as the pair runner compares it: the answer, the error, the
//!    cost, the root hash and the proof. A stream ends where the pair
//!    runner ends one: at its last step, or at a step at which neither
//!    engine can be observed. No stream of the quick form ends at such a
//!    step, which the quick form asserts, and the long form says how many
//!    of its cuts did. At the end what both store is listed and compared
//!    again, and the migrated grove is closed, which in a debug build
//!    checks its store with Merk's rules.
//!
//! A cut is made where the stream has committed and holds no snapshot: a
//! migration is of a database at rest.

// The engines are compared through the `Engine` seam, which the engine
// the golden corpus is pinned to does not have (`build.rs`).
#![cfg(grovedb_engine_seam)]

use std::{
    collections::BTreeSet,
    ffi::OsString,
    io::{self, BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime},
};

use grovedb::{
    batch::QualifiedGroveDbOp, BackwardsReferences, Element, GroveDb, LigninEngine, RocksDbEngine,
};
use grovedb_merk::{
    cost_model::Keyspace as MerkKeyspace,
    store::lignin::{keeps_flat_records, left_behind_key, tree_type_bytes},
    TreeType,
};
use grovedb_storage::{
    flat::pending_prefix_drops_namespace, prefix::build_prefix, Storage, StorageContext,
};
use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};
use lignin_io::{
    sim::{CrashPoint, Fault, FaultRule, OpKind, SimConfig, SimVfs},
    Vfs,
};
use lignin_store::wal::SyncPolicy;
use lignin_testkit::{
    diff::{
        pair::differences,
        state::{first_difference, stored, Keyspace as Listed},
    },
    golden::{
        plan,
        run::{Runner, STACK_BYTES},
    },
    workload::{Op, StreamEngine},
};
use lignin_tools::{
    migrate::{
        migrate, work_dir, Migration, Unreachable, MARK_TEXT, WORK_DATABASE, WORK_MARK,
        WORK_MARK_NEW,
    },
    Outcome,
};
use lignindb::{
    commit::NoStaleHashes,
    txn::{Batch, Column, RecordWrite, Subtree, SubtreeWrite},
    Db, Options,
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
        options: Options::default(),
        batch_bytes,
        progress: Duration::from_secs(5),
        unreachable: Unreachable::Leave,
        memory_limit: None,
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

/// What both groves store, listed through the seam and compared, with
/// the answers of the trees that keep flat records ([`flat_reads`]): how
/// many records both store.
fn same_stored<A: StreamEngine, B: StreamEngine>(
    rocks: &GroveDb<A>,
    lignin: &GroveDb<B>,
    version: &GroveVersion,
    when: &str,
) -> Result<u64, String> {
    same_stored_and_read(rocks, lignin, version, when).map(|(records, _)| records)
}

/// [`same_stored`], with how many trees that keep flat records both
/// groves have and how many answers of their calls were alike. A tree
/// that is not asked on an engine is an error, and is counted as nothing.
fn same_stored_and_read<A: StreamEngine, B: StreamEngine>(
    rocks: &GroveDb<A>,
    lignin: &GroveDb<B>,
    version: &GroveVersion,
    when: &str,
) -> Result<(u64, (u64, u64)), String> {
    let before = BTreeSet::new();
    let (reference, candidate) = {
        let (was, is) = (rocks.start_transaction(), lignin.start_transaction());
        (
            stored(rocks, &was, version, &before)?,
            stored(lignin, &is, version, &before)?,
        )
    };
    if let Some((reference, candidate)) = first_difference(&reference.records, &candidate.records) {
        return Err(format!(
            "{when}, the groves store otherwise:\n  RocksDB:  {reference:?}\n  LigninDB: \
             {candidate:?}"
        ));
    }
    // Each tree that keeps flat records is asked through its own calls, on
    // both engines.
    let flat = (reference.places.iter()).filter(|place| place.data == Listed::Flat);
    let (mut trees, mut answers) = (0, 0);
    for place in flat {
        let named = || {
            let path: Vec<String> = place.path.iter().map(hex::encode).collect();
            path.join("/")
        };
        let asked = |reads: Result<Vec<String>, String>, engine: &str| {
            reads.map_err(|error| {
                format!(
                    "{when}, the tree of flat records at /{} is not asked on {engine}: {error}",
                    named()
                )
            })
        };
        let was = asked(flat_reads(rocks, &place.path, version), "RocksDB")?;
        let is = asked(flat_reads(lignin, &place.path, version), "LigninDB")?;
        if let Some(at) = (0..was.len().max(is.len())).find(|at| was.get(*at) != is.get(*at)) {
            return Err(format!(
                "{when}, the tree of flat records at /{} answers otherwise:\n  RocksDB:  \
                 {:?}\n  LigninDB: {:?}",
                named(),
                was.get(at),
                is.get(at)
            ));
        }
        trees += 1;
        answers += was.len() as u64;
    }
    Ok((reference.records.len() as u64, (trees, answers)))
}

/// What the calls of a tree that keeps flat records answer for the tree
/// at `path`: its state root, how many leaves it holds, its root hash or
/// its anchor where it has a call for one, and its first and its last
/// leaves, each as the call gave it. It is an error where the element at
/// `path` is not read or is no tree that keeps flat records: no call of
/// such a tree was made then.
fn flat_reads<E: grovedb::Engine>(
    db: &GroveDb<E>,
    path: &[Vec<u8>],
    v: &GroveVersion,
) -> Result<Vec<String>, String> {
    /// How many leaves are read at each end of a tree.
    const EDGE: u64 = 48;
    let (key, above) = path
        .split_last()
        .ok_or_else(|| "the root tree keeps no flat records".to_owned())?;
    let above: Vec<&[u8]> = above.iter().map(Vec::as_slice).collect();
    let above = above.as_slice();
    let mut reads = Vec::new();
    let mut read = |what: String, answer: String| reads.push(format!("{what}: {answer}"));
    // The answer of a call, or its error, in words.
    macro_rules! answer {
        ($call:expr) => {
            $call.unwrap().map_err(|error| error.to_string())
        };
    }
    // The first leaves and the last ones.
    let ends = |count: u64| (0..count.min(EDGE)).chain(count.saturating_sub(EDGE).max(EDGE)..count);
    let element = answer!(db.get_raw(above.into(), key, None, v))
        .map_err(|error| format!("the element of the tree is not read: {error}"))?;
    // The element as it is, also where it is wrapped as one that is not
    // counted or not summed.
    let kind = element.underlying();
    if !kind.uses_non_merk_data_storage() {
        return Err(format!(
            "the element is no tree that keeps flat records: {element:?}"
        ));
    }
    // The state root of a tree of any of the kinds, as `verify_grovedb`
    // makes it.
    let root = db.subtree_root_as(above, key, &element, None, v);
    let root = root.map_err(|error| error.to_string());
    read(
        "state root".to_owned(),
        format!("{:?}", root.map(hex::encode)),
    );
    match kind {
        Element::CommitmentTree(..) => {
            let count = answer!(db.commitment_tree_count(above, key, None, v));
            read("count".to_owned(), format!("{count:?}"));
            let anchor = answer!(db.commitment_tree_anchor(above, key, None, v));
            let anchor = anchor.map(|anchor| hex::encode(anchor.to_bytes()));
            read("anchor".to_owned(), format!("{anchor:?}"));
            let count = count.unwrap_or(0);
            for from in [0, count.saturating_sub(EDGE)] {
                let page = db.commitment_tree_get_range(above, key, from, EDGE as u16, None, v);
                read(
                    format!("leaves from {from}"),
                    format!("{:?}", answer!(page)),
                );
            }
            for at in [0, count / 2, count.saturating_sub(1)] {
                let leaf = db.commitment_tree_get_value(above, key, at, None, v);
                read(format!("leaf {at}"), format!("{:?}", answer!(leaf)));
            }
        }
        Element::MmrTree(..) => {
            let count = answer!(db.mmr_tree_leaf_count(above, key, None, v));
            read("count".to_owned(), format!("{count:?}"));
            let root = answer!(db.mmr_tree_root_hash(above, key, None, v));
            read("root".to_owned(), format!("{:?}", root.map(hex::encode)));
            for at in ends(count.unwrap_or(0)) {
                let leaf = db.mmr_tree_get_value(above, key, at, None, v);
                read(format!("leaf {at}"), format!("{:?}", answer!(leaf)));
            }
        }
        Element::BulkAppendTree(..) => {
            let count = answer!(db.bulk_count(above, key, None, v));
            read("count".to_owned(), format!("{count:?}"));
            let chunks = answer!(db.bulk_chunk_count(above, key, None, v));
            read("chunks".to_owned(), format!("{chunks:?}"));
            for from in [0, count.unwrap_or(0).saturating_sub(EDGE)] {
                let page = db.bulk_get_range(above, key, from, EDGE as u16, None, v);
                read(
                    format!("leaves from {from}"),
                    format!("{:?}", answer!(page)),
                );
            }
        }
        Element::DenseAppendOnlyFixedSizeTree(..) => {
            let count = answer!(db.dense_tree_count(above, key, None, v));
            read("count".to_owned(), format!("{count:?}"));
            let root = answer!(db.dense_tree_root_hash(above, key, None, v));
            read("root".to_owned(), format!("{:?}", root.map(hex::encode)));
            for at in ends(u64::from(count.unwrap_or(0))) {
                let leaf = db.dense_tree_get(above, key, at as u16, None, v);
                read(format!("leaf {at}"), format!("{:?}", answer!(leaf)));
            }
        }
        Element::PrivateDocumentStore(..) => {
            let count = answer!(db.private_document_store_count(above, key, None, v));
            read("count".to_owned(), format!("{count:?}"));
            for from in [0, count.unwrap_or(0).saturating_sub(EDGE)] {
                let page =
                    db.private_document_store_get_range(above, key, from, EDGE as u16, None, v);
                read(
                    format!("leaves from {from}"),
                    format!("{:?}", answer!(page)),
                );
            }
        }
        other => {
            return Err(format!(
                "no call is known here for the tree of flat records {other:?}"
            ));
        }
    }
    Ok(reads)
}

/// What a trace that was cut, migrated and gone on with came to.
#[derive(Debug, Default)]
struct Went {
    /// The steps after the cut that both engines answered alike.
    steps: u64,
    /// The operations after a step at which neither engine could be
    /// observed, both for the same reason: the stream ends there, as in
    /// the pair runner, and they were not applied.
    unobserved: u64,
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
    /// The value hashes that the physical checker names in the target as
    /// left by a delete: as many as `verify_grovedb` reports on the
    /// source.
    left_hashes: u64,
    /// The trees with no node that `verify_grovedb` reports on the
    /// source for the value hash of their element.
    emptied: u64,
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
        "the work directory is removed"
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
    // `verify_grovedb` reports each tree whose element does not hold the
    // value hash its Merk gives. Where that Merk holds a node, the
    // physical checker names the hash in the target as one a delete left
    // (GroveDB #686). Where the Merk is empty it follows no root from the
    // element, and names nothing.
    went.left_hashes = word(&out, "migrate: verify", "left_hashes");
    let issues = was.as_ref().map_err(String::clone)?;
    went.emptied = (issues.values())
        .filter(|(root, ..)| *root == [0; 32])
        .count() as u64;
    if issues.len() as u64 != went.left_hashes + went.emptied {
        return Err(format!(
            "verify_grovedb reports {} issues on the source, {} of them of a tree with no \
             node, and the checker names {} left hashes in the target:\n{was:?}",
            issues.len(),
            went.emptied,
            went.left_hashes
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
                // same, and the stream ends there, as in the pair runner:
                // that step and what follows it are counted as not
                // compared.
                match was == is {
                    true => {
                        went.unobserved = (trace.ops.len() - step) as u64;
                        break;
                    }
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
    // Every operation after the cut was compared, or follows one that
    // neither engine could be observed at.
    assert_eq!(
        went.steps + went.unobserved,
        (trace.ops.len() - cut) as u64,
        "{} cut at {cut}",
        trace.name
    );
    Ok(Some(went))
}

/// [`gone_on`] at the first place at or after `wanted` where the stream
/// can be cut, or, where it has none there, at the last place before it,
/// on a thread with the stack GroveDB's recursion takes. A panic is what
/// the cut came to.
fn cut_at(
    trace: &Trace,
    wanted: usize,
    scratch: &Path,
    batch: usize,
) -> Result<(usize, Went), String> {
    let places = boundaries(&trace.ops);
    let after = places.iter().filter(|place| **place >= wanted);
    let before = places.iter().rev().filter(|place| **place < wanted);
    for cut in after.chain(before).copied() {
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
    Err("the stream has no place to cut at".to_owned())
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

fn checked(traces: &[Trace], plan: &[(usize, usize)], batch: usize) -> usize {
    let ended = run(traces, plan, batch);
    let mut sum = Went::default();
    let (mut went_on, mut failed) = (0usize, Vec::new());
    // The cuts whose stream ended at a step neither engine could be
    // observed at, and those after which no step was compared at all.
    let (mut ended_early, mut no_step) = (0usize, Vec::new());
    for ((trace, wanted), came_to) in plan.iter().zip(ended) {
        let trace = &traces[*trace];
        match came_to {
            Ok((cut, went)) => {
                went_on += 1;
                ended_early += usize::from(went.unobserved > 0);
                if went.steps == 0 {
                    let version = trace.version.protocol_version;
                    no_step.push(format!("v{version} {} at {cut}", trace.name));
                }
                sum.unobserved += went.unobserved;
                sum.steps += went.steps;
                sum.compared += went.compared;
                sum.listed += went.listed;
                sum.unlinked += went.unlinked;
                sum.left_behind += went.left_behind;
                sum.marks += went.marks;
                sum.left_hashes += went.left_hashes;
                sum.emptied += went.emptied;
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
        "{} cuts of {} traces: {went_on} went on the same, {} did not; {ended_early} of them \
         ended at a step that neither engine could be observed at, with {} operations from \
         such a step on that were not compared; {} compared no step after the cut; {sum:?}",
        plan.len(),
        traces.len(),
        failed.len(),
        sum.unobserved,
        no_step.len()
    );
    assert!(failed.is_empty(), "{}", failed.join("\n"));
    // A cut after which nothing was compared shows nothing of how the
    // history goes on.
    assert!(no_step.is_empty(), "{}", no_step.join("\n"));
    // What the migrated sources held: a comparison of groves with nothing
    // in them would pass as well.
    assert!(
        sum.steps > 0 && sum.nodes > 0 && sum.compared > sum.nodes,
        "{sum:?}"
    );
    ended_early
}

/// Every trace of the corpus, cut once near its middle. Each stream is
/// compared to its last step.
#[test]
fn the_corpus_goes_on_the_same_after_a_migration_at_its_middle() {
    let traces = corpus();
    assert!(!traces.is_empty());
    let plan: Vec<(usize, usize)> = (0..traces.len())
        .map(|trace| (trace, traces[trace].ops.len() / 2))
        .collect();
    let ended_early = checked(&traces, &plan, 64 << 20);
    assert_eq!(
        ended_early, 0,
        "a stream ended at a step that neither engine could be observed at"
    );
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
    // How many of the cuts ended at a step that neither engine could be
    // observed at is in the line that `checked` prints.
    checked(&traces, &plan, 1 << 20);
}

// ---------------------------------------------------------------------
// The tool itself: that the source is never written, what it refuses,
// what a stop leaves, and the shapes it carries as they are.

type Rocks = GroveDb<RocksDbEngine>;
type Lignin = GroveDb<LigninEngine>;

const ROOT: &[&[u8]] = &[];

/// The path of the tree that [`with_orphans`] deletes with what it holds.
const GONE: &[&[u8]] = &[b"c", b"gone"];

fn rocks(dir: &Path) -> Rocks {
    Rocks::open_with_engine(dir).expect("a grove on RocksDB")
}

fn root_of<E: grovedb::Engine>(db: &GroveDb<E>) -> [u8; 32] {
    let root = db.root_hash(None, GroveVersion::latest());
    root.unwrap().expect("a root hash")
}

fn put(db: &Rocks, path: &[&[u8]], key: &[u8], element: Element) {
    let inserted = db.insert(path, key, element, None, None, GroveVersion::latest());
    inserted.unwrap().expect("an element");
}

/// Fills a grove: trees three deep, of items and of sums, aux records, a
/// tree with one item, a tree of flat records, and a tree that is dropped
/// flat and not drained.
fn fill(db: &Rocks) {
    let v = GroveVersion::latest();
    for name in [b"a", b"c", b"d", b"e"] {
        put(db, ROOT, name, Element::empty_tree());
    }
    put(db, ROOT, b"m", Element::empty_mmr_tree());
    for n in 0..5u8 {
        let appended = db.mmr_tree_append(ROOT, b"m", vec![n; 60], None, v);
        appended.unwrap().expect("an append");
    }
    put(db, ROOT, b"b", Element::empty_sum_tree());
    for n in 0..40u8 {
        put(db, &[b"a"], &[b'k', n], Element::new_item(vec![n; 50]));
    }
    for n in 0..12u8 {
        put(db, &[b"b"], &[b's', n], Element::new_sum_item(i64::from(n)));
    }
    put(db, &[b"c"], b"in", Element::empty_tree());
    put(db, &[b"c", b"in"], b"deep", Element::empty_tree());
    for n in 0..9u8 {
        let item = Element::new_item(vec![n; 300]);
        put(db, &[b"c", b"in", b"deep"], &[n], item);
    }
    for n in 0..6u8 {
        put(db, &[b"d"], &[n], Element::new_item(vec![n; 20]));
    }
    put(
        db,
        &[b"e"],
        b"only",
        Element::new_item(b"one item".to_vec()),
    );
    for n in 0..5u8 {
        let aux = db.put_aux([b'x', n], &[n; 30], None, None);
        aux.unwrap().expect("an aux record");
    }
    let unchecked = BackwardsReferences::DontCheck;
    let transaction = db.start_transaction();
    let dropped = db.drop_flat_subtree(ROOT, b"d", unchecked, Some(&transaction), v);
    dropped.unwrap().expect("a flat drop");
    let committed = db.commit_transaction(transaction);
    committed.unwrap().expect("the commit of the flat drop");
}

/// The nodes of the Merks of a grove of [`fill`]: five trees at the root,
/// the dropped one is none of them, and what the others hold.
const NODES: u64 = 5 + 40 + 12 + 1 + 1 + 9 + 1;

/// A grove of [`fill`] on RocksDB in `dir`, closed. Returns its root hash.
fn small(dir: &Path) -> [u8; 32] {
    let db = rocks(dir);
    fill(&db);
    root_of(&db)
}

/// Writes one record beside GroveDB into the RocksDB grove in `dir`,
/// under `prefix`: into the data (`d`), the aux (`a`), the roots (`r`) or
/// the meta (`m`) keyspace, or removes the data record with no `value`.
fn raw(dir: &Path, prefix: [u8; 32], keyspace: char, key: &[u8], value: Option<&[u8]>) {
    let db = rocks(dir);
    let storage = db.engine().storage();
    let transaction = storage.start_transaction();
    {
        let context = storage
            .get_immediate_storage_context_by_subtree_prefix(prefix, &transaction)
            .value;
        let written = match (keyspace, value) {
            ('d', Some(value)) => context.put(key, value, None, None),
            ('d', None) => context.delete(key, None),
            ('a', Some(value)) => context.put_aux(key, value, None),
            ('r', Some(value)) => context.put_root(key, value, None),
            ('m', Some(value)) => context.put_meta(key, value, None),
            other => panic!("no raw write {other:?}"),
        };
        written.unwrap().expect("a raw write");
    }
    let committed = storage.commit_transaction(transaction);
    committed.unwrap().expect("a raw commit");
}

/// The data record under `key` of `prefix` in the RocksDB grove in `dir`.
fn raw_record(dir: &Path, prefix: [u8; 32], key: &[u8]) -> Vec<u8> {
    let db = rocks(dir);
    let storage = db.engine().storage();
    let transaction = storage.start_transaction();
    let context = storage
        .get_immediate_storage_context_by_subtree_prefix(prefix, &transaction)
        .value;
    let record = context.get(key).unwrap().expect("a raw read");
    record.expect("the record is there")
}

fn prefix_of(path: &[&[u8]]) -> [u8; 32] {
    build_prefix(path.into()).value
}

/// A grove of [`fill`] in `dir` with records that nothing leads to, in
/// the shape a batch leaves them: the tree at [`GONE`] is deleted by a
/// batch while it holds one element, an empty sum tree, and RocksDB keeps
/// that node. A record of the tree's root key is written beside it, and
/// an aux record under a prefix of no path. Returns the root hash.
fn with_orphans(dir: &Path) -> [u8; 32] {
    let v = GroveVersion::latest();
    let root = {
        let db = rocks(dir);
        fill(&db);
        put(&db, &[b"c"], b"gone", Element::empty_tree());
        put(&db, GONE, &[1], Element::empty_sum_tree());
        let delete = QualifiedGroveDbOp::delete_op(vec![b"c".to_vec()], b"gone".to_vec());
        let deleted = db.apply_batch(vec![delete], None, None, v);
        deleted.unwrap().expect("the batch that deletes the tree");
        root_of(&db)
    };
    // The record of the root key of the deleted tree, as the grove
    // versions that keep one for every tree leave it.
    raw(dir, prefix_of(GONE), 'r', b"r", Some(&[1]));
    raw(dir, [0xAB; 32], 'a', b"an aux record", Some(b"of no tree"));
    root
}

/// A migration of `source` into `target` that commits wherever a load can
/// and says a line at each commit.
fn migration(source: &Path, target: &Path) -> Migration {
    Migration {
        source: source.to_path_buf(),
        target: Some(target.to_path_buf()),
        options: Options::default(),
        batch_bytes: 1,
        progress: Duration::ZERO,
        unreachable: Unreachable::Refuse,
        memory_limit: None,
    }
}

/// Runs `migration`: how it ended, and what it wrote.
fn ran(migration: &Migration) -> (Result<Outcome, String>, String) {
    let mut out = Vec::new();
    let outcome = migrate(migration, &mut out).map_err(|error| error.to_string());
    (outcome, String::from_utf8_lossy(&out).into_owned())
}

/// Every file of `dir` with its length, its modification time and its
/// bytes.
fn files(dir: &Path) -> Vec<(OsString, u64, SystemTime, Vec<u8>)> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).expect("a directory") {
        let entry = entry.expect("an entry");
        let metadata = entry.metadata().expect("its metadata");
        let bytes = std::fs::read(entry.path()).expect("its bytes");
        let modified = metadata.modified().expect("its time");
        files.push((entry.file_name(), metadata.len(), modified, bytes));
    }
    files.sort();
    files
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir(to).expect("a directory");
    for entry in std::fs::read_dir(from).expect("a directory") {
        let entry = entry.expect("an entry");
        std::fs::copy(entry.path(), to.join(entry.file_name())).expect("a copy");
    }
}

/// The groves in `source` and in `target` store the same and are checked
/// alike by GroveDB: how many records both hold.
fn alike(source: &Path, target: &Path) -> u64 {
    let v = GroveVersion::latest();
    let (rocks, lignin) = (
        rocks(source),
        Lignin::open_with_engine(target).expect("the target"),
    );
    let (records, _) =
        same_stored_and_read(&rocks, &lignin, v, "after the migration").expect("the same");
    let checked = |issues: Result<_, grovedb::Error>| issues.map_err(|error| error.to_string());
    let was = checked(rocks.verify_grovedb(None, true, false, v));
    let is = checked(lignin.verify_grovedb(None, true, false, v));
    assert_eq!(was, is, "verify_grovedb");
    let roots = (
        rocks.root_hash(None, v).unwrap(),
        lignin.root_hash(None, v).unwrap(),
    );
    assert_eq!(roots.0.expect("a root hash"), roots.1.expect("a root hash"));
    records
}

/// The source is read and never written: every file of its directory has
/// the bytes, the length and the modification time it had, also where the
/// node left its commits in the write-ahead log, which the migration then
/// carries.
#[test]
fn the_source_is_never_written_and_its_log_is_carried() {
    let dir = tempfile::tempdir().expect("a directory");
    let (source, target) = (dir.path().join("rocksdb"), dir.path().join("lignindb"));
    let root = small(&source);
    let before = files(&source);
    // The grove was closed with no flush: its records are in the log.
    let ends_with = |name: &OsString, end: &str| name.to_string_lossy().ends_with(end);
    let logged = before
        .iter()
        .any(|(name, len, ..)| ends_with(name, ".log") && *len > 0);
    assert!(
        logged,
        "the source has a write-ahead log that holds records"
    );
    let tables = before.iter().filter(|(name, ..)| ends_with(name, ".sst"));
    assert_eq!(
        tables.count(),
        0,
        "nothing of the source was flushed to a table"
    );

    let (outcome, out) = ran(&migration(&source, &target));
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    assert!(
        files(&source) == before,
        "the source's directory is as it was"
    );
    assert!(out.contains("no process has it open"), "{out}");
    assert_eq!(word(&out, "migrate: survey", "nodes"), NODES);
    assert_eq!(word(&out, "migrate: loaded", "nodes"), NODES);
    assert_eq!(word(&out, "migrate: survey", "pending_drops"), 1);
    let flat = word(&out, "migrate: survey", "flat");
    assert!(flat >= 5, "{out}");
    assert_eq!(word(&out, "migrate: loaded", "flat"), flat);
    // The rate of a run that made the whole target, by its own count.
    assert_eq!(word(&out, "migrate: done", "elements"), NODES + flat);
    assert!(
        word(&out, "migrate: done", "elements_per_second") > 0,
        "{out}"
    );
    assert_eq!(word(&out, "migrate: survey", "aux"), 5);
    assert!(
        out.contains(&format!("root_hash={}", hex::encode(root))),
        "{out}"
    );
    assert_eq!(
        root_of(&Lignin::open_with_engine(&target).expect("the target")),
        root
    );
    assert!(alike(&source, &target) > 60);

    // With `--survey` nothing is made, and the source is as it was.
    let survey = Migration {
        target: None,
        ..migration(&source, &target)
    };
    let (outcome, out) = ran(&survey);
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    assert!(out.contains("the source can be migrated"), "{out}");
}

/// The name of the variable that tells [`a_process_that_holds_a_grove`]
/// which directory to hold, and what it prints once it does.
const HELD: &str = "LIGNIN_MIGRATE_TEST_HOLDS";
const HOLDS: &str = "the grove is held open";

/// No test of its own: the process that the test after this one starts,
/// which fills a grove and holds it open until it is killed.
#[test]
fn a_process_that_holds_a_grove() {
    let Some(dir) = std::env::var_os(HELD) else {
        return;
    };
    let db = rocks(Path::new(&dir));
    fill(&db);
    println!("{HOLDS}");
    // Until the test that started this process ends, or kills it.
    let mut end = [0];
    let _ = std::io::stdin().read(&mut end);
    drop(db);
}

/// A process that is ended when the test leaves it.
struct Held(Child);

impl Held {
    fn kill(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.kill();
    }
}

/// A source that a process has open is refused, and nothing is made. Once
/// that process is killed, with nothing closed or flushed, the migration
/// is of the state a node recovers the directory to.
#[test]
fn a_held_source_is_refused_and_the_source_of_a_killed_process_is_migrated() {
    let dir = tempfile::tempdir().expect("a directory");
    let (source, target) = (dir.path().join("rocksdb"), dir.path().join("lignindb"));
    let mut child = Command::new(std::env::current_exe().expect("this binary"));
    child.args(["--exact", "a_process_that_holds_a_grove", "--nocapture"]);
    child.env(HELD, &source);
    child.stdin(Stdio::piped()).stdout(Stdio::piped());
    let mut child = child.spawn().expect("a process");
    let said = BufReader::new(child.stdout.take().expect("its output"));
    let mut holder = Held(child);
    let mut lines = said.lines();
    let holds = lines.any(|line| line.expect("a line").contains(HOLDS));
    assert!(holds, "the process holds the grove");

    let (outcome, out) = ran(&migration(&source, &target));
    let refused = outcome.expect_err("a held source is refused");
    assert!(
        refused.contains("a process has it open"),
        "{refused}\n{out}"
    );
    assert!(!target.exists() && !work_dir(&target).unwrap().exists());

    holder.kill();
    let before = files(&source);
    let (outcome, out) = ran(&migration(&source, &target));
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    assert!(
        files(&source) == before,
        "the source's directory is as it was"
    );
    // What a node recovers of the directory: a copy of it, opened. It is
    // the grove that the process filled.
    let (copy, same) = (dir.path().join("recovered"), dir.path().join("same"));
    copy_dir(&source, &copy);
    let recovered = root_of(&rocks(&copy));
    assert_eq!(recovered, small(&same));
    assert_eq!(
        root_of(&Lignin::open_with_engine(&target).expect("the target")),
        recovered
    );
    assert!(alike(&copy, &target) > 60);
}

/// A checkpoint is a source: the migration is of the grove as the
/// checkpoint holds it, whatever was written since.
#[test]
fn a_checkpoint_is_a_source() {
    let dir = tempfile::tempdir().expect("a directory");
    let live = dir.path().join("rocksdb");
    let (checkpoint, target) = (dir.path().join("checkpoint"), dir.path().join("lignindb"));
    let root = {
        let db = rocks(&live);
        fill(&db);
        let root = root_of(&db);
        db.create_checkpoint(&checkpoint).expect("a checkpoint");
        put(&db, ROOT, b"later", Element::new_item(b"since".to_vec()));
        assert_ne!(root_of(&db), root);
        root
    };
    let before = files(&checkpoint);
    let (outcome, out) = ran(&migration(&checkpoint, &target));
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    assert!(files(&checkpoint) == before, "the checkpoint is as it was");
    assert_eq!(
        root_of(&Lignin::open_with_engine(&target).expect("the target")),
        root
    );
}

/// Nothing is made at a target that holds something, and an empty
/// directory is taken. A target that a migration left is refused by the
/// next run, which says that it is whole.
#[test]
fn a_target_that_holds_something_is_refused_and_an_empty_directory_is_taken() {
    let dir = tempfile::tempdir().expect("a directory");
    let (source, target) = (dir.path().join("rocksdb"), dir.path().join("lignindb"));
    let root = small(&source);
    std::fs::create_dir(&target).expect("a directory");
    std::fs::write(target.join("a file"), b"of someone").expect("a file");
    let (outcome, out) = ran(&migration(&source, &target));
    let refused = outcome.expect_err("a target that holds a file is refused");
    assert!(
        refused.contains("is there and is not empty"),
        "{refused}\n{out}"
    );
    assert!(!work_dir(&target).unwrap().exists());
    assert_eq!(std::fs::read_dir(&target).expect("the target").count(), 1);

    std::fs::remove_file(target.join("a file")).expect("the file is removed");
    let (outcome, out) = ran(&migration(&source, &target));
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    assert_eq!(
        root_of(&Lignin::open_with_engine(&target).expect("the target")),
        root
    );

    let (outcome, out) = ran(&migration(&source, &target));
    let refused = outcome.expect_err("a target that is there is refused");
    assert!(
        refused.contains("a database that a migration left at the target is whole"),
        "{out}"
    );
    assert_eq!(
        root_of(&Lignin::open_with_engine(&target).expect("the target")),
        root
    );

    let (outcome, _) = ran(&migration(&source, Path::new("/")));
    let refused = outcome.expect_err("a target with no name is refused");
    assert!(refused.contains("has no name of its own"), "{refused}");

    // A name under which no database is opened.
    let creating = dir.path().join("Backup.CREATING");
    let (outcome, _) = ran(&migration(&source, &creating));
    let refused = outcome.expect_err("a target that ends in .creating is refused");
    assert!(refused.contains("ends in .creating"), "{refused}");
    assert!(!creating.exists() && !work_dir(&creating).unwrap().exists());

    // A target inside the source, also by another name of the source:
    // nothing is made there.
    let before = files(&source);
    let link = dir.path().join("a link to the source");
    std::os::unix::fs::symlink(&source, &link).expect("a link");
    for inside in [source.join("out"), link.join("out"), source.clone()] {
        let (outcome, _) = ran(&migration(&source, &inside));
        let refused = outcome.expect_err("a target inside the source is refused");
        assert!(refused.contains("one inside the other"), "{refused}");
    }
    assert!(
        files(&source) == before,
        "the source's directory is as it was"
    );

    // A target that is a file, and one that is a link to a directory
    // with nothing in it: neither is a directory a database is put at.
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("a directory");
    let (file, link) = (dir.path().join("a file"), dir.path().join("a link"));
    std::fs::write(&file, b"of someone").expect("a file");
    std::os::unix::fs::symlink(&elsewhere, &link).expect("a link");
    for taken in [&file, &link] {
        let before = held(taken);
        let (outcome, out) = ran(&migration(&source, taken));
        let refused = outcome.expect_err("a target that is no directory is refused");
        assert!(
            refused.contains("is there and is no directory"),
            "{refused}\n{out}"
        );
        assert_eq!(held(taken), before);
        assert!(!work_dir(taken).unwrap().exists());
    }
    assert_eq!(held(&elsewhere), Vec::<String>::new());

    // A target beside the source that is named through the source's
    // directory is beside it, and is taken.
    let beside = source.join("..").join("beside the source");
    let (outcome, out) = ran(&migration(&source, &beside));
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    assert!(dir.path().join("beside the source").is_dir());

    // A file at the target with the command's work directory beside it:
    // no run gave a file the name of a database, and the work directory
    // is left as it is.
    let work = work_dir(&file).unwrap();
    std::fs::create_dir(&work).expect("a directory");
    std::fs::write(work.join(WORK_MARK), MARK_TEXT).expect("its file");
    let before = held(&work);
    let (outcome, _) = ran(&migration(&source, &file));
    let refused = outcome.expect_err("a target that is a file is refused");
    assert!(
        refused.contains("is there and is no directory"),
        "{refused}"
    );
    assert_eq!(held(&work), before);
    std::fs::remove_dir_all(&work).expect("the directory is removed");

    // The directory a target is to be in is not made.
    let below = dir.path().join("no such directory");
    let (outcome, out) = ran(&migration(&source, &below.join("lignindb")));
    let refused = outcome.expect_err("a target in no directory is refused");
    assert!(
        refused.contains("is not there: make it first"),
        "{refused}\n{out}"
    );
    assert!(!below.exists());

    // A grove with nothing in it is a source as any other.
    let (empty, of_empty) = (dir.path().join("empty"), dir.path().join("of-empty"));
    let nothing = root_of(&rocks(&empty));
    assert_eq!(nothing, [0; 32]);
    let (outcome, out) = ran(&migration(&empty, &of_empty));
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    assert_eq!(
        root_of(&Lignin::open_with_engine(&of_empty).expect("the target")),
        nothing
    );
    let (outcome, _) = ran(&migration(&dir.path().join("nothing"), &target));
    assert!(outcome.is_err(), "a source that is not there is refused");
}

/// What is at `path`, without following a link: a link with where it
/// leads, a file with its bytes, or a directory with what its entries
/// are.
fn held(path: &Path) -> Vec<String> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return vec!["nothing".to_owned()];
    };
    if metadata.is_symlink() {
        let leads = std::fs::read_link(path).expect("where the link leads");
        return vec![format!("a link to {leads:?}")];
    }
    if metadata.is_file() {
        let bytes = std::fs::read(path).expect("the bytes of the file");
        return vec![format!("a file of {bytes:?}")];
    }
    let entries = std::fs::read_dir(path).expect("a directory");
    let mut entries: Vec<String> = entries
        .map(|entry| {
            let entry = entry.expect("an entry");
            format!("{:?}: {:?}", entry.file_name(), held(&entry.path()))
        })
        .collect();
    entries.sort();
    entries
}

/// What is at the path of the work directory and is not the command's is
/// refused, and nothing there is changed, followed or removed: a
/// directory with another entry than the two the command makes, with the
/// name of its file on something else, with other bytes in that file, or
/// with a database and no file; a link; and a file.
#[test]
fn what_is_at_the_work_directory_and_is_not_the_commands_is_refused_untouched() {
    let dir = tempfile::tempdir().expect("a directory");
    let source = dir.path().join("rocksdb");
    small(&source);
    let target = dir.path().join("lignindb");
    let work = work_dir(&target).unwrap();
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("a directory");
    let made = |work: &Path| std::fs::create_dir(work).expect("a directory");
    let file =
        |path: std::path::PathBuf, bytes: &[u8]| std::fs::write(path, bytes).expect("a file");
    let link = |to: &Path, at: std::path::PathBuf| {
        std::os::unix::fs::symlink(to, at).expect("a link");
    };
    let longer = [MARK_TEXT, b"and more"].concat();
    type Put<'a> = (&'a str, Box<dyn Fn(&Path) + 'a>);
    let foreign: Vec<Put<'_>> = vec![
        (
            "a file of someone",
            Box::new(|work| {
                made(work);
                file(work.join("README"), b"of someone");
            }),
        ),
        (
            "a directory with the name of the file",
            Box::new(|work| {
                made(work);
                made(&work.join(WORK_MARK));
            }),
        ),
        (
            "a file of that name with other bytes",
            Box::new(|work| {
                made(work);
                file(work.join(WORK_MARK), b"of someone");
            }),
        ),
        (
            "a file of that name with more bytes",
            Box::new(|work| {
                made(work);
                file(work.join(WORK_MARK), &longer);
            }),
        ),
        (
            "the command's file and a file beside it",
            Box::new(|work| {
                made(work);
                file(work.join(WORK_MARK), MARK_TEXT);
                file(work.join("README"), b"of someone");
            }),
        ),
        (
            "a database and no file",
            Box::new(|work| {
                made(work);
                made(&work.join(WORK_DATABASE));
            }),
        ),
        (
            "a database beside a file with the first bytes of the command's",
            Box::new(|work| {
                made(work);
                file(work.join(WORK_MARK), &MARK_TEXT[..7]);
                made(&work.join(WORK_DATABASE));
            }),
        ),
        (
            "a file of that name with no byte",
            Box::new(|work| {
                made(work);
                file(work.join(WORK_MARK), b"");
            }),
        ),
        (
            "a file of that name with the first bytes of the command's",
            Box::new(|work| {
                made(work);
                file(work.join(WORK_MARK), &MARK_TEXT[..7]);
            }),
        ),
        (
            "a database beside a file of the name the command's is made under",
            Box::new(|work| {
                made(work);
                file(work.join(WORK_MARK_NEW), MARK_TEXT);
                made(&work.join(WORK_DATABASE));
            }),
        ),
        (
            "a file of the name the command's is made under that is longer than it",
            Box::new(|work| {
                made(work);
                file(work.join(WORK_MARK_NEW), &longer);
            }),
        ),
        (
            "a file of the name the command's is made under with other bytes",
            Box::new(|work| {
                made(work);
                file(work.join(WORK_MARK_NEW), b"of someone");
            }),
        ),
        (
            "a directory of the name the command's file is made under",
            Box::new(|work| {
                made(work);
                made(&work.join(WORK_MARK_NEW));
            }),
        ),
        (
            "a link with the name of the database",
            Box::new(|work| {
                made(work);
                file(work.join(WORK_MARK), MARK_TEXT);
                link(&elsewhere, work.join(WORK_DATABASE));
            }),
        ),
        (
            "a link to a directory",
            Box::new(|work| link(&elsewhere, work.to_path_buf())),
        ),
        (
            "a file",
            Box::new(|work| file(work.to_path_buf(), b"of someone")),
        ),
    ];
    for (what, put) in &foreign {
        put(&work);
        let before = held(&work);
        let (outcome, out) = ran(&migration(&source, &target));
        let refused = outcome.expect_err(what);
        assert!(
            refused.contains("is not the work of a migration"),
            "{what}: {refused}\n{out}"
        );
        assert_eq!(held(&work), before, "{what}: it is as it was");
        assert_eq!(held(&elsewhere), Vec::<String>::new(), "{what}");
        assert!(!target.exists(), "{what}");
        match std::fs::symlink_metadata(&work)
            .expect("it is there")
            .is_dir()
        {
            true => std::fs::remove_dir_all(&work).expect("the directory is removed"),
            false => std::fs::remove_file(&work).expect("the file is removed"),
        }
    }
}

/// What a stop leaves while the work directory is made is the command's:
/// a directory with nothing in it, one with its file and no byte of it or
/// its first bytes, and one with its whole file and no database. Each is
/// gone on with.
#[test]
fn a_work_directory_that_a_stop_left_at_its_making_is_taken() {
    let dir = tempfile::tempdir().expect("a directory");
    let source = dir.path().join("rocksdb");
    let root = small(&source);
    // The file before it has its name holds whatever the stop left of it:
    // no byte, its first bytes, bytes that were never written, or all.
    let never_written = vec![0; MARK_TEXT.len()];
    let left: [Option<(&str, &[u8])>; 6] = [
        None,
        Some((WORK_MARK_NEW, b"")),
        Some((WORK_MARK_NEW, &MARK_TEXT[..10])),
        Some((WORK_MARK_NEW, &never_written)),
        Some((WORK_MARK_NEW, MARK_TEXT)),
        Some((WORK_MARK, MARK_TEXT)),
    ];
    for (number, file) in left.into_iter().enumerate() {
        let target = dir.path().join(format!("lignindb-{number}"));
        let work = work_dir(&target).unwrap();
        std::fs::create_dir(&work).expect("the work directory");
        if let Some((name, bytes)) = file {
            std::fs::write(work.join(name), bytes).expect("its file");
        }
        let (outcome, out) = ran(&migration(&source, &target));
        assert_eq!(outcome, Ok(Outcome::Done), "{file:?}: {out}");
        assert!(!work.exists(), "{file:?}");
        assert_eq!(
            root_of(&Lignin::open_with_engine(&target).expect("the target")),
            root
        );
    }
}

/// What a migration writes, with something done once a line that holds
/// `at` was written, the first time: what it does is how that write
/// ends.
struct AtLine<'a> {
    at: &'static str,
    then: Option<Box<dyn FnOnce() -> io::Result<()> + 'a>>,
    out: Vec<u8>,
}

impl Write for AtLine<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.out.extend_from_slice(bytes);
        if bytes.ends_with(b"\n") {
            let whole = &self.out[..self.out.len() - 1];
            let from = whole
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |at| at + 1);
            let met = String::from_utf8_lossy(&whole[from..]).contains(self.at);
            if let Some(then) = self.then.take_if(|_| met) {
                then()?;
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// An error of the report is none of the device that the database is on:
/// a writer that says it is full fails the run, with the status of a
/// failure and no word of a full device, and the same command goes on.
#[test]
fn a_report_that_cannot_be_written_is_no_full_device() {
    let dir = tempfile::tempdir().expect("a directory");
    let (source, target) = (dir.path().join("rocksdb"), dir.path().join("lignindb"));
    let root = small(&source);
    let migration = migration(&source, &target);
    let mut out = AtLine {
        at: "the load needs about",
        then: Some(Box::new(|| {
            Err(io::Error::from(io::ErrorKind::StorageFull))
        })),
        out: Vec::new(),
    };
    let failed = migrate(&migration, &mut out).expect_err("the writer failed");
    let said = String::from_utf8_lossy(&out.out).into_owned();
    assert_eq!(failed.status(), 1, "{failed}");
    assert!(!said.contains("IS FULL"), "{said}");
    assert!(!target.exists());

    let (outcome, out) = ran(&migration);
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    assert_eq!(
        root_of(&Lignin::open_with_engine(&target).expect("the target")),
        root
    );
}

/// A writer of the report that panics ends the call with its panic: the
/// migration does not wait for a write that is never made, and what it
/// left is gone on with.
#[test]
fn a_writer_that_panics_ends_the_migration() {
    let dir = tempfile::tempdir().expect("a directory");
    let (source, target) = (dir.path().join("rocksdb"), dir.path().join("lignindb"));
    let root = small(&source);
    let migration = migration(&source, &target);
    let mut out = AtLine {
        at: "the load needs about",
        then: Some(Box::new(|| -> io::Result<()> {
            panic!("the writer of the report panics")
        })),
        out: Vec::new(),
    };
    let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        migrate(&migration, &mut out).map_err(|error| error.to_string())
    }));
    let panic = ended.expect_err("the panic of the writer");
    assert_eq!(
        panic.downcast_ref::<&str>().copied(),
        Some("the writer of the report panics")
    );
    drop(out);

    let (outcome, out) = ran(&migration);
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    assert_eq!(
        root_of(&Lignin::open_with_engine(&target).expect("the target")),
        root
    );
}

/// A load is complete where its database holds a record and no journal,
/// also where every record it holds is in the namespace that no element
/// hosts: a grove with nothing in it, and a record of a root key under a
/// prefix of no tree. Such a store is compared with its source as it is
/// now, and is not loaded again over what it holds.
#[test]
fn a_complete_load_with_records_in_the_namespace_alone_is_compared() {
    let dir = tempfile::tempdir().expect("a directory");
    let (source, target) = (dir.path().join("rocksdb"), dir.path().join("lignindb"));
    drop(rocks(&source));
    let of_no_tree = [0x77; 32];
    raw(&source, of_no_tree, 'r', b"r", Some(b"a key"));
    let (outcome, out) = ran(&migration(&source, &target));
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    assert_eq!(word(&out, "migrate: loaded", "root_keys"), 1, "{out}");

    unnamed(&target);
    let (outcome, out) = ran(&migration(&source, &target));
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    assert!(!out.contains("migrate: loaded"), "{out}");

    unnamed(&target);
    raw(&source, of_no_tree, 'r', b"r", Some(b"another key"));
    let (outcome, out) = ran(&migration(&source, &target));
    assert_eq!(outcome, Ok(Outcome::NotSound), "{out}");
    assert!(!out.contains("migrate: loaded"), "{out}");
    assert!(out.contains("DIFFERS: the record of the root key"), "{out}");
}

/// A sync that fails once the database has its name ends the run with
/// what is at the target: the database, under a name that is not durable.
/// It is the status of a failure, also where the device says that it is
/// full. The same command then makes the syncs, removes the work
/// directory, and refuses the target as one that is there; a power cut
/// after it leaves the database at its name.
#[test]
fn a_sync_that_fails_after_the_rename_is_said_and_made_by_the_next_run() {
    let dir = tempfile::tempdir().expect("a directory");
    let source = dir.path().join("rocksdb");
    let root = small(&source);
    let work = work_dir(Path::new(ON_DEVICE)).unwrap();
    let failing = [
        (work.as_path(), Fault::Eio),
        (Path::new("/"), Fault::Eio),
        (work.as_path(), Fault::Enospc),
        (Path::new("/"), Fault::Enospc),
    ];
    for (number, (at, fault)) in failing.into_iter().enumerate() {
        let device = SimVfs::new(SimConfig::new(number as u64));
        let migration = on(
            &device,
            &source,
            Unreachable::Refuse,
            SyncPolicy::EveryCommit,
        );
        let mut out = AtLine {
            at: "migrate: verified",
            then: Some(Box::new(|| {
                device.add_rule(FaultRule::new(fault).on(OpKind::SyncDir).at(at));
                Ok(())
            })),
            out: Vec::new(),
        };
        let failed = migrate(&migration, &mut out).expect_err("a sync failed");
        let said = String::from_utf8_lossy(&out.out).into_owned();
        drop(out);
        assert_eq!(failed.status(), 1, "{failed}");
        let failed = failed.to_string();
        assert!(
            failed.contains("could not be made durable"),
            "{failed}\n{said}"
        );
        assert!(
            !said.contains("IS FULL") && !said.contains("migrate: done"),
            "{said}"
        );
        device.clear_rules();
        // The database has its name on the device as it is, and the work
        // directory holds its file alone. Nothing opens the database
        // here: an open is a session, which syncs.
        let named = device.read_dir(Path::new(ON_DEVICE)).expect("the target");
        assert!(!named.is_empty(), "{at:?} {fault}");
        let left = device.read_dir(&work).expect("the work directory");
        assert_eq!(left.len(), 1, "{left:?}");
        // A power cut now takes the name back: the database is in the
        // work directory again, and the same command goes on from there.
        let image = device.crash_image();
        let lost = image.recover(&image.lose_all()).expect("a device");
        let unnamed = lost.read_dir(Path::new(ON_DEVICE));
        assert!(unnamed.is_err(), "the name was not durable: {unnamed:?}");
        let again = on(&lost, &source, Unreachable::Refuse, SyncPolicy::EveryCommit);
        let (outcome, said) = ran(&again);
        assert_eq!(outcome, Ok(Outcome::Done), "{said}");
        assert_eq!(root_of(&grove_on(&lost)), root, "{at:?} {fault}");

        // With no power cut, the same command makes the syncs.
        let (outcome, said) = ran(&migration);
        let refused = outcome.expect_err("the target is there");
        assert!(
            refused.contains("a database that a migration left at the target is whole"),
            "{refused}\n{said}"
        );
        let gone = device.read_dir(&work).expect_err("the work directory");
        assert_eq!(gone.kind(), io::ErrorKind::NotFound);
        // What no sync made durable is lost: the name is not.
        let image = device.crash_image();
        let cut = image.recover(&image.lose_all()).expect("a device");
        assert_eq!(root_of(&grove_on(&cut)), root, "{at:?} {fault}");
    }
}

/// What a migration writes, which turns the power of the device off once
/// a line that holds `at` was written `left` times.
struct CutAt {
    device: SimVfs,
    at: &'static str,
    left: usize,
    out: Vec<u8>,
}

impl Write for CutAt {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.out.extend_from_slice(bytes);
        if bytes.ends_with(b"\n") && self.left > 0 {
            let whole = &self.out[..self.out.len() - 1];
            let from = whole
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |at| at + 1);
            if String::from_utf8_lossy(&whole[from..]).contains(self.at) {
                self.left -= 1;
                if self.left == 0 {
                    self.device.power_off();
                }
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The directory of the target on a simulated device.
const ON_DEVICE: &str = "/lignindb";

fn on(device: &SimVfs, source: &Path, unreachable: Unreachable, sync: SyncPolicy) -> Migration {
    let options = Options {
        vfs: Arc::new(device.clone()),
        sync,
        ..Options::default()
    };
    Migration {
        options,
        unreachable,
        ..migration(source, Path::new(ON_DEVICE))
    }
}

/// Runs `migration` on `device` until the line that holds `at` was written
/// `nth` times, where the power goes off, and returns the device as the
/// cut left it with what was written.
fn cut(device: &SimVfs, migration: &Migration, at: &'static str, nth: usize) -> (SimVfs, String) {
    let mut out = CutAt {
        device: device.clone(),
        at,
        left: nth,
        out: Vec::new(),
    };
    let outcome = migrate(migration, &mut out);
    let said = String::from_utf8_lossy(&out.out).into_owned();
    assert!(
        device.is_powered_off(),
        "no line {at:?} number {nth} in:\n{said}"
    );
    assert!(
        outcome.is_err(),
        "the power went off at {at:?} number {nth}:\n{said}"
    );
    (device.crash(nth as u64), said)
}

fn grove_on(device: &SimVfs) -> Lignin {
    let options = Options {
        vfs: Arc::new(device.clone()),
        create_if_missing: false,
        ..Options::default()
    };
    let engine = LigninEngine::open_with(ON_DEVICE, &options).expect("the migrated store");
    GroveDb::from_engine(engine)
}

/// A load that the power stops is gone on with from where it stood: in
/// each part of the load, once it is complete, and after the store was
/// compared and verified and before it has its name. Every commit is
/// made durable here, so the place the next run finds is the place the
/// line told of.
#[test]
fn a_load_that_a_power_cut_stops_goes_on_from_where_it_stood() {
    let dir = tempfile::tempdir().expect("a directory");
    let source = dir.path().join("rocksdb");
    let root = with_orphans(&source);
    let carry = Unreachable::Carry;
    let cuts: [(&'static str, usize, Option<&str>); 10] = [
        ("phase=pending-drops", 3, Some("pending-drops")),
        ("phase=grove", 1, Some("grove")),
        ("phase=grove", 6, Some("grove")),
        ("phase=grove", 11, Some("grove")),
        ("phase=root-keys", 1, Some("root-keys")),
        ("phase=root-keys", 2, Some("root-keys")),
        ("phase=pending-drops", 1, Some("pending-drops")),
        ("phase=pending-drops", 2, Some("pending-drops")),
        ("migrate: loaded", 1, None),
        ("migrate: verified", 1, None),
    ];
    for (seed, (at, nth, resumed)) in cuts.into_iter().enumerate() {
        let device = SimVfs::new(SimConfig::new(seed as u64));
        let migration = on(&device, &source, carry, SyncPolicy::EveryCommit);
        let (left, first) = cut(&device, &migration, at, nth);
        let (outcome, out) = ran(&on(&left, &source, carry, SyncPolicy::EveryCommit));
        assert_eq!(
            outcome,
            Ok(Outcome::Done),
            "after {at} {nth}:\n{first}\n{out}"
        );
        let phase = out.lines().find_map(|line| {
            let words = line.strip_prefix("migrate: resumed phase=")?;
            words.split_whitespace().next()
        });
        assert_eq!(phase, resumed, "after {at} {nth}:\n{out}");
        // A load that was complete is not made again.
        assert_eq!(out.contains("migrate: loaded"), resumed.is_some(), "{out}");
        assert!(out.contains("migrate: verified"), "{out}");
        assert_eq!(
            word(&out, "migrate: compared", "unreachable_carried"),
            2,
            "{out}"
        );
        assert_eq!(root_of(&grove_on(&left)), root);
        if resumed.is_some() {
            // Each record is counted once, by the run that carried it or
            // by the one that went on.
            assert_eq!(
                word(&out, "migrate: loaded", "nodes"),
                NODES,
                "{first}\n{out}"
            );
            assert_eq!(
                word(&out, "migrate: loaded", "aux"),
                5 + 1,
                "{first}\n{out}"
            );
            assert_eq!(
                word(&out, "migrate: loaded", "unlinked"),
                1,
                "{first}\n{out}"
            );
        }
    }
}

/// The power goes off after any number of changes to the device, the
/// load is gone on with on what a crash leaves, the power goes off once
/// more, and the run after that ends with the store at its name: the same
/// grove, and still there after one more crash. A run that finds the
/// store at its name, because the power went off after the rename, says
/// that it is whole and makes nothing.
#[test]
fn power_cuts_at_any_change_leave_a_load_that_goes_on() {
    let dir = tempfile::tempdir().expect("a directory");
    let source = dir.path().join("rocksdb");
    let root = with_orphans(&source);
    let carry = Unreachable::Carry;
    let whole = SimVfs::new(SimConfig::new(0));
    let (outcome, out) = ran(&on(&whole, &source, carry, SyncPolicy::Never));
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    let compared = |out: &str| -> Vec<u64> {
        let names = ["subtrees", "data", "aux", "meta", "root_keys", "marks"];
        let names = names
            .into_iter()
            .chain(["unreachable_carried", "differing"]);
        names
            .map(|name| word(out, "migrate: compared", name))
            .collect()
    };
    let expected = compared(&out);
    let changes = whole.stats().changes;
    assert!(changes > 100, "{changes} changes");

    let (mut cuts, mut gone_on, mut fresh, mut named, mut ended) = (0, 0, 0, 0, 0);
    let step = (changes / 60).max(1);
    for after in (1..=changes).step_by(step as usize) {
        let mut device = SimVfs::new(SimConfig::new(after));
        // The first run is cut, and the second a third of the way on.
        device.crash_at(CrashPoint::AfterChange(after));
        for run in 0.. {
            let (outcome, out) = ran(&on(&device, &source, carry, SyncPolicy::Never));
            if !device.is_powered_off() {
                match outcome {
                    Ok(Outcome::Done) => {
                        assert_eq!(compared(&out), expected, "after {after}:\n{out}");
                        gone_on += u64::from(out.contains("migrate: resumed"));
                        let loaded = out.contains("migrate: loaded");
                        fresh += u64::from(loaded && !out.contains("migrate: resumed"));
                    }
                    // The run before gave the store its name, and the
                    // power went off before it said so.
                    Err(refused) => {
                        let whole = "a database that a migration left at the target is whole";
                        assert!(refused.contains(whole), "after {after}: {refused}\n{out}");
                        named += 1;
                    }
                    other => panic!("after {after}: {other:?}\n{out}"),
                }
                // The cut that this run did not come to is not made.
                device.crash_at(CrashPoint::AfterChange(u64::MAX));
                break;
            }
            // The power went off inside the run, or after its last
            // change, which is the removal of the work directory. A
            // check that could not read the store it checks says that
            // the store is not sound, and gives it no name.
            let ended = matches!(outcome, Err(_) | Ok(Outcome::Done) | Ok(Outcome::NotSound));
            assert!(ended, "after {after}: {outcome:?}\n{out}");
            cuts += 1;
            device = device.crash(after);
            if run == 0 {
                device.crash_at(CrashPoint::AfterChange(changes / 3));
            }
        }
        assert_eq!(root_of(&grove_on(&device)), root, "after {after}");
        let later = device.crash(after + 1);
        assert_eq!(
            root_of(&grove_on(&later)),
            root,
            "after {after} and a crash"
        );
        ended += 1;
    }
    println!(
        "{ended} loads of {changes} changes, cut {cuts} times: {gone_on} last runs went on with \
         a load, {fresh} began one, {named} found the store at its name"
    );
    assert!(cuts > 0, "the load that was cut after one change was cut");
}

/// A device that is full stops the load with the status of a migration
/// that is not complete, and the same command goes on once there is room.
#[test]
fn a_full_device_stops_a_load_that_goes_on_once_there_is_room() {
    let dir = tempfile::tempdir().expect("a directory");
    let source = dir.path().join("rocksdb");
    let root = small(&source);
    let refuse = Unreachable::Refuse;
    let whole = SimVfs::new(SimConfig::new(0));
    let (outcome, out) = ran(&on(&whole, &source, refuse, SyncPolicy::Never));
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    let used = whole.used_bytes();

    assert_eq!(Outcome::IncompleteMigration.status(), 4);
    // A device with room for nothing, for the first files of a database,
    // and for a part of the load: each run ends as one that is not
    // complete, and the run after room was made goes on to the end.
    let mut stopped = 0;
    for capacity in [
        0,
        64,
        4 << 10,
        used / 16,
        used / 4,
        used / 2,
        used - used / 8,
    ] {
        let device = SimVfs::new(SimConfig::new(capacity));
        device.set_capacity(Some(capacity));
        let (outcome, out) = ran(&on(&device, &source, refuse, SyncPolicy::Never));
        match outcome {
            Ok(Outcome::IncompleteMigration) => {
                assert!(out.contains("IS FULL"), "{out}");
                stopped += 1;
            }
            // The device had room for all of it.
            Ok(Outcome::Done) => {}
            other => panic!("a device of {capacity} bytes: {other:?}\n{out}"),
        }
        device.set_capacity(None);
        let (outcome, out) = ran(&on(&device, &source, refuse, SyncPolicy::Never));
        let whole = "a database that a migration left at the target is whole";
        match outcome {
            Ok(Outcome::Done) => {}
            Err(refused) if refused.contains(whole) => {}
            other => panic!("after a device of {capacity} bytes: {other:?}\n{out}"),
        }
        assert_eq!(root_of(&grove_on(&device)), root, "{capacity}");
    }
    assert!(stopped >= 4, "{stopped} devices were full");

    // Devices of 97 sizes, from none to what the migration of a grove
    // with nothing in it uses: the smallest of them fill while the work
    // directory is made and in the first commit of the database. On each,
    // a full device up to the rename is never another failure, and after
    // the rename the run says what is at the target.
    let empty = dir.path().join("empty");
    drop(rocks(&empty));
    let whole = SimVfs::new(SimConfig::new(0));
    let (outcome, out) = ran(&on(&whole, &empty, refuse, SyncPolicy::Never));
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    let used = whole.used_bytes();
    let mut stopped = 0;
    for capacity in (0..=used).step_by((used / 96).max(1) as usize) {
        let device = SimVfs::new(SimConfig::new(capacity));
        device.set_capacity(Some(capacity));
        let (outcome, out) = ran(&on(&device, &empty, refuse, SyncPolicy::Never));
        match outcome {
            Ok(Outcome::IncompleteMigration) => {
                assert!(out.contains("IS FULL"), "{out}");
                stopped += 1;
            }
            Ok(Outcome::Done) => {}
            Err(named) if named.contains("could not be made durable") => {}
            other => panic!("a device of {capacity} bytes: {other:?}\n{out}"),
        }
        device.set_capacity(None);
        let (outcome, out) = ran(&on(&device, &empty, refuse, SyncPolicy::Never));
        let whole = "a database that a migration left at the target is whole";
        match outcome {
            Ok(Outcome::Done) => {}
            Err(refused) if refused.contains(whole) => {}
            other => panic!("after a device of {capacity} bytes: {other:?}\n{out}"),
        }
        assert_eq!(root_of(&grove_on(&device)), [0; 32], "{capacity}");
    }
    assert!(stopped >= 24, "{stopped} devices were full");
}

/// A load goes on from the source it was begun on, in the state it was
/// begun on, with what it was told of the records nothing leads to.
#[test]
fn a_load_of_another_state_of_the_source_is_refused() {
    let dir = tempfile::tempdir().expect("a directory");
    let source = dir.path().join("rocksdb");
    with_orphans(&source);
    let every = SyncPolicy::EveryCommit;
    let device = SimVfs::new(SimConfig::new(0));
    let (left, _) = cut(
        &device,
        &on(&device, &source, Unreachable::Leave, every),
        "phase=grove",
        9,
    );

    // A run that is told nothing of the records nothing leads to is
    // refused by its survey, as a first run is, and one that is told
    // otherwise than the load was begun with by the journal.
    for (other, said) in [
        (Unreachable::Refuse, "holds what a migration does not carry"),
        (Unreachable::Carry, "was begun with --unreachable leave"),
    ] {
        let (outcome, out) = ran(&on(&left, &source, other, every));
        let refused = outcome.expect_err("another choice is refused");
        assert!(refused.contains(said), "{refused}\n{out}");
    }
    put(
        &rocks(&source),
        ROOT,
        b"later",
        Element::new_item(b"since".to_vec()),
    );
    let (outcome, out) = ran(&on(&left, &source, Unreachable::Leave, every));
    let refused = outcome.expect_err("another state of the source is refused");
    assert!(
        refused.contains("of another state of this one"),
        "{refused}\n{out}"
    );
}

/// A target that does not hold what its source holds is not given its
/// name: the run ends with the status of a store that is not sound.
#[test]
fn a_target_that_differs_from_its_source_is_not_named() {
    let dir = tempfile::tempdir().expect("a directory");
    let (source, target) = (dir.path().join("rocksdb"), dir.path().join("lignindb"));
    small(&source);
    let (outcome, out) = ran(&migration(&source, &target));
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    // As a run leaves it that ended before the rename, with one item more.
    let work = work_dir(&target).unwrap();
    let database = unnamed(&target);
    {
        let lignin = Lignin::open_with_engine(&database).expect("the store");
        let item = Element::new_item(b"not of the source".to_vec());
        let inserted = lignin.insert(
            &[b"a".as_slice()],
            b"more",
            item,
            None,
            None,
            GroveVersion::latest(),
        );
        inserted.unwrap().expect("an item");
    }
    let (outcome, out) = ran(&migration(&source, &target));
    assert_eq!(outcome, Ok(Outcome::NotSound), "{out}");
    assert_eq!(Outcome::NotSound.status(), 3);
    assert!(out.contains("THE TARGET IS NOT ITS SOURCE"), "{out}");
    assert!(out.contains("DIFFERS:"), "{out}");
    assert!(!out.contains("migrate: loaded"), "{out}");
    assert!(work.exists() && !target.exists());
}

/// What both engines answer at the path of a tree whose records nothing
/// leads to, once a tree is made there again.
fn made_again<E: grovedb::Engine>(db: &GroveDb<E>) -> Vec<String> {
    let v = GroveVersion::latest();
    let mut answers = Vec::new();
    let mut say = |what: &str, answer: String| answers.push(format!("{what}: {answer}"));
    say(
        "has_raw",
        format!("{:?}", db.has_raw(GONE, &[1], None, v).unwrap()),
    );
    let made = db.insert(
        &[b"c".as_slice()],
        b"gone",
        Element::empty_tree(),
        None,
        None,
        v,
    );
    say("made again", format!("{:?}", made.unwrap()));
    say(
        "is_empty_tree",
        format!("{:?}", db.is_empty_tree(GONE, None, v).unwrap()),
    );
    say("get", format!("{:?}", db.get(GONE, &[1], None, v).unwrap()));
    let item = Element::new_item(b"new".to_vec());
    say(
        "insert",
        format!("{:?}", db.insert(GONE, &[2], item, None, None, v).unwrap()),
    );
    say(
        "root",
        format!("{:?}", db.root_hash(None, v).unwrap().map(hex::encode)),
    );
    answers
}

/// Records under a prefix that nothing leads to: the survey says what
/// they are, and the source is refused, or they are left behind, or they
/// are carried, as the migration is told.
#[test]
fn records_that_nothing_leads_to_are_refused_left_or_carried() {
    let dir = tempfile::tempdir().expect("a directory");
    let source = dir.path().join("rocksdb");
    let root = with_orphans(&source);
    let gone = hex::encode(prefix_of(GONE));

    for target in [None, Some(dir.path().join("refused"))] {
        let refusing = Migration {
            target: target.clone(),
            ..migration(&source, Path::new("unused"))
        };
        let (outcome, out) = ran(&refusing);
        let refused = outcome.expect_err("the source is refused");
        assert!(
            refused.contains("holds what a migration does not carry"),
            "{refused}"
        );
        assert!(refused.contains("nothing was written"), "{refused}");
        if let Some(target) = target {
            assert!(!target.exists() && !work_dir(&target).unwrap().exists());
        }
        assert_eq!(word(&out, "migrate: survey", "orphan_prefixes"), 2, "{out}");
        assert_eq!(word(&out, "migrate: survey", "orphan_records"), 2, "{out}");
        let line = out
            .lines()
            .find(|line| line.contains(&gone))
            .expect("its line");
        assert!(line.contains("NOT CARRIED: prefix"), "{line}");
        assert!(line.contains("data 1 ("), "{line}");
        assert!(
            line.contains("its data records are the nodes of a"),
            "{line}"
        );
        assert!(
            line.contains("its root-key record names the key 01"),
            "{line}"
        );
        let record = out
            .lines()
            .skip_while(|line| !line.contains(&gone))
            .nth(1)
            .unwrap();
        assert!(record.contains("key 01: a node of a"), "{record}");
        assert!(record.contains("SumTree"), "{record}");
        let other = out
            .lines()
            .find(|line| line.contains(&hex::encode([0xAB; 32])));
        let other = other.expect("the line of the other prefix");
        assert!(other.contains("aux 1 (") && other.contains("its data records are none"));
    }

    for (told, name) in [
        (Unreachable::Leave, "left"),
        (Unreachable::Carry, "carried"),
    ] {
        let target = dir.path().join(name);
        let telling = Migration {
            unreachable: told,
            ..migration(&source, &target)
        };
        let (outcome, out) = ran(&telling);
        assert_eq!(outcome, Ok(Outcome::Done), "{out}");
        let carried = u64::from(told == Unreachable::Carry) * 2;
        assert_eq!(
            word(&out, "migrate: compared", "unreachable_carried"),
            carried,
            "{out}"
        );
        assert_eq!(
            word(&out, "migrate: compared", "unreachable_left"),
            2 - carried,
            "{out}"
        );
        assert_eq!(
            word(&out, "migrate: loaded", "unreachable"),
            carried,
            "{out}"
        );
        assert_eq!(
            word(&out, "migrate: loaded", "unlinked"),
            carried / 2,
            "{out}"
        );
        let said = format!(
            "AS --unreachable {} SAYS: 2 records under 2 prefixes",
            told.name()
        );
        assert!(out.contains(&said), "{out}");
        let nowhere = word(&out, "migrate: survey", "root_keys_where_no_tree");
        assert_eq!(nowhere, 1, "{out}");
        assert_eq!(word(&out, "migrate: compared", "root_keys"), 2, "{out}");
        assert_eq!(
            root_of(&Lignin::open_with_engine(&target).expect("the target")),
            root
        );

        let copy = dir.path().join(format!("{name}-source"));
        copy_dir(&source, &copy);
        let was = made_again(&rocks(&copy));
        let is = made_again(&Lignin::open_with_engine(&target).expect("the target"));
        // What the two engines answer alike once a tree is made again at
        // the path, as each engine stands (`engine.md`, "A subtree leaves
        // with its element"): `has_raw`, the insert of the tree,
        // `is_empty_tree`, a read of the old key, an insert under the
        // tree, and the root hash.
        let alike: Vec<bool> = was.iter().zip(&is).map(|(was, is)| was == is).collect();
        let expected = match told {
            // Left behind, the old records are read by nothing: RocksDB
            // answers the two reads that go by the prefix from them.
            Unreachable::Leave => [false, true, false, true, true, true],
            // Carried, they are read alike, and the engine takes no write
            // into the tree that is made again over them.
            _ => [true, true, true, true, false, false],
        };
        assert_eq!(alike, expected, "{}:\n{was:#?}\n{is:#?}", told.name());
        if told == Unreachable::Carry {
            assert!(was[4].ends_with("Ok(())"), "{was:#?}");
            assert!(is[4].contains("is entered under no element"), "{is:#?}");
        }
    }

    // What a migration that carries does not carry: data records that are
    // not the nodes of one Merk.
    raw(
        &source,
        [0xCD; 32],
        'd',
        b"some key",
        Some(b"no node of a Merk"),
    );
    let target = dir.path().join("not carried");
    let carrying = Migration {
        unreachable: Unreachable::Carry,
        ..migration(&source, &target)
    };
    let (outcome, out) = ran(&carrying);
    assert!(outcome.is_err(), "{out}");
    let line = out
        .lines()
        .find(|line| line.contains(&hex::encode([0xCD; 32])));
    let line = line.expect("the line of the prefix");
    assert!(
        line.contains("NOT CARRIED") && line.contains("not the nodes of one Merk"),
        "{line}"
    );
    assert!(!target.exists() && !work_dir(&target).unwrap().exists());
}

/// Shapes that RocksDB holds and no operation of GroveDB makes today are
/// carried as they are and counted: a node record that no tree links, and
/// a root Merk whose record of the root key names a node that is not its
/// root (GroveDB #1065), which is said loudly. Of such a Merk the engine
/// takes no further node, where RocksDB goes on with the part of the tree
/// that the record names.
#[test]
fn records_no_tree_links_and_a_root_key_that_names_no_root_are_carried() {
    let dir = tempfile::tempdir().expect("a directory");
    let (source, target) = (dir.path().join("rocksdb"), dir.path().join("lignindb"));
    small(&source);
    let e = prefix_of(&[b"e"]);
    let only = raw_record(&source, e, b"only");
    raw(&source, e, 'd', b"ghost", Some(&only));
    let (outcome, out) = ran(&migration(&source, &target));
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    assert_eq!(word(&out, "migrate: survey", "unlinked"), 1, "{out}");
    assert_eq!(word(&out, "migrate: loaded", "unlinked"), 1, "{out}");
    assert!(alike(&source, &target) > 60);

    // The record of the root key of the root Merk names a node that is
    // not the root.
    let stale = dir.path().join("stale");
    small(&stale);
    let named = {
        let db = rocks(&stale);
        let storage = db.engine().storage();
        let transaction = storage.start_transaction();
        let context = storage
            .get_immediate_storage_context_by_subtree_prefix([0; 32], &transaction)
            .value;
        context
            .get_root(b"r")
            .unwrap()
            .expect("a read")
            .expect("the record")
    };
    let mut other = [b"a", b"b", b"c", b"e"].into_iter();
    let other = other.find(|key| key.as_slice() != named.as_slice());
    let other = other.expect("a node that is not the root");
    raw(&stale, [0; 32], 'r', b"r", Some(other.as_slice()));
    let target = dir.path().join("of-stale");
    let (outcome, out) = ran(&migration(&stale, &target));
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    assert_eq!(word(&out, "migrate: survey", "stale_roots"), 1, "{out}");
    assert!(out.contains("(GroveDB #1065): 1"), "{out}");
    assert!(
        out.contains("AT /: THE MERK IS READ AS ITS RECORD NAMES IT"),
        "{out}"
    );
    assert!(alike(&stale, &target) > 0);

    // The next write of a node of that Merk. RocksDB takes it into the
    // tree its record names, which is a part of the tree. The engine
    // takes no node of a Merk whose root-key record does not name its
    // root, and leaves the store as it was: the close of the grove checks
    // it.
    let later = || Element::new_item(b"later".to_vec());
    let v = GroveVersion::latest();
    let was = rocks(&stale).insert(ROOT, b"z", later(), None, None, v);
    was.unwrap().expect("RocksDB takes the write");
    let lignin = Lignin::open_with_engine(&target).expect("the target");
    let before = root_of(&lignin);
    let is = lignin.insert(ROOT, b"z", later(), None, None, v).unwrap();
    let refused = is.expect_err("the engine refuses the write").to_string();
    assert!(refused.contains("does not name its root"), "{refused}");
    assert!(refused.contains("GroveDB #686 and #1065"), "{refused}");
    assert_eq!(root_of(&lignin), before);
}

/// On the grove versions 1 to 3 a tree that keeps flat records can hold a
/// keyed child, whose node lies among the flat records of the tree, and
/// which can be a tree with records under a prefix of its own. The engine
/// keeps such a child as a node of a Merk, which this command does not
/// make: the source is refused, whatever it is told of the records that
/// nothing leads to, and nothing is made.
#[test]
fn a_keyed_child_under_a_tree_of_flat_records_refuses_the_source() {
    for (number, version) in GROVE_VERSIONS.iter().take(3).enumerate() {
        let dir = tempfile::tempdir().expect("a directory");
        let (source, target) = (dir.path().join("rocksdb"), dir.path().join("lignindb"));
        {
            let db = rocks(&source);
            let tree = Element::empty_mmr_tree();
            let made = QualifiedGroveDbOp::insert_or_replace_op(vec![], b"mmr".to_vec(), tree);
            // On the first of the three versions the child is a tree
            // that holds an item.
            let child = match number {
                0 => Element::empty_tree(),
                _ => Element::new_item(b"a keyed child".to_vec()),
            };
            let under = vec![b"mmr".to_vec()];
            let child = QualifiedGroveDbOp::insert_or_replace_op(under, b"child".to_vec(), child);
            let mut batch = vec![made, child];
            if number == 0 {
                let below = vec![b"mmr".to_vec(), b"child".to_vec()];
                let item = Element::new_item(b"below a keyed child".to_vec());
                batch.push(QualifiedGroveDbOp::insert_or_replace_op(
                    below,
                    b"item".to_vec(),
                    item,
                ));
            }
            let applied = db.apply_batch(batch, None, None, version);
            applied.unwrap().expect("the batch is taken");
            for n in 0..3u8 {
                let appended = db.mmr_tree_append(ROOT, b"mmr", vec![n; 40], None, version);
                appended.unwrap().expect("an append");
            }
        }
        for told in Unreachable::ALL {
            let telling = Migration {
                unreachable: told,
                ..migration(&source, &target)
            };
            let (outcome, out) = ran(&telling);
            let refused = outcome.expect_err("the source is refused");
            assert!(
                refused.contains("holds what a migration does not carry"),
                "{refused}"
            );
            assert_eq!(word(&out, "migrate: survey", "keyed_children"), 1, "{out}");
            let line = (out.lines())
                .find(|line| line.contains("NOT CARRIED") && line.contains("keyed children"));
            let line = line.unwrap_or_else(|| panic!("no line of the keyed child in:\n{out}"));
            assert!(line.contains("NOT CARRIED: /mmr:"), "{line}");
            assert!(!target.exists() && !work_dir(&target).unwrap().exists());
        }
    }
}

/// A flat record is taken for a keyed child where its bytes are those of
/// a Merk node of the key it lies under, with an element for its value.
/// The bytes of a node of another key, which a tree was given for a
/// value, are a flat record as any other, and are carried. A value that
/// is the node of its own slot is not told from a keyed child, and
/// refuses the source.
#[test]
fn a_flat_record_is_a_keyed_child_as_the_node_of_its_own_key_alone() {
    let v = GroveVersion::latest();
    let dir = tempfile::tempdir().expect("a directory");
    // The first value of a dense tree lies under the key of two zero
    // bytes.
    let keys: [(&[u8], bool); 2] = [(b"another key", false), (&[0, 0], true)];
    for (number, (key, taken)) in keys.into_iter().enumerate() {
        let source = dir.path().join(format!("rocksdb-{number}"));
        let target = dir.path().join(format!("lignindb-{number}"));
        {
            let db = rocks(&source);
            put(&db, ROOT, b"t", Element::empty_tree());
            put(&db, &[b"t"], key, Element::new_item(b"an item".to_vec()));
            put(&db, ROOT, b"dense", Element::empty_dense_tree(3));
        }
        let node = raw_record(&source, prefix_of(&[b"t"]), key);
        {
            let db = rocks(&source);
            let inserted = db.dense_tree_insert(ROOT, b"dense", node.clone(), None, v);
            let (_, position) = inserted.unwrap().expect("a value of the dense tree");
            assert_eq!(position, 0);
        }
        assert_eq!(raw_record(&source, prefix_of(&[b"dense"]), &[0, 0]), node);

        let (outcome, out) = ran(&migration(&source, &target));
        let children = word(&out, "migrate: survey", "keyed_children");
        assert_eq!(children, u64::from(taken), "{out}");
        match taken {
            false => {
                assert_eq!(outcome, Ok(Outcome::Done), "{out}");
                alike(&source, &target);
            }
            true => {
                let refused = outcome.expect_err("the source is refused");
                assert!(
                    refused.contains("holds what a migration does not carry"),
                    "{refused}"
                );
                assert!(!target.exists() && !work_dir(&target).unwrap().exists());
            }
        }
    }
}

/// A run says what peak of memory it projects, and a source that projects
/// more than the limit is refused before anything is made, by a run that
/// migrates and by one that only surveys. The refusal says what is above
/// the limit: what a run holds beside any Merk, or the largest of what it
/// holds whole, which for a prefix that is carried counts the aux and
/// meta records that are read with it. No sum of the projection wraps.
/// And a commit ends by the nodes it holds.
#[test]
fn a_source_that_projects_more_memory_than_the_limit_is_refused() {
    let dir = tempfile::tempdir().expect("a directory");
    let (source, target) = (dir.path().join("rocksdb"), dir.path().join("lignindb"));
    let root = small(&source);
    let word_of = |out: &str, name: &str| word(out, "migrate: memory", name);
    let nothing_made = || !target.exists() && !work_dir(&target).unwrap().exists();
    let plain = migration(&source, &target);
    // The floor: 256 MiB, what a commit takes and the buffer pool.
    let floor = (256 << 20) + plain.batch_bytes as u64 + plain.options.cache_size as u64;
    // The largest Merk that is handed over is the tree of forty items.
    let nodes = 40;

    // A limit below the floor: no Merk is what is above it.
    for surveys in [true, false] {
        let limited = Migration {
            target: (!surveys).then(|| target.clone()),
            memory_limit: Some(1 << 20),
            ..plain.clone()
        };
        let (outcome, out) = ran(&limited);
        let refused = outcome.expect_err("the projection is above the limit");
        assert!(refused.contains("above the limit of 1 MiB"), "{refused}");
        assert!(
            refused.contains("the limit is below the 256 MiB"),
            "{refused}"
        );
        assert!(!refused.contains("smaller ones take it"), "{refused}");
        assert!(!refused.contains("is not built"), "{refused}");
        assert_eq!(word_of(&out, "limit_bytes"), 1 << 20, "{out}");
        assert!(word_of(&out, "projected_peak_bytes") > floor, "{out}");
        assert!(nothing_made());
    }
    // A limit that the 256 MiB of any run with the largest Merk are
    // above: no setting of the run answers it.
    let limited = Migration {
        memory_limit: Some((256 << 20) + 1_000 * nodes),
        ..plain.clone()
    };
    let (outcome, out) = ran(&limited);
    let refused = outcome.expect_err("the largest Merk is above the limit");
    assert!(
        refused.contains("the largest of what it holds whole is 40 nodes"),
        "{refused}\n{out}"
    );
    assert!(refused.contains("is not built"), "{refused}");
    assert!(!refused.contains("smaller ones take it"), "{refused}");
    assert!(nothing_made());
    // A limit that only the sum with what a commit takes and the buffer
    // pool is above: smaller ones answer it.
    let limited = Migration {
        memory_limit: Some(floor + 1_000 * nodes),
        ..plain.clone()
    };
    let (outcome, out) = ran(&limited);
    let refused = outcome.expect_err("the sum is above the limit");
    assert!(refused.contains("smaller ones take it"), "{refused}\n{out}");
    assert!(!refused.contains("is not built"), "{refused}");
    assert!(nothing_made());
    // A commit of every size there is, where a size has the width of the
    // limit: the sum is one that no number of that width holds, and is
    // above the largest limit there is.
    if cfg!(target_pointer_width = "64") {
        let endless = Migration {
            batch_bytes: usize::MAX,
            memory_limit: Some(u64::MAX),
            ..plain.clone()
        };
        let (outcome, out) = ran(&endless);
        let refused = outcome.expect_err("the projection is above the limit");
        assert!(refused.contains("smaller ones take it"), "{refused}");
        assert_eq!(word_of(&out, "projected_peak_bytes"), u64::MAX, "{out}");
        assert!(nothing_made());
    }

    // The projection of a run that is taken grows with the nodes of the
    // largest Merk that is handed over.
    let roomy = Migration {
        memory_limit: Some(1 << 40),
        ..plain.clone()
    };
    let (outcome, out) = ran(&roomy);
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    let projected = word_of(&out, "projected_peak_bytes");
    assert!(
        projected > floor + 1_000 * nodes && projected < floor + 4_000 * nodes,
        "{projected}: {out}"
    );
    assert_eq!(word(&out, "migrate: survey", "largest_merk_nodes"), nodes);
    assert_eq!(
        root_of(&Lignin::open_with_engine(&target).expect("the target")),
        root
    );

    // A prefix that nothing leads to is read whole by a load that carries
    // it, its aux records with it: they count for a run that carries
    // them, and for no run that leaves them.
    let (source, target) = (dir.path().join("rocksdb-2"), dir.path().join("lignindb-2"));
    small(&source);
    let large = vec![7; 200_000];
    raw(&source, [0xCD; 32], 'a', b"an aux record", Some(&large));
    for told in [Unreachable::Leave, Unreachable::Carry] {
        let limited = Migration {
            target: None,
            unreachable: told,
            memory_limit: Some(floor + 300_000),
            ..migration(&source, &target)
        };
        let (outcome, out) = ran(&limited);
        if told == Unreachable::Leave {
            assert_eq!(outcome, Ok(Outcome::Done), "{out}");
            continue;
        }
        let refused = outcome.expect_err("the carried prefix is above the limit");
        assert!(refused.contains("above the limit"), "{refused}");
        assert!(out.contains("1 aux and meta records"), "{out}");
        // The record counts as a node does, with three times its bytes.
        let projected = word_of(&out, "projected_peak_bytes");
        assert!(projected > floor + 600_000, "{projected}: {out}");
    }

    // A commit ends by the nodes it holds. Two trees of 400 small items
    // each are in one commit where a commit takes 64 MiB, and in a commit
    // each where it takes 300,000 bytes, which their keys, their values
    // and a page each are far below.
    let source = dir.path().join("rocksdb-3");
    let into = dir.path().join("lignindb-3");
    std::fs::create_dir(&into).expect("a directory");
    {
        let db = rocks(&source);
        let mut batch = Vec::new();
        for tree in [b"x", b"y"] {
            batch.push(QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                tree.to_vec(),
                Element::empty_tree(),
            ));
            for n in 0..400u16 {
                batch.push(QualifiedGroveDbOp::insert_or_replace_op(
                    vec![tree.to_vec()],
                    n.to_be_bytes().to_vec(),
                    Element::new_item(vec![1]),
                ));
            }
        }
        let applied = db.apply_batch(batch, None, None, GroveVersion::latest());
        applied.unwrap().expect("the batch is taken");
    }
    let commits = |source: &Path, name: &str, batch_bytes: usize| {
        let sized = Migration {
            batch_bytes,
            unreachable: Unreachable::Carry,
            ..migration(source, &into.join(format!("{name}-{batch_bytes}")))
        };
        let (outcome, out) = ran(&sized);
        assert_eq!(outcome, Ok(Outcome::Done), "{out}");
        word(&out, "migrate: loaded", "commits")
    };
    let (few, more) = (
        commits(&source, "nodes", 64 << 20),
        commits(&source, "nodes", 300_000),
    );
    assert!(
        more >= few + 2,
        "{few} commits of 64 MiB, {more} of 300,000 bytes"
    );

    // And by the records it holds that are no nodes, as the projection
    // counts them. Three prefixes that nothing leads to, each with an aux
    // record of 100,000 bytes, are carried in one commit where a commit
    // takes 64 MiB, and in a commit each where it takes 250,000 bytes,
    // which the bytes of two of them are below.
    let source = dir.path().join("rocksdb-4");
    drop(rocks(&source));
    let large = vec![7; 100_000];
    for prefix in [[0xC1; 32], [0xC2; 32], [0xC3; 32]] {
        raw(&source, prefix, 'a', b"an aux record", Some(&large));
    }
    let (few, more) = (
        commits(&source, "records", 64 << 20),
        commits(&source, "records", 250_000),
    );
    assert!(
        more >= few + 3,
        "{few} commits of 64 MiB, {more} of 250,000 bytes"
    );
}

/// The database of a work directory is held apart from the source as the
/// target is: a work directory of the command's whose `database` leads
/// into the source is refused before anything is opened there.
#[test]
fn a_database_that_leads_into_the_source_is_refused() {
    let dir = tempfile::tempdir().expect("a directory");
    let (source, target) = (dir.path().join("rocksdb"), dir.path().join("lignindb"));
    small(&source);
    let inside = source.join("extra");
    std::fs::create_dir(&inside).expect("a directory in the source");
    let work = work_dir(&target).unwrap();
    std::fs::create_dir(&work).expect("the work directory");
    std::fs::write(work.join(WORK_MARK), MARK_TEXT).expect("its file");
    std::os::unix::fs::symlink(&inside, work.join(WORK_DATABASE)).expect("a link");
    let (outcome, out) = ran(&migration(&source, &target));
    let refused = outcome.expect_err("the database would be made in the source");
    assert!(refused.contains("one inside the other"), "{refused}\n{out}");
    assert_eq!(held(&inside), Vec::<String>::new());
    assert!(!target.exists());
}

/// A source that holds what a migration does not carry is refused by the
/// survey, with what was found, before anything is made.
#[test]
fn a_source_that_is_not_carried_is_refused_before_anything_is_made() {
    let dir = tempfile::tempdir().expect("a directory");
    let namespace = *grovedb_storage::flat::pending_prefix_drops_namespace();
    let a = prefix_of(&[b"a"]);
    let e = prefix_of(&[b"e"]);
    type Write<'a> = ([u8; 32], char, &'a [u8], Option<&'a [u8]>);
    let x: Option<&[u8]> = Some(b"x");
    let faults: [(&str, &[Write<'_>], &str); 8] = [
        (
            "roots",
            &[(a, 'r', b"other", x)],
            "is no record of a root key",
        ),
        (
            "namespace",
            &[(namespace, 'd', b"k", x)],
            "where GroveDB writes none",
        ),
        ("pending", &[(namespace, 'm', b"k", x)], "is none"),
        ("node", &[(a, 'd', &[b'k', 3], x)], "is no node of a Merk"),
        // A tree whose element names a root: with no record under its
        // prefix, and with a record there that is not its root.
        ("dangling", &[(e, 'd', b"only", None)], "names a root"),
        (
            "root",
            &[(e, 'd', b"only", None), (e, 'a', b"k", x)],
            "its element names the root key",
        ),
        (
            "journal",
            &[([0; 32], 'a', b"lignin_migration_in_progress", x)],
            "its journal",
        ),
        (
            "restore",
            &[([0; 32], 'a', b"grovedb_state_sync_restore_in_progress", x)],
            "a state-sync restore that did not finish",
        ),
    ];
    for (name, writes, said) in faults {
        let (source, target) = (dir.path().join(name), dir.path().join(format!("{name}-to")));
        small(&source);
        for (prefix, keyspace, key, value) in writes {
            raw(&source, *prefix, *keyspace, key, *value);
        }
        for target in [None, Some(target)] {
            let refusing = Migration {
                target: target.clone(),
                unreachable: Unreachable::Carry,
                ..migration(&source, Path::new("unused"))
            };
            let (outcome, out) = ran(&refusing);
            let refused = outcome.expect_err("the source is refused");
            assert!(
                refused.contains("holds what a migration does not carry"),
                "{name}: {refused}"
            );
            let line = out
                .lines()
                .find(|line| line.contains("NOT CARRIED") && line.contains(said));
            assert!(line.is_some(), "{name}: no line of {said:?} in:\n{out}");
            if let Some(target) = target {
                assert!(!target.exists() && !work_dir(&target).unwrap().exists());
            }
        }
    }
}

/// The names of the variables of
/// [`a_database_of_the_environment_is_migrated_and_read_alike`].
const REAL_SOURCE: &str = "LIGNIN_MIGRATE_SOURCE";
const REAL_VERSION: &str = "LIGNIN_MIGRATE_GROVE_VERSION";
const REAL_UNREACHABLE: &str = "LIGNIN_MIGRATE_UNREACHABLE";
const REAL_TARGET: &str = "LIGNIN_MIGRATE_TARGET";

/// A RocksDB GroveDB that the environment names is migrated, and the two
/// groves are then read through the seam: what both store is listed and
/// compared, `verify_grovedb` reports the same on both, and its issues
/// are the hashes the physical checker names with the trees that have no
/// node.
///
/// | Variable | |
/// | --- | --- |
/// | `LIGNIN_MIGRATE_SOURCE` | the directory of the RocksDB GroveDB. **A copy**: the migration writes nothing there, and the test then opens it as a node does, which does |
/// | `LIGNIN_MIGRATE_GROVE_VERSION` | the grove version its chain is on, 1 to 5 |
/// | `LIGNIN_MIGRATE_UNREACHABLE` | `refuse` unless given: `leave` or `carry` |
/// | `LIGNIN_MIGRATE_TARGET` | where the LigninDB database is made: a path that is not there. A temporary directory unless given |
#[test]
#[ignore = "takes a RocksDB GroveDB that the environment names"]
fn a_database_of_the_environment_is_migrated_and_read_alike() {
    let variable = |name: &str| std::env::var(name).ok();
    let source = variable(REAL_SOURCE).expect("LIGNIN_MIGRATE_SOURCE names a directory");
    let source = Path::new(&source);
    let number = variable(REAL_VERSION).expect("LIGNIN_MIGRATE_GROVE_VERSION is given");
    let number: usize = number.parse().expect("a grove version");
    let version = &GROVE_VERSIONS[number - 1];
    let unreachable = variable(REAL_UNREACHABLE).map_or(Unreachable::Refuse, |name| {
        Unreachable::named(&name).expect("refuse, leave or carry")
    });
    let scratch = tempfile::tempdir().expect("a directory");
    let target = variable(REAL_TARGET).map_or(scratch.path().join("lignindb"), Into::into);

    let listing = |dir: &Path| -> Vec<(OsString, u64, SystemTime)> {
        let mut listing = Vec::new();
        for entry in std::fs::read_dir(dir).expect("a directory") {
            let entry = entry.expect("an entry");
            let metadata = entry.metadata().expect("its metadata");
            let modified = metadata.modified().expect("its time");
            listing.push((entry.file_name(), metadata.len(), modified));
        }
        listing.sort();
        listing
    };
    let before = listing(source);
    let migration = Migration {
        source: source.to_path_buf(),
        target: Some(target.clone()),
        options: Options::default(),
        // What a commit of the command takes unless it is told.
        batch_bytes: lignin_tools::args::DEFAULT_MIGRATE_BATCH,
        progress: Duration::from_secs(5),
        unreachable,
        memory_limit: None,
    };
    let (outcome, out) = ran(&migration);
    for line in out.lines().filter(|line| line.starts_with("migrate:")) {
        println!("{line}");
    }
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    assert!(
        listing(source) == before,
        "the source's directory is as it was"
    );

    let read = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, || {
                let rocks = rocks(source);
                let lignin = Lignin::open_with_engine(&target).expect("the target");
                let listed = same_stored_and_read(&rocks, &lignin, version, "after the migration");
                let (listed, (flat_trees, flat_answers)) =
                    listed.expect("the groves store the same");
                println!(
                    "trees that keep flat records: {flat_trees}, asked through their own calls \
                     on both engines: {flat_answers} answers alike"
                );
                let checked =
                    |issues: Result<_, grovedb::Error>| issues.map_err(|error| error.to_string());
                let was = checked(rocks.verify_grovedb(None, true, false, version));
                let is = checked(lignin.verify_grovedb(None, true, false, version));
                assert_eq!(was, is, "verify_grovedb");
                let issues = was.expect("verify_grovedb answers");
                let emptied = issues.values().filter(|(root, ..)| *root == [0; 32]);
                let roots = (
                    rocks
                        .root_hash(None, version)
                        .unwrap()
                        .expect("a root hash"),
                    lignin
                        .root_hash(None, version)
                        .unwrap()
                        .expect("a root hash"),
                );
                assert_eq!(roots.0, roots.1);
                (listed, issues.len() as u64, emptied.count() as u64, roots.0)
            })
            .expect("a thread")
            .join()
    });
    let (listed, issues, emptied, root) = read.expect("the groves are read");
    let named = word(&out, "migrate: verify", "left_hashes");
    println!(
        "read alike through the seam: {listed} records listed on both, root hash {}; \
         verify_grovedb reports {issues} issues on both, {emptied} of them of trees with no \
         node; the physical checker names {named} hashes a delete left",
        hex::encode(root)
    );
    assert_eq!(issues, named + emptied);
}

/// Puts the database at `target` back where a run leaves it that ended
/// before the rename: in the work directory of a migration to `target`,
/// which has the file that marks it. Returns where the database is.
fn unnamed(target: &Path) -> std::path::PathBuf {
    let work = work_dir(target).unwrap();
    std::fs::create_dir(&work).expect("the work directory");
    std::fs::write(work.join(WORK_MARK), MARK_TEXT).expect("its mark");
    let database = work.join(WORK_DATABASE);
    std::fs::rename(target, &database).expect("the database");
    database
}

/// Writes `batch` into the store in `dir`, beside GroveDB.
fn written(dir: &Path, batch: &Batch<'_>) {
    let options = Options {
        create_if_missing: false,
        ..Options::default()
    };
    let db = Db::open(dir, &options).expect("the store");
    db.unchecked_at_close();
    let transaction = db.begin();
    {
        let scope = transaction.scope().expect("a scope");
        (scope.apply_by_kind(batch, keeps_flat_records)).expect("the write is taken");
        scope.merge();
    }
    (db.commit(&transaction, &mut NoStaleHashes)).expect("a commit");
    drop(transaction);
    db.close().expect("a close");
}

/// What a target holds that its source does not is found by the
/// comparison, also where it changes neither the root hash nor how many
/// subtrees the directory has: a record beside the grove's own in each
/// keyspace, a mark with a value, and a subtree that nothing hosts.
#[test]
fn what_a_target_holds_beside_what_its_source_holds_is_found() {
    let dir = tempfile::tempdir().expect("a directory");
    let source = dir.path().join("rocksdb");
    small(&source);
    let namespace = Subtree::new(*pending_prefix_drops_namespace());
    let a = Subtree::new(prefix_of(&[b"a"]));
    let mark = left_behind_key(MerkKeyspace::Data, &prefix_of(&[b"d"])).expect("a mark");
    let kept = [[0x55; 32].as_slice(), b"r"].concat();
    let normal = tree_type_bytes(TreeType::NormalTree);
    let put = RecordWrite::put;
    let cases: [(&str, Vec<RecordWrite<'_>>, &str); 6] = [
        (
            "aux",
            vec![put(Column::Aux, a, b"more", b"x")],
            "aux keyspace",
        ),
        (
            "meta",
            vec![put(Column::Meta, a, b"more", b"x")],
            "meta keyspace",
        ),
        (
            "mark",
            vec![put(Column::Aux, namespace, &mark, b"x")],
            "a mark has a value",
        ),
        (
            "roots",
            vec![put(Column::Roots, a, b"r", b"k")],
            "the roots keyspace of the subtree",
        ),
        (
            "kept",
            vec![put(Column::Roots, namespace, &kept, b"k")],
            "records of root keys",
        ),
        ("subtree", Vec::new(), "subtrees in use"),
    ];
    for (name, records, said) in cases {
        let target = dir.path().join(name);
        let (outcome, out) = ran(&migration(&source, &target));
        assert_eq!(outcome, Ok(Outcome::Done), "{out}");
        assert_eq!(word(&out, "migrate: compared", "marks"), 1, "{out}");
        // As a run leaves it that ended before the rename.
        let work = work_dir(&target).unwrap();
        let database = unnamed(&target);
        let entered = [SubtreeWrite::Enter {
            subtree: Subtree::new([0xEE; 32]),
            tree_type: &normal,
            host: None,
        }];
        let batch = Batch {
            records: &records,
            subtrees: match name {
                "subtree" => &entered[..],
                _ => &[],
            },
            ..Batch::default()
        };
        written(&database, &batch);
        let (outcome, out) = ran(&migration(&source, &target));
        assert_eq!(outcome, Ok(Outcome::NotSound), "{name}:\n{out}");
        let line = out.lines().find(|line| line.contains("DIFFERS:"));
        let line = line.unwrap_or_else(|| panic!("{name}: no difference in:\n{out}"));
        assert!(line.contains(said), "{name}: {line}");
        assert!(work.exists() && !target.exists());
    }
}

/// A grove as deep as the walk follows is migrated on a thread with the
/// stack a thread has unless it asks for more, and one that is a tree
/// deeper is refused in words before anything is made.
#[test]
fn a_grove_deeper_than_the_walk_follows_is_refused() {
    const DEEPEST: usize = 256;
    let dir = tempfile::tempdir().expect("a directory");
    let source = dir.path().join("rocksdb");
    let deeper = |from: usize, to: usize| {
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(STACK_BYTES)
                .spawn_scoped(scope, || {
                    let db = rocks(&source);
                    for depth in from..to {
                        let path: Vec<&[u8]> = vec![b"t".as_slice(); depth];
                        put(&db, &path, b"t", Element::empty_tree());
                    }
                    let path: Vec<&[u8]> = vec![b"t".as_slice(); to];
                    put(&db, &path, b"item", Element::new_item(vec![to as u8]));
                })
                .expect("a thread")
                .join()
                .expect("the grove is made");
        });
    };
    deeper(0, DEEPEST);
    let target = dir.path().join("lignindb");
    let (outcome, out) = ran(&migration(&source, &target));
    assert_eq!(outcome, Ok(Outcome::Done), "{out}");
    assert_eq!(
        word(&out, "migrate: survey", "subtrees"),
        DEEPEST as u64 + 1,
        "{out}"
    );

    deeper(DEEPEST, DEEPEST + 1);
    let target = dir.path().join("deeper");
    let (outcome, out) = ran(&migration(&source, &target));
    let refused = outcome.expect_err("a grove that is too deep is refused");
    assert!(
        refused.contains("more than 256 trees deep"),
        "{refused}\n{out}"
    );
    assert!(!target.exists() && !work_dir(&target).unwrap().exists());
}
