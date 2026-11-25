//! The benchmark suite: one harness for every row of the plan's prediction
//! table, on any engine.
//!
//! A run applies the plan's reference workload,
//! [`Documents::reference`](crate::workload::drive::Documents::reference),
//! through [`crate::workload::apply()`] to an [`Engine`], and writes one
//! [`Report`] as JSON: the machine, the commit, the workload and seed, and
//! per engine every row with its unit, the raw samples behind it, the value
//! the plan recorded and the ratio to RocksDB in the same run. The
//! `lignin-bench` binary runs it and compares two reports.
//!
//! # Rows
//!
//! Count rows are equal in every run of one commit with the same
//! [`Settings`], on any machine.
//! Measured rows ([`RowKind::Measured`]) are bytes and sizes that are not:
//! `bytes_logged_per_transition`, `physical_bytes_per_transition`,
//! `flush_bytes_per_transition`, `compaction_bytes_per_transition`,
//! `process_write_bytes_per_transition`, `size_on_disk`, and every row an
//! engine adds for one of its [`Detail`]s. Where a log file
//! starts, what a flush or compaction writes and in which phase it is seen,
//! which obsolete files are gone, and what the rest of the process wrote
//! all follow the schedule. Time rows depend on the machine. Each row
//! carries a `source` saying where its number comes from.
//!
//! | Row | Phase | Unit | From |
//! | --- | --- | --- | --- |
//! | `logical_bytes_per_transition` | write phases | B | each key written plus its serialized element |
//! | `node_records_per_transition` | write phases | records | [`Counters::node_records`] |
//! | `bytes_handed_per_transition` | write phases | B | [`Counters::node_record_bytes`] and [`Counters::other_record_bytes`] |
//! | `bytes_logged_per_transition` | write phases | B | [`Counters::log_bytes`] |
//! | `physical_bytes_per_transition` | write phases | B | [`Counters::physical_bytes`] |
//! | `keyed_lookups_per_transition` | write phases | lookups | [`Counters::keyed_lookups`] where the engine counts them, which are its searches by key that went on below the root of a Merk; else `OperationCost::seek_count` less one per record logged |
//! | `seeks_per_transition` | write phases | seeks | `OperationCost::seek_count` |
//! | `hashes_per_transition` | write phases | calls | `OperationCost::hash_node_calls` |
//! | `engine_hashes_per_transition` | write phases | calls | [`Counters::hashes`], where the engine counts them |
//! | `node_record_*_share` | write phases | fraction | [`NodeAnatomy`], where the engine writes node records |
//! | `<detail>_per_transition` | write phases | the detail's | [`Engine::details`]: where LigninDB's logged and background bytes went |
//! | `size_on_disk` | `at-size`, `flushed` | B | file lengths in the database directory |
//! | `point_proof_bytes`, `references_proof_bytes` | `warm` | B | proof lengths |
//! | `wall_per_transition` | write phases | µs | apply time over transitions |
//! | `block_commit` | write phases | ms | transaction commit |
//! | `bulk_load` | `load` | s | apply and commit time of the load |
//! | `point_get`, `point_query_proof`, `references_proof` | `warm`, `cold` | µs | one read |
//! | `checkpoint` | `at-size`, `flushed` | ms | one checkpoint |
//! | `reopen` | `flushed`, `cold` | ms | one open |
//!
//! The write phases are `load` (the million documents), then `create`,
//! `update` and `delete` at size. `at-size` is the state right after the
//! load, `flushed` the same after a flush. [`mod@run`] says what each phase
//! does.
//!
//! GroveDB's storage layer charges one seek for every record it writes, so
//! `keyed_lookups_per_transition` takes the records the engine logged off
//! `seek_count`, leaving the lookups. A delete without cost information
//! is charged two seeks, one of them a real read, and stays counted once.
//!
//! # Warm and cold
//!
//! A warm read follows untimed warm-up reads on an open database. A cold
//! read is the first read after the database was closed and opened again;
//! where the machine allows it, every file of the database is also dropped
//! from the operating system's page cache while it is closed
//! ([`ColdMethod`]). The report says which was used.
//!
//! # Noise
//!
//! Time rows are medians of many samples. Beside wall time a row carries
//! the measuring thread's CPU time per sample where the system reports it
//! (`/proc/thread-self/schedstat`), and its user-space instructions per
//! sample when `perf` is asked for (`perf stat -e instructions:u`,
//! attached to the thread and switched on only around the measured calls).
//! A run that asks for it where `perf` counts nothing is refused at its
//! start. A row during which `perf` fails later keeps its time and has no
//! count, and a table says so where no row of it has one.

