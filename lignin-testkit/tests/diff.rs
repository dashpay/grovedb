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
    let mut shape = Shape::default()
        .for_grove_version(4)
        .expect("a grove version");
    shape.scenes = 0;
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
    assert_eq!(families.answers, [0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(families.answer_seed[2], Some(case.seed));
}

/// A tree is dropped flat and the drop committed. The next block makes
/// the tree again at the dropped path and writes into it, and a drain is
/// called before the block commits. The drain reads committed state,
/// finds no element at the path and reclaims the record of the drop.
/// RocksDB's deletes the range of the dropped prefix beside the block,
/// over the keys the block wrote under it, and answers the block's
/// commit with a conflict; LigninDB dropped the subtree with the commit
/// of the drop, its drain writes no key of the block, and it takes the
/// commit. It is the open difference of a dropped tree until its drain:
/// the case ends at the commit, counted for the family, on grove
/// versions 4 and 5, and is no divergence. Grove versions 1 to 3 have no
/// flat drop: both engines refuse it, and the case runs to its end.
///
/// With the drain after the block's commit, which finds the path live
/// and leaves the record, and with a block that puts the tree's element
/// and writes nothing into the tree, both engines take the commit and
/// the case runs to its end.
#[test]
fn a_commit_that_rocksdb_refuses_after_a_drain_beside_a_tree_made_again_ends_the_case() {
    use lignin_testkit::{
        diff::{Case, Family},
        workload::{BatchOp, Op, Read, Write},
    };

    let path = |segments: &[&[u8]]| -> Vec<Vec<u8>> {
        segments.iter().map(|segment| segment.to_vec()).collect()
    };
    let only = |at: &[&[u8]], key: &[u8], element: grovedb::Element| BatchOp::InsertOnly {
        path: path(at),
        key: key.to_vec(),
        element,
    };
    let tree = grovedb::Element::empty_tree;
    let item = || grovedb::Element::new_item(vec![7; 8]);
    let made = || {
        Op::Batch(vec![
            only(&[b"a"], b"t", tree()),
            only(&[b"a", b"t"], b"k", item()),
        ])
    };
    let read = || {
        Op::Read(Read::Get {
            path: path(&[b"a", b"t"]),
            key: b"k".to_vec(),
        })
    };
    let dropped = [
        Op::Write(Write::Insert {
            path: Vec::new(),
            key: b"a".to_vec(),
            element: tree(),
        }),
        made(),
        Op::Write(Write::DropFlat {
            path: path(&[b"a"]),
            key: b"t".to_vec(),
        }),
        Op::Commit,
    ];
    const DROP: usize = 2;
    const COMMIT: usize = 6;
    let beside = [made(), Op::FlushPendingDrops, Op::Commit, read()];
    let after = [
        made(),
        Op::Commit,
        Op::FlushPendingDrops,
        read(),
        Op::Commit,
    ];
    let element_alone = [
        Op::Batch(vec![only(&[b"a"], b"t", tree())]),
        Op::FlushPendingDrops,
        Op::Commit,
        Op::Read(Read::IsEmptyTree {
            path: path(&[b"a", b"t"]),
        }),
    ];
    let failed = |lines: &[String]| {
        let mut failed = lines.iter().filter(|line| line.starts_with("failed "));
        failed.next().cloned()
    };
    for (number, grove_version) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let ops: Vec<Op> = dropped.iter().chain(&beside).cloned().collect();
        let case = Case::of(43, grove_version, ops);
        let run = ran(&case);
        if let Some(divergence) = &run.divergence {
            let report = divergence.report(&case, "RocksDB", "LigninDB");
            panic!("grove version {number}:\n{}", report.join("\n"));
        }
        if number < 4 {
            assert_eq!(run.failed_steps, [DROP], "grove version {number}");
            assert_eq!(run.ended_at, None, "grove version {number}");
            assert_eq!(run.tally.steps as usize, case.ops.len(), "{number}");
            assert_eq!(run.tally.families.answers, [0; 11], "{number}");
            continue;
        }
        assert_eq!(run.ended_at, Some(Family::DroppedTree), "{number}");
        let ended = run.ended_with.as_ref().expect("the answers it ended with");
        assert_eq!(ended.step, COMMIT, "grove version {number}");
        assert_eq!(run.tally.steps as usize, COMMIT + 1, "{number}");
        assert_eq!(case.ops[COMMIT], Op::Commit);
        assert!(ended.parts.contains(&Part::Error), "{ended:?}");
        let conflict = Some("failed Commit::Conflict".to_string());
        assert_eq!(failed(&ended.reference), conflict, "{ended:?}");
        assert_eq!(failed(&ended.candidate), None, "{ended:?}");
        println!(
            "grove version {number}: the commit differs in {:?}\n  RocksDB: {:?}\n  LigninDB: {:?}",
            ended.parts, ended.reference, ended.candidate
        );
        let families = run.tally.families;
        assert_eq!(families.answers, [0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(families.answer_seed[2], Some(case.seed));

        for (what, block) in [
            ("the drain after", &after[..]),
            ("the element alone", &element_alone[..]),
        ] {
            let ops: Vec<Op> = dropped.iter().chain(block).cloned().collect();
            let case = Case::of(44, grove_version, ops);
            let run = ran(&case);
            if let Some(divergence) = &run.divergence {
                let report = divergence.report(&case, "RocksDB", "LigninDB");
                panic!("{what} on grove version {number}:\n{}", report.join("\n"));
            }
            assert_eq!(run.ended_at, None, "{what} on grove version {number}");
            assert_eq!(
                run.tally.steps as usize,
                case.ops.len(),
                "{what} on {number}"
            );
            assert!(
                run.failed_steps.is_empty(),
                "{what}: {:?}",
                run.failed_steps
            );
            assert_eq!(run.tally.families.answers, [0; 11], "{what} on {number}");
        }
    }
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
        assert_eq!(ran.tally.families.answers, [0; 11]);

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
    assert_eq!(families.answers, [0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0]);
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
    assert_eq!(families.answers, [0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0]);
    assert_eq!(families.histories[5], 1);
    assert_eq!(families.answer_seed[5], Some(seed));
}

/// Runs `case` on RocksDB and on LigninDB, with what they store compared
/// after every write.
fn ran(case: &lignin_testkit::diff::Case) -> lignin_testkit::diff::Run {
    use lignin_testkit::{
        diff::{run, Stored},
        golden::run::STACK_BYTES,
    };

    let scratch = scratch();
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK_BYTES)
            .spawn_scoped(scope, || {
                run(
                    case,
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
    .expect("the run")
}

/// A tree that holds an element is removed by a batch's plain delete,
/// which RocksDB keeps the records of and LigninDB does not.
///
/// The records that differ are passed over and counted, and so is a
/// read of the old key under the tree's path, which ends no case. The
/// delete, the tree made again at the key and the element put under the
/// old key are answered alike, with their root hashes and the proofs of
/// the reads after them, and so is every read once the new tree has
/// written the key. A batch that deletes what the tree holds before the
/// tree makes no place of the family.
#[test]
fn a_read_below_a_plain_delete_of_a_tree_with_an_element_is_counted_and_the_case_goes_on() {
    use lignin_testkit::{
        diff::{Case, Family},
        workload::{BatchOp, Op, Query, QueryItem, Read, Write},
    };

    let path = |segments: &[&[u8]]| -> Vec<Vec<u8>> {
        segments.iter().map(|segment| segment.to_vec()).collect()
    };
    let put = |at: &[&[u8]], key: &[u8], element: grovedb::Element| BatchOp::InsertOrReplace {
        path: path(at),
        key: key.to_vec(),
        element,
    };
    let delete = |at: &[&[u8]], key: &[u8]| BatchOp::Delete {
        path: path(at),
        key: key.to_vec(),
    };
    let has = |at: &[&[u8]], key: &[u8]| {
        Op::Read(Read::HasRaw {
            path: path(at),
            key: key.to_vec(),
        })
    };
    let range = |at: &[&[u8]]| {
        Op::Read(Read::Query(Query {
            path: path(at),
            items: vec![QueryItem::RangeFull],
            subquery_path: None,
            subquery_all: false,
            limit: None,
            left_to_right: true,
            raw: true,
        }))
    };
    let tree = grovedb::Element::empty_tree;
    let held = grovedb::Element::empty_sum_tree;
    let made = || {
        vec![
            Op::Write(Write::Insert {
                path: Vec::new(),
                key: b"p".to_vec(),
                element: tree(),
            }),
            Op::Batch(vec![
                put(&[b"p"], b"t", tree()),
                put(&[b"p", b"t"], &[1], held()),
            ]),
            Op::Commit,
            range(&[b"p", b"t"]),
        ]
    };
    for (number, grove_version) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        // The plain delete alone, and the old key read under the path:
        // the read is passed over, and the tree is made again after it.
        let mut ops = made();
        ops.extend([
            Op::Batch(vec![delete(&[b"p"], b"t")]),
            Op::Commit,
            range(&[b"p"]),
            has(&[b"p", b"t"], &[1]),
            Op::Batch(vec![
                put(&[b"p"], b"t", tree()),
                put(&[b"p", b"t"], &[1], held()),
            ]),
            Op::Commit,
            has(&[b"p", b"t"], &[1]),
            range(&[b"p", b"t"]),
        ]);
        let case = Case::of(7, grove_version, ops);
        let run = ran(&case);
        if let Some(divergence) = &run.divergence {
            let report = divergence.report(&case, "RocksDB", "LigninDB");
            panic!("grove version {number}:\n{}", report.join("\n"));
        }
        assert_eq!(run.ended_at, None, "{number}");
        assert_eq!(run.tally.steps as usize, case.ops.len(), "{number}");
        assert!(run.failed_steps.is_empty(), "{:?}", run.failed_steps);
        assert!(!Family::BelowPlainDelete.ends_a_case());
        let families = run.tally.families;
        assert_eq!(
            families.histories,
            [0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0],
            "{number}"
        );
        assert_eq!(
            families.stored,
            [0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0],
            "{number}"
        );
        assert_eq!(
            families.answers,
            [0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0],
            "{number}"
        );
        assert_eq!(run.tally.closes.checked, 1, "{number}");

        // The tree made again and its key written: every answer alike.
        let mut ops = made();
        ops.extend([
            Op::Batch(vec![delete(&[b"p"], b"t")]),
            Op::Commit,
            range(&[b"p"]),
            Op::Batch(vec![put(&[b"p"], b"t", tree())]),
            Op::Commit,
            range(&[b"p"]),
            Op::Batch(vec![put(&[b"p", b"t"], &[1], held())]),
            Op::Commit,
            has(&[b"p", b"t"], &[1]),
            range(&[b"p", b"t"]),
            range(&[b"p"]),
        ]);
        let case = Case::of(8, grove_version, ops);
        let run = ran(&case);
        assert_eq!(run.divergence, None, "grove version {number}");
        assert_eq!(run.ended_at, None, "grove version {number}");
        assert_eq!(run.tally.steps as usize, case.ops.len(), "{number}");
        let families = run.tally.families;
        assert_eq!(
            families.histories,
            [0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0],
            "{number}"
        );
        assert_eq!(
            families.stored,
            [0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0],
            "{number}"
        );
        assert_eq!(families.answers, [0; 11], "{number}");
        assert!(run.tally.proofs >= 5, "{:?}", run.tally);

        // What the tree holds deleted by the same batch: no place.
        let mut ops = made();
        ops.extend([
            Op::Batch(vec![delete(&[b"p", b"t"], &[1]), delete(&[b"p"], b"t")]),
            Op::Commit,
            has(&[b"p", b"t"], &[1]),
            range(&[b"p"]),
        ]);
        let case = Case::of(9, grove_version, ops);
        let run = ran(&case);
        assert_eq!(run.divergence, None, "grove version {number}");
        assert_eq!(run.ended_at, None, "grove version {number}");
        assert!(!run.failed_steps.contains(&made().len()), "{number}");
        assert_eq!(run.tally.families.histories, [0; 11], "{number}");
    }
}

/// On grove versions 1 to 3 the delete of a tree that holds an element
/// at the root path leaves the root-key record behind where it changes
/// the root of the root Merk. RocksDB takes the next write there and
/// LigninDB refuses it: the case ends at that answer, counted apart,
/// with LigninDB's store checked. A delete that leaves the root where it
/// was is counted as a history and ends no case, and on grove versions
/// 4 and 5 the engines answer alike to the end.
#[test]
fn a_write_after_a_root_key_record_was_left_behind_ends_the_case_counted_apart() {
    use lignin_testkit::{
        diff::{Case, Family},
        workload::{Op, Read, Write},
    };

    let insert = |at: &[&[u8]], key: &[u8], element: grovedb::Element| {
        Op::Write(Write::Insert {
            path: at.iter().map(|segment| segment.to_vec()).collect(),
            key: key.to_vec(),
            element,
        })
    };
    let item = || grovedb::Element::new_item(vec![7]);
    let delete = |key: &[u8]| {
        Op::Write(Write::Delete {
            path: Vec::new(),
            key: key.to_vec(),
        })
    };
    let get = |key: &[u8]| {
        Op::Read(Read::Get {
            path: Vec::new(),
            key: key.to_vec(),
        })
    };
    // Three trees: `b` is the root of the root Merk, `a` and `c` are
    // its children.
    let made = || {
        let mut ops = Vec::new();
        for key in [b"b", b"a", b"c"] {
            ops.push(insert(&[], key, grovedb::Element::empty_tree()));
            ops.push(insert(&[key], b"held", item()));
        }
        ops.push(Op::Commit);
        ops
    };
    for (number, grove_version) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        // The delete of a leaf leaves the root, and the write is taken.
        // The delete of the root changes it: the record names a node
        // that is gone, both engines open the root Merk with no node,
        // and a tree put at the root path is a write of a node there.
        let mut ops = made();
        ops.extend([
            delete(b"c"),
            Op::Commit,
            get(b"c"),
            insert(&[b"a"], b"more", item()),
            Op::Commit,
            delete(b"b"),
            Op::Commit,
            get(b"b"),
            get(b"a"),
            insert(&[], b"n", grovedb::Element::empty_tree()),
            Op::Commit,
            get(b"n"),
        ]);
        let case = Case::of(11, grove_version, ops);
        let run = ran(&case);
        if let Some(divergence) = &run.divergence {
            let report = divergence.report(&case, "RocksDB", "LigninDB");
            panic!("grove version {number}:\n{}", report.join("\n"));
        }
        let families = run.tally.families;
        assert_eq!(run.tally.closes.checked, 1, "{number}");
        // The write after the delete of the leaf is taken by both, and
        // what both fail alike is a read: of each deleted tree.
        const WRITE_AFTER_THE_LEAF: usize = 10;
        assert!(matches!(case.ops[WRITE_AFTER_THE_LEAF], Op::Write(_)));
        assert!(
            !run.failed_steps.contains(&WRITE_AFTER_THE_LEAF),
            "{number}"
        );
        assert!(run.failed_steps.contains(&9) && run.failed_steps.contains(&14));
        for failed in &run.failed_steps {
            assert!(matches!(case.ops[*failed], Op::Read(_)), "{number}");
        }
        if number > 3 {
            assert_eq!(run.ended_at, None, "grove version {number}");
            assert_eq!(run.tally.steps as usize, case.ops.len(), "{number}");
            assert_eq!(families.histories, [0; 11], "{number}");
            continue;
        }
        assert_eq!(run.ended_at, Some(Family::RootKeyLeftBehind), "{number}");
        let ended = run.ended_with.as_ref().expect("the answers it ended with");
        assert_eq!(ended.step, case.ops.len() - 3, "{ended:?}");
        assert!(ended.parts.contains(&Part::Error), "{ended:?}");
        let failed = |lines: &[String]| lines.iter().any(|line| line.starts_with("failed "));
        assert!(!failed(&ended.reference), "{ended:?}");
        assert!(
            ended
                .candidate
                .iter()
                .any(|line| line == "failed NotSupported"),
            "{ended:?}"
        );
        assert_eq!(
            families.histories,
            [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0],
            "{number}"
        );
        assert_eq!(
            families.answers,
            [0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0],
            "{number}"
        );
        assert_eq!(families.answer_seed[7], Some(11));
    }
}

/// The streams of the two scenes, each alone on every grove version it
/// is for, from sixteen seeds: no divergence, the store of LigninDB
/// sound after every one, and the cases counted for their families. A
/// plain delete leaves a record in most streams, which run to their
/// end with the reads that differ passed over, and on grove versions 4
/// and 5 with the cost of a removal of a tree made again; a delete at the root
/// path leaves the root-key record behind where it changes the root,
/// and the next write that RocksDB takes ends the case.
#[test]
fn the_streams_of_the_scenes_run_alike_but_for_their_families() {
    use lignin_testkit::{
        diff::{Case, Family, Scene},
        workload::Op,
    };

    const SEEDS: u64 = 16;
    let mut ended = [0u64; 2];
    let mut to_the_end = [0u64; 2];
    let mut made = [0u64; 2];
    let mut passed = 0;
    let mut passed_costs = [0u64; 5];
    for (at, scene) in Scene::ALL.into_iter().enumerate() {
        let family = match scene {
            Scene::PlainDelete => Family::BelowPlainDelete,
            Scene::RootDelete => Family::RootKeyLeftBehind,
        };
        let of_the_family = Family::ALL
            .iter()
            .position(|other| *other == family)
            .expect("a family");
        let versions = GROVE_VERSIONS
            .iter()
            .filter(|version| scene.is_for(version));
        for (number, grove_version) in versions.enumerate() {
            let number = number + 1;
            for seed in 0..SEEDS {
                let case = Case::of(seed, grove_version, scene.stream(seed));
                let run = ran(&case);
                if let Some(divergence) = &run.divergence {
                    let report = divergence.report(&case, "RocksDB", "LigninDB");
                    panic!("{scene:?} {seed} on {number}:\n{}", report.join("\n"));
                }
                assert_eq!(run.cut_short, None, "{scene:?} {seed} on {number}");
                // No write of a plain delete's stream is refused: what
                // both engines fail alike is a read of what a delete
                // removed. After a delete at the root path that left the
                // root-key record behind, both engines open the root
                // Merk without some of its trees, and fail a write into
                // one of those alike.
                for failed in &run.failed_steps {
                    let op = &case.ops[*failed];
                    let a_read = matches!(op, Op::Read(_));
                    assert!(a_read || scene == Scene::RootDelete, "{seed}: {op:?}");
                }
                assert_eq!(run.tally.closes.checked, 1, "{scene:?} {seed}");
                let families = run.tally.families;
                made[at] += families.histories[of_the_family];
                if scene == Scene::PlainDelete {
                    // A read that differs ends no case of this family,
                    // and a stream as generated asks for no write that
                    // an engine refuses.
                    assert_eq!(run.ended_at, None, "{seed} on {number}");
                    assert!(run.passed_refusals.is_empty(), "{seed} on {number}");
                    passed += families.answers[of_the_family];
                    // The cost of a removal of a tree made again is
                    // passed over on grove versions 4 and 5 alone.
                    assert_eq!(families.answers[8], families.histories[8]);
                    assert_eq!(families.answers[8] == 1, !run.passed_costs.is_empty());
                    passed_costs[number - 1] += families.answers[8];
                }
                match run.ended_at {
                    Some(at_family) => {
                        assert_eq!(at_family, family, "{scene:?} {seed} on {number}");
                        ended[at] += 1;
                    }
                    None => {
                        assert_eq!(run.tally.steps as usize, case.ops.len());
                        to_the_end[at] += 1;
                    }
                }
            }
        }
    }
    assert_eq!(ended[0] + to_the_end[0], 5 * SEEDS);
    assert_eq!(ended[1] + to_the_end[1], 3 * SEEDS);
    // A stream of plain deletes that empties every tree first makes no
    // place of the family; every stream of deletes at the root path has
    // a tree with an element deleted.
    assert!(made[0] > 3 * SEEDS && made[0] <= 5 * SEEDS, "{made:?}");
    assert_eq!(made[1], 3 * SEEDS);
    assert_eq!(to_the_end[0], 5 * SEEDS);
    assert_eq!(passed_costs[..3], [0, 0, 0]);
    assert!(
        passed_costs[3] > 0 && passed_costs[4] > 0,
        "{passed_costs:?}"
    );
    assert!(passed > 0, "{passed}");
    assert!(ended[1] > 0, "{ended:?} {to_the_end:?}");
}

/// On grove versions 4 and 5 a batch looks through a tree that a plain
/// delete removes for elements that take part in backward references,
/// and RocksDB reads there a record it kept of a tree that an earlier
/// plain delete removed. The batch is charged a seek and the bytes of
/// that record more on RocksDB and differs in nothing else: the step is
/// counted apart and passed over, and the case runs to its end. On
/// grove versions 1 to 3 the batch costs the same on both engines.
#[test]
fn the_cost_of_a_plain_delete_of_a_tree_made_again_is_counted_apart_on_versions_4_and_5() {
    use lignin_testkit::{
        diff::{Case, Family},
        workload::{BatchOp, Op, Read, Write},
    };

    let path = |segments: &[&[u8]]| -> Vec<Vec<u8>> {
        segments.iter().map(|segment| segment.to_vec()).collect()
    };
    let put = |at: &[&[u8]], key: &[u8], element: grovedb::Element| {
        Op::Batch(vec![BatchOp::InsertOrReplace {
            path: path(at),
            key: key.to_vec(),
            element,
        }])
    };
    let delete = |at: &[&[u8]], key: &[u8]| {
        Op::Batch(vec![BatchOp::Delete {
            path: path(at),
            key: key.to_vec(),
        }])
    };
    let ops = vec![
        Op::Write(Write::Insert {
            path: Vec::new(),
            key: b"p".to_vec(),
            element: grovedb::Element::empty_tree(),
        }),
        put(&[b"p"], b"t", grovedb::Element::empty_tree()),
        put(&[b"p", b"t"], &[0], grovedb::Element::new_item(vec![7; 12])),
        Op::Commit,
        delete(&[b"p"], b"t"),
        Op::Commit,
        put(&[b"p"], b"t", grovedb::Element::empty_tree()),
        put(&[b"p", b"t"], &[1], grovedb::Element::empty_sum_tree()),
        Op::Commit,
        delete(&[b"p"], b"t"),
        Op::Commit,
        Op::Read(Read::HasRaw {
            path: path(&[b"p"]),
            key: b"t".to_vec(),
        }),
    ];
    const SECOND_DELETE: usize = 9;
    assert_eq!(ops[SECOND_DELETE], delete(&[b"p"], b"t"));
    for (number, grove_version) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let case = Case::of(13, grove_version, ops.clone());
        let run = ran(&case);
        if let Some(divergence) = &run.divergence {
            let report = divergence.report(&case, "RocksDB", "LigninDB");
            panic!("grove version {number}:\n{}", report.join("\n"));
        }
        assert_eq!(run.ended_at, None, "grove version {number}");
        assert_eq!(run.tally.steps as usize, case.ops.len(), "{number}");
        assert!(run.failed_steps.is_empty(), "{:?}", run.failed_steps);
        assert_eq!(run.tally.closes.checked, 1, "{number}");
        let families = run.tally.families;
        assert_eq!(families.histories[6], 1, "grove version {number}");
        let passed = u64::from(number >= 4);
        assert_eq!(run.passed_costs.len() as u64, passed, "{number}");
        for cost in &run.passed_costs {
            // RocksDB was charged one seek and the bytes of the record
            // it kept more, and nothing else differs.
            assert_eq!(cost.step, SECOND_DELETE, "grove version {number}");
            assert_eq!(cost.parts, [Part::Cost], "grove version {number}");
            let apart = fields_apart(cost_of(&cost.reference), cost_of(&cost.candidate));
            let expected = [("seeks".to_string(), -1), ("loaded".to_string(), -181)];
            assert_eq!(apart, expected, "grove version {number}");
        }
        assert_eq!(families.histories[8], passed, "grove version {number}");
        assert_eq!(families.answers[8], passed, "grove version {number}");
        assert_eq!(families.answer_seed[8], (passed == 1).then_some(13));
        assert!(!Family::KeptRecordsCost.ends_a_case());
    }
}

/// The record that RocksDB kept below a plain delete is the element of
/// a tree: on grove versions 4 and 5 the look of a later plain delete of
/// the tree made again opens the subtree under it, and RocksDB is
/// charged the hash of its path beside a seek for the record, a seek in
/// the subtree and the bytes (D-394). The shape is one the fuzzer met:
/// a tree with an empty sum tree under `01` removed by a plain delete,
/// made again, an item put under `02`, and removed again. The step is
/// counted apart and passed over with both engines' whole costs as they
/// are here. With the sum tree under a key of 32 bytes the message of
/// its subtree's path is longer than 64 bytes, and its hash is two
/// calls (D-403).
#[test]
fn the_cost_of_a_plain_delete_over_a_kept_tree_differs_by_the_hash_of_its_path() {
    const LONG_REFERENCE: [u32; 3] = [20, 1758, 30];
    const LONG_CANDIDATE: [u32; 3] = [18, 1203, 28];

    use lignin_testkit::{
        diff::{Case, Family},
        workload::{BatchOp, Op, Write},
    };

    let flags = || Some(vec![0, 0, 4]);
    let tree = || grovedb::Element::empty_tree_with_flags(flags());
    let mut parent: Vec<Vec<u8>> = Vec::new();
    let mut ops = Vec::new();
    for key in [
        &b"removed-by-a-plain-delete"[..],
        b"l\x01",
        b"l\x02",
        b"l\x03",
    ] {
        ops.push(Op::Write(Write::Insert {
            path: parent.clone(),
            key: key.to_vec(),
            element: grovedb::Element::empty_tree(),
        }));
        parent.push(key.to_vec());
    }
    let mut t = vec![0; 16];
    t[15] = 1;
    let inside: Vec<Vec<u8>> = parent.iter().cloned().chain([t.clone()]).collect();
    let put =
        |path: &Vec<Vec<u8>>, key: &[u8], element: grovedb::Element| BatchOp::InsertOrReplace {
            path: path.clone(),
            key: key.to_vec(),
            element,
        };
    let delete = || {
        Op::Batch(vec![BatchOp::Delete {
            path: parent.clone(),
            key: t.clone(),
        }])
    };
    let item = grovedb::Element::new_item_with_flags(
        vec![0x25, 0xa4, 0x59, 0x51, 0x69, 0x5f, 0x88, 0x09],
        flags(),
    );
    let held = || grovedb::Element::empty_sum_tree_with_flags(flags());
    let made = ops;
    // The key of the kept sum tree, and what the look is charged more
    // for it on RocksDB: the costs of the second delete on RocksDB and
    // on LigninDB, as seeks, bytes loaded and hash calls.
    let short = (vec![1], [20, 1665, 29], [18, 1203, 28]);
    let long = (vec![1; 32], LONG_REFERENCE, LONG_CANDIDATE);
    let histories = [short, long].map(|(key, reference, candidate)| {
        let mut ops = made.clone();
        ops.extend([
            Op::Batch(vec![put(&parent, &t, tree()), put(&inside, &key, held())]),
            delete(),
            Op::Batch(vec![put(&parent, &t, tree())]),
            Op::Batch(vec![put(&inside, &[2], item.clone())]),
            delete(),
            Op::Commit,
        ]);
        (ops, reference, candidate)
    });
    const SECOND_DELETE: usize = 8;
    let versions = GROVE_VERSIONS.iter().enumerate();
    for ((number, grove_version), (ops, was, is)) in
        versions.flat_map(|version| histories.iter().map(move |history| (version, history)))
    {
        assert_eq!(ops[SECOND_DELETE], delete());
        let number = number + 1;
        let case = Case::of(31, grove_version, ops.clone());
        let run = ran(&case);
        if let Some(divergence) = &run.divergence {
            let report = divergence.report(&case, "RocksDB", "LigninDB");
            panic!("grove version {number}:\n{}", report.join("\n"));
        }
        assert_eq!(run.ended_at, None, "grove version {number}");
        assert_eq!(run.tally.steps as usize, case.ops.len(), "{number}");
        assert!(run.failed_steps.is_empty(), "{:?}", run.failed_steps);
        let passed = u64::from(number >= 4);
        assert_eq!(run.passed_costs.len() as u64, passed, "{number}");
        for cost in &run.passed_costs {
            assert_eq!(cost.step, SECOND_DELETE, "grove version {number}");
            assert_eq!(cost.parts, [Part::Cost], "grove version {number}");
            let (reference, candidate) = (cost_of(&cost.reference), cost_of(&cost.candidate));
            println!("grove version {number}: {reference} | {candidate}");
            // Both engines' whole costs: the bytes removed are of the
            // element of the tree, which grove version 5 stores shorter.
            let removed = if number == 4 { 176 } else { 143 };
            let whole = |[seeks, loaded, hashes]: [u32; 3]| {
                format!(
                    "cost seeks={seeks} loaded={loaded} added=0 replaced=327 removed={removed} \
                     hashes={hashes} sinsemilla=0"
                )
            };
            assert_eq!(reference, whole(*was), "grove version {number}");
            assert_eq!(candidate, whole(*is), "grove version {number}");
        }
        let families = run.tally.families;
        assert_eq!(families.histories[8], passed, "grove version {number}");
        assert_eq!(families.answers[8], passed, "grove version {number}");
        assert!(!Family::KeptRecordsCost.ends_a_case());
    }
}

/// A checked batch that removes a tree made again over a record that
/// RocksDB kept, and that both engines refuse for another operation it
/// lists, a delete in a tree that was never made. On grove versions 4
/// and 5 a batch looks through the trees it removes before it applies
/// any operation, so RocksDB has read the kept record, and opened the
/// subtree under it, before the batch is refused: the refusal differs
/// in the cost alone, and is counted with the costs of such batches
/// and passed over. On grove versions 1 to 3 the batch is refused alike.
#[test]
fn the_cost_of_a_refused_batch_that_looked_through_a_tree_over_a_kept_record_is_passed_over() {
    use lignin_testkit::{
        diff::Case,
        workload::{BatchOp, Op, Write},
    };

    let path = |segments: &[&[u8]]| -> Vec<Vec<u8>> {
        segments.iter().map(|segment| segment.to_vec()).collect()
    };
    let put = |at: &[&[u8]], key: &[u8], element: grovedb::Element| {
        Op::Batch(vec![BatchOp::InsertOrReplace {
            path: path(at),
            key: key.to_vec(),
            element,
        }])
    };
    let delete = |at: &[&[u8]], key: &[u8]| BatchOp::Delete {
        path: path(at),
        key: key.to_vec(),
    };
    let tree = grovedb::Element::empty_tree;
    let ops = vec![
        Op::Write(Write::Insert {
            path: Vec::new(),
            key: b"p".to_vec(),
            element: tree(),
        }),
        put(&[b"p"], b"t", tree()),
        put(&[b"p", b"t"], &[1], grovedb::Element::empty_sum_tree()),
        Op::Batch(vec![delete(&[b"p"], b"t")]),
        put(&[b"p"], b"t", tree()),
        put(&[b"p", b"t"], &[2], grovedb::Element::new_item(vec![7; 8])),
        Op::Batch(vec![delete(&[b"p"], b"t"), delete(&[b"p", b"never"], &[1])]),
        Op::Commit,
    ];
    const REFUSED: usize = 6;
    for (number, grove_version) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let case = Case::of(41, grove_version, ops.clone());
        let run = ran(&case);
        if let Some(divergence) = &run.divergence {
            let report = divergence.report(&case, "RocksDB", "LigninDB");
            panic!("grove version {number}:\n{}", report.join("\n"));
        }
        assert_eq!(run.ended_at, None, "grove version {number}");
        assert_eq!(run.tally.steps as usize, case.ops.len(), "{number}");
        let families = run.tally.families;
        if number < 4 {
            assert_eq!(run.failed_steps, [REFUSED], "grove version {number}");
            assert!(run.passed_costs.is_empty(), "grove version {number}");
            assert_eq!(families.answers[8], 0, "grove version {number}");
            continue;
        }
        assert!(run.failed_steps.is_empty(), "{:?}", run.failed_steps);
        assert_eq!(run.passed_costs.len(), 1, "grove version {number}");
        let cost = &run.passed_costs[0];
        assert_eq!(cost.step, REFUSED, "grove version {number}");
        assert_eq!(cost.parts, [Part::Cost], "grove version {number}");
        let failed = |lines: &[String]| {
            let mut failed = lines.iter().filter(|line| line.starts_with("failed "));
            failed.next().cloned()
        };
        let refused = Some("failed InvalidPath".to_string());
        assert_eq!(failed(&cost.reference), refused, "{cost:?}");
        assert_eq!(failed(&cost.candidate), refused, "{cost:?}");
        let (reference, candidate) = (cost_of(&cost.reference), cost_of(&cost.candidate));
        println!("refused batch on grove version {number}: {reference} | {candidate}");
        assert_eq!(reference, REFUSED_ON_ROCKSDB, "grove version {number}");
        assert_eq!(candidate, REFUSED_ON_LIGNINDB, "grove version {number}");
        assert_eq!(families.answers[8], 1, "grove version {number}");
    }
}

