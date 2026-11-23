//! The golden corpus's operation streams on RocksDB and on LigninDB, side
//! by side (LigninDB task GROVE-10).
//!
//! The corpus itself is what RocksDB answered at the pinned commit, and
//! `tests/golden.rs` replays it on RocksDB. Here the streams the corpus was
//! recorded from are generated again and applied to a fresh database of
//! each engine through the fuzzer's pair runner
//! (`lignin_testkit::diff::pair::run`), which compares the result, the
//! error, the cost, the root hash and the proofs of every step, and at the
//! end of a trace what the two databases store.
//!
//! A database of LigninDB is checked by the physical invariant checker when
//! it is closed in a debug build, with Merk's hash rules, and panics on a
//! finding. So a trace that leaves an unsound store fails here although
//! every answer was RocksDB's.
//!
//! The comparison is strict. The pair runner counts the open differences
//! that the fuzzer passes over (`diff::families`); a trace of the corpus
//! that came to one would be a trace that differs, and fails here.

// The pair runner compares two engines through the `Engine` seam, which the
// engine the golden corpus is pinned to does not have (`build.rs`).
#![cfg(grovedb_engine_seam)]

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};

use grovedb::{LigninEngine, RocksDbEngine};
use grovedb_version::version::GROVE_VERSIONS;
use lignin_testkit::{
    diff::{
        case::{Arrangement, Case},
        pair::{self, Side, Stored},
    },
    golden::{plan, run::STACK_BYTES},
    workload::{Op, Write},
};

/// The traces that may differ on this head, by grove version and name, each
/// with the unit that removes the difference: at a conversion of a tree to
/// a big sum tree, which is the difference that unit removes, and at no
/// other operation. A trace that is not listed has to be the same on both
/// engines to its last step, and a listed one that panics, is cut short or
/// cannot be run fails as any other does.
const OPEN: &[(u32, &str, &str)] = &[
    (5, "conversions", "GROVE-04-F1"),
    (5, "suite", "GROVE-04-F1"),
    (5, "legacy-convert-twice", "GROVE-04-F1"),
    (5, "legacy-convert-rows-of-both-signs", "GROVE-04-F1"),
];

/// How a trace ended.
enum Ended {
    /// Both engines answered every step alike, and stored the same.
    Same {
        steps: u64,
        proofs: u64,
        records: u64,
    },
    /// The engines answered a step differently: whether the operation of
    /// the step is a conversion to a big sum tree, and the difference.
    Differs { at_a_conversion: bool, what: String },
    /// What happened instead, as text: a panic, a run that could not be
    /// made or was cut short, or an open difference of the fuzzer's that
    /// the trace came to.
    Other(String),
}

/// One trace on both engines.
fn replayed(case: &Case, scratch: &std::path::Path) -> Ended {
    let run = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, || {
                pair::run(
                    case,
                    &Side::<RocksDbEngine>::of("RocksDB"),
                    &Side::<LigninEngine>::of("LigninDB"),
                    scratch,
                    Stored::AtTheEnd,
                    &|| false,
                )
            })
            .expect("a thread")
            .join()
    });
    match run {
        Err(panic) => {
            let message = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(ToString::to_string))
                .unwrap_or_default();
            Ended::Other(format!("panicked: {message}"))
        }
        Ok(Err(error)) => Ended::Other(format!("could not run: {error:?}")),
        Ok(Ok(run)) => {
            let families = run.tally.families;
            let passed_over = families.stored != [0, 0] || families.answers != [0, 0];
            match (run.divergence, run.cut_short) {
                (Some(divergence), _) => Ended::Differs {
                    at_a_conversion: matches!(
                        case.ops[divergence.step],
                        Op::Write(Write::ConvertToBigSumTree { .. })
                    ),
                    what: format!(
                        "differs at step {} of {} in {:?}: {:?}\n  RocksDB:  {}\n  LigninDB: {}",
                        divergence.step,
                        case.ops.len(),
                        divergence.parts,
                        case.ops[divergence.step],
                        divergence.reference.join("\n            "),
                        divergence.candidate.join("\n            "),
                    ),
                },
                (None, Some(why)) => Ended::Other(format!("cut short {why}")),
                (None, None) if run.ended_at.is_some() || passed_over => Ended::Other(format!(
                    "came to an open difference that the fuzzer passes over: {}",
                    families.lines().join("; ")
                )),
                (None, None) => Ended::Same {
                    steps: run.tally.steps,
                    proofs: run.tally.proofs,
                    records: run.tally.records,
                },
            }
        }
    }
}

#[test]
fn the_corpus_streams_answer_alike_on_rocksdb_and_on_lignindb() {
    let scratch = tempfile::tempdir().expect("a directory");
    let cases: Vec<(u32, String, Case)> = GROVE_VERSIONS
        .iter()
        .flat_map(plan::standard)
        .map(|planned| {
            let spec = planned.spec;
            let ops: Vec<Op> = (spec.segments.iter())
                .flat_map(|segment| segment.source.ops(spec.seed).collect::<Vec<_>>())
                .collect();
            let case = Case {
                seed: spec.seed,
                grove_version: spec.grove_version,
                ops,
                sources: Vec::new(),
                arrangement: Arrangement::Sequential,
                mutations: Vec::new(),
            };
            (spec.grove_version.protocol_version, spec.name, case)
        })
        .collect();
    assert!(!cases.is_empty());
    for (version, name, unit) in OPEN {
        assert!(
            cases.iter().any(|(v, n, _)| v == version && n == name),
            "v{version} {name}, open with {unit}, is no trace of the corpus"
        );
    }

    let next = AtomicUsize::new(0);
    let ended: Mutex<Vec<(usize, Ended)>> = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get().min(8));
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let at = next.fetch_add(1, Ordering::Relaxed);
                let Some((_, _, case)) = cases.get(at) else {
                    break;
                };
                let outcome = replayed(case, scratch.path());
                ended.lock().expect("no worker panics").push((at, outcome));
            });
        }
    });
    let mut ended = ended.into_inner().expect("no worker panics");
    ended.sort_by_key(|(at, _)| *at);
    assert_eq!(ended.len(), cases.len());

    let (mut same, mut steps, mut proofs, mut records) = (0usize, 0u64, 0u64, 0u64);
    let mut open = Vec::new();
    let mut failed = Vec::new();
    for (at, outcome) in ended {
        let (version, name, case) = &cases[at];
        match outcome {
            Ended::Same {
                steps: s,
                proofs: p,
                records: r,
            } => {
                assert_eq!(s, case.ops.len() as u64, "v{version} {name}");
                same += 1;
                steps += s;
                proofs += p;
                records += r;
            }
            Ended::Differs {
                at_a_conversion,
                what,
            } => {
                let line = format!("v{version} {name}: {what}");
                let listed = OPEN.iter().find(|(v, n, _)| v == version && n == name);
                match listed.filter(|_| at_a_conversion) {
                    Some((_, _, unit)) => open.push(format!("{line}\n  open with {unit}")),
                    None => failed.push(line),
                }
            }
            Ended::Other(what) => failed.push(format!("v{version} {name}: {what}")),
        }
    }
    println!(
        "{} traces: {same} the same to the end ({steps} steps, {proofs} proofs, {records} stored \
         records compared), {} differ with an open unit, {} differ otherwise",
        cases.len(),
        open.len(),
        failed.len()
    );
    for line in &open {
        println!("{line}");
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}
