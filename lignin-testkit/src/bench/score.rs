//! The plan's prediction table and the scoring of reports against it
//! (`lignin-bench score`).
//!
//! The plan's section "Performance targets" (dashpay/grovedb#1052) gives
//! each row a value measured on RocksDB, a target and an accept, the edge
//! of the predicted range, all on a machine the project does not have. So
//! a row is scored as a ratio: LigninDB's value over RocksDB's for the
//! same row in the same run, against the target and the accept over the
//! plan's RocksDB value. The page reads a transition, of which RocksDB
//! counts none and the plan's value is an estimate, are scored by their
//! value, as the gate's rulings read that row. Every scored row is
//! lower-is-better.
//!
//! A row is scored only in the state the plan means: the reference
//! workload; the write rows at steady state, in a write phase at size of
//! at least [`STEADY_TRANSITIONS`] transitions after a churn; the reads
//! warm and settled; the size, the checkpoint and the reopen after the
//! flush. [`TARGETS`] holds every row of the table; for a row that no
//! report of this harness measures, it says what does.

use std::fmt::Write as _;

use super::{Report, Row, RowKind};
use crate::workload::drive::Documents;

/// One row of the plan's prediction table.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Target {
    /// The plan's words for the row.
    pub measure: &'static str,
    /// The report rows that measure it, each an id with the phases it is
    /// scored in; none for a row this harness does not measure.
    pub rows: &'static [(&'static str, &'static [&'static str])],
    /// The plan's RocksDB value, where it measured one: the row is scored
    /// as a ratio to RocksDB in the same run. `None` scores the value.
    pub rocksdb: Option<f64>,
    /// The target, in the unit of the rows.
    pub target: f64,
    /// The accept, the edge of the predicted range, in the same unit.
    pub accept: f64,
    /// Whether the row is scored at steady state only ([`STEADY_TRANSITIONS`]).
    pub steady: bool,
    /// For a row scored by its value, why; for a row no report measures,
    /// what measures it; else empty.
    pub note: &'static str,
}

impl Target {
    /// The target as a ratio to RocksDB, for a row scored as a ratio.
    pub fn target_ratio(&self) -> Option<f64> {
        self.rocksdb.map(|rocksdb| self.target / rocksdb)
    }

    /// The accept as a ratio to RocksDB, for a row scored as a ratio.
    pub fn accept_ratio(&self) -> Option<f64> {
        self.rocksdb.map(|rocksdb| self.accept / rocksdb)
    }

    /// How `value`, a ratio for a ratio row and a value else, scores
    /// against the target and the accept. Lower is better.
    pub fn verdict(&self, value: f64) -> Verdict {
        let (target, accept) = match (self.target_ratio(), self.accept_ratio()) {
            (Some(target), Some(accept)) => (target, accept),
            _ => (self.target, self.accept),
        };
        if value <= target {
            Verdict::Target
        } else if value <= accept {
            Verdict::Accept
        } else {
            Verdict::Missed
        }
    }
}

/// Where a scored value stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// At or under the target.
    Target,
    /// Over the target, at or under the accept.
    Accept,
    /// Over the accept.
    Missed,
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Verdict::Target => "target met",
            Verdict::Accept => "accept met",
            Verdict::Missed => "missed",
        })
    }
}

/// The transitions a write phase at size has at least for its write rows
/// to be scored, after a churn: the gate's rulings read
/// the write rows at steady state, and shorter phases end before the value
/// cleaner wakes.
pub const STEADY_TRANSITIONS: u64 = 100_000;

/// The names of the two engines in a report.
const LIGNINDB: &str = "lignindb";
const ROCKSDB: &str = "rocksdb";

/// The write phases at size.
const AT_SIZE: &[&str] = &["create", "update", "delete"];
/// The write phases of a count that no regime moves: the load and the
/// writes at size.
const WRITES: &[&str] = &["load", "create", "update", "delete"];
/// The warm reads: right after the load, and settled.
const READS: &[&str] = &["warm", "settled"];

