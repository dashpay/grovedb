//! The acceptance tests of the differential fuzzer (LigninDB task VER-01,
//! `lignin_testkit::diff`), for as long as a test may take:
//!
//! - **It finds a seeded one-byte cost bug within one minute.** A defect is
//!   planted in the stand-in engine that charges one byte too many for one
//!   size of write and changes nothing else. The fuzzer has a minute.
//! - **It is clean on the engines as they are.** The candidate answers a
//!   few dozen cases as the reference does.
//!
//! The run of an hour is the command's: `lignin-diff fuzz --duration 3600`.

// The fuzzer compares two engines through the `Engine` seam, which the
// engine the golden corpus is pinned to does not have (`build.rs`).
#![cfg(grovedb_engine_seam)]

use std::time::{Duration, Instant};

use grovedb_version::version::GROVE_VERSIONS;

use lignin_testkit::diff::{
    cli::SEEDED_FAULT, fuzz, Budget, Candidate, Fault, Part, Reference, Settings, Shape, Side,
    StandIn,
};

const A_MINUTE: Duration = Duration::from_secs(60);

/// How many cases the candidate is given on each grove version.
const CASES_OF_A_VERSION: u64 = 8;

/// A directory for the databases, in the temporary directory: whoever runs
/// the tests says with `TMPDIR` where that is, and the minute is measured
/// on what is there.
fn scratch() -> tempfile::TempDir {
    tempfile::tempdir().expect("a directory")
}

fn settings(scratch: &tempfile::TempDir) -> Settings {
    Settings {
        scratch: scratch.path().to_path_buf(),
        ..Settings::default()
    }
}

/// The line of a step's values that holds its cost.
fn cost_of(lines: &[String]) -> &str {
    lines
        .iter()
        .find(|line| line.starts_with("cost "))
        .expect("a step has a cost")
}

/// The fields in which two cost lines differ: the name of each, and by how
/// much the candidate's number is above the reference's.
fn fields_apart(reference: &str, candidate: &str) -> Vec<(String, i128)> {
    let fields = |line: &str| -> Vec<(String, String)> {
        line.split_whitespace()
            .filter_map(|field| field.split_once('='))
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    };
    let (reference, candidate) = (fields(reference), fields(candidate));
    assert_eq!(reference.len(), candidate.len());
    reference
        .into_iter()
        .zip(candidate)
        .filter(|(was, is)| was != is)
        .map(|((name, was), (_, is))| {
            let number = |value: &str| value.parse::<i128>().unwrap_or(i128::MIN / 2);
            (name, number(&is) - number(&was))
        })
        .collect()
}

#[test]
fn a_seeded_one_byte_cost_bug_is_found_within_a_minute() {
    // Two defects, each in about one write of 4,500: of the 185,138 writes
    // of one run of 240 cases, 40 added 71 bytes, and 41 loaded 413 bytes
    // for the records they delete without a cost, which are read to learn
    // what the deletes free.
    let faults = [
        (SEEDED_FAULT, "added"),
        (Fault::LoadedByte { when_loaded: 413 }, "loaded"),
    ];
    for (fault, field) in faults {
        let scratch = scratch();
        let started = Instant::now();
        let summary = fuzz(
            &Side::<Reference>::of("reference"),
            &StandIn::side(fault),
            &Settings {
                duration: Some(A_MINUTE),
                // The minute is for finding the defect. The shrinking after
                // it takes the time it takes.
                shrink: Some(Budget {
                    time: Duration::MAX,
                    ..Budget::default()
                }),
                ..settings(&scratch)
            },
            &mut |_| {},
        )
        .expect("the run");
        let finding = summary
            .finding
            .unwrap_or_else(|| panic!("{fault} was not found within a minute"));
        assert_eq!(finding.not_shrunk, None);
        let shrunk = finding.shrunk.as_ref().expect("the stream is shrunk");
        println!(
            "{fault}: found after {:.1} s in case {} at step {} of {}; {} steps were compared \
             before it; shrunk to {} operations in {} runs and {:.1} s; {:.1} s in all",
            finding.after.as_secs_f64(),
            finding.index,
            finding.divergence.step,
            finding.case.ops.len(),
            summary.progress.tally.steps,
            shrunk.case.ops.len(),
            shrunk.runs,
            shrunk.took.as_secs_f64(),
            started.elapsed().as_secs_f64(),
        );
        assert!(finding.after < A_MINUTE, "found after {:?}", finding.after);
        // A cost and nothing else: the root hash, the result and the proofs
        // of the step are the same. And of the cost one field, by one byte.
        for divergence in [&finding.divergence, &shrunk.divergence] {
            assert_eq!(divergence.parts, [Part::Cost]);
            let apart = fields_apart(
                cost_of(&divergence.reference),
                cost_of(&divergence.candidate),
            );
            assert_eq!(apart, [(field.to_string(), 1)]);
        }
        // And the stream is shrunk to the few operations that show it.
        assert!(shrunk.reproduced && shrunk.minimal);
        assert!(shrunk.case.ops.len() <= 16, "{:?}", shrunk.case.ops);
        assert!(shrunk.case.ops.len() <= finding.divergence.step + 1);
    }
}