/// The whole costs of the refused batch of the test above.
const REFUSED_ON_ROCKSDB: &str =
    "cost seeks=7 loaded=1070 added=0 replaced=0 removed=- hashes=5 sinsemilla=0";
const REFUSED_ON_LIGNINDB: &str =
    "cost seeks=5 loaded=612 added=0 replaced=0 removed=- hashes=4 sinsemilla=0";

/// A batch that writes below a tree that a plain delete removed and
/// also into a tree that was never made, which both engines refuse:
/// LigninDB at the subtree under the record that the delete left, which
/// it does not hold, and RocksDB, which opens that subtree by the kept
/// record, at the tree that was never made. On grove versions 1 to 3,
/// which open the deepest subtree first, the step is counted apart and
/// passed over in whichever order the batch lists the two, and the case
/// goes on. Grove versions 4 and 5 look at the operations in the order
/// listed: with the tree that was never made first, both engines refuse
/// the batch alike, and with the kept subtree first the step is counted
/// apart and passed over. The same batch with only the tree that was
/// never made is refused alike by both on every grove version.
#[test]
fn a_batch_refused_at_a_kept_record_by_lignindb_and_further_on_by_rocksdb_is_passed_over() {
    use lignin_testkit::{
        diff::{Case, Family},
        workload::{BatchOp, Op, Write},
    };

    let path = |segments: &[&[u8]]| -> Vec<Vec<u8>> {
        segments.iter().map(|segment| segment.to_vec()).collect()
    };
    let put = |at: &[&[u8]], key: &[u8], element: grovedb::Element| BatchOp::InsertOrReplace {
        path: path(at),
        key: key.to_vec(),
        element,
    };
    let delete = |at: &[&[u8]], key: &[u8]| BatchOp::Delete {
        path: path(at),
        key: key.to_vec(),
    };
    let tree = grovedb::Element::empty_tree;
    let ops = vec![
        Op::Write(Write::Insert {
            path: Vec::new(),
            key: b"p".to_vec(),
            element: tree(),
        }),
        Op::Batch(vec![
            put(&[b"p"], b"t", tree()),
            put(&[b"p", b"t"], &[1], grovedb::Element::empty_sum_tree()),
        ]),
        Op::Commit,
        Op::Batch(vec![delete(&[b"p"], b"t")]),
        Op::Commit,
        // Into the tree that was never made alone: refused alike.
        Op::Batch(vec![delete(&[b"p", b"never"], &[1])]),
        // With a delete below the old key of the removed tree.
        Op::Batch(vec![
            delete(&[b"p", b"never"], &[1]),
            delete(&[b"p", b"t", &[1]], b"k"),
        ]),
        // The same two, the kept subtree listed first.
        Op::Batch(vec![
            delete(&[b"p", b"t", &[1]], b"k"),
            delete(&[b"p", b"never"], &[1]),
        ]),
        Op::Batch(vec![put(&[b"p"], b"t", tree())]),
        Op::Commit,
    ];
    const ALIKE: usize = 5;
    const REFUSED: usize = 6;
    const KEPT_FIRST: usize = 7;
    for (number, grove_version) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let case = Case::of(37, grove_version, ops.clone());
        let run = ran(&case);
        if let Some(divergence) = &run.divergence {
            let report = divergence.report(&case, "RocksDB", "LigninDB");
            panic!("grove version {number}:\n{}", report.join("\n"));
        }
        assert_eq!(run.ended_at, None, "grove version {number}");
        assert_eq!(run.tally.steps as usize, case.ops.len(), "{number}");
        let families = run.tally.families;
        let (alike, passed): (&[usize], &[usize]) = match number {
            1..=3 => (&[ALIKE], &[REFUSED, KEPT_FIRST]),
            _ => (&[ALIKE, REFUSED], &[KEPT_FIRST]),
        };
        assert_eq!(run.failed_steps, alike, "grove version {number}");
        let steps: Vec<usize> = run
            .passed_refusals
            .iter()
            .map(|passed| passed.step)
            .collect();
        assert_eq!(steps, passed, "grove version {number}");
        for refused in &run.passed_refusals {
            assert_eq!(refused.parts, [Part::Cost], "{refused:?}");
            let said = |lines: &[String]| {
                let mut failed = lines.iter().filter(|line| line.starts_with("# failed"));
                failed.next().cloned().expect("the words of a refusal")
            };
            let (reference, candidate) = (said(&refused.reference), said(&refused.candidate));
            println!("grove version {number}: {reference} | {candidate}");
            assert!(
                reference.ends_with("at path [p] for key never"),
                "{reference}"
            );
            assert!(
                candidate.ends_with("at path [p/t] for key 0x01"),
                "{candidate}"
            );
        }
        assert_eq!(families.answers[9], 1, "grove version {number}");
        assert!(!Family::RefusedBelowPlainDelete.ends_a_case());
    }
}