pub mod batches;
pub mod cli;
pub mod engine;
#[cfg(grovedb_engine_seam)]
pub mod lignindb;
pub mod machine;
pub mod report;
pub mod rocksdb;
pub mod run;

use std::path::PathBuf;

use grovedb_version::version::GroveVersion;

#[cfg(grovedb_engine_seam)]
pub use self::lignindb::LigninDbEngine;
pub use self::{
    engine::{Block, Counters, Detail, Engine, EngineSettings, NodeAnatomy},
    machine::{ColdMethod, Machine},
    report::{compare, Agreement, Report, Row, RowKind},
    rocksdb::RocksDbEngine,
    run::Settings,
};

/// Why a run failed.
#[derive(Debug)]
pub enum BenchError {
    /// GroveDB refused an operation.
    Grove(Box<grovedb::Error>),
    /// A file operation failed.
    Io {
        /// What the harness was doing.
        context: String,
        /// The error.
        error: std::io::Error,
    },
    /// A write-ahead log could not be read.
    Wal {
        /// The log file.
        file: String,
        /// The error.
        error: rocksdb::wal::WalError,
    },
    /// The run or its inputs are not what the harness expects.
    Check(String),
}

impl BenchError {
    pub(crate) fn io(context: impl Into<String>, error: std::io::Error) -> Self {
        BenchError::Io {
            context: context.into(),
            error,
        }
    }
}

impl std::fmt::Display for BenchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BenchError::Grove(error) => write!(f, "GroveDB: {error}"),
            BenchError::Io { context, error } => write!(f, "{context}: {error}"),
            BenchError::Wal { file, error } => write!(f, "write-ahead log {file}: {error}"),
            BenchError::Check(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for BenchError {}

impl From<grovedb::Error> for BenchError {
    fn from(error: grovedb::Error) -> Self {
        BenchError::Grove(Box::new(error))
    }
}

/// The engines a run can measure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineChoice {
    /// [`RocksDbEngine`].
    RocksDb,
    /// [`LigninDbEngine`].
    #[cfg(grovedb_engine_seam)]
    LigninDb,
}

impl EngineChoice {
    /// The engine named `name` in reports.
    pub fn named(name: &str) -> Option<EngineChoice> {
        match name {
            RocksDbEngine::NAME => Some(EngineChoice::RocksDb),
            #[cfg(grovedb_engine_seam)]
            LigninDbEngine::NAME => Some(EngineChoice::LigninDb),
            _ => None,
        }
    }
}

/// A run of the harness: what to apply, to which engines, and how to
/// describe it.
#[derive(Clone, Debug)]
pub struct RunOptions {
    /// What to apply and how much to sample.
    pub settings: Settings,
    /// The engines, measured one after another; RocksDB first, so the
    /// others' ratios have a base.
    pub engines: Vec<EngineChoice>,
    /// A label for the machine, such as `strixbox`.
    pub machine_label: Option<String>,
    /// The commit to report; found with `git` when not given.
    pub commit: Option<String>,
    /// The GroveVersion to apply.
    pub grove_version: &'static GroveVersion,
}

impl RunOptions {
    /// Refuses a setting that no engine of the run takes, and a setting
    /// that the engine which takes settings cannot take. [`run`] asks
    /// this before anything is measured.
    pub fn check_engine_settings(&self) -> Result<(), BenchError> {
        let settings = &self.settings.engine;
        if settings.is_empty() {
            return Ok(());
        }
        #[cfg(grovedb_engine_seam)]
        if self.engines.contains(&EngineChoice::LigninDb) {
            return lignindb::Tuning::new(settings).map(drop);
        }
        Err(BenchError::Check(
            "--set: no engine of this run takes a setting".to_string(),
        ))
    }
}

/// A thread big enough for GroveDB's recursion on deep paths.
pub const STACK_BYTES: usize = 64 << 20;

/// Measures every engine of `options` on a thread with [`STACK_BYTES`] of
/// stack and returns the report.
///
/// For each engine, in a fresh database:
///
/// 1. `load`: the contract set-up block (untimed), then `documents`
///    creates. Bulk load, and every count and measured row averaged over
///    the load.
/// 2. At size: `size_on_disk`, warm reads after warm-up reads, the proof
///    sizes (the first proof of each kind is verified against the root
///    hash), checkpoints, a flush, the size and checkpoints again, reopens,
///    the warm reads once more in the settled state ([`run::SETTLED`]),
///    and cold reads. Then every file is read once and the warm-up reads
///    are repeated, so the writes start warm.
/// 3. `create`, `update`, `delete`: the writes at size, sampled per block.
pub fn run(options: &RunOptions) -> Result<Report, BenchError> {
    options.check_engine_settings()?;
    on_the_benchmark_thread(options, Rows::All)
}