#[test]
fn the_candidate_answers_as_the_reference_does() {
    let scratch = scratch();
    let (mut cases, mut steps, mut proofs, mut walks) = (0, 0, 0, 0);
    // A run of each grove version by itself, so that a case it reports is
    // the case `lignin-diff case --seed SEED --grove-version N` runs again.
    for number in 1..=GROVE_VERSIONS.len() {
        let shape = Shape::default()
            .for_grove_version(number as u32)
            .expect("a grove version");
        let summary = fuzz(
            &Side::<Reference>::of("reference"),
            &Side::<Candidate>::of("candidate"),
            &Settings {
                cases: Some(CASES_OF_A_VERSION),
                shape,
                ..settings(&scratch)
            },
            &mut |_| {},
        )
        .expect("the run");
        let tally = summary.progress.tally;
        println!(
            "grove version {number}: {} cases, {} steps, {} operations failed alike, {} proofs, \
             {} comparisons of {} stored records, {} cases cut short, in {:.1} s",
            summary.progress.cases,
            tally.steps,
            tally.failed,
            tally.proofs,
            tally.walks,
            tally.records,
            tally.cut_short,
            summary.progress.elapsed.as_secs_f64()
        );
        if let Some(finding) = &summary.finding {
            let shrunk = finding.shrunk.as_ref().map_or_else(
                || format!("not shrunk: {:?}", finding.not_shrunk),
                |shrunk| {
                    let ops: Vec<String> = (shrunk.case.ops.iter().enumerate())
                        .map(|(at, op)| format!("  {at}: {op:?}"))
                        .collect();
                    format!(
                        "shrunk to {} operations:\n{}\n{}",
                        ops.len(),
                        ops.join("\n"),
                        shrunk
                            .divergence
                            .report(&shrunk.case, "reference", "candidate")
                            .join("\n")
                    )
                },
            );
            panic!(
                "the engines differ on grove version {number} in case {} (lignin-diff case --seed \
                 {:#x} --grove-version {number}):\n{}\n{shrunk}",
                finding.index,
                finding.case.seed,
                finding
                    .divergence
                    .report(&finding.case, "reference", "candidate")
                    .join("\n")
            );
        }
        assert_eq!(summary.progress.cases, CASES_OF_A_VERSION);
        assert_eq!(summary.first_cut_short, None);
        cases += summary.progress.cases;
        steps += tally.steps;
        proofs += tally.proofs;
        walks += tally.walks;
    }
    assert!(steps > 15_000, "{steps} steps");
    assert!(proofs > 500, "{proofs} proofs");
    assert!(walks > cases, "{walks} walks");
}

/// Two cases the fuzzer reported on grove version 4, in which a batch
/// moves a tree to the path a private-document store was moved away from,
/// or the other way round. RocksDB keeps the nodes of a Merk and the
/// records of such a store in one column of a subtree, so the walk of what
/// is stored saw the tree that lives at the path a second time, as
/// records left behind under the other kind of data, and LigninDB, which
/// keeps the two apart, showed none. The walk lists such a record once
/// (`diff::state`, "One column shown twice"), and the engines answer and
/// store these cases alike to their last operation.
#[test]
fn a_tree_moved_to_the_path_of_a_tree_of_the_other_kind_is_listed_once() {
    use lignin_testkit::{
        diff::{run, Case, Stored},
        golden::run::STACK_BYTES,
    };

    let scratch = scratch();
    let shape = Shape::default()
        .for_grove_version(4)
        .expect("a grove version");
    for seed in [0x6eb9_0601_2079_bb8a_u64, 0x1aeb_7a21_f504_221b] {
        let case = Case::generate(seed, &shape);
        let moves = |op: &lignin_testkit::workload::Op| {
            op.batch_ops()
                .any(|op| matches!(op, lignin_testkit::workload::BatchOp::Move { .. }))
        };
        assert!(case.ops.iter().any(moves), "the case of {seed:#x} moves");
        let ran = std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(STACK_BYTES)
                .spawn_scoped(scope, || {
                    run(
                        &case,
                        &Side::<Reference>::of("RocksDB"),
                        &Side::<Candidate>::of("LigninDB"),
                        scratch.path(),
                        Stored::Every(1),
                        &|| false,
                    )
                })
                .expect("a thread")
                .join()
                .expect("the case runs")
        })
        .expect("the run");
        if let Some(divergence) = &ran.divergence {
            panic!(
                "the case of {seed:#x}:\n{}",
                divergence.report(&case, "RocksDB", "LigninDB").join("\n")
            );
        }
        assert_eq!((ran.cut_short, ran.ended_at), (None, None), "{seed:#x}");
        assert_eq!(ran.tally.steps, case.ops.len() as u64, "{seed:#x}");
        assert!(ran.tally.walks > 1, "{seed:#x}");
    }
}