/// The batch of the test above that lists the kept subtree first, after
/// a rollback. A plain delete removes a tree with an element and the
/// block is committed; the next block is rolled back, or abandoned,
/// with nothing written. The record that RocksDB kept is in committed
/// state, a rollback takes nothing of it away, and the batch is refused
/// by LigninDB at the kept subtree and by RocksDB further on, as with
/// no rollback: the step is counted apart and passed over on every
/// grove version, and the case goes on.
///
/// Where the plain delete is in the block that is rolled back, the tree
/// stands again with its element on both engines, which refuse the
/// batch alike at the tree that was never made.
#[test]
fn a_record_that_a_commit_kept_is_known_after_a_rollback_and_one_of_the_block_is_not() {
    use lignin_testkit::{
        diff::Case,
        workload::{BatchOp, Op, TransactionOp, Write},
    };

    let path = |segments: &[&[u8]]| -> Vec<Vec<u8>> {
        segments.iter().map(|segment| segment.to_vec()).collect()
    };
    let put = |at: &[&[u8]], key: &[u8], element: grovedb::Element| BatchOp::InsertOrReplace {
        path: path(at),
        key: key.to_vec(),
        element,
    };
    let delete = |at: &[&[u8]], key: &[u8]| BatchOp::Delete {
        path: path(at),
        key: key.to_vec(),
    };
    let tree = grovedb::Element::empty_tree;
    let made = [
        Op::Write(Write::Insert {
            path: Vec::new(),
            key: b"p".to_vec(),
            element: tree(),
        }),
        Op::Batch(vec![
            put(&[b"p"], b"t", tree()),
            put(&[b"p", b"t"], &[1], grovedb::Element::empty_sum_tree()),
        ]),
        Op::Commit,
        Op::Batch(vec![delete(&[b"p"], b"t")]),
    ];
    let refused = || {
        Op::Batch(vec![
            delete(&[b"p", b"t", &[1]], b"k"),
            delete(&[b"p", b"never"], &[1]),
        ])
    };
    const REFUSED: usize = 6;
    const REFUSED_ALIKE: usize = 5;
    for (number, grove_version) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        for end in [TransactionOp::Rollback, TransactionOp::Abandon] {
            // The delete is committed before the block that is given up.
            let mut ops = made.to_vec();
            ops.extend([Op::Commit, Op::Transaction(end.clone()), refused()]);
            ops.extend([Op::Batch(vec![put(&[b"p"], b"t", tree())]), Op::Commit]);
            let case = Case::of(45, grove_version, ops);
            let run = ran(&case);
            if let Some(divergence) = &run.divergence {
                let report = divergence.report(&case, "RocksDB", "LigninDB");
                panic!("{end:?} on grove version {number}:\n{}", report.join("\n"));
            }
            assert_eq!(run.ended_at, None, "{end:?} on grove version {number}");
            assert_eq!(run.tally.steps as usize, case.ops.len(), "{number}");
            assert!(run.failed_steps.is_empty(), "{:?}", run.failed_steps);
            let steps: Vec<usize> = run
                .passed_refusals
                .iter()
                .map(|passed| passed.step)
                .collect();
            assert_eq!(steps, [REFUSED], "{end:?} on grove version {number}");
            let passed = &run.passed_refusals[0];
            assert_eq!(passed.parts, [Part::Cost], "{passed:?}");
            let said = |lines: &[String]| {
                let mut failed = lines.iter().filter(|line| line.starts_with("# failed"));
                failed.next().cloned().expect("the words of a refusal")
            };
            let (reference, candidate) = (said(&passed.reference), said(&passed.candidate));
            assert!(
                reference.ends_with("at path [p] for key never"),
                "{reference}"
            );
            assert!(
                candidate.ends_with("at path [p/t] for key 0x01"),
                "{candidate}"
            );
            assert_eq!(run.tally.families.answers[9], 1, "{end:?} on {number}");

            // The delete is in the block that is given up.
            let mut ops = made.to_vec();
            ops.extend([Op::Transaction(end.clone()), refused(), Op::Commit]);
            let case = Case::of(46, grove_version, ops);
            let run = ran(&case);
            if let Some(divergence) = &run.divergence {
                let report = divergence.report(&case, "RocksDB", "LigninDB");
                panic!("{end:?} on grove version {number}:\n{}", report.join("\n"));
            }
            assert_eq!(run.ended_at, None, "{end:?} on grove version {number}");
            assert_eq!(run.tally.steps as usize, case.ops.len(), "{number}");
            assert_eq!(run.failed_steps, [REFUSED_ALIKE], "{end:?} on {number}");
            assert!(run.passed_refusals.is_empty(), "{end:?} on {number}");
            assert_eq!(run.tally.families.answers[9], 0, "{end:?} on {number}");
        }
    }
}