/// Measures the warm read rows alone, for every engine of `options`, over
/// the databases that a run with `keep` left in the scratch directory.
/// The settings are that run's seed, documents and owners; it applied no
/// creates, updates or deletes after its load. The report holds the
/// warm rows of the settled state ([`run::SETTLED`]) and nothing else,
/// and reads back and compares as any other.
pub fn reads(options: &RunOptions) -> Result<Report, BenchError> {
    options.check_engine_settings()?;
    on_the_benchmark_thread(options, Rows::WarmReads)
}

/// Which rows a call measures.
#[derive(Clone, Copy)]
enum Rows {
    /// Every row, over a fresh database.
    All,
    /// The warm reads, over a kept database.
    WarmReads,
}

fn on_the_benchmark_thread(options: &RunOptions, rows: Rows) -> Result<Report, BenchError> {
    std::fs::create_dir_all(&options.settings.scratch)
        .map_err(|error| BenchError::io("creating the scratch directory", error))?;
    let options = options.clone();
    std::thread::Builder::new()
        .name("lignin-bench".to_string())
        .stack_size(STACK_BYTES)
        .spawn(move || run_here(&options, rows))
        .map_err(|error| BenchError::io("starting the benchmark thread", error))?
        .join()
        .map_err(|_| BenchError::Check("the benchmark thread panicked".to_string()))?
}

/// What a run that asks for instruction counts is told where `perf`
/// counts none.
const NO_PERF: &str = "--perf: `perf stat -e instructions:u` counts nothing for a thread of this \
                       process here (is `perf` installed, and does `kernel.perf_event_paranoid` \
                       allow it?). Without --perf the run measures and counts no instructions";

/// Refuses a run that asks for instruction counts where `perf` counts
/// none (`works`, which is asked only then, before the run measures
/// anything).
fn counts_instructions(asked: bool, works: impl FnOnce() -> bool) -> Result<(), BenchError> {
    match asked && !works() {
        true => Err(BenchError::Check(NO_PERF.to_string())),
        false => Ok(()),
    }
}

fn run_here(options: &RunOptions, rows: Rows) -> Result<Report, BenchError> {
    options.settings.check()?;
    let settings = &options.settings;
    counts_instructions(settings.perf, || machine::Perf::works(&settings.scratch))?;
    let cold_method = ColdMethod::detect(&settings.scratch);
    let commit = options
        .commit
        .clone()
        .or_else(|| machine::source_commit(&PathBuf::from(env!("CARGO_MANIFEST_DIR"))));
    let mut report = Report {
        schema: report::SCHEMA.to_string(),
        started_at_unix: run::unix_now(),
        machine: Machine::detect(options.machine_label.clone()),
        load_average_before: machine::load_average(),
        load_average_after: None,
        commit,
        grove_protocol_version: options.grove_version.protocol_version,
        workload: settings.spec(),
        method: settings.method(cold_method, settings.perf),
        engines: Vec::new(),
    };
    for engine in &options.engines {
        let grove_version = options.grove_version;
        report.engines.push(match (engine, rows) {
            (EngineChoice::RocksDb, Rows::All) => {
                run::run_engine::<RocksDbEngine>(settings, cold_method, grove_version)?
            }
            (EngineChoice::RocksDb, Rows::WarmReads) => {
                run::read_engine::<RocksDbEngine>(settings, grove_version)?
            }
            #[cfg(grovedb_engine_seam)]
            (EngineChoice::LigninDb, Rows::All) => {
                run::run_engine::<LigninDbEngine>(settings, cold_method, grove_version)?
            }
            #[cfg(grovedb_engine_seam)]
            (EngineChoice::LigninDb, Rows::WarmReads) => {
                run::read_engine::<LigninDbEngine>(settings, grove_version)?
            }
        });
    }
    report.load_average_after = machine::load_average();
    report.fill_ratios();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_that_asks_for_instructions_is_refused_where_perf_counts_none() {
        // A run that does not ask does not look for a counter.
        let unasked = || -> bool { panic!("a run without --perf looked for a counter") };
        assert!(counts_instructions(false, unasked).is_ok());
        assert!(counts_instructions(true, || true).is_ok());
        match counts_instructions(true, || false) {
            Err(BenchError::Check(why)) => assert!(why.starts_with("--perf: "), "{why}"),
            other => panic!("not refused: {other:?}"),
        }
    }
}
