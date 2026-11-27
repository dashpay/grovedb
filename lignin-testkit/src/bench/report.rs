//! The machine-readable result of a run, and how runs are compared.
//!
//! A run writes one JSON document, a [`Report`]: the machine, the commit,
//! the workload and seed, and per engine every row with its unit, the raw
//! samples behind it, and its ratio to RocksDB in the same run. PERF-04,
//! PERF-05 and PERF-09 read it.

use serde::{Deserialize, Serialize};

use super::{
    engine::{Counters, Engine, NodeAnatomy},
    machine::{Machine, StorageReads},
    BenchError,
};

/// The `schema` field of a report this version writes.
pub const SCHEMA: &str = "lignin-bench/1";

/// One run of the harness.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Report {
    /// Always [`SCHEMA`].
    pub schema: String,
    /// When the run started, in seconds since the Unix epoch.
    pub started_at_unix: u64,
    /// The machine.
    pub machine: Machine,
    /// The system's load averages (1, 5, 15 minutes) before the run.
    pub load_average_before: Option<[f64; 3]>,
    /// The system's load averages after the run.
    pub load_average_after: Option<[f64; 3]>,
    /// The commit of the source the harness was built from, `-dirty` if it
    /// had changes.
    pub commit: Option<String>,
    /// The GroveVersion the run applied.
    pub grove_protocol_version: u32,
    /// The workload, its seed and its sizes.
    pub workload: WorkloadSpec,
    /// How the run measured.
    pub method: Method,
    /// One entry per engine, in the order they ran.
    pub engines: Vec<EngineReport>,
}

/// The workload a run applied.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadSpec {
    /// The workload's name, as [`crate::workload::Workload::name`] gives it.
    pub name: String,
    /// The seed.
    pub seed: u64,
    /// Documents created before anything is measured at size.
    pub documents: u64,
    /// Owners the documents are spread over.
    pub owners: u64,
    /// Bytes of a document body.
    pub document_size: u32,
    /// State transitions per block.
    pub transitions_per_block: u32,
    /// Documents created after the load as the phase `churn`, before the
    /// measured creates. A report from before the phase has none.
    #[serde(default)]
    pub churn: u64,
    /// Documents created after the load, measured as the write rows.
    pub measured_creates: u64,
    /// Document replacements after those.
    pub updates: u64,
    /// Document deletions after those.
    pub deletes: u64,
}

/// How a run measured: sample counts and the methods it could use here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Method {
    /// Timed point gets and point proofs on a warm database.
    pub point_reads: u32,
    /// Timed proofs of 100 references on a warm database.
    pub reference_reads: u32,
    /// Reads made before the timed ones, to warm the caches.
    pub warmup_reads: u32,
    /// Cold reads of each kind, each after its own reopen.
    pub cold_reads: u32,
    /// Checkpoints taken.
    pub checkpoints: u32,
    /// Reopens timed.
    pub reopens: u32,
    /// How a cold read was made cold.
    pub cold_method: super::machine::ColdMethod,
    /// Whether `perf stat` counted instructions beside wall time.
    pub perf_instructions: bool,
    /// Whether thread CPU time was read beside wall time.
    pub thread_cpu_time: bool,
    /// What the engines were asked beyond their defaults, each as
    /// `KEY=VALUE`. Empty in a run that asked nothing, and in a report
    /// from before there were settings.
    #[serde(default)]
    pub engine_settings: Vec<String>,
}

/// What one engine measured.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EngineReport {
    /// The engine's name, such as `rocksdb`.
    pub engine: String,
    /// The root hash after the load, hex.
    pub root_hash_after_load: String,
    /// The root hash at the end of the run, hex.
    pub root_hash_at_end: String,
    /// The rows.
    pub rows: Vec<Row>,
    /// Raw totals per write phase, from which the per-transition rows are
    /// computed.
    pub phases: Vec<PhaseTotals>,
    /// Files in the database directory after the load, before a flush.
    /// `None` in a report of a measurement that did not take it: one of
    /// the warm reads alone.
    pub disk_after_load: Option<DiskUsage>,
    /// The same after `flush`.
    pub disk_after_flush: Option<DiskUsage>,
}

impl EngineReport {
    /// The row `id` of `phase`.
    pub fn row(&self, id: &str, phase: &str) -> Option<&Row> {
        self.rows
            .iter()
            .find(|row| row.id == id && row.phase == phase)
    }
}

