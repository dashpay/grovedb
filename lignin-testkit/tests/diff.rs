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

#[test]
fn a_seeded_one_byte_cost_bug_is_found_within_a_minute() {
    // Two defects, each in about one write of 4,500: of the 185,138 writes
    // of one run of 240 cases, 40 added 71 bytes, and 41 loaded 413 bytes
    // for the records they delete without a cost, which are read to learn
    // what the deletes free.
    let faults = [
        (SEEDED_FAULT, "added=71 ", "added=72 "),
        (Fault::LoadedByte { when_loaded: 413 }, "loaded=", "loaded="),
    ];
    for (fault, was, is) in faults {
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
        let (shrunk, shrinking) = finding.shrunk.as_ref().expect("the stream is shrunk");
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
            shrinking.as_secs_f64(),
            started.elapsed().as_secs_f64(),
        );
        assert!(finding.after < A_MINUTE, "found after {:?}", finding.after);
        // A cost, and nothing else: the root hash, the result and the
        // proofs of the step are the same.
        assert_eq!(finding.divergence.parts, [Part::Cost]);
        let (reference, candidate) = (
            cost_of(&finding.divergence.reference),
            cost_of(&finding.divergence.candidate),
        );
        assert!(reference.contains(was), "{reference}");
        assert!(candidate.contains(is), "{candidate}");
        assert_eq!(
            one_field_one_apart(reference, candidate),
            Some(1),
            "{reference} | {candidate}"
        );
        // And the stream is shrunk to the few operations that show it.
        assert!(shrunk.reproduced && shrunk.minimal);
        assert_eq!(shrunk.divergence.parts, [Part::Cost]);
        assert!(shrunk.case.ops.len() <= 16, "{:?}", shrunk.case.ops);
        assert!(shrunk.case.ops.len() <= finding.divergence.step + 1);
    }
}

/// By how much the one number differs in which two cost lines differ.
/// None when they differ in more than one number, or in none.
fn one_field_one_apart(reference: &str, candidate: &str) -> Option<u64> {
    let numbers = |line: &str| -> Vec<u64> {
        line.split(|character: char| !character.is_ascii_digit())
            .filter(|digits| !digits.is_empty())
            .filter_map(|digits| digits.parse().ok())
            .collect()
    };
    let (reference, candidate) = (numbers(reference), numbers(candidate));
    if reference.len() != candidate.len() {
        return None;
    }
    let mut apart = reference
        .iter()
        .zip(&candidate)
        .filter(|(was, is)| was != is)
        .map(|(was, is)| is.abs_diff(*was));
    let first = apart.next()?;
    apart.next().is_none().then_some(first)
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
fn the_numbers_of_two_cost_lines_are_compared() {
    let cost = "cost seeks=3 loaded=120 added=185 replaced=40 removed=- hashes=7 sinsemilla=0";
    let more = "cost seeks=3 loaded=120 added=186 replaced=40 removed=- hashes=7 sinsemilla=0";
    let two = "cost seeks=3 loaded=121 added=186 replaced=40 removed=- hashes=7 sinsemilla=0";
    assert_eq!(one_field_one_apart(cost, more), Some(1));
    assert_eq!(one_field_one_apart(cost, cost), None);
    assert_eq!(one_field_one_apart(cost, two), None);
    assert_eq!(one_field_one_apart(cost, "cost seeks=3"), None);
    assert_eq!(cost_of(&["root 00".to_string(), cost.to_string()]), cost);
}