/// A case the fuzzer reported on grove version 4: a tree is made again at
/// the path of a tree that was dropped flat while the record of the drop
/// is pending, and a query of the new tree with no subquery reads the old
/// tree's elements on RocksDB, which keeps them until the drain, and the
/// new tree's alone on LigninDB. It is the open difference of a dropped
/// tree until its drain: the case ends at that answer, counted for the
/// family, and is no divergence.
#[test]
fn a_tree_made_again_before_the_drain_of_its_flat_drop_ends_the_case_for_its_family() {
    use lignin_testkit::{
        diff::{run, Case, Family, Stored},
        golden::run::STACK_BYTES,
        workload::{Op, Read},
    };

    let scratch = scratch();
    let shape = Shape::default()
        .for_grove_version(4)
        .expect("a grove version");
    let case = Case::generate(0x9c66_50fd_c2d8_568e, &shape);
    let ran = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, || {
                run(
                    &case,
                    &Side::<Reference>::of("RocksDB"),
                    &Side::<Candidate>::of("LigninDB"),
                    scratch.path(),
                    Stored::Every(32),
                    &|| false,
                )
            })
            .expect("a thread")
            .join()
            .expect("the case runs")
    })
    .expect("the run");
    if let Some(divergence) = &ran.divergence {
        panic!(
            "{}",
            divergence.report(&case, "RocksDB", "LigninDB").join("\n")
        );
    }
    assert_eq!(ran.ended_at, Some(Family::DroppedTree));
    let last = &case.ops[ran.tally.steps as usize - 1];
    let Op::Read(Read::Query(query)) = last else {
        panic!("the case ended at {last:?}");
    };
    assert!(query.subquery_path.is_none() && !query.subquery_all);
    let families = ran.tally.families;
    assert_eq!(families.answers, [0, 0, 1, 0]);
    assert_eq!(families.answer_seed, [None, None, Some(case.seed), None]);
}

/// Two cases the fuzzer reported on grove versions 2 and 3: a batch puts
/// an empty indexed tree at a key that holds an indexed tree with a row,
/// which RocksDB takes and LigninDB refuses as not supported, since those
/// grove versions leave the rows of the old tree where they are. It is
/// the open difference of an indexed tree over an indexed tree: each case
/// ends at that answer, counted for the family, and is no divergence.
#[test]
fn an_indexed_tree_over_an_indexed_tree_ends_the_case_for_its_family() {
    use lignin_testkit::{
        diff::{run, Case, Family, Stored},
        golden::run::STACK_BYTES,
    };

    let scratch = scratch();
    for (version, seed, at) in [
        (2, 0x837d_de9f_e613_4f53_u64, 234),
        (3, 0xfe7c_8837_0e0d_d536, 319),
    ] {
        let shape = Shape::default()
            .for_grove_version(version)
            .expect("a grove version");
        let case = Case::generate(seed, &shape);
        let ran = std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(STACK_BYTES)
                .spawn_scoped(scope, || {
                    run(
                        &case,
                        &Side::<Reference>::of("RocksDB"),
                        &Side::<Candidate>::of("LigninDB"),
                        scratch.path(),
                        Stored::Every(32),
                        &|| false,
                    )
                })
                .expect("a thread")
                .join()
                .expect("the case runs")
        })
        .expect("the run");
        if let Some(divergence) = &ran.divergence {
            panic!(
                "the case of {seed:#x}:\n{}",
                divergence.report(&case, "RocksDB", "LigninDB").join("\n")
            );
        }
        assert_eq!(ran.ended_at, Some(Family::IndexedOverIndexed), "{seed:#x}");
        assert_eq!(ran.tally.steps, at + 1, "{seed:#x}");
        let families = ran.tally.families;
        assert_eq!(families.answers[3], 1, "{seed:#x}");
        assert_eq!(families.answer_seed[3], Some(seed));
        assert_eq!(families.histories[3], 1, "{seed:#x}");
    }
}

#[test]
fn the_fields_of_two_cost_lines_are_compared() {
    let cost = "cost seeks=3 loaded=120 added=185 replaced=40 removed=- hashes=7 sinsemilla=0";
    let more = "cost seeks=3 loaded=120 added=186 replaced=40 removed=- hashes=7 sinsemilla=0";
    let two = "cost seeks=3 loaded=119 added=186 replaced=40 removed=9 hashes=7 sinsemilla=0";
    assert_eq!(fields_apart(cost, more), [("added".to_string(), 1)]);
    assert_eq!(fields_apart(cost, cost), []);
    let apart = fields_apart(cost, two);
    assert_eq!(apart.len(), 3);
    assert_eq!(
        apart[..2],
        [("loaded".to_string(), -1), ("added".to_string(), 1)]
    );
    assert_eq!(apart[2].0, "removed");
    assert_eq!(cost_of(&["root 00".to_string(), cost.to_string()]), cost);
}