/// Totals over one write phase.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PhaseTotals {
    /// `load`, `create`, `update` or `delete`.
    pub phase: String,
    /// State transitions applied.
    pub transitions: u64,
    /// Blocks committed.
    pub blocks: u64,
    /// Wall time applying operations, commits excluded, in seconds.
    pub apply_seconds: f64,
    /// Wall time committing, in seconds.
    pub commit_seconds: f64,
    /// Key and value bytes the operations carried: each key plus its
    /// serialized element.
    pub logical_bytes: u64,
    /// `OperationCost::seek_count`, summed.
    pub seek_count: u64,
    /// `OperationCost::storage_loaded_bytes`, summed.
    pub storage_loaded_bytes: u64,
    /// `OperationCost::hash_node_calls`, summed.
    pub hash_node_calls: u64,
    /// `OperationCost::storage_cost.added_bytes`, summed.
    pub storage_added_bytes: u64,
    /// `OperationCost::storage_cost.replaced_bytes`, summed.
    pub storage_replaced_bytes: u64,
    /// The engine's counters over the phase.
    pub counters: Counters,
    /// How the node records written in the phase divide up, for an engine
    /// that writes whole node records.
    pub node_anatomy: Option<NodeAnatomy>,
    /// Bytes the operating system counted as written by the process
    /// (`/proc/self/io` `write_bytes`), where it counts them.
    pub process_write_bytes: Option<u64>,
    /// What the operating system counted of the process's reads over the
    /// phase, where it counts them. A report from before the field reads
    /// back with no count.
    #[serde(default)]
    pub storage_reads: StorageReads,
}

/// Files under a database directory, in its subdirectories too.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskUsage {
    /// Files.
    pub files: u64,
    /// Their lengths, summed.
    pub bytes: u64,
    /// Space the file system allocated for them, where it says.
    pub allocated_bytes: Option<u64>,
    /// Lengths by file extension (`sst`, `log`, and `""` for none).
    pub by_extension: std::collections::BTreeMap<String, u64>,
}

/// Whether a row's value depends on the machine or on the schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RowKind {
    /// Bytes and counts the workload fixes: equal in every run of one
    /// commit with the same settings (workload, seed, sizes and read
    /// counts), on any machine.
    Count,
    /// Bytes and sizes that also depend on when the engine's background
    /// work ran, or on what the rest of the process wrote: reported, not
    /// expected to repeat.
    Measured,
    /// Wall time.
    Time,
}

impl RowKind {
    /// Whether a row of kind `self` in one run and `other` in another is
    /// the same quantity: a row reclassified from `Count` to `Measured` is.
    fn same_family(self, other: RowKind) -> bool {
        (self == RowKind::Time) == (other == RowKind::Time)
    }

    /// Of two runs' kinds for one row, the one the comparison holds it to;
    /// the first run's where the two are not [`RowKind::same_family`].
    fn looser(self, other: RowKind) -> RowKind {
        if other == RowKind::Measured && self.same_family(other) {
            other
        } else {
            self
        }
    }
}

/// One measured row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Row {
    /// The measure, such as `bytes_logged_per_transition`.
    pub id: String,
    /// Where in the run it was measured: `load`, `create`, `update`,
    /// `delete`, `at-size` (right after the load), `flushed` (after a flush
    /// at size), `warm` or `cold`.
    pub phase: String,
    /// The unit of `value` and `samples`.
    pub unit: String,
    /// Count, measured or time.
    pub kind: RowKind,
    /// The value: a total divided by its count for count and measured
    /// rows, the median of `samples` for time rows and proof sizes.
    pub value: f64,
    /// The samples behind `value`, in the order taken.
    pub samples: Vec<f64>,
    /// Their summary.
    pub summary: Option<Summary>,
    /// User-space instructions per sample, where `perf` counted them.
    pub instructions_per_sample: Option<f64>,
    /// Thread CPU time per sample in the row's unit, where the system
    /// reports it.
    pub cpu_per_sample: Option<f64>,
    /// Bytes the operating system read from storage per sample, for cold
    /// reads where it counts them.
    pub storage_read_bytes_per_sample: Option<f64>,
    /// The value the plan recorded for this row, if any.
    pub recorded: Option<Recorded>,
    /// `value` over the recorded value.
    pub ratio_to_recorded: Option<f64>,
    /// `value` over RocksDB's value for the same row in the same run.
    pub ratio_to_rocksdb: Option<f64>,
    /// Where the number comes from.
    pub source: String,
}

