//! The plan's prediction table and the scoring of reports against it
//! (`lignin-bench score`).
//!
//! The plan's section "Performance targets" (dashpay/grovedb#1052) gives
//! each row a value measured on RocksDB, a target and an accept, the edge
//! of the predicted range, all on a machine the project does not have. So
//! a row is scored as a ratio: LigninDB's value over RocksDB's for the
//! same row in the same run, against the target and the accept over the
//! plan's RocksDB value. A row whose RocksDB value the plan only
//! estimated, or did not measure, is scored by its value. Every row is
//! lower-is-better.
//!
//! [`TARGETS`] holds every row of the table. The rows that no report of
//! this harness measures are listed by the score with what measures them
//! instead.

use std::fmt::Write as _;

use super::{Engine, Report, Row};

/// One row of the plan's prediction table.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Target {
    /// The plan's words for the row.
    pub measure: &'static str,
    /// The ids of the report rows that measure it; none for a row this
    /// harness does not measure.
    pub rows: &'static [&'static str],
    /// The plan's RocksDB value, where it measured one: the row is scored
    /// as a ratio to RocksDB in the same run. `None` scores the value.
    pub rocksdb: Option<f64>,
    /// The target, in the unit of the rows.
    pub target: f64,
    /// The accept, the edge of the predicted range, in the same unit.
    pub accept: f64,
    /// For a row no report measures, why or what does.
    pub elsewhere: &'static str,
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

    /// How `value` scores against the target and the accept.
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

const NOT_HERE: &str = "";