/// A write into a subtree below a tree that a plain delete removed,
/// which both engines refuse: RocksDB gets further before it refuses,
/// for the records it kept, so the cost of the refusal differs, and on
/// grove versions 4 and 5 the error. The step is counted apart and
/// passed over, nothing is written on either engine, and the case goes
/// on: the tree is made again and written, alike on both.
#[test]
fn a_write_that_both_engines_refuse_below_a_plain_delete_is_counted_apart_and_passed_over() {
    use lignin_testkit::{
        diff::{Case, Family},
        workload::{BatchOp, Op, Write},
    };

    let path = |segments: &[&[u8]]| -> Vec<Vec<u8>> {
        segments.iter().map(|segment| segment.to_vec()).collect()
    };
    let put = |at: &[&[u8]], key: &[u8], element: grovedb::Element| BatchOp::InsertOrReplace {
        path: path(at),
        key: key.to_vec(),
        element,
    };
    let tree = grovedb::Element::empty_tree;
    let held = grovedb::Element::empty_sum_tree;
    let sum = || grovedb::Element::new_sum_item(4);
    let ops = vec![
        Op::Write(Write::Insert {
            path: Vec::new(),
            key: b"p".to_vec(),
            element: tree(),
        }),
        Op::Batch(vec![
            put(&[b"p"], b"t", tree()),
            put(&[b"p", b"t"], &[1], held()),
            put(&[b"p", b"t", &[1]], b"k", sum()),
        ]),
        Op::Commit,
        Op::Batch(vec![BatchOp::Delete {
            path: path(&[b"p"]),
            key: b"t".to_vec(),
        }]),
        Op::Commit,
        // The tree is gone: both engines refuse.
        Op::Batch(vec![put(&[b"p", b"t", &[1]], b"k", sum())]),
        Op::Batch(vec![
            put(&[b"p"], b"t", tree()),
            put(&[b"p", b"t"], &[1], held()),
            put(&[b"p", b"t", &[1]], b"k", sum()),
        ]),
        Op::Commit,
    ];
    const REFUSED: usize = 5;
    for (number, grove_version) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let case = Case::of(19, grove_version, ops.clone());
        let run = ran(&case);
        if let Some(divergence) = &run.divergence {
            let report = divergence.report(&case, "RocksDB", "LigninDB");
            panic!("grove version {number}:\n{}", report.join("\n"));
        }
        assert_eq!(run.ended_at, None, "grove version {number}");
        assert_eq!(run.tally.steps as usize, case.ops.len(), "{number}");
        // It differs, so it is no common failure, and no other step
        // fails.
        assert!(run.failed_steps.is_empty(), "{:?}", run.failed_steps);
        assert_eq!(run.passed_refusals.len(), 1, "grove version {number}");
        let refused = &run.passed_refusals[0];
        assert_eq!(refused.step, REFUSED, "grove version {number}");
        assert!(refused.parts.contains(&Part::Cost), "{refused:?}");
        assert!(
            refused
                .parts
                .iter()
                .all(|part| matches!(part, Part::Cost | Part::Error)),
            "{refused:?}"
        );
        let failed = |lines: &[String]| lines.iter().any(|line| line.starts_with("failed "));
        assert!(
            failed(&refused.reference) && failed(&refused.candidate),
            "{refused:?}"
        );
        let families = run.tally.families;
        assert_eq!(families.histories[9], 1, "grove version {number}");
        assert_eq!(families.answers[9], 1, "grove version {number}");
        assert_eq!(families.answer_seed[9], Some(19));
        assert!(!Family::RefusedBelowPlainDelete.ends_a_case());
        assert_eq!(run.tally.closes.checked, 1, "{number}");
    }
}

