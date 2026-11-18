//! The benchmark suite: one harness for every row of the plan's prediction
//! table, on any engine.
//!
//! A run applies the plan's reference workload,
//! [`Documents::reference`](crate::workload::drive::Documents::reference),
//! through [`crate::workload::apply`] to an [`Engine`], and writes one
//! [`Report`] as JSON: the machine, the commit, the workload and seed, and
//! per engine every row with its unit, the raw samples behind it, the value
//! the plan recorded and the ratio to RocksDB in the same run. The
//! `lignin-bench` binary runs it and compares two reports.
//!
//! # Rows
//!
//! Count rows are the same on any machine; time rows are not. Each row
//! carries a `source` saying where its number comes from.
//!
//! | Row | Phase | Unit | From |
//! | --- | --- | --- | --- |
//! | `logical_bytes_per_transition` | write phases | B | each key written plus its serialized element |
//! | `node_records_per_transition` | write phases | records | [`Counters::node_records`] |
//! | `bytes_handed_per_transition` | write phases | B | [`Counters::node_record_bytes`] and [`Counters::other_record_bytes`] |
//! | `bytes_logged_per_transition` | write phases | B | [`Counters::log_bytes`] |
//! | `physical_bytes_per_transition` | write phases | B | [`Counters::physical_bytes`] |
//! | `keyed_lookups_per_transition` | write phases | lookups | `OperationCost::seek_count` less one per record logged |
//! | `seeks_per_transition` | write phases | seeks | `OperationCost::seek_count` |
//! | `hashes_per_transition` | write phases | calls | `OperationCost::hash_node_calls` |
//! | `node_record_*_share` | write phases | fraction | [`NodeAnatomy`], where the engine writes node records |
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
//! load, `flushed` the same after a flush. [`run`] says what each phase
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
//! sample when `perf` is asked for and works (`perf stat -e
//! instructions:u`, attached to the thread and switched on only around the
//! measured calls).

pub mod cli;
pub mod engine;
pub mod machine;
pub mod report;
pub mod rocksdb;
pub mod run;

use std::path::PathBuf;

use grovedb_version::version::GroveVersion;

pub use self::{
    engine::{Block, Counters, Engine, NodeAnatomy},
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
}

impl EngineChoice {
    /// The engine named `name` in reports.
    pub fn named(name: &str) -> Option<EngineChoice> {
        match name {
            RocksDbEngine::NAME => Some(EngineChoice::RocksDb),
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

/// A thread big enough for GroveDB's recursion on deep paths.
pub const STACK_BYTES: usize = 64 << 20;

/// Measures every engine of `options` on a thread with [`STACK_BYTES`] of
/// stack and returns the report.
///
/// For each engine, in a fresh database:
///
/// 1. `load`: the contract set-up block (untimed), then `documents`
///    creates. Bulk load, and every count row averaged over the load.
/// 2. At size: `size_on_disk`, warm reads after warm-up reads, the proof
///    sizes (the first proof of each kind is verified against the root
///    hash), checkpoints, a flush, the size and checkpoints again, reopens,
///    and cold reads. Then every file is read once and the warm-up reads
///    are repeated, so the writes start warm.
/// 3. `create`, `update`, `delete`: the writes at size, sampled per block.
pub fn run(options: &RunOptions) -> Result<Report, BenchError> {
    std::fs::create_dir_all(&options.settings.scratch)
        .map_err(|error| BenchError::io("creating the scratch directory", error))?;
    let options = options.clone();
    std::thread::Builder::new()
        .name("lignin-bench".to_string())
        .stack_size(STACK_BYTES)
        .spawn(move || run_here(&options))
        .map_err(|error| BenchError::io("starting the benchmark thread", error))?
        .join()
        .map_err(|_| BenchError::Check("the benchmark thread panicked".to_string()))?
}

fn run_here(options: &RunOptions) -> Result<Report, BenchError> {
    options.settings.check()?;
    let settings = &Settings {
        perf: options.settings.perf && machine::Perf::works(&options.settings.scratch),
        ..options.settings.clone()
    };
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
        report.engines.push(match engine {
            EngineChoice::RocksDb => {
                run::run_engine::<RocksDbEngine>(settings, cold_method, options.grove_version)?
            }
        });
    }
    report.load_average_after = machine::load_average();
    report.fill_ratios();
    Ok(report)
}