/// The plan's prediction table, row by row, in its order.
pub const TARGETS: &[Target] = &[
    Target {
        measure: "Bytes logged per transition",
        rows: &[("bytes_logged_per_transition", AT_SIZE)],
        rocksdb: Some(23_900.0),
        target: 2_400.0,
        accept: 3_200.0,
        steady: true,
        note: "",
    },
    Target {
        measure: "Physical bytes written per transition",
        rows: &[("physical_bytes_per_transition", AT_SIZE)],
        rocksdb: Some(56_000.0),
        target: 5_000.0,
        accept: 8_000.0,
        steady: true,
        note: "",
    },
    Target {
        measure: "Wall time per transition",
        rows: &[("wall_per_transition", AT_SIZE)],
        rocksdb: Some(298.0),
        target: 50.0,
        accept: 80.0,
        steady: true,
        note: "",
    },
    Target {
        measure: "Keyed lookups per transition",
        rows: &[("keyed_lookups_per_transition", WRITES)],
        rocksdb: Some(70.0),
        target: 0.0,
        accept: 0.0,
        steady: false,
        note: "",
    },
    Target {
        measure: "Hashes computed per transition, iteration two",
        rows: &[],
        rocksdb: Some(122.0),
        target: 61.0,
        accept: 70.0,
        steady: false,
        note: "iteration two (PERF-02): not scored before stop points",
    },
    Target {
        measure: "Commit of a 20-transition block",
        rows: &[("block_commit", AT_SIZE)],
        rocksdb: Some(1.0),
        target: 0.2,
        accept: 0.5,
        steady: true,
        note: "",
    },
    Target {
        measure: "Write throughput, 8 threads on disjoint Merks, iteration two",
        rows: &[],
        rocksdb: None,
        target: 5.0,
        accept: 3.0,
        steady: false,
        note: "iteration two (PERF-10): not scored before parallel writers; higher is better",
    },
    Target {
        measure: "Bulk load of 1M transitions",
        rows: &[],
        rocksdb: Some(144.5),
        target: 25.0,
        accept: 50.0,
        steady: false,
        note: "the bulk loader's test through the engine (TREE-10, TXN-10: lignin-testkit's \
               `the_reference_workload_loads_through_the_engine_within_the_time_and_fill`), \
               against GroveDB's build of the same grove on RocksDB in that run; RocksDB's \
               value is the middle of the plan's 138 to 151 s. A report's `bulk_load` is the \
               load through GroveDB's write transactions, not the loader",
    },
    Target {
        measure: "Point get",
        rows: &[("point_get", READS)],
        rocksdb: Some(8.9),
        target: 2.0,
        accept: 5.0,
        steady: false,
        note: "",
    },
    Target {
        measure: "Point query with proof",
        rows: &[("point_query_proof", READS)],
        rocksdb: Some(73.6),
        target: 10.0,
        accept: 20.0,
        steady: false,
        note: "",
    },
    Target {
        measure: "100 references to documents, with proof",
        rows: &[("references_proof", READS)],
        rocksdb: Some(1_262.0),
        target: 450.0,
        accept: 750.0,
        steady: false,
        note: "",
    },
    Target {
        measure: "Page reads per transition, state far larger than cache",
        rows: &[("page_reads_per_transition", AT_SIZE)],
        rocksdb: None,
        target: 2.0,
        accept: 3.0,
        steady: true,
        note: "by its value: RocksDB counts no page reads and the plan's 8 is an estimate \
               (the gate's rulings read the row so)",
    },
    Target {
        measure: "Size after 1M transitions, random documents",
        rows: &[("size_on_disk", &["flushed"])],
        rocksdb: Some(1_010_000_000.0),
        target: 850_000_000.0,
        accept: 1_000_000_000.0,
        steady: false,
        note: "",
    },
    Target {
        measure: "Checkpoint",
        rows: &[("checkpoint", &["flushed"])],
        rocksdb: Some(10.0),
        target: 15.0,
        accept: 40.0,
        steady: false,
        note: "",
    },
    Target {
        measure: "Reopen",
        rows: &[("reopen", &["flushed"])],
        rocksdb: Some(31.0),
        target: 20.0,
        accept: 60.0,
        steady: false,
        note: "",
    },
    Target {
        measure: "Crash recovery, 256 MB of log",
        rows: &[],
        rocksdb: None,
        target: 1.0,
        accept: 5.0,
        steady: false,
        note: "by its value in s, as the plan measured none on RocksDB: lignin-store's \
               ignored test `recovery_after_256_mb_of_log_completes_within_5_s` (STORE-07)",
    },
    Target {
        measure: "Root hash, proof bytes, OperationCost",
        rows: &[],
        rocksdb: None,
        target: 0.0,
        accept: 0.0,
        steady: false,
        note: "identical: the golden corpus replayed on LigninDB (VER-02), which compares \
               root hashes, proof bytes and every OperationCost; a report shows only that \
               both engines end on one root hash",
    },
];