/// A tree is removed by a plain delete with a sum tree in it and made
/// again, and the key of the sum tree is not written again. A write
/// into the subtree under that key is taken by RocksDB, which opens the
/// subtree by the record it kept though its tree of elements holds no
/// element there, and is refused by LigninDB, which has no such
/// subtree. The case ends at that answer, counted apart, with LigninDB's
/// store checked and what the reference's own check answers kept.
#[test]
fn a_write_that_rocksdb_alone_takes_below_a_plain_delete_ends_the_case_counted_apart() {
    use lignin_testkit::{
        diff::{Case, Family},
        workload::{BatchOp, Op, Write},
    };

    let path = |segments: &[&[u8]]| -> Vec<Vec<u8>> {
        segments.iter().map(|segment| segment.to_vec()).collect()
    };
    let put = |at: &[&[u8]], key: &[u8], element: grovedb::Element| {
        Op::Batch(vec![BatchOp::InsertOrReplace {
            path: path(at),
            key: key.to_vec(),
            element,
        }])
    };
    let delete = |at: &[&[u8]], key: &[u8]| BatchOp::Delete {
        path: path(at),
        key: key.to_vec(),
    };
    let tree = grovedb::Element::empty_tree;
    let item = || grovedb::Element::new_item(vec![7; 8]);
    let sum_tree = grovedb::Element::empty_sum_tree;
    // A tree with a sum tree under `01`, removed by a plain delete and
    // made again: RocksDB keeps the record of `01`.
    let kept = |name: &[u8]| {
        vec![
            put(&[b"p"], name, tree()),
            put(&[b"p", name], &[1], sum_tree()),
            Op::Commit,
            Op::Batch(vec![delete(&[b"p"], name)]),
            Op::Commit,
            put(&[b"p"], name, tree()),
        ]
    };
    let beside = [
        put(&[b"p", b"t"], &[0], item()),
        put(&[b"p", b"t"], &[2], item()),
    ];
    let into_it = put(&[b"p", b"t", &[1]], b"k", grovedb::Element::new_sum_item(4));
    // A batch that names the key and that both engines refuse, for an
    // operation it lists twice: it puts nothing under the key.
    let twice = BatchOp::InsertOrReplace {
        path: path(&[b"p", b"t"]),
        key: vec![1],
        element: sum_tree(),
    };
    let refused_alike = Op::Batch(vec![twice.clone(), twice]);
    // A batch that deletes below the old key, the old key and the tree,
    // as one that removes the tree with what it holds would: LigninDB's
    // refusal names the subtree under the old key.
    let removal = Op::Batch(vec![
        delete(&[b"p", b"t", &[1]], b"k"),
        delete(&[b"p", b"t"], &[1]),
        delete(&[b"p"], b"t"),
    ]);
    // A second such tree `u`, made again with nothing in it: over its
    // kept element the reference's own check fails
    // (`grovedb/src/engine/lignin/tests/plain_delete.rs`), until the
    // element is put again.
    let repair = put(&[b"p", b"u"], &[1], sum_tree());
    // The histories: whether `u` is made, what follows the trees made
    // again, how many of those operations both engines refuse alike
    // before the last, and whether the reference's own check, asked in
    // its block with the last write, finds nothing.
    let histories = [
        (
            false,
            [&beside[..], &[Op::Commit, into_it.clone()][..]].concat(),
            0,
            true,
        ),
        // With `u` as it is the check does not pass; with `u` repaired
        // in the block of the write, and not committed, it finds
        // nothing: it reads the block, which holds the write too.
        (
            true,
            [&beside[..], &[Op::Commit, into_it.clone()][..]].concat(),
            0,
            false,
        ),
        (
            true,
            [&beside[..], &[Op::Commit, repair, into_it.clone()][..]].concat(),
            0,
            true,
        ),
        (
            false,
            [
                &beside[..],
                &[Op::Commit, refused_alike, into_it.clone()][..],
            ]
            .concat(),
            1,
            true,
        ),
        (false, vec![Op::Commit, removal], 0, true),
    ];
    for (number, grove_version) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        for (history, (with_u, after, refused_before, nothing)) in histories.iter().enumerate() {
            let what = format!("history {history} on grove version {number}");
            let mut ops = vec![Op::Write(Write::Insert {
                path: Vec::new(),
                key: b"p".to_vec(),
                element: tree(),
            })];
            if *with_u {
                ops.extend(kept(b"u"));
            }
            ops.extend(kept(b"t"));
            ops.extend(after.iter().cloned());
            let taken_by_one = ops.len() - 1;
            ops.push(Op::Commit);
            let case = Case::of(29, grove_version, ops);
            let run = ran(&case);
            if let Some(divergence) = &run.divergence {
                let report = divergence.report(&case, "RocksDB", "LigninDB");
                panic!("{what}:\n{}", report.join("\n"));
            }
            println!(
                "{what}: ended at {:?}, {} failed alike, the reference's own check with the write: {:?}",
                run.ended_at,
                run.failed_steps.len(),
                run.reference_check
            );
            assert_eq!(run.ended_at, Some(Family::TakenBelowPlainDelete), "{what}");
            let ended = run.ended_with.as_ref().expect("the answers it ended with");
            assert_eq!(ended.step, taken_by_one, "{what}: {ended:?}");
            let failed = |lines: &[String]| {
                let mut failed = lines.iter().filter(|line| line.starts_with("failed "));
                failed.next().cloned()
            };
            assert_eq!(failed(&ended.reference), None, "{what}: {ended:?}");
            let refused = Some("failed InvalidPath".to_string());
            assert_eq!(failed(&ended.candidate), refused, "{what}: {ended:?}");
            assert!(ended.parts.contains(&Part::Error), "{what}: {ended:?}");
            assert_eq!(run.failed_steps.len(), *refused_before, "{what}");
            let families = run.tally.families;
            assert_eq!(families.histories[10], 1, "{what}");
            assert_eq!(families.answers[10], 1, "{what}");
            assert_eq!(families.answer_seed[10], Some(29), "{what}");
            assert_eq!(run.tally.closes.checked, 1, "{what}");
            let check = run.reference_check.expect("the reference's own check");
            let found_nothing = check == lignin_testkit::diff::ReferenceCheck::Nothing;
            assert_eq!(found_nothing, *nothing, "{what}: {check:?}");
        }
    }
}