/// The order statistics of a row's samples.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// Samples.
    pub count: usize,
    /// The median.
    pub median: f64,
    /// The mean.
    pub mean: f64,
    /// The smallest sample.
    pub min: f64,
    /// The 10th percentile.
    pub p10: f64,
    /// The 90th percentile.
    pub p90: f64,
    /// The largest sample.
    pub max: f64,
}

impl Summary {
    /// The summary of `samples`, or `None` if there are none.
    pub fn of(samples: &[f64]) -> Option<Summary> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        let at = |fraction: f64| {
            let position = fraction * (sorted.len() - 1) as f64;
            let low = position.floor() as usize;
            let high = position.ceil() as usize;
            sorted[low] + (sorted[high] - sorted[low]) * (position - low as f64)
        };
        Some(Summary {
            count: sorted.len(),
            median: at(0.5),
            mean: sorted.iter().sum::<f64>() / sorted.len() as f64,
            min: sorted[0],
            p10: at(0.1),
            p90: at(0.9),
            max: sorted[sorted.len() - 1],
        })
    }
}

/// A value the plan recorded on the RocksDB engine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recorded {
    /// The value, or the middle of a recorded range.
    pub value: f64,
    /// The range's ends, where the plan gave a range.
    pub range: Option<[f64; 2]>,
    /// The plan's words.
    pub text: String,
}

/// A row id, its phase, the recorded value, its range and the plan's words.
pub type RecordedEntry = (
    &'static str,
    &'static str,
    f64,
    Option<[f64; 2]>,
    &'static str,
);

/// The values the plan's sections "Why" and "Performance targets" recorded
/// on RocksDB (Apple M5 Ultra; one million 400-byte documents plus index
/// references, one document and one reference per state transition, 20
/// per block), and the BASE-02 brief's two proof sizes. Keyed by row id
/// and phase. Times were measured on a machine the project does not have,
/// so their ratio is information, not a test.
pub const RECORDED: &[RecordedEntry] = &[
    (
        "logical_bytes_per_transition",
        "load",
        551.0,
        None,
        "A state transition adds 551 bytes of keys and values.",
    ),
    (
        "node_records_per_transition",
        "load",
        58.8,
        None,
        "Merk rewrites 58.8 node records for it",
    ),
    (
        "bytes_handed_per_transition",
        "load",
        23_900.0,
        None,
        "23.9 KB handed to RocksDB",
    ),
    (
        "bytes_logged_per_transition",
        "load",
        23_900.0,
        None,
        "Bytes logged per transition: 23.9 KB",
    ),
    (
        "physical_bytes_per_transition",
        "load",
        56_000.0,
        None,
        "which then writes about 56 KB physically (log, flush, compaction)",
    ),
    (
        "keyed_lookups_per_transition",
        "load",
        70.0,
        None,
        "Keyed lookups per transition: 70",
    ),
    (
        "hashes_per_transition",
        "load",
        122.0,
        None,
        "Hashes computed per transition: about 122",
    ),
    (
        "point_proof_bytes",
        "warm",
        2_848.0,
        None,
        "a 2,848-byte point proof (BASE-02 brief)",
    ),
    (
        "references_proof_bytes",
        "warm",
        49_835.0,
        None,
        "a 49,835-byte proof of 100 references to documents (BASE-02 brief)",
    ),
    (
        "size_on_disk",
        "flushed",
        1_010_000_000.0,
        None,
        "Size after 1M transitions, random documents: 1.01 GB",
    ),
    (
        "wall_per_transition",
        "create",
        298.0,
        None,
        "Wall time per transition: 298 µs",
    ),
    (
        "block_commit",
        "create",
        1.0,
        None,
        "Commit of a 20-transition block: about 1.0 ms",
    ),
    (
        "bulk_load",
        "load",
        144.5,
        Some([138.0, 151.0]),
        "Bulk load of 1M transitions: 138 to 151 s",
    ),
    ("point_get", "warm", 8.9, None, "Point get: 8.9 µs"),
    (
        "point_query_proof",
        "warm",
        73.6,
        None,
        "Point query with proof: 73.6 µs",
    ),
    (
        "references_proof",
        "warm",
        1_262.0,
        None,
        "100 references to documents, with proof: 1,262 µs",
    ),
    ("checkpoint", "flushed", 10.0, None, "Checkpoint: 10 ms"),
    ("reopen", "flushed", 31.0, None, "Reopen: 31 ms"),
];

