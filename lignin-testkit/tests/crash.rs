//! The store crash campaign (VER-03): its short form, and its own
//! acceptance test.
//!
//! The long form is the command `lignin-crash` (`lignin-ci.sh crash`):
//! these are the runs a debug build gets through beside the other tests.

use std::collections::BTreeSet;

use lignin_io::sim::UnknownFate;
use lignin_testkit::crash::{
    campaign::{campaign, run, unknown_of_seed, Faults, RunShape, Settings, ACTED},
    check::Kind,
    cli::DEFECTS,
};

/// A run that a debug build gets through in seconds: two lineages of ten
/// operations, the first and one that goes on from a state a crash left.
fn short() -> RunShape {
    RunShape {
        ops: 10,
        lineages: 2,
        depth: 1,
        again_one_in: 16,
        ..RunShape::default()
    }
}

fn settings(seed: u64, runs: u64, shape: RunShape) -> Settings {
    Settings {
        seed,
        runs: Some(runs),
        shape,
        ..Settings::default()
    }
}

/// **The short form of the campaign.** Twelve runs, which between them
/// have every kind of faults with every setting of what a failed sync
/// leaves: every state that a crash can leave at each of their write
/// boundaries recovers to a committed version.
#[test]
fn every_crash_state_of_a_short_campaign_recovers_to_a_committed_version() {
    let seeds = 1..=12u64;
    let unknown = |seed| match unknown_of_seed(seed) {
        UnknownFate::Lost => "lost",
        UnknownFate::Kept => "kept",
        UnknownFate::Drawn(_) => "drawn",
    };
    let kinds: BTreeSet<_> = seeds
        .clone()
        .map(|seed| (Faults::of_seed(seed), unknown(seed)))
        .collect();
    assert_eq!(kinds.len(), 9, "{kinds:?}");

    let report = campaign(&settings(1, 12, short()), &mut |_, _| {});
    println!("{report}");
    assert!(report.clean(), "{report}");
    assert_eq!(report.runs, 12);
    let tally = &report.tally;
    // A few states are the same in every run: those of a store that is
    // being made.
    assert!(report.points <= tally.get("distinct crash states"));
    assert!(report.points >= 8_000, "{} states", report.points);
    assert_eq!(tally.get("violations"), 0);
    // What the runs went through, so that a campaign which crashes
    // nothing worth crashing does not pass.
    for (name, least) in [
        ("write boundaries", 1_500),
        ("commits made", 60),
        ("table checkpoints", 12),
        ("log files dropped", 12),
        ("values placed", 40),
        ("reclaiming steps made", 1),
        ("delta runs written", 5),
        ("base images written", 40),
        ("sessions", 24),
        ("faults injected", 8),
        ("errors after a fault", 4),
        ("states above the durable floor", 500),
        ("states that lost commits", 500),
        ("states that replayed commits", 5_000),
        ("states from a table checkpoint", 1_000),
        ("states at a reclaiming step", 10),
        ("states at an item whose append had not returned", 1_500),
        ("states with no store yet", 12),
        ("states recovered again", 300),
        ("commits replayed", 15_000),
        ("background changes replayed", 8_000),
        ("pages freed in replay", 800),
        ("pages spilled in replay", 5_000),
        ("pending values recovered", 10_000),
        ("recoveries with faults that returned a store", 300),
        ("recoveries with faults that failed", 100),
        ("floors raised by a durable state", 1),
    ] {
        assert!(tally.get(name) >= least, "{name}: {}", tally.get(name));
    }
}

/// **The same seed, the same run**: the states in the order they were
/// recovered, where each landed, and the schedule of every file system,
/// for a run of each kind of faults.
#[test]
fn the_same_seed_gives_the_same_run() {
    for seed in [1, 2, 3] {
        let (first, second) = (run(seed, short()), run(seed, short()));
        assert_eq!(first.digest, second.digest, "seed {seed}");
        assert_eq!(first.fingerprints, second.fingerprints, "seed {seed}");
        assert_eq!(first.tally, second.tally, "seed {seed}");
        assert!(first.fingerprints.len() > 200, "seed {seed}");
        assert_ne!(first.digest, run(seed + 4, short()).digest, "seed {seed}");
    }
}

