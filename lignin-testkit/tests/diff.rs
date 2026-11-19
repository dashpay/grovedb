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

use lignin_testkit::diff::{
    cli::SEEDED_FAULT, fuzz, Candidate, Fault, Part, Reference, Settings, Side, StandIn,
};

const A_MINUTE: Duration = Duration::from_secs(60);

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
        let scratch = tempfile::tempdir().expect("a directory");
        let started = Instant::now();
        let summary = fuzz(
            &Side::<Reference>::of("reference"),
            &StandIn::side(fault),
            &Settings {
                duration: Some(A_MINUTE),
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
    let scratch = tempfile::tempdir().expect("a directory");
    let summary = fuzz(
        &Side::<Reference>::of("reference"),
        &Side::<Candidate>::of("candidate"),
        &Settings {
            cases: Some(32),
            ..settings(&scratch)
        },
        &mut |_| {},
    )
    .expect("the run");
    let tally = summary.progress.tally;
    println!(
        "{} cases, {} steps, {} operations failed alike, {} proofs, {} comparisons of {} stored \
         records, {} cases cut short, in {:.1} s",
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
        panic!(
            "the engines differ in case {} (seed {:#x}):\n{}",
            finding.index,
            finding.case.seed,
            finding
                .divergence
                .report(&finding.case, "reference", "candidate")
                .join("\n")
        );
    }
    assert_eq!(summary.progress.cases, 32);
    assert!(tally.steps > 15_000, "{} steps", tally.steps);
    assert!(tally.proofs > 500, "{} proofs", tally.proofs);
    assert!(tally.walks > 32, "{} walks", tally.walks);
    assert_eq!(summary.first_cut_short, None);
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