/// The plan's value for row `id` of `phase`.
pub fn recorded(id: &str, phase: &str) -> Option<Recorded> {
    RECORDED
        .iter()
        .find(|(row, row_phase, ..)| *row == id && *row_phase == phase)
        .map(|(_, _, value, range, text)| Recorded {
            value: *value,
            range: *range,
            text: (*text).to_string(),
        })
}

impl Report {
    /// Sets every row's ratio to RocksDB's same row in this run.
    pub fn fill_ratios(&mut self) {
        let Some(rocksdb) = self
            .engines
            .iter()
            .find(|engine| engine.engine == super::RocksDbEngine::NAME)
            .cloned()
        else {
            return;
        };
        for engine in &mut self.engines {
            for row in &mut engine.rows {
                row.ratio_to_rocksdb = rocksdb
                    .row(&row.id, &row.phase)
                    .and_then(|base| ratio(row.value, base.value));
            }
        }
    }

    /// Reads a report from JSON.
    pub fn from_json(text: &str) -> Result<Report, BenchError> {
        let report: Report = serde_json::from_str(text)
            .map_err(|error| BenchError::Check(format!("not a bench report: {error}")))?;
        if report.schema != SCHEMA {
            return Err(BenchError::Check(format!(
                "report schema {} is not {SCHEMA}",
                report.schema
            )));
        }
        Ok(report)
    }

    /// The report as pretty-printed JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a report serializes")
    }
}

/// `value / base`, if `base` is not zero.
pub fn ratio(value: f64, base: f64) -> Option<f64> {
    (base != 0.0).then(|| value / base)
}

/// How one row compares between two runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Agreement {
    /// The engine.
    pub engine: String,
    /// The row id.
    pub id: String,
    /// The row's phase.
    pub phase: String,
    /// The looser of the two runs' kinds: measured if one run has the row
    /// as measured and the other as count or measured; otherwise the first
    /// run's.
    pub kind: RowKind,
    /// The unit.
    pub unit: String,
    /// The value in the first run.
    pub first: f64,
    /// The value in the second run.
    pub second: f64,
    /// `(second - first) / first`.
    pub change: Option<f64>,
    /// For a time row, the same change in user-space instructions per
    /// sample, where both runs counted them.
    pub instructions_change: Option<f64>,
}

impl Agreement {
    /// Whether the two runs' instruction counts agree within `tolerance`;
    /// `None` where they were not counted.
    pub fn instructions_within(&self, tolerance: f64) -> Option<bool> {
        self.instructions_change
            .map(|change| change.abs() <= tolerance)
    }

    /// Whether the two runs agree within `tolerance`, a fraction.
    pub fn within(&self, tolerance: f64) -> bool {
        match self.change {
            Some(change) => change.abs() <= tolerance,
            None => self.first == self.second,
        }
    }
}

/// Refuses two runs that did not apply the same thing: a different
/// workload, seed, size or GroveVersion makes their rows incomparable.
pub fn comparable(first: &Report, second: &Report) -> Result<(), BenchError> {
    if first.workload != second.workload {
        return Err(BenchError::Check(format!(
            "the runs applied different workloads: {:?} and {:?}",
            first.workload, second.workload
        )));
    }
    if first.grove_protocol_version != second.grove_protocol_version {
        return Err(BenchError::Check(format!(
            "the runs applied GroveVersion {} and {}",
            first.grove_protocol_version, second.grove_protocol_version
        )));
    }
    for engine in &first.engines {
        let Some(peer) = second
            .engines
            .iter()
            .find(|peer| peer.engine == engine.engine)
        else {
            continue;
        };
        for row in &engine.rows {
            if let Some(other) = peer.row(&row.id, &row.phase)
                && (other.unit != row.unit || !other.kind.same_family(row.kind))
            {
                return Err(BenchError::Check(format!(
                    "{} {} in {} is {:?} in {} in one run and {:?} in {} in the other",
                    engine.engine, row.id, row.phase, row.kind, row.unit, other.kind, other.unit
                )));
            }
        }
    }
    Ok(())
}