/// A campaign is the same whatever number of its runs are made at once,
/// and ends with the run that reaches the points it was asked for.
#[test]
fn a_campaign_is_the_same_whatever_runs_at_once() {
    let goal = Settings {
        runs: None,
        points: Some(2_000),
        ..settings(20, 1, short())
    };
    let one = campaign(&goal, &mut |_, _| {});
    let mut seeds = Vec::new();
    let three = campaign(
        &Settings {
            jobs: 3,
            ..goal.clone()
        },
        &mut |run, points| {
            seeds.push((run.seed, points));
        },
    );
    assert!(one.clean(), "{one}");
    assert_eq!(
        (one.runs, one.points, one.digest),
        (three.runs, three.points, three.digest)
    );
    assert_eq!(one.tally, three.tally);
    assert!(one.points >= 2_000 && one.runs >= 2, "{one}");
    // The runs were taken in in the order of their seeds, and the one
    // before the last had not reached the points.
    let expected: Vec<u64> = (20..20 + one.runs).collect();
    assert_eq!(
        seeds.iter().map(|(seed, _)| *seed).collect::<Vec<_>>(),
        expected
    );
    assert!(seeds[seeds.len() - 2].1 < 2_000);
    let said = one.to_string();
    assert!(
        said.contains(&format!("(seeds 20 to {})", 19 + one.runs)),
        "{said}"
    );
}

/// **The campaign's own acceptance test.** Each planted defect is found,
/// by the check that is there for it.
#[test]
fn every_planted_defect_is_found() {
    let found = |name: &str| {
        let (_, planted) = DEFECTS
            .into_iter()
            .find(|(known, _)| *known == name)
            .expect("a defect");
        let shape = RunShape {
            ops: 30,
            faults: Some(Faults::None),
            planted: Some(planted),
            ..short()
        };
        let mut acted = 0;
        let report = campaign(&settings(1, 6, shape), &mut |run, _| {
            if !run.violations.is_empty() {
                acted = run.tally.get(ACTED);
            }
        });
        let first = report.violations.first();
        let found = first.unwrap_or_else(|| panic!("{name} was not found: {report}"));
        println!("{name}: {found}");
        // The run that found it is one in which the defect did something.
        assert!(acted > 0, "{name}: {report}");
        // A violation says where in its run it was found.
        let said = found.to_string();
        assert!(
            said.starts_with(&format!("seed {}, lineage ", found.seed)),
            "{said}"
        );
        assert!(said.contains("the last operations: "), "{said}");
        assert_eq!(found.boundary.is_some(), found.plan.is_some());
        (found.violation.kind, found.violation.detail.clone())
    };
    assert_eq!(DEFECTS.len(), 8);

    // Commits that were reported durable and are not: the recovery lands
    // below them.
    let (kind, detail) = found("log-sync");
    assert_eq!(kind, Kind::NotCommitted, "{detail}");
    // A table checkpoint that names blocks which are not durable.
    let (kind, detail) = found("segment-sync");
    assert!(
        matches!(kind, Kind::Inconsistent | Kind::NotRecovered),
        "{kind}: {detail}"
    );
    // A manifest that came back, and files that never were.
    for name in [
        "new-log-sync",
        "value-sync",
        "table-sync",
        "manifest-sync",
        "dir-sync",
    ] {
        let (kind, detail) = found(name);
        assert!(
            matches!(
                kind,
                Kind::NotRecovered | Kind::NotCommitted | Kind::Inconsistent
            ),
            "{name}: {kind}: {detail}"
        );
    }
    // A model that is wrong by one hash: the store that was not crashed
    // says otherwise.
    let (kind, detail) = found("forgotten-hash");
    assert_eq!(kind, Kind::Session, "{detail}");
    assert!(detail.contains("says"), "{detail}");
}
