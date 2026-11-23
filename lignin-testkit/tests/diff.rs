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
        let mut shape = Shape::default()
            .for_grove_version(number as u32)
            .expect("a grove version");
        // GroveDB asserts in a debug build what a tree made again at a
        // dropped path before the drain of its drop breaks, and the
        // reference panics there: a debug run on a grove version with
        // flat drops keeps the contract of their caller (D-348, D-352).
        shape.keeps_the_drop_contract = cfg!(debug_assertions) && number >= 4;
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
/// store these cases alike to their last operation, with no record
/// passed over for a named open difference. In each a batch that moves
/// is taken by both engines.
///
/// The rule of the walk is held by the tests of `diff::state`. This holds
/// that the two cases, as their seeds generate them, run alike to their
/// end.
#[test]
fn two_cases_reported_for_a_column_shown_twice_run_alike_to_their_end() {
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
        let taken = |step: &usize| moves(&case.ops[*step]) && !ran.failed_steps.contains(step);
        let taken = (0..case.ops.len()).filter(taken).count();
        assert!(taken > 0, "{seed:#x}: no batch that moves was taken");
        let families = ran.tally.families;
        let passed_over = (families.stored_differences(), families.answer_differences());
        assert_eq!(passed_over, (0, 0), "{seed:#x}: {:?}", families.lines());
    }
}