/// The plan's prediction table, row by row, in its order.
pub const TARGETS: &[Target] = &[
    Target {
        measure: "Bytes logged per transition",
        rows: &["bytes_logged_per_transition"],
        rocksdb: Some(23_900.0),
        target: 2_400.0,
        accept: 3_200.0,
        elsewhere: NOT_HERE,
    },
    Target {
        measure: "Physical bytes written per transition",
        rows: &["physical_bytes_per_transition"],
        rocksdb: Some(56_000.0),
        target: 5_000.0,
        accept: 8_000.0,
        elsewhere: NOT_HERE,
    },
    Target {
        measure: "Wall time per transition",
        rows: &["wall_per_transition"],
        rocksdb: Some(298.0),
        target: 50.0,
        accept: 80.0,
        elsewhere: NOT_HERE,
    },
    Target {
        measure: "Keyed lookups per transition",
        rows: &["keyed_lookups_per_transition"],
        rocksdb: Some(70.0),
        target: 0.0,
        accept: 0.0,
        elsewhere: NOT_HERE,
    },
    Target {
        measure: "Hashes computed per transition, iteration two",
        rows: &[],
        rocksdb: Some(122.0),
        target: 61.0,
        accept: 70.0,
        elsewhere: "iteration two (PERF-02): not scored before stop points",
    },
    Target {
        measure: "Commit of a 20-transition block",
        rows: &["block_commit"],
        rocksdb: Some(1.0),
        target: 0.2,
        accept: 0.5,
        elsewhere: NOT_HERE,
    },
    Target {
        measure: "Write throughput, 8 threads on disjoint Merks, iteration two",
        rows: &[],
        rocksdb: None,
        target: 5.0,
        accept: 3.0,
        elsewhere: "iteration two (PERF-10): not scored before parallel writers",
    },
    Target {
        measure: "Bulk load of 1M transitions",
        rows: &["bulk_load"],
        rocksdb: Some(144.5),
        target: 25.0,
        accept: 50.0,
        elsewhere: NOT_HERE,
    },
    Target {
        measure: "Point get",
        rows: &["point_get"],
        rocksdb: Some(8.9),
        target: 2.0,
        accept: 5.0,
        elsewhere: NOT_HERE,
    },
    Target {
        measure: "Point query with proof",
        rows: &["point_query_proof"],
        rocksdb: Some(73.6),
        target: 10.0,
        accept: 20.0,
        elsewhere: NOT_HERE,
    },
    Target {
        measure: "100 references to documents, with proof",
        rows: &["references_proof"],
        rocksdb: Some(1_262.0),
        target: 450.0,
        accept: 750.0,
        elsewhere: NOT_HERE,
    },
    Target {
        measure: "Page reads per transition, state far larger than cache",
        rows: &["page_reads_per_transition"],
        rocksdb: None,
        target: 2.0,
        accept: 3.0,
        elsewhere: NOT_HERE,
    },
    Target {
        measure: "Size after 1M transitions, random documents",
        rows: &["size_on_disk", "size_on_disk_at_end"],
        rocksdb: Some(1_010_000_000.0),
        target: 850_000_000.0,
        accept: 1_000_000_000.0,
        elsewhere: NOT_HERE,
    },
    Target {
        measure: "Checkpoint",
        rows: &["checkpoint"],
        rocksdb: Some(10.0),
        target: 15.0,
        accept: 40.0,
        elsewhere: NOT_HERE,
    },
    Target {
        measure: "Reopen",
        rows: &["reopen"],
        rocksdb: Some(31.0),
        target: 20.0,
        accept: 60.0,
        elsewhere: NOT_HERE,
    },
    Target {
        measure: "Crash recovery, 256 MB of log",
        rows: &[],
        rocksdb: None,
        target: 1.0,
        accept: 5.0,
        elsewhere: "s, absolute: lignin-store's ignored test \
                    `recovery_after_256_mb_of_log_completes_within_5_s` (STORE-07)",
    },
    Target {
        measure: "Root hash, proof bytes, OperationCost",
        rows: &[],
        rocksdb: None,
        target: 0.0,
        accept: 0.0,
        elsewhere: "identical: the golden corpus replayed on LigninDB (VER-02), and the root \
                    hashes and proof sizes of both engines in every report",
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
    /// else the row's value. `None` for a ratio row of a report without
    /// RocksDB.
    pub scored: Option<f64>,
}

/// The rows of `report` that the plan's table scores: every LigninDB row
/// whose id a [`Target`] names, in the table's order and then the
/// report's.
pub fn scored(report: &Report) -> Vec<Scored<'_>> {
    let engine = |name: &str| report.engines.iter().find(|engine| engine.engine == name);
    let Some(lignindb) = engine(super::LigninDbEngine::NAME) else {
        return Vec::new();
    };
    let rocksdb = engine(super::RocksDbEngine::NAME);
    let mut scored = Vec::new();
    for target in TARGETS {
        for row in lignindb
            .rows
            .iter()
            .filter(|row| target.rows.contains(&row.id.as_str()))
        {
            let rocksdb = rocksdb.and_then(|engine| {
                engine
                    .rows
                    .iter()
                    .find(|other| other.id == row.id && other.phase == row.phase)
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

/// The score of `report` as a Markdown table: for each scored row the
/// plan's target and accept (as ratios to RocksDB where the row is scored
/// so), LigninDB's value, RocksDB's in the same run, the scored value and
/// where it stands; then the rows of the table that the report does not
/// measure, with what measures them.
pub fn score(report: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "| Measure | Row | Phase | Target | Accept | LigninDB | RocksDB, same run | Scored | |"
    );
    let _ = writeln!(
        out,
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- |"
    );
    let rows = scored(report);
    for one in &rows {
        let target = one.target;
        let (goal, edge, how) = match (target.target_ratio(), target.accept_ratio()) {
            (Some(goal), Some(edge)) => (goal, edge, "x RocksDB"),
            _ => (target.target, target.accept, one.row.unit.as_str()),
        };
        let rocksdb = one
            .rocksdb
            .map_or_else(|| "-".to_string(), |row| number(row.value));
        let (scored, verdict) = match one.scored {
            Some(value) => (number(value), target.verdict(value).to_string()),
            None if one.rocksdb.is_some() => ("-".to_string(), "RocksDB's row is 0".to_string()),
            None => ("-".to_string(), "no RocksDB in the run".to_string()),
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
    let mut missing = TARGETS
        .iter()
        .filter(|target| !rows.iter().any(|one| one.target == *target))
        .peekable();
    if missing.peek().is_some() {
        let _ = writeln!(out, "\nNot in this report:\n");
        for target in missing {
            let elsewhere = match target.elsewhere {
                NOT_HERE => "measured by another kind of report (`run`, `reads` or `writes`)",
                elsewhere => elsewhere,
            };
            let _ = writeln!(out, "- {}: {elsewhere}", target.measure);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::report::tests::{engine, report, row};

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

    /// Every row of the plan's table is in it once, in its order, and a
    /// row that no report of the harness measures says what measures it.
    #[test]
    fn the_table_holds_every_row_of_the_plan() {
        let measures: Vec<_> = TARGETS.iter().map(|target| target.measure).collect();
        assert_eq!(measures.len(), 17);
        assert_eq!(measures[0], "Bytes logged per transition");
        assert_eq!(measures[16], "Root hash, proof bytes, OperationCost");
        for target in TARGETS {
            assert_eq!(
                target.rows.is_empty(),
                target.elsewhere != NOT_HERE,
                "{target:?}"
            );
            assert!(target.target <= target.accept || target.measure.contains("threads"));
        }
    }

    /// LigninDB's rows are scored as their ratio to RocksDB's same row of
    /// the same report, or by their value; a RocksDB row of 0 gives no
    /// ratio; the table names the rows the report does not measure.
    #[test]
    fn a_report_is_scored_row_by_row() {
        let both = report(vec![
            engine(
                "rocksdb",
                vec![
                    row("wall_per_transition", "create", 500.0),
                    row("point_get", "warm", 20.0),
                    row("keyed_lookups_per_transition", "create", 50.0),
                    row("bytes_logged_per_transition", "create", 0.0),
                ],
            ),
            engine(
                "lignindb",
                vec![
                    row("wall_per_transition", "create", 1_100.0),
                    row("point_get", "warm", 10.0),
                    row("keyed_lookups_per_transition", "create", 0.0),
                    row("bytes_logged_per_transition", "create", 4_500.0),
                    row("page_reads_per_transition", "create", 2.8),
                    row("hashes_per_transition", "create", 159.0),
                ],
            ),
        ]);
        let rows = scored(&both);
        let found: Vec<_> = rows
            .iter()
            .map(|one| (one.row.id.as_str(), one.scored))
            .collect();
        assert_eq!(
            found,
            vec![
                ("bytes_logged_per_transition", None),
                ("wall_per_transition", Some(2.2)),
                ("keyed_lookups_per_transition", Some(0.0)),
                ("point_get", Some(0.5)),
                ("page_reads_per_transition", Some(2.8)),
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
        assert!(text.contains("- Crash recovery, 256 MB of log: s, absolute"));
        assert!(text.contains("- Reopen: measured by another kind of report"));
        assert!(!text.contains("- Point get:"));

        let alone = report(vec![engine(
            "lignindb",
            vec![row("point_get", "warm", 10.0)],
        )]);
        assert_eq!(scored(&alone)[0].scored, None);
        assert!(score(&alone).contains("| - | - | no RocksDB in the run |"));
        let rocksdb_alone = report(vec![engine("rocksdb", vec![row("point_get", "warm", 1.0)])]);
        assert!(scored(&rocksdb_alone).is_empty());
        assert!(score(&rocksdb_alone).contains("- Point get: measured by another kind"));
    }
}