/// The records RocksDB keeps below an element written over a tree are
/// read like those below a plain delete where a checked batch replaces
/// the tree above on grove versions 4 and 5: the batch differs in the
/// seeks and the bytes loaded alone, and is counted apart and passed
/// over.
#[test]
fn the_cost_of_a_put_over_a_tree_above_an_element_written_over_a_tree_is_counted_apart() {
    use lignin_testkit::{
        diff::Case,
        workload::{BatchOp, Op, Write},
    };

    let path = |segments: &[&[u8]]| -> Vec<Vec<u8>> {
        segments.iter().map(|segment| segment.to_vec()).collect()
    };
    let put = |at: &[&[u8]], key: &[u8], element: grovedb::Element| {
        Op::Batch(vec![BatchOp::InsertOrReplace {
            path: path(at),
            key: key.to_vec(),
            element,
        }])
    };
    let tree = grovedb::Element::empty_tree;
    let held = grovedb::Element::empty_sum_tree;
    let ops = vec![
        Op::Write(Write::Insert {
            path: Vec::new(),
            key: b"p".to_vec(),
            element: tree(),
        }),
        put(&[b"p"], b"t", tree()),
        put(&[b"p", b"t"], &[1], held()),
        put(&[b"p", b"t", &[1]], b"k", grovedb::Element::new_sum_item(4)),
        Op::Commit,
        // An empty sum tree over the sum tree that holds an element.
        put(&[b"p", b"t"], &[1], held()),
        Op::Commit,
        // A tree over the tree above it.
        put(&[b"p"], b"t", tree()),
        Op::Commit,
    ];
    const OVER_THE_TREE_ABOVE: usize = 7;
    for (number, grove_version) in GROVE_VERSIONS.iter().enumerate() {
        let number = number + 1;
        let case = Case::of(23, grove_version, ops.clone());
        let run = ran(&case);
        if let Some(divergence) = &run.divergence {
            let report = divergence.report(&case, "RocksDB", "LigninDB");
            panic!("grove version {number}:\n{}", report.join("\n"));
        }
        assert_eq!(run.ended_at, None, "grove version {number}");
        assert_eq!(run.tally.steps as usize, case.ops.len(), "{number}");
        assert!(run.failed_steps.is_empty(), "{:?}", run.failed_steps);
        let families = run.tally.families;
        assert_eq!(families.histories[0], 1, "grove version {number}");
        assert_eq!(families.histories[6], 0, "grove version {number}");
        let passed = usize::from(number >= 4);
        assert_eq!(run.passed_costs.len(), passed, "grove version {number}");
        assert_eq!(families.answers[8], passed as u64, "grove version {number}");
        for cost in &run.passed_costs {
            assert_eq!(cost.step, OVER_THE_TREE_ABOVE, "grove version {number}");
            let apart = fields_apart(cost_of(&cost.reference), cost_of(&cost.candidate));
            let names: Vec<&str> = apart.iter().map(|(name, _)| name.as_str()).collect();
            assert_eq!(names, ["seeks", "loaded"], "{apart:?}");
            assert!(apart.iter().all(|(_, more)| *more < 0), "{apart:?}");
        }
    }
}