/// A case the fuzzer reported on grove version 4: a tree is made again at
/// the path of a tree that was dropped flat while the record of the drop
/// is pending, and a query of the new tree with no subquery reads the old
/// tree's elements on RocksDB, which keeps them until the drain, and the
/// new tree's alone on LigninDB. It is the open difference of a dropped
/// tree until its drain: the case ends at that answer, in which RocksDB
/// returns the new tree's element and three of the old tree and LigninDB
/// the new tree's element alone, counted for the family, and is no
/// divergence.
#[test]
fn a_tree_made_again_before_the_drain_of_its_flat_drop_ends_the_case_for_its_family() {
    use lignin_testkit::{
        diff::{run, Case, Family, Part, Stored},
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
    let ended = ran.ended_with.as_ref().expect("the answers it ended with");
    assert_eq!(ended.step as u64 + 1, ran.tally.steps);
    assert!(ended.parts.contains(&Part::Result), "{ended:?}");
    assert_eq!(ended.step, 113, "{ended:?}");
    let remade: [&[u8]; 2] = [b"drops", b"round-1:provable-count-provable-sum"];
    assert_eq!(query.path, remade);
    // The rows an engine returned, each with its path, its key and the
    // bytes of its element: the element of the new tree, and on RocksDB
    // the three of the old one after it.
    let rows = |lines: &[String]| -> Vec<String> {
        let rows = lines.iter().filter(|line| line.starts_with("row "));
        rows.cloned().collect()
    };
    let hex = |bytes: &[u8]| -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() };
    let row = |key: &[u8], element: &str| {
        let (above, tree) = (hex(remade[0]), hex(remade[1]));
        format!("row {above}/{tree} {} {element}", hex(key))
    };
    let again = row(b"again", "0008f9de85776181375e00");
    let kept = [
        row(b"r0", "03fc000e9f0900"),
        row(b"r1", "03fc00093b0000"),
        row(b"r2", "03fc000d44a400"),
    ];
    assert_eq!(rows(&ended.reference)[0], again);
    assert_eq!(rows(&ended.reference)[1..], kept);
    assert_eq!(rows(&ended.candidate), [again]);
    let families = ran.tally.families;
    assert_eq!(families.answers, [0, 0, 1, 0, 0, 0]);
    assert_eq!(families.answer_seed[2], Some(case.seed));
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
        diff::{run, Case, Family, Part, Stored},
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
        let ended = ran.ended_with.as_ref().expect("the answers it ended with");
        assert_eq!(ended.step as u64, at, "{seed:#x}");
        assert!(ended.parts.contains(&Part::Error), "{seed:#x}: {ended:?}");
        // RocksDB took the batch, and LigninDB refused it.
        let failed = |lines: &[String]| {
            let mut failed = lines.iter().filter(|line| line.starts_with("failed "));
            failed.next().cloned()
        };
        let rooted = |line: &String| line.starts_with("root ");
        assert_eq!(failed(&ended.reference), None, "{seed:#x}: {ended:?}");
        assert!(ended.reference.iter().any(rooted), "{seed:#x}: {ended:?}");
        let refused = Some("failed NotSupported".to_string());
        assert_eq!(failed(&ended.candidate), refused, "{seed:#x}: {ended:?}");
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

/// A case the fuzzer reported on grove version 5: a commitment tree is
/// dropped flat and made again by a repeated batch before the drain of
/// its drop, which skips its record, and a raw query of the tree above
/// with no subquery returns it. A proof binds such a tree to its own
/// records, where RocksDB reads those it kept of the dropped tree.
///
/// In a release build RocksDB's proof is refused by its own verifier,
/// LigninDB's verifies and gives the rows both returned, and the case
/// ends there, counted apart (D-348). In a debug build GroveDB's
/// assertion fails on RocksDB and the reference panics, which is a
/// divergence of the run; with the switch that keeps the contract of a
/// flat drop's caller the tree is not made again, and the case of the
/// seed has no divergence.
#[test]
fn a_proof_of_a_tree_made_again_before_the_drain_ends_the_case_counted_apart() {
    use lignin_testkit::{
        diff::{run, Case, Family, Part, Stored},
        golden::run::STACK_BYTES,
        workload::{Op, Read},
    };

    let scratch = scratch();
    let on_both = |case: &Case| {
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .stack_size(STACK_BYTES)
                .spawn_scoped(scope, || {
                    run(
                        case,
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
        .expect("the run")
    };
    let shape = Shape::default()
        .for_grove_version(5)
        .expect("a grove version");
    let seed = 0x3d7d_3d53_98d4_88dc;
    let case = Case::generate(seed, &shape);
    let ran = on_both(&case);
    let step = 1552;
    let Op::Read(Read::Query(query)) = &case.ops[step] else {
        panic!("step {step} is {:?}", case.ops[step]);
    };
    let above: [&[u8]; 1] = [b"drops"];
    assert_eq!(query.path, above);
    assert!(query.raw && query.subquery_path.is_none() && !query.subquery_all);
    let hex = |bytes: &[u8]| -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() };
    let made_again = hex(b"round-0:commitment");
    let has = |lines: &[String], text: &str| lines.iter().any(|line| line.contains(text));

    if cfg!(debug_assertions) {
        // The reference panics at GroveDB's assertion for that tree: a
        // divergence, of no family.
        let divergence = ran.divergence.as_ref().expect("a divergence");
        assert_eq!(divergence.step, step);
        assert_eq!(divergence.parts, [Part::Panic]);
        let origin = "/operations/proof/bind_terminal_non_merk_tree/";
        let asserted = format!("non-Merk tree at key {made_again} has state root ");
        let panicked = |line: &String| line.contains(origin) && line.contains(&asserted);
        assert!(divergence.reference.iter().any(panicked), "{divergence:?}");
        assert_eq!(ran.ended_at, None);
        assert_eq!(ran.tally.families.answers, [0; 6]);

        // With the contract kept, the case of the seed has no divergence.
        let mut keeping = shape.clone();
        keeping.keeps_the_drop_contract = true;
        let kept = Case::generate(seed, &keeping);
        assert!(kept.ops.len() < case.ops.len());
        let ran = on_both(&kept);
        if let Some(divergence) = &ran.divergence {
            panic!(
                "{}",
                divergence.report(&kept, "RocksDB", "LigninDB").join("\n")
            );
        }
        assert_eq!(ran.cut_short, None);
        assert_ne!(ran.ended_at, Some(Family::DroppedTreeProof));
        return;
    }

    if let Some(divergence) = &ran.divergence {
        panic!(
            "{}",
            divergence.report(&case, "RocksDB", "LigninDB").join("\n")
        );
    }
    let ended = ran.ended_with.as_ref().expect("the answers it ended with");
    assert_eq!(ended.step, step, "{ended:?}");
    assert_eq!(ended.parts, [Part::Proof]);
    // LigninDB returned the tree that was made again, its proof verifies,
    // RocksDB's is refused, and both returned the same rows.
    let row = format!("row {} {made_again} ", hex(b"drops"));
    let returned = |line: &String| line.starts_with(&row);
    assert!(ended.candidate.iter().any(returned), "{ended:?}");
    assert!(has(&ended.candidate, "verification verified"), "{ended:?}");
    assert!(has(&ended.reference, "verification refused"), "{ended:?}");
    let rows = |lines: &[String]| -> Vec<String> {
        let rows = lines.iter().filter(|line| line.starts_with("row "));
        rows.cloned().collect()
    };
    assert_eq!(rows(&ended.reference), rows(&ended.candidate));
    assert_eq!(ran.ended_at, Some(Family::DroppedTreeProof));
    let families = ran.tally.families;
    assert_eq!(families.answers, [0, 0, 0, 0, 1, 0]);
    assert_eq!(families.answer_seed[4], Some(case.seed));
}

/// A case the fuzzer reported on grove version 4: a batch of inserts that
/// their caller knows not to replace, repeated by the fuzzer, puts an
/// empty indexed tree at a key where an indexed tree with rows stands.
/// GroveDB reads no old element for such an insert and removes no row,
/// so RocksDB takes the batch, and LigninDB refuses it as not supported.
/// It is the open difference of an indexed tree over an indexed tree by
/// an insert known not to replace: the case ends at that answer, counted
/// apart, and is no divergence (D-354).
#[test]
fn an_insert_known_not_to_replace_over_an_indexed_tree_ends_the_case_counted_apart() {
    use lignin_testkit::{
        diff::{run, Case, Family, Part, Stored},
        golden::run::STACK_BYTES,
        workload::{BatchOp, Op},
    };

    let scratch = scratch();
    let shape = Shape::default()
        .for_grove_version(4)
        .expect("a grove version");
    let seed = 0xb444_9622_848f_067e;
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
            "{}",
            divergence.report(&case, "RocksDB", "LigninDB").join("\n")
        );
    }
    assert_eq!(ran.ended_at, Some(Family::InsertOnlyOverIndexed));
    let ended = ran.ended_with.as_ref().expect("the answers it ended with");
    assert_eq!(ended.step, 628, "{ended:?}");
    // The batch lists an insert known not to replace of an indexed tree
    // under the key `tag`.
    let Op::Batch(ops) = &case.ops[ended.step] else {
        panic!("the case ended at {:?}", case.ops[ended.step]);
    };
    let puts_the_tree = |op: &BatchOp| {
        matches!(op, BatchOp::InsertOnly { key, element, .. }
            if key.as_slice() == b"tag" && element.is_indexed_tree())
    };
    assert!(ops.iter().any(puts_the_tree), "{ops:?}");
    // RocksDB took the batch, and LigninDB refused it in the words of
    // that refusal for that key.
    assert!(ended.parts.contains(&Part::Error), "{ended:?}");
    let failed = |lines: &[String]| {
        let mut failed = lines.iter().filter(|line| line.starts_with("failed "));
        failed.next().cloned()
    };
    assert_eq!(failed(&ended.reference), None, "{ended:?}");
    assert!(ended.reference.iter().any(|line| line.starts_with("root ")));
    let refused = Some("failed NotSupported".to_string());
    assert_eq!(failed(&ended.candidate), refused, "{ended:?}");
    let words = "# failed: not supported: lignindb: an empty indexed tree written under the key \
                 746167 over an indexed tree, a secondary of which holds rows that the batch does \
                 not remove";
    assert!(
        ended.candidate.iter().any(|line| line == words),
        "{ended:?}"
    );
    let families = ran.tally.families;
    assert_eq!(families.answers, [0, 0, 0, 0, 0, 1]);
    assert_eq!(families.histories[5], 1);
    assert_eq!(families.answer_seed[5], Some(seed));
}