/// One scored row of a report.
#[derive(Clone, Debug, PartialEq)]
pub struct Scored<'a> {
    /// The plan's row.
    pub target: &'a Target,
    /// LigninDB's row.
    pub row: &'a Row,
    /// RocksDB's row of the same id and phase in the same report.
    pub rocksdb: Option<&'a Row>,
    /// The value scored: the ratio to RocksDB for a row scored as a ratio,
    /// else the row's value. `None` for a ratio row with no RocksDB row
    /// to divide by, or one of 0.
    pub scored: Option<f64>,
}

/// Why `report` is not of the reference workload, if it is not: no row of
/// it is scored then.
pub fn not_reference(report: &Report) -> Option<String> {
    let reference = Documents::reference();
    let workload = &report.workload;
    let fields = [
        ("workload", workload.name.clone(), "documents".to_string()),
        ("seed", workload.seed.to_string(), "1".to_string()),
        (
            "documents",
            workload.documents.to_string(),
            reference.documents.to_string(),
        ),
        (
            "owners",
            workload.owners.to_string(),
            reference.owners.to_string(),
        ),
        (
            "document size",
            workload.document_size.to_string(),
            reference.document_size.to_string(),
        ),
        (
            "transitions a block",
            workload.transitions_per_block.to_string(),
            reference.transitions_per_block.to_string(),
        ),
    ];
    let differ: Vec<String> = (fields.iter())
        .filter(|(_, found, plan)| found != plan)
        .map(|(name, found, plan)| format!("{name} {found} where the plan's is {plan}"))
        .collect();
    (!differ.is_empty()).then(|| {
        format!(
            "not the reference workload ({}): no row is scored",
            differ.join(", ")
        )
    })
}

/// Whether a row of `phase` of `report` is at steady state: a write phase
/// at size of at least [`STEADY_TRANSITIONS`] transitions, after a churn,
/// of one regime ([`ONE_REGIME`]).
fn steady(report: &Report, engine: &str, phase: &str) -> bool {
    let of_engine = report.engines.iter().find(|one| one.engine == engine);
    let transitions = of_engine
        .and_then(|one| one.phases.iter().find(|totals| totals.phase == phase))
        .map_or(0, |totals| totals.transitions);
    let share = |at: &str| {
        of_engine
            .and_then(|one| one.row(&format!("value_log_dead_share_at_{at}"), phase))
            .map(|row| row.value)
    };
    let one_regime = match (share("start"), share("end")) {
        (Some(start), Some(end)) => (end - start).abs() <= ONE_REGIME,
        _ => false,
    };
    report.workload.churn > 0 && transitions >= STEADY_TRANSITIONS && one_regime
}

/// How far the value log's dead share may move from a phase's start to
/// its end for the phase to be taken as of one regime: a point in a
/// hundred. A phase that moves further holds two, the value cleaner
/// behind its target and then catching up, and its write rows are not
/// scored.
pub const ONE_REGIME: f64 = 0.01;