/// The rows, as engine, id and phase, that one run measured and the other
/// did not.
pub fn unmatched(first: &Report, second: &Report) -> Vec<(String, String, String)> {
    let missing_from = |from: &Report, other: &Report| {
        from.engines
            .iter()
            .flat_map(|engine| {
                let peer = other
                    .engines
                    .iter()
                    .find(|peer| peer.engine == engine.engine);
                engine
                    .rows
                    .iter()
                    .filter(move |row| {
                        peer.and_then(|peer| peer.row(&row.id, &row.phase))
                            .is_none()
                    })
                    .map(|row| (engine.engine.clone(), row.id.clone(), row.phase.clone()))
            })
            .collect::<Vec<_>>()
    };
    let mut rows = missing_from(first, second);
    rows.extend(missing_from(second, first));
    rows
}

/// Every row two runs both measured, in the first run's order.
pub fn compare(first: &Report, second: &Report) -> Vec<Agreement> {
    let mut agreements = Vec::new();
    for engine in &first.engines {
        let Some(other) = second
            .engines
            .iter()
            .find(|other| other.engine == engine.engine)
        else {
            continue;
        };
        for row in &engine.rows {
            if let Some(other_row) = other.row(&row.id, &row.phase) {
                agreements.push(Agreement {
                    engine: engine.engine.clone(),
                    id: row.id.clone(),
                    phase: row.phase.clone(),
                    kind: row.kind.looser(other_row.kind),
                    unit: row.unit.clone(),
                    first: row.value,
                    second: other_row.value,
                    change: ratio(other_row.value - row.value, row.value),
                    instructions_change: row
                        .instructions_per_sample
                        .zip(other_row.instructions_per_sample)
                        .and_then(|(first, second)| ratio(second - first, first)),
                });
            }
        }
    }
    agreements
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::bench::machine::ColdMethod;

    pub(crate) fn row(id: &str, phase: &str, value: f64) -> Row {
        Row {
            id: id.to_string(),
            phase: phase.to_string(),
            unit: "us".to_string(),
            kind: RowKind::Time,
            value,
            samples: vec![value],
            summary: Summary::of(&[value]),
            instructions_per_sample: None,
            cpu_per_sample: None,
            storage_read_bytes_per_sample: None,
            recorded: recorded(id, phase),
            ratio_to_recorded: None,
            ratio_to_rocksdb: None,
            source: "test".to_string(),
        }
    }

    pub(crate) fn engine(name: &str, rows: Vec<Row>) -> EngineReport {
        EngineReport {
            engine: name.to_string(),
            root_hash_after_load: String::new(),
            root_hash_at_end: String::new(),
            rows,
            phases: vec![],
            disk_after_load: Some(DiskUsage::default()),
            disk_after_flush: Some(DiskUsage::default()),
        }
    }

    pub(crate) fn report(engines: Vec<EngineReport>) -> Report {
        Report {
            schema: SCHEMA.to_string(),
            started_at_unix: 0,
            machine: Machine::default(),
            load_average_before: None,
            load_average_after: None,
            commit: None,
            grove_protocol_version: 0,
            workload: WorkloadSpec {
                name: "documents".to_string(),
                seed: 1,
                documents: 1,
                owners: 1,
                document_size: 1,
                transitions_per_block: 1,
                churn: 0,
                measured_creates: 0,
                updates: 0,
                deletes: 0,
            },
            method: Method {
                point_reads: 0,
                reference_reads: 0,
                warmup_reads: 0,
                cold_reads: 0,
                checkpoints: 0,
                reopens: 0,
                cold_method: ColdMethod::Reopen,
                perf_instructions: false,
                thread_cpu_time: false,
                engine_settings: Vec::new(),
            },
            engines,
        }
    }

    #[test]
    fn a_table_says_when_no_row_of_it_has_a_count_of_instructions() {
        let second_line = |report: &Report| {
            let table = crate::bench::cli::table(report);
            table.lines().nth(1).map(str::to_string)
        };
        let says = |report: &Report, what: &str| {
            second_line(report).is_some_and(|line| {
                line.starts_with("instr/sample is empty") && line.contains(what)
            })
        };
        // With no row, and with rows that have no count.
        let uncounted = vec![engine("rocksdb", vec![row("point_get", "warm", 1.0)])];
        for engines in [Vec::new(), uncounted] {
            let mut run = report(engines);
            assert!(says(&run, "`run --perf` counts them"));
            run.method.perf_instructions = true;
            assert!(says(&run, "asked for instructions"));
        }
        // One row with a count, the last of the second engine: the rows
        // follow the first line.
        let mut counted = row("point_query_proof", "warm", 2.0);
        counted.instructions_per_sample = Some(0.0);
        let first = engine("rocksdb", vec![row("point_get", "warm", 1.0)]);
        let second = engine("lignindb", vec![row("point_get", "warm", 1.0), counted]);
        let mut run = report(vec![first, second]);
        for asked in [false, true] {
            run.method.perf_instructions = asked;
            assert!(second_line(&run).is_some_and(|line| line.starts_with("engine")));
        }
    }

    #[test]
    fn summaries_interpolate_percentiles() {
        assert_eq!(Summary::of(&[]), None);
        let summary = Summary::of(&[5.0, 1.0, 3.0, 2.0, 4.0]).unwrap();
        assert_eq!(summary.count, 5);
        assert_eq!(summary.median, 3.0);
        assert_eq!(summary.mean, 3.0);
        assert_eq!(summary.min, 1.0);
        assert_eq!(summary.max, 5.0);
        assert!((summary.p10 - 1.4).abs() < 1e-9);
        assert!((summary.p90 - 4.6).abs() < 1e-9);
        assert_eq!(Summary::of(&[1.0, 2.0]).unwrap().median, 1.5);
    }

    #[test]
    fn every_recorded_row_is_found_once() {
        for (id, phase, value, _, _) in RECORDED {
            let found = recorded(id, phase).unwrap();
            assert_eq!(found.value, *value);
            assert_eq!(
                RECORDED
                    .iter()
                    .filter(|(other, other_phase, ..)| other == id && other_phase == phase)
                    .count(),
                1
            );
        }
        assert_eq!(
            recorded("bulk_load", "load").unwrap().range,
            Some([138.0, 151.0])
        );
        assert_eq!(recorded("bulk_load", "create"), None);
    }

    #[test]
    fn ratios_are_to_rocksdb_in_the_same_run() {
        let mut run = report(vec![
            engine(
                "rocksdb",
                vec![row("point_get", "warm", 10.0), row("zero", "warm", 0.0)],
            ),
            engine(
                "lignindb",
                vec![
                    row("point_get", "warm", 2.5),
                    row("zero", "warm", 1.0),
                    row("only_here", "warm", 1.0),
                ],
            ),
        ]);
        run.fill_ratios();
        let lignin = &run.engines[1];
        assert_eq!(
            lignin.row("point_get", "warm").unwrap().ratio_to_rocksdb,
            Some(0.25)
        );
        assert_eq!(lignin.row("zero", "warm").unwrap().ratio_to_rocksdb, None);
        assert_eq!(
            lignin.row("only_here", "warm").unwrap().ratio_to_rocksdb,
            None
        );
        assert_eq!(
            run.engines[0]
                .row("point_get", "warm")
                .unwrap()
                .ratio_to_rocksdb,
            Some(1.0)
        );

        let mut without = report(vec![engine(
            "lignindb",
            vec![row("point_get", "warm", 1.0)],
        )]);
        without.fill_ratios();
        assert_eq!(without.engines[0].rows[0].ratio_to_rocksdb, None);
    }

    #[test]
    fn runs_compare_row_by_row() {
        let first = report(vec![
            engine(
                "rocksdb",
                vec![
                    row("point_get", "warm", 10.0),
                    row("reopen", "at-size", 0.0),
                    row("gone", "warm", 1.0),
                ],
            ),
            engine("other", vec![row("point_get", "warm", 1.0)]),
        ]);
        let second = report(vec![engine(
            "rocksdb",
            vec![
                row("point_get", "warm", 10.9),
                row("reopen", "at-size", 0.0),
            ],
        )]);
        let agreements = compare(&first, &second);
        assert_eq!(agreements.len(), 2);
        assert!((agreements[0].change.unwrap() - 0.09).abs() < 1e-9);
        assert!(agreements[0].within(0.10));
        assert!(!agreements[0].within(0.05));
        assert_eq!(agreements[1].change, None);
        assert!(agreements[1].within(0.0));
        assert_eq!(agreements[0].instructions_change, None);
        assert_eq!(agreements[0].instructions_within(0.1), None);

        let mut counted = first.clone();
        counted.engines[0].rows[0].instructions_per_sample = Some(1000.0);
        let mut recounted = second.clone();
        recounted.engines[0].rows[0].instructions_per_sample = Some(1150.0);
        let agreement = &compare(&counted, &recounted)[0];
        assert!((agreement.instructions_change.unwrap() - 0.15).abs() < 1e-9);
        assert_eq!(agreement.instructions_within(0.10), Some(false));
        assert_eq!(agreement.instructions_within(0.20), Some(true));
    }

    #[test]
    fn only_runs_of_the_same_workload_compare() {
        let first = report(vec![engine("rocksdb", vec![row("point_get", "warm", 1.0)])]);
        let mut second = first.clone();
        assert!(comparable(&first, &second).is_ok());
        assert!(unmatched(&first, &second).is_empty());

        second.engines[0].rows.push(row("reopen", "flushed", 1.0));
        second
            .engines
            .push(engine("lignindb", vec![row("point_get", "warm", 1.0)]));
        assert_eq!(
            unmatched(&first, &second),
            vec![
                (
                    "rocksdb".to_string(),
                    "reopen".to_string(),
                    "flushed".to_string()
                ),
                (
                    "lignindb".to_string(),
                    "point_get".to_string(),
                    "warm".to_string()
                ),
            ]
        );

        let mut other_unit = second.clone();
        other_unit.engines[0].rows[0].unit = "ms".to_string();
        assert_eq!(
            comparable(&first, &other_unit).unwrap_err().to_string(),
            "rocksdb point_get in warm is Time in us in one run and Time in ms in the other"
        );
        let mut other_kind = second.clone();
        other_kind.engines[0].rows[0].kind = RowKind::Count;
        assert!(comparable(&first, &other_kind).is_err());
        other_kind.engines[0].rows[0].kind = RowKind::Measured;
        assert!(comparable(&first, &other_kind).is_err());

        second.grove_protocol_version = 4;
        assert_eq!(
            comparable(&first, &second).unwrap_err().to_string(),
            "the runs applied GroveVersion 0 and 4"
        );
        second.workload.seed = 2;
        assert!(comparable(&first, &second)
            .unwrap_err()
            .to_string()
            .starts_with("the runs applied different workloads"));
    }

    #[test]
    fn a_row_reclassified_as_measured_compares_as_measured() {
        let of_kind = |kind| {
            let mut row = row("size_on_disk", "flushed", 100.0);
            row.unit = "B".to_string();
            row.kind = kind;
            report(vec![engine("rocksdb", vec![row])])
        };
        let older = of_kind(RowKind::Count);
        let newer = of_kind(RowKind::Measured);
        assert!(comparable(&older, &newer).is_ok());
        assert_eq!(compare(&older, &newer)[0].kind, RowKind::Measured);
        assert_eq!(compare(&newer, &older)[0].kind, RowKind::Measured);
        assert_eq!(compare(&older, &older)[0].kind, RowKind::Count);
        assert_eq!(
            compare(&of_kind(RowKind::Time), &newer)[0].kind,
            RowKind::Time
        );
        assert_eq!(
            comparable(&newer, &of_kind(RowKind::Time))
                .unwrap_err()
                .to_string(),
            "rocksdb size_on_disk in flushed is Measured in B in one run and Time in B in the other"
        );
        assert_eq!(
            serde_json::to_string(&RowKind::Measured).unwrap(),
            "\"measured\""
        );
    }

    #[test]
    fn reports_round_trip_through_json() {
        let run = report(vec![engine(
            "rocksdb",
            vec![row("point_get", "warm", 10.0)],
        )]);
        assert_eq!(Report::from_json(&run.to_json()).unwrap(), run);

        let mut other = run.clone();
        other.schema = "something-else".to_string();
        let error = Report::from_json(&other.to_json()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "report schema something-else is not lignin-bench/1"
        );
        assert!(Report::from_json("{")
            .unwrap_err()
            .to_string()
            .starts_with("not a bench report"));
    }
}