/// A partial batch that puts a node into tree pages that are full of
/// cells it does not hold: the case of one seed on grove version 5, cut
/// after that batch, which is its operation 297 and pauses at height 3.
///
/// LigninDB refused the batch with an error of its tree pages, where
/// RocksDB wrote it in segments, until the write found room for such a
/// cell. The engines answer every operation of the cut case alike, the
/// batch is taken, and what they store is the same after every write.
#[test]
fn a_partial_batch_into_full_tree_pages_is_answered_alike() {
    use lignin_testkit::{diff::Case, workload::Op};

    let mut shape = Shape::default()
        .for_grove_version(5)
        .expect("a grove version");
    shape.scenes = 0;
    let case = Case::generate(0x2de2_9de8_f6fb_659c, &shape);
    assert_eq!(
        case.digest(),
        "06bb3e3f74d545fa667afe1269f39f77fa30e5ee41a81a3b1362577dbd6af320"
    );
    assert_eq!(case.ops.len(), 1902);
    const AT: usize = 297;
    assert!(
        matches!(&case.ops[AT], Op::PartialBatch(batch) if batch.pause_height == Some(3)),
        "{:?}",
        case.ops[AT]
    );
    let cut = case.with_ops(case.ops[..=AT].to_vec());
    let run = ran(&cut);
    if let Some(divergence) = &run.divergence {
        let report = divergence.report(&cut, "RocksDB", "LigninDB");
        panic!("{}", report.join("\n"));
    }
    assert_eq!(run.ended_at, None);
    assert_eq!(run.cut_short, None);
    assert_eq!(run.tally.steps as usize, AT + 1);
    assert!(!run.failed_steps.contains(&AT), "{:?}", run.failed_steps);
    assert_eq!(run.tally.closes.checked, 1);
    // No answer and no stored record was passed over for a family.
    assert_eq!(run.tally.families.answers, [0; 11]);
    assert_eq!(run.tally.families.stored, [0; 11]);
    assert!(run.passed_costs.is_empty());
}