/// Whether RocksDB's phase `phase` of `report` is as long as LigninDB's:
/// a ratio divides rows of phases of the same length. A report the
/// harness made has them so by construction; one put together by hand may
/// not.
fn same_length(report: &Report, phase: &str) -> bool {
    let length = |engine: &str| {
        (report.engines.iter())
            .find(|one| one.engine == engine)
            .and_then(|one| one.phases.iter().find(|totals| totals.phase == phase))
            .map(|totals| totals.transitions)
    };
    length(ROCKSDB) == length(LIGNINDB)
}

/// A time row with no sample was not measured: a run that asked for none
/// of its kind still has the row, at 0.
fn measured(row: &Row) -> bool {
    row.kind != RowKind::Time || !row.samples.is_empty()
}

/// The rows of `report` that the plan's table scores: every LigninDB row
/// whose id and phase a [`Target`] names, in the state the target asks
/// for, of a report of the reference workload, in the table's order and
/// then the report's.
pub fn scored(report: &Report) -> Vec<Scored<'_>> {
    let engine = |name: &str| report.engines.iter().find(|engine| engine.engine == name);
    let Some(lignindb) = engine(LIGNINDB) else {
        return Vec::new();
    };
    if not_reference(report).is_some() {
        return Vec::new();
    }
    let rocksdb = engine(ROCKSDB);
    let mut scored = Vec::new();
    for target in TARGETS {
        let of_target = |row: &&Row| {
            (target.rows.iter())
                .any(|(id, phases)| *id == row.id && phases.contains(&row.phase.as_str()))
                && measured(row)
                && (!target.steady || steady(report, LIGNINDB, &row.phase))
        };
        for row in lignindb.rows.iter().filter(of_target) {
            let rocksdb = rocksdb.and_then(|engine| {
                (engine.rows.iter())
                    .find(|other| other.id == row.id && other.phase == row.phase)
                    .filter(|other| measured(other) && same_length(report, &other.phase))
            });
            let value = match target.rocksdb {
                Some(_) => rocksdb
                    .filter(|other| other.value != 0.0)
                    .map(|other| row.value / other.value),
                None => Some(row.value),
            };
            scored.push(Scored {
                target,
                row,
                rocksdb,
                scored: value,
            });
        }
    }
    scored
}

fn number(value: f64) -> String {
    if value.abs() >= 1_000_000.0 {
        format!("{:.1} M", value / 1_000_000.0)
    } else if value.abs() >= 100.0 || value == value.trunc() {
        format!("{value:.0}")
    } else if value.abs() >= 1.0 {
        format!("{value:.2}")
    } else {
        format!("{value:.3}")
    }
}

fn share(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_string(), |value| format!("{:.1}%", value * 100.0))
}

/// The steady state of `report`'s write phases at size and churn, for a
/// report whose LigninDB rows say what its files held: the dead shares of
/// the segments and of the value log at each phase's start and end, the
/// values the value cleaner moved a transition, and the size of each
/// engine's database at the phase's end.
fn steady_state(report: &Report) -> String {
    let mut out = String::new();
    let Some(lignindb) = report.engines.iter().find(|one| one.engine == LIGNINDB) else {
        return out;
    };
    let rocksdb = report.engines.iter().find(|one| one.engine == ROCKSDB);
    let phases: Vec<_> = (lignindb.phases.iter())
        .filter(|totals| {
            lignindb
                .row("value_log_dead_share_at_start", &totals.phase)
                .is_some()
        })
        .collect();
    if phases.is_empty() {
        return out;
    }
    let _ = writeln!(
        out,
        "\n| Phase | Transitions | Segments dead, start / end | Value log dead, start / end | \
         Values the value cleaner moved a transition | Size at the end, LigninDB / RocksDB |"
    );
    let _ = writeln!(out, "| --- | --- | --- | --- | --- | --- |");
    for totals in phases {
        let phase = totals.phase.as_str();
        let value = |id: &str| lignindb.row(id, phase).map(|row| row.value);
        let size = |engine: Option<&super::report::EngineReport>| {
            engine
                .and_then(|one| one.row("size_on_disk_at_end", phase))
                .map_or_else(|| "-".to_string(), |row| number(row.value))
        };
        let _ = writeln!(
            out,
            "| {phase} | {} | {} / {} | {} / {} | {} | {} / {} B |",
            totals.transitions,
            share(value("segment_dead_share_at_start")),
            share(value("segment_dead_share_at_end")),
            share(value("value_log_dead_share_at_start")),
            share(value("value_log_dead_share_at_end")),
            value("cleaner_values_moved_per_transition").map_or_else(|| "-".to_string(), number),
            size(Some(lignindb)),
            size(rocksdb),
        );
    }
    out
}

/// The score of `report` as Markdown: for each scored row the plan's
/// target and accept (as ratios to RocksDB where the row is scored so),
/// LigninDB's value, RocksDB's in the same run, the scored value and where
/// it stands; the steady state of its write phases; then the rows of the
/// table that the report does not score, with what measures them.
pub fn score(report: &Report) -> String {
    let mut out = String::new();
    if let Some(why) = not_reference(report) {
        let _ = writeln!(out, "{why}\n");
    }
    let rows = scored(report);
    let _ = writeln!(
        out,
        "| Measure | Row | Phase | Target | Accept | LigninDB | RocksDB, same run | Scored | \
         Verdict |"
    );
    let _ = writeln!(
        out,
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"
    );
    for one in &rows {
        let target = one.target;
        let (goal, edge, how) = match (target.target_ratio(), target.accept_ratio()) {
            (Some(goal), Some(edge)) => (goal, edge, "x RocksDB"),
            _ => (target.target, target.accept, one.row.unit.as_str()),
        };
        let rocksdb = one
            .rocksdb
            .map_or_else(|| "-".to_string(), |row| number(row.value));
        let (scored, verdict) = match (one.scored, one.rocksdb) {
            (Some(value), _) => (number(value), target.verdict(value).to_string()),
            (None, Some(_)) => ("-".to_string(), "RocksDB's row is 0".to_string()),
            (None, None) => ("-".to_string(), "no RocksDB row in the run".to_string()),
        };
        let _ = writeln!(
            out,
            "| {} | `{}` | {} | {} {how} | {} {how} | {} {} | {rocksdb} | {scored} | {verdict} |",
            target.measure,
            one.row.id,
            one.row.phase,
            number(goal),
            number(edge),
            number(one.row.value),
            one.row.unit,
        );
    }
    out.push_str(&steady_state(report));
    let mut missing = TARGETS
        .iter()
        .filter(|target| !rows.iter().any(|one| one.target == *target))
        .peekable();
    if missing.peek().is_some() {
        let _ = writeln!(out, "\nNot scored in this report:\n");
        for target in missing {
            let why = match (target.rows.is_empty(), target.steady) {
                (true, _) => target.note,
                (false, true) => {
                    "scored at steady state: a `writes` or `run` report with a churn, in \
                     phases at size of 100,000 transitions or more and of one regime"
                }
                (false, false) => {
                    "no LigninDB row of its phases with samples in this report (a `run` or \
                     `reads` report has it)"
                }
            };
            let _ = writeln!(out, "- {}: {why}", target.measure);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::report::{
        tests::{engine, report, row},
        PhaseTotals,
    };

    fn target(measure: &str) -> &'static Target {
        TARGETS
            .iter()
            .find(|target| target.measure == measure)
            .expect("a row of the table")
    }

    /// A ratio row's target and accept are the plan's over its RocksDB
    /// value; a value at a bound meets it, a value above the accept is a
    /// miss, and a row with no RocksDB value is scored by its value.
    #[test]
    fn a_row_is_scored_against_its_bounds_as_ratios_or_values() {
        let wall = target("Wall time per transition");
        assert_eq!(wall.target_ratio(), Some(50.0 / 298.0));
        assert_eq!(wall.accept_ratio(), Some(80.0 / 298.0));
        assert_eq!(wall.verdict(50.0 / 298.0), Verdict::Target);
        assert_eq!(wall.verdict(0.2), Verdict::Accept);
        assert_eq!(wall.verdict(80.0 / 298.0), Verdict::Accept);
        assert_eq!(wall.verdict(2.2), Verdict::Missed);
        let pages = target("Page reads per transition, state far larger than cache");
        assert_eq!((pages.target_ratio(), pages.accept_ratio()), (None, None));
        assert_eq!(pages.verdict(2.0), Verdict::Target);
        assert_eq!(pages.verdict(2.83), Verdict::Accept);
        assert_eq!(pages.verdict(3.1), Verdict::Missed);
        let lookups = target("Keyed lookups per transition");
        assert_eq!(lookups.verdict(0.0), Verdict::Target);
        assert_eq!(lookups.verdict(0.01), Verdict::Missed);
        for (verdict, text) in [
            (Verdict::Target, "target met"),
            (Verdict::Accept, "accept met"),
            (Verdict::Missed, "missed"),
        ] {
            assert_eq!(verdict.to_string(), text);
        }
    }

    type Expected = (
        &'static str,
        &'static [(&'static str, &'static [&'static str])],
        Option<f64>,
        f64,
        f64,
        bool,
    );

    const AT: &[&str] = &["create", "update", "delete"];

    /// Every row of the plan's table, in its order, with its values, the
    /// rows and phases it is scored in, and whether at steady state only.
    #[test]
    fn the_table_holds_every_row_of_the_plan() {
        let expected: &[Expected] = &[
            (
                "Bytes logged per transition",
                &[("bytes_logged_per_transition", AT)],
                Some(23_900.0),
                2_400.0,
                3_200.0,
                true,
            ),
            (
                "Physical bytes written per transition",
                &[("physical_bytes_per_transition", AT)],
                Some(56_000.0),
                5_000.0,
                8_000.0,
                true,
            ),
            (
                "Wall time per transition",
                &[("wall_per_transition", AT)],
                Some(298.0),
                50.0,
                80.0,
                true,
            ),
            (
                "Keyed lookups per transition",
                &[(
                    "keyed_lookups_per_transition",
                    &["load", "create", "update", "delete"],
                )],
                Some(70.0),
                0.0,
                0.0,
                false,
            ),
            (
                "Hashes computed per transition, iteration two",
                &[],
                Some(122.0),
                61.0,
                70.0,
                false,
            ),
            (
                "Commit of a 20-transition block",
                &[("block_commit", AT)],
                Some(1.0),
                0.2,
                0.5,
                true,
            ),
            (
                "Write throughput, 8 threads on disjoint Merks, iteration two",
                &[],
                None,
                5.0,
                3.0,
                false,
            ),
            (
                "Bulk load of 1M transitions",
                &[],
                Some(144.5),
                25.0,
                50.0,
                false,
            ),
            (
                "Point get",
                &[("point_get", &["warm", "settled"])],
                Some(8.9),
                2.0,
                5.0,
                false,
            ),
            (
                "Point query with proof",
                &[("point_query_proof", &["warm", "settled"])],
                Some(73.6),
                10.0,
                20.0,
                false,
            ),
            (
                "100 references to documents, with proof",
                &[("references_proof", &["warm", "settled"])],
                Some(1_262.0),
                450.0,
                750.0,
                false,
            ),
            (
                "Page reads per transition, state far larger than cache",
                &[("page_reads_per_transition", AT)],
                None,
                2.0,
                3.0,
                true,
            ),
            (
                "Size after 1M transitions, random documents",
                &[("size_on_disk", &["flushed"])],
                Some(1_010_000_000.0),
                850_000_000.0,
                1_000_000_000.0,
                false,
            ),
            (
                "Checkpoint",
                &[("checkpoint", &["flushed"])],
                Some(10.0),
                15.0,
                40.0,
                false,
            ),
            (
                "Reopen",
                &[("reopen", &["flushed"])],
                Some(31.0),
                20.0,
                60.0,
                false,
            ),
            ("Crash recovery, 256 MB of log", &[], None, 1.0, 5.0, false),
            (
                "Root hash, proof bytes, OperationCost",
                &[],
                None,
                0.0,
                0.0,
                false,
            ),
        ];
        assert_eq!(TARGETS.len(), expected.len());
        for (target, expected) in TARGETS.iter().zip(expected) {
            let found = (
                target.measure,
                target.rows,
                target.rocksdb,
                target.target,
                target.accept,
                target.steady,
            );
            assert_eq!(found, *expected);
            // A row scored by its value, and one no report measures, say why.
            let unscored = target.rows.is_empty() || target.rocksdb.is_none();
            assert_eq!(unscored, !target.note.is_empty(), "{}", target.measure);
        }
    }

    /// A report of the reference workload, its write phases at size after
    /// a churn of `churn` of `transitions` each.
    fn reference(
        churn: u64,
        transitions: u64,
        engines: Vec<super::super::report::EngineReport>,
    ) -> Report {
        let mut report = report(engines);
        let workload = &mut report.workload;
        let documents = Documents::reference();
        (workload.seed, workload.documents, workload.owners) =
            (1, documents.documents, documents.owners);
        workload.document_size = documents.document_size;
        workload.transitions_per_block = documents.transitions_per_block;
        workload.churn = churn;
        for engine in &mut report.engines {
            engine.phases = ["churn", "create", "update", "delete"]
                .map(|phase| PhaseTotals {
                    phase: phase.to_string(),
                    transitions,
                    ..PhaseTotals::default()
                })
                .to_vec();
        }
        report
    }

    fn engines() -> Vec<super::super::report::EngineReport> {
        let mut unmeasured = row("reopen", "flushed", 0.0);
        unmeasured.samples.clear();
        vec![
            engine(
                "rocksdb",
                vec![
                    row("wall_per_transition", "create", 500.0),
                    row("point_get", "warm", 20.0),
                    row("point_get", "cold", 1_000.0),
                    row("keyed_lookups_per_transition", "load", 50.0),
                    row("bytes_logged_per_transition", "create", 0.0),
                    unmeasured.clone(),
                ],
            ),
            engine(
                "lignindb",
                vec![
                    row("wall_per_transition", "create", 1_100.0),
                    row("wall_per_transition", "churn", 1_000.0),
                    row("point_get", "warm", 10.0),
                    row("point_get", "cold", 900.0),
                    row("keyed_lookups_per_transition", "load", 3.0),
                    row("bytes_logged_per_transition", "create", 4_500.0),
                    row("page_reads_per_transition", "create", 2.8),
                    row("page_reads_per_transition", "load", 0.0),
                    row("hashes_per_transition", "create", 159.0),
                    row("value_log_dead_share_at_start", "create", 0.17),
                    row("value_log_dead_share_at_end", "create", 0.18),
                    row("segment_dead_share_at_start", "create", 0.15),
                    row("segment_dead_share_at_end", "create", 0.16),
                    row("cleaner_values_moved_per_transition", "create", 4.8),
                    row("size_on_disk_at_end", "create", 980_000_000.0),
                    unmeasured,
                ],
            ),
        ]
    }

    fn ids<'a>(rows: &'a [Scored<'a>]) -> Vec<(&'a str, &'a str, Option<f64>)> {
        (rows.iter())
            .map(|one| (one.row.id.as_str(), one.row.phase.as_str(), one.scored))
            .collect()
    }

    /// LigninDB's rows are scored as their ratio to RocksDB's same row of
    /// the same report, or by their value, in the phases and the state the
    /// plan means: no churn, cold read, load's page reads or unmeasured
    /// row is scored, and a RocksDB row of 0 gives no ratio.
    #[test]
    fn a_report_is_scored_in_the_plans_state() {
        let both = reference(100_000, 200_000, engines());
        let rows = scored(&both);
        assert_eq!(
            ids(&rows),
            vec![
                ("bytes_logged_per_transition", "create", None),
                ("wall_per_transition", "create", Some(2.2)),
                ("keyed_lookups_per_transition", "load", Some(0.06)),
                ("point_get", "warm", Some(0.5)),
                ("page_reads_per_transition", "create", Some(2.8)),
            ]
        );
        let text = score(&both);
        assert!(text.contains("| Wall time per transition | `wall_per_transition` | create |"));
        assert!(text.contains("| 1100 us | 500 | 2.20 | missed |"), "{text}");
        assert!(
            text.contains("| 10 us | 20 | 0.500 | accept met |"),
            "{text}"
        );
        assert!(
            text.contains("| 2.80 us | - | 2.80 | accept met |"),
            "{text}"
        );
        assert!(
            text.contains("| 4500 us | 0 | - | RocksDB's row is 0 |"),
            "{text}"
        );
        assert!(
            text.contains(
                "| create | 200000 | 15.0% / 16.0% | 17.0% / 18.0% | 4.80 | 980.0 M / - B |"
            ),
            "{text}"
        );
        assert!(text.contains("- Bulk load of 1M transitions: the bulk loader's test"));
        assert!(text.contains("- Reopen: no LigninDB row of its phases with samples"));
        assert!(!text.contains("- Point get:"));
        assert!(!text.contains("not the reference workload"));
    }

    /// The write rows are scored at steady state only: not without a
    /// churn, and not in a phase shorter than the steady state's; a count
    /// that no regime moves is scored all the same.
    #[test]
    fn the_write_rows_are_scored_at_steady_state_only() {
        for unsteady in [
            reference(0, 200_000, engines()),
            reference(100_000, 20_000, engines()),
        ] {
            assert_eq!(
                ids(&scored(&unsteady)),
                vec![
                    ("keyed_lookups_per_transition", "load", Some(0.06)),
                    ("point_get", "warm", Some(0.5)),
                ]
            );
            assert!(score(&unsteady).contains("- Wall time per transition: scored at steady state"));
        }
        // A phase whose value log moved more than a point holds two
        // regimes: its write rows are not scored.
        let mut mixed = reference(100_000, 200_000, engines());
        let lignindb = &mut mixed.engines[1];
        let end = (lignindb.rows.iter_mut())
            .find(|row| row.id == "value_log_dead_share_at_end")
            .expect("the row");
        end.value = 0.11;
        assert_eq!(
            ids(&scored(&mixed)),
            vec![
                ("keyed_lookups_per_transition", "load", Some(0.06)),
                ("point_get", "warm", Some(0.5)),
            ]
        );
    }

    /// No row of a report of another workload is scored, and the score
    /// says why; a ratio row of a report with no RocksDB has no score.
    #[test]
    fn only_the_reference_workload_is_scored() {
        let small = report(engines());
        assert!(scored(&small).is_empty());
        assert!(score(&small).starts_with(
            "not the reference workload (documents 1 where the plan's is 1000000, owners 1 where"
        ));
        let mut renamed = reference(100_000, 200_000, engines());
        renamed.workload.name = "adversarial".to_string();
        assert!(scored(&renamed).is_empty());
        assert!(score(&renamed).starts_with(
            "not the reference workload (workload adversarial where the plan's is documents): "
        ));
        // A RocksDB phase of another length than LigninDB's gives no ratio.
        let mut uneven = reference(100_000, 200_000, engines());
        uneven.engines[0].phases[1].transitions = 100_000;
        let rows = scored(&uneven);
        let wall = rows
            .iter()
            .find(|one| one.row.id == "wall_per_transition")
            .expect("the row is scored");
        assert_eq!((wall.rocksdb, wall.scored), (None, None));
        let alone = reference(
            0,
            0,
            vec![engine("lignindb", vec![row("point_get", "warm", 10.0)])],
        );
        assert_eq!(scored(&alone)[0].scored, None);
        assert!(score(&alone).contains("| - | - | no RocksDB row in the run |"));
        let rocksdb_alone = reference(
            0,
            0,
            vec![engine("rocksdb", vec![row("point_get", "warm", 1.0)])],
        );
        assert!(scored(&rocksdb_alone).is_empty());
        assert!(score(&rocksdb_alone).contains("- Point get: no LigninDB row of its phases"));
    }
}
