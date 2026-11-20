//! One run of the harness over one engine.
//!
//! The run applies [`Documents`] in this order and measures as it goes:
//!
//! 1. **Load** (`load`): the contract set-up block, untimed, then
//!    `documents` creates, 20 per block. Bulk load, and every count and
//!    measured row averaged over the whole load.
//! 2. **At size** (`at-size`, `warm`, `cold`): the size on disk, warm point
//!    gets and proofs, checkpoints, a flush, reopens, then cold reads.
//! 3. **Writes at size** (`create`, `update`, `delete`): `measured_creates`
//!    more creates, then `updates` replacements and `deletes` deletions,
//!    with per-block samples.
//!
//! Every count and measured row comes from the engine's
//! [`Counters`](super::Counters), GroveDB's `OperationCost`, the serialized
//! sizes of what is written, proof lengths, file lengths or `/proc`; every
//! time row from `Instant` around the
//! engine call alone, with the engine's bookkeeping (`settle`, `counters`)
//! outside.
//!
//! A write phase ends by waiting for the engine's background work to go
//! idle, so the flushes and compactions it set off are counted in it rather
//! than in whatever comes next. What is still in the memtable is flushed,
//! and counted, later.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use grovedb::{Element, GroveDb, PathQuery};
use grovedb_costs::OperationCost;
use grovedb_version::version::GroveVersion;

use super::{
    engine::{timed, Block, Engine},
    machine::{self, ColdMethod, Perf},
    report::{
        ratio, recorded, DiskUsage, EngineReport, Method, PhaseTotals, Row, RowKind, Summary,
        WorkloadSpec,
    },
    BenchError,
};
use crate::workload::{drive::Documents, op::BatchOp, Op, Read, Rng, Stream, Workload, Write};

const DOMAIN_BENCH_READS: u64 = 0xBE7C;
const REFERENCES_PER_PROOF: u16 = 100;

/// What a run applies and how much it samples.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The workload's seed.
    pub seed: u64,
    /// Documents loaded before measuring at size.
    pub documents: u64,
    /// Owners the documents are spread over.
    pub owners: u64,
    /// Documents created after the load, timed per block.
    pub measured_creates: u64,
    /// Document replacements after those.
    pub updates: u64,
    /// Document deletions after those.
    pub deletes: u64,
    /// Timed point gets, and as many timed point proofs.
    pub point_reads: u32,
    /// Timed proofs of 100 references.
    pub reference_reads: u32,
    /// Untimed reads before the timed ones.
    pub warmup_reads: u32,
    /// Cold reads of each kind.
    pub cold_reads: u32,
    /// Checkpoints taken.
    pub checkpoints: u32,
    /// Reopens timed.
    pub reopens: u32,
    /// Count instructions with `perf` where it works.
    pub perf: bool,
    /// Where the databases are created; each engine gets a subdirectory,
    /// removed at the end unless `keep` is set.
    pub scratch: PathBuf,
    /// Keep the databases.
    pub keep: bool,
}

impl Settings {
    /// The plan's reference workload: one million documents, then twenty
    /// thousand of each write and enough reads for stable medians.
    pub fn reference(scratch: PathBuf) -> Self {
        let reference = Documents::reference();
        Settings {
            seed: 1,
            documents: reference.documents,
            owners: reference.owners,
            measured_creates: 20_000,
            updates: 20_000,
            deletes: 20_000,
            point_reads: 5_000,
            reference_reads: 500,
            warmup_reads: 2_000,
            cold_reads: 40,
            checkpoints: 15,
            reopens: 15,
            perf: false,
            scratch,
            keep: false,
        }
    }

    /// The documents workload these settings apply.
    pub fn workload(&self) -> Documents {
        Documents {
            documents: self.documents + self.measured_creates,
            owners: self.owners,
            updates: self.updates,
            deletes: self.deletes,
            ..Documents::reference()
        }
    }

    /// The workload's description in a report.
    pub fn spec(&self) -> WorkloadSpec {
        let workload = self.workload();
        WorkloadSpec {
            name: Workload::Documents(workload).name().to_string(),
            seed: self.seed,
            documents: self.documents,
            owners: self.owners,
            document_size: workload.document_size,
            transitions_per_block: workload.transitions_per_block,
            measured_creates: self.measured_creates,
            updates: self.updates,
            deletes: self.deletes,
        }
    }

    /// The method a report records, for `cold_method` found on this
    /// machine.
    pub fn method(&self, cold_method: ColdMethod, perf_instructions: bool) -> Method {
        Method {
            point_reads: self.point_reads,
            reference_reads: self.reference_reads,
            warmup_reads: self.warmup_reads,
            cold_reads: self.cold_reads,
            checkpoints: self.checkpoints,
            reopens: self.reopens,
            cold_method,
            perf_instructions,
            thread_cpu_time: machine::thread_cpu_ns().is_some(),
        }
    }

    /// Refuses settings the run cannot apply as blocks.
    pub fn check(&self) -> Result<(), BenchError> {
        let per_block = u64::from(Documents::reference().transitions_per_block);
        let phases = [
            ("documents", self.documents),
            ("measured creates", self.measured_creates),
            ("updates", self.updates),
            ("deletes", self.deletes),
        ];
        for (name, count) in phases {
            if count % per_block != 0 {
                return Err(BenchError::Check(format!(
                    "{name} ({count}) must be a multiple of {per_block}, the transitions per block"
                )));
            }
        }
        if self.documents == 0 || self.owners == 0 {
            return Err(BenchError::Check(
                "the run needs documents and owners".to_string(),
            ));
        }
        if self.deletes > self.documents + self.measured_creates {
            return Err(BenchError::Check("more deletes than documents".to_string()));
        }
        Ok(())
    }
}

/// Measures `cpu` time and, where `perf` works, instructions over the
/// sections a row times.
struct Meter {
    perf: Option<Perf>,
    perf_failed: bool,
    cpu_ns: Option<u64>,
}

struct Mark {
    cpu_ns: Option<u64>,
}

impl Meter {
    fn new(settings: &Settings, label: &str) -> Meter {
        let perf = if settings.perf {
            machine::thread_id()
                .and_then(|tid| Perf::attach(tid, &settings.scratch.join(format!("perf-{label}"))))
        } else {
            None
        };
        Meter {
            perf_failed: settings.perf && perf.is_none(),
            perf,
            cpu_ns: machine::thread_cpu_ns().map(|_| 0),
        }
    }

    fn idle() -> Meter {
        Meter {
            perf: None,
            perf_failed: false,
            cpu_ns: None,
        }
    }

    fn start(&mut self) -> Mark {
        if let Some(perf) = &mut self.perf {
            self.perf_failed |= !perf.enable();
        }
        Mark {
            cpu_ns: machine::thread_cpu_ns(),
        }
    }

    fn stop(&mut self, mark: Mark) {
        match (self.cpu_ns.as_mut(), mark.cpu_ns, machine::thread_cpu_ns()) {
            (Some(total), Some(before), Some(after)) => *total += after.saturating_sub(before),
            _ => self.cpu_ns = None,
        }
        if let Some(perf) = &mut self.perf {
            self.perf_failed |= !perf.disable();
        }
    }

    /// Instructions and CPU nanoseconds over every measured section.
    fn finish(self) -> (Option<u64>, Option<u64>) {
        let instructions = self.perf.and_then(Perf::finish);
        (instructions.filter(|_| !self.perf_failed), self.cpu_ns)
    }
}

/// A time unit of a row.
#[derive(Clone, Copy)]
enum Unit {
    Micros,
    Millis,
    Seconds,
}

impl Unit {
    fn name(self) -> &'static str {
        match self {
            Unit::Micros => "us",
            Unit::Millis => "ms",
            Unit::Seconds => "s",
        }
    }

    fn of(self, duration: Duration) -> f64 {
        self.of_ns(duration.as_nanos() as f64)
    }

    fn of_ns(self, nanos: f64) -> f64 {
        match self {
            Unit::Micros => nanos / 1e3,
            Unit::Millis => nanos / 1e6,
            Unit::Seconds => nanos / 1e9,
        }
    }
}

fn row(id: &str, phase: &str, unit: &str, kind: RowKind, value: f64, samples: Vec<f64>) -> Row {
    let recorded = recorded(id, phase);
    Row {
        id: id.to_string(),
        phase: phase.to_string(),
        unit: unit.to_string(),
        kind,
        value,
        summary: Summary::of(&samples),
        samples,
        instructions_per_sample: None,
        cpu_per_sample: None,
        storage_read_bytes_per_sample: None,
        ratio_to_recorded: recorded
            .as_ref()
            .and_then(|recorded| ratio(value, recorded.value)),
        recorded,
        ratio_to_rocksdb: None,
        source: String::new(),
    }
}

/// A time row whose value is the median of `samples`, with the meter's
/// totals spread over `per` units of work.
fn time_row(
    id: &str,
    phase: &str,
    unit: Unit,
    samples: Vec<f64>,
    meter: (Option<u64>, Option<u64>),
    per: u64,
    source: &str,
) -> Row {
    let median = Summary::of(&samples).map_or(0.0, |summary| summary.median);
    let mut row = row(id, phase, unit.name(), RowKind::Time, median, samples);
    let per = per.max(1) as f64;
    row.instructions_per_sample = meter.0.map(|count| count as f64 / per);
    row.cpu_per_sample = meter.1.map(|nanos| unit.of_ns(nanos as f64) / per);
    row.source = source.to_string();
    row
}

/// A count row: `total` over `per`.
fn count_row(id: &str, phase: &str, unit: &str, total: f64, per: u64, source: &str) -> Row {
    let value = total / per.max(1) as f64;
    let mut row = row(id, phase, unit, RowKind::Count, value, vec![value]);
    row.source = source.to_string();
    row
}

/// A [`RowKind::Measured`] row: `total` over `per`.
fn measured_row(id: &str, phase: &str, unit: &str, total: f64, per: u64, source: &str) -> Row {
    Row {
        kind: RowKind::Measured,
        ..count_row(id, phase, unit, total, per, source)
    }
}

/// A count row whose value is the median of `samples`.
fn sampled_count_row(id: &str, phase: &str, unit: &str, samples: Vec<f64>, source: &str) -> Row {
    let median = Summary::of(&samples).map_or(0.0, |summary| summary.median);
    let mut row = row(id, phase, unit, RowKind::Count, median, samples);
    row.source = source.to_string();
    row
}

/// Whether `op` is a state transition: a direct write or a batch, whole or
/// in segments, under GroveDB's callbacks or Drive's.
fn is_transition(op: &Op) -> bool {
    match op {
        Op::Write(_)
        | Op::Batch(_)
        | Op::PartialBatch(_)
        | Op::FlagsBatch(_)
        | Op::FlagsPartialBatch(_)
        | Op::FlagsHandoverBatch(_) => true,
        Op::Read(_) | Op::Commit | Op::FlushPendingDrops | Op::Transaction(_) => false,
    }
}

/// Key and value bytes `op` writes: each key plus its serialized element.
/// An element is counted as it is written: the storage flags Drive's
/// callbacks store with it may be longer.
fn logical_bytes(op: &Op, grove_version: &GroveVersion) -> u64 {
    match op {
        Op::Write(
            Write::Insert { key, element, .. }
            | Write::InsertIfNotExists { key, element, .. }
            | Write::InsertIfChanged { key, element, .. }
            | Write::IndexedInsert { key, element, .. },
        ) => entry_bytes(key, element, grove_version),
        Op::Batch(_)
        | Op::PartialBatch(_)
        | Op::FlagsBatch(_)
        | Op::FlagsPartialBatch(_)
        | Op::FlagsHandoverBatch(_) => batch_bytes(op.batch_ops(), grove_version),
        Op::Write(_) | Op::Read(_) | Op::Commit | Op::FlushPendingDrops | Op::Transaction(_) => 0,
    }
}

/// A key plus its element as GroveDB serializes it.
fn entry_bytes(key: &[u8], element: &Element, grove_version: &GroveVersion) -> u64 {
    key.len() as u64
        + element
            .serialized_size(grove_version)
            .map_or(0, |size| size as u64)
}

/// [`logical_bytes`] of the operations of a batch.
fn batch_bytes<'a>(ops: impl Iterator<Item = &'a BatchOp>, grove_version: &GroveVersion) -> u64 {
    ops.map(|op| match op {
        BatchOp::InsertOrReplace { key, element, .. }
        | BatchOp::InsertOnly { key, element, .. }
        | BatchOp::InsertIfNotExists { key, element, .. }
        | BatchOp::Replace { key, element, .. }
        | BatchOp::Patch { key, element, .. } => entry_bytes(key, element, grove_version),
        BatchOp::RefreshReference {
            key,
            reference,
            max_hop,
            flags,
            non_counted,
            ..
        } => {
            let reference = Element::Reference(reference.clone(), *max_hop, flags.clone());
            let stored = if *non_counted {
                Element::NonCounted(Box::new(reference))
            } else {
                reference
            };
            entry_bytes(key, &stored, grove_version)
        }
        _ => 0,
    })
    .sum()
}

/// What a write phase measured.
struct PhaseRun {
    totals: PhaseTotals,
    /// Per block: microseconds per transition, commits excluded.
    transition_us: Vec<f64>,
    /// Per block: commit milliseconds.
    commit_ms: Vec<f64>,
    apply_meter: (Option<u64>, Option<u64>),
    commit_meter: (Option<u64>, Option<u64>),
}

/// The next block of `stream`, its commit last. Empty at the end.
fn next_block(stream: &mut Stream) -> Vec<Op> {
    let mut block = Vec::new();
    for op in stream.by_ref() {
        let end = matches!(op, Op::Commit);
        block.push(op);
        if end {
            break;
        }
    }
    block
}

/// Applies blocks of `stream` until `transitions` state transitions have
/// been applied.
fn write_phase<E: Engine>(
    engine: &mut E,
    stream: &mut Stream,
    phase: &str,
    transitions: u64,
    settings: &Settings,
    grove_version: &GroveVersion,
) -> Result<PhaseRun, BenchError> {
    let mut totals = PhaseTotals {
        phase: phase.to_string(),
        ..Default::default()
    };
    let mut transition_us = Vec::new();
    let mut commit_ms = Vec::new();
    let mut apply_meter = Meter::new(settings, &format!("{phase}-apply"));
    let mut commit_meter = Meter::new(settings, &format!("{phase}-commit"));
    let counters_before = engine.counters()?;
    let anatomy_before = engine.node_anatomy();
    let written_before = machine::process_io("write_bytes");
    while totals.transitions < transitions {
        let block = next_block(stream);
        let Some((Op::Commit, ops)) = block.split_last() else {
            return Err(BenchError::Check(format!(
                "the stream ended {} transitions into the {phase} phase",
                totals.transitions
            )));
        };
        let mut cost = OperationCost::default();
        let mut apply_time = Duration::ZERO;
        let mut writes = 0u64;
        let mut transaction = engine.begin_block();
        let mark = apply_meter.start();
        for op in ops {
            let (result, elapsed) = timed(|| transaction.apply(op, grove_version));
            cost += result?;
            apply_time += elapsed;
            writes += u64::from(is_transition(op));
        }
        apply_meter.stop(mark);
        let mark = commit_meter.start();
        let (committed, commit_time) = timed(|| transaction.commit());
        commit_meter.stop(mark);
        committed?;
        engine.settle()?;

        totals.logical_bytes += ops
            .iter()
            .map(|op| logical_bytes(op, grove_version))
            .sum::<u64>();
        totals.transitions += writes;
        totals.blocks += 1;
        totals.apply_seconds += apply_time.as_secs_f64();
        totals.commit_seconds += commit_time.as_secs_f64();
        totals.seek_count += u64::from(cost.seek_count);
        totals.storage_loaded_bytes += cost.storage_loaded_bytes;
        totals.hash_node_calls += u64::from(cost.hash_node_calls);
        totals.storage_added_bytes += u64::from(cost.storage_cost.added_bytes);
        totals.storage_replaced_bytes += u64::from(cost.storage_cost.replaced_bytes);
        transition_us.push(Unit::Micros.of(apply_time) / writes.max(1) as f64);
        commit_ms.push(Unit::Millis.of(commit_time));
    }
    // Flushes and compactions the phase set off are counted in it.
    engine.wait_for_background()?;
    totals.counters = engine.counters()?.since(&counters_before);
    totals.node_anatomy = engine
        .node_anatomy()
        .zip(anatomy_before)
        .map(|(now, then)| now.since(&then));
    totals.process_write_bytes = match (written_before, machine::process_io("write_bytes")) {
        (Some(before), Some(after)) => Some(after.saturating_sub(before)),
        _ => None,
    };
    Ok(PhaseRun {
        totals,
        transition_us,
        commit_ms,
        apply_meter: apply_meter.finish(),
        commit_meter: commit_meter.finish(),
    })
}

const COST_SOURCE: &str = "GroveDB's OperationCost of every operation, summed";

/// The count rows of a write phase, per state transition.
fn count_rows(run: &PhaseRun, engine: &str) -> Vec<Row> {
    let totals = &run.totals;
    let phase = totals.phase.as_str();
    let n = totals.transitions;
    let counters = &totals.counters;
    let log_source = format!("{engine} engine counters: write-ahead log");
    let mut rows = vec![
        count_row(
            "logical_bytes_per_transition",
            phase,
            "B",
            totals.logical_bytes as f64,
            n,
            "each key written plus its element as GroveDB serializes it",
        ),
        count_row(
            "node_records_per_transition",
            phase,
            "records",
            counters.node_records as f64,
            n,
            &format!("{log_source}, Merk node records"),
        ),
        count_row(
            "other_records_per_transition",
            phase,
            "records",
            counters.other_records as f64,
            n,
            &format!("{log_source}, records other than Merk nodes"),
        ),
        count_row(
            "bytes_handed_per_transition",
            phase,
            "B",
            (counters.node_record_bytes + counters.other_record_bytes) as f64,
            n,
            &format!("{log_source}, key and value bytes of every record"),
        ),
        measured_row(
            "bytes_logged_per_transition",
            phase,
            "B",
            counters.log_bytes as f64,
            n,
            &format!("{log_source}, bytes appended, framing included"),
        ),
        measured_row(
            "physical_bytes_per_transition",
            phase,
            "B",
            counters.physical_bytes() as f64,
            n,
            &format!("{engine} engine counters: log, flush and compaction bytes"),
        ),
        measured_row(
            "flush_bytes_per_transition",
            phase,
            "B",
            counters.flush_bytes as f64,
            n,
            &format!("{engine} engine counters: flush bytes"),
        ),
        measured_row(
            "compaction_bytes_per_transition",
            phase,
            "B",
            counters.compaction_bytes as f64,
            n,
            &format!("{engine} engine counters: compaction bytes"),
        ),
        match counters.keyed_lookups {
            Some(lookups) => count_row(
                "keyed_lookups_per_transition",
                phase,
                "lookups",
                lookups as f64,
                n,
                &format!("{engine} engine counters: searches by key"),
            ),
            None => count_row(
                "keyed_lookups_per_transition",
                phase,
                "lookups",
                totals
                    .seek_count
                    .saturating_sub(counters.node_records + counters.other_records)
                    as f64,
                n,
                &format!(
                    "{COST_SOURCE}: seek_count, less the one seek the storage layer charges \
                     for each record it writes (the {engine} log's records)"
                ),
            ),
        },
        count_row(
            "seeks_per_transition",
            phase,
            "seeks",
            totals.seek_count as f64,
            n,
            &format!("{COST_SOURCE}: seek_count, writes included"),
        ),
        count_row(
            "hashes_per_transition",
            phase,
            "calls",
            totals.hash_node_calls as f64,
            n,
            &format!("{COST_SOURCE}: hash_node_calls"),
        ),
        count_row(
            "loaded_bytes_per_transition",
            phase,
            "B",
            totals.storage_loaded_bytes as f64,
            n,
            &format!("{COST_SOURCE}: storage_loaded_bytes"),
        ),
    ];
    if let Some(hashes) = counters.hashes {
        rows.push(count_row(
            "engine_hashes_per_transition",
            phase,
            "calls",
            hashes as f64,
            n,
            &format!("{engine} engine counters: hashes computed"),
        ));
    }
    if let Some(anatomy) = &totals.node_anatomy {
        let whole = anatomy.total();
        let source = format!(
            "{engine} engine: the Merk node records in its log, decoded; share of their bytes"
        );
        for (id, part) in [
            ("node_record_value_share", anatomy.value_bytes),
            ("node_record_key_share", anatomy.key_bytes),
            ("node_record_node_hash_share", anatomy.node_hash_bytes),
            ("node_record_link_hash_share", anatomy.link_hash_bytes),
            ("node_record_other_share", anatomy.other_bytes),
        ] {
            rows.push(count_row(
                id,
                phase,
                "fraction",
                part as f64,
                whole,
                &source,
            ));
        }
    }
    if let Some(written) = totals.process_write_bytes {
        rows.push(measured_row(
            "process_write_bytes_per_transition",
            phase,
            "B",
            written as f64,
            n,
            "/proc/self/io write_bytes of the whole process: a cross-check of physical bytes",
        ));
    }
    rows
}

/// The time rows of a write phase sampled per block.
fn window_time_rows(run: PhaseRun) -> Vec<Row> {
    let phase = run.totals.phase.clone();
    let transitions = run.totals.transitions;
    let blocks = run.totals.blocks;
    vec![
        time_row(
            "wall_per_transition",
            &phase,
            Unit::Micros,
            run.transition_us,
            run.apply_meter,
            transitions,
            "Instant around each operation's apply in the block's transaction; \
             one sample per block, the block's time over its transitions",
        ),
        time_row(
            "block_commit",
            &phase,
            Unit::Millis,
            run.commit_ms,
            run.commit_meter,
            blocks,
            "Instant around the commit of each block's transaction",
        ),
    ]
}

/// Files under `dir`, in its subdirectories too.
pub fn disk_usage(dir: &Path) -> Result<DiskUsage, BenchError> {
    let mut usage = DiskUsage {
        allocated_bytes: Some(0),
        ..Default::default()
    };
    let io = |error| BenchError::io("measuring the database", error);
    for path in machine::files_under(dir).map_err(io)? {
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io(error)),
        };
        usage.files += 1;
        usage.bytes += metadata.len();
        usage.allocated_bytes = usage
            .allocated_bytes
            .zip(allocated(&metadata))
            .map(|(sum, more)| sum + more);
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
            .to_string();
        *usage.by_extension.entry(extension).or_default() += metadata.len();
    }
    Ok(usage)
}

#[cfg(unix)]
fn allocated(metadata: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.blocks() * 512)
}

#[cfg(not(unix))]
fn allocated(_metadata: &std::fs::Metadata) -> Option<u64> {
    None
}

/// The reads measured at size.
struct Reads {
    documents: Documents,
    seed: u64,
    rng: Rng,
    /// Owners with at least 100 documents.
    full_owners: Vec<u64>,
}

impl Reads {
    fn new(settings: &Settings) -> Result<Reads, BenchError> {
        let documents = settings.workload();
        let mut per_owner = vec![0u32; settings.owners as usize];
        for index in 0..settings.documents {
            per_owner[documents.owner_of(settings.seed, index) as usize] += 1;
        }
        let full_owners: Vec<u64> = (0..settings.owners)
            .filter(|owner| per_owner[*owner as usize] >= u32::from(REFERENCES_PER_PROOF))
            .collect();
        if full_owners.is_empty() {
            return Err(BenchError::Check(format!(
                "no owner has {REFERENCES_PER_PROOF} documents to read through references"
            )));
        }
        Ok(Reads {
            documents,
            seed: settings.seed,
            rng: Rng::derive(settings.seed, DOMAIN_BENCH_READS, 0),
            full_owners,
        })
    }

    fn document(&mut self, loaded: u64) -> u64 {
        self.rng.below(loaded)
    }

    fn get(&mut self, loaded: u64) -> Op {
        let index = self.document(loaded);
        Op::Read(Read::Get {
            path: self.documents.primary_path(self.seed),
            key: self.documents.document_id(self.seed, index).to_vec(),
        })
    }

    fn point_query(&mut self, loaded: u64) -> PathQuery {
        let index = self.document(loaded);
        self.documents.point_query(self.seed, index).to_path_query()
    }

    fn references_query(&mut self) -> PathQuery {
        let owner = self.full_owners[self.rng.below(self.full_owners.len() as u64) as usize];
        self.documents
            .owner_query(self.seed, owner, REFERENCES_PER_PROOF)
            .to_path_query()
    }
}

/// Checks a proof against the root hash and the rows it must hold.
fn check_proof(
    proof: &[u8],
    query: &PathQuery,
    root_hash: &[u8; 32],
    rows: usize,
    grove_version: &GroveVersion,
) -> Result<(), BenchError> {
    let (proved_root, results) = GroveDb::verify_query(proof, query, grove_version)?;
    if &proved_root != root_hash {
        return Err(BenchError::Check(
            "a proof verified to another root hash".to_string(),
        ));
    }
    if results.len() != rows {
        return Err(BenchError::Check(format!(
            "a proof holds {} rows, {rows} expected",
            results.len()
        )));
    }
    Ok(())
}

/// A read prepared outside the timed region.
enum Request {
    Get(Op),
    Prove(PathQuery),
}

/// What kind of read a cold sample makes.
#[derive(Clone, Copy)]
enum ReadKind {
    Get,
    PointProof,
    ReferencesProof,
}

impl ReadKind {
    fn id(self) -> &'static str {
        match self {
            ReadKind::Get => "point_get",
            ReadKind::PointProof => "point_query_proof",
            ReadKind::ReferencesProof => "references_proof",
        }
    }

    /// Makes the read once; returns the proof's length for proofs.
    fn read<E: Engine>(
        self,
        engine: &E,
        reads: &mut Reads,
        loaded: u64,
        grove_version: &GroveVersion,
        meter: &mut Meter,
    ) -> Result<(Duration, Option<usize>), BenchError> {
        let request = match self {
            ReadKind::Get => Request::Get(reads.get(loaded)),
            ReadKind::PointProof => Request::Prove(reads.point_query(loaded)),
            ReadKind::ReferencesProof => Request::Prove(reads.references_query()),
        };
        let mark = meter.start();
        let (result, elapsed) = match &request {
            Request::Get(op) => {
                let (result, elapsed) = timed(|| engine.read(op, grove_version));
                (result.map(|_| None), elapsed)
            }
            Request::Prove(query) => {
                let (result, elapsed) = timed(|| engine.prove(query, grove_version));
                (result.map(|(proof, _)| Some(proof.len())), elapsed)
            }
        };
        meter.stop(mark);
        Ok((elapsed, result?))
    }
}

/// Times `settings.checkpoints` checkpoints of `engine`, each into a new
/// directory removed afterwards.
fn checkpoints<E: Engine>(engine: &E, settings: &Settings, phase: &str) -> Result<Row, BenchError> {
    let mut meter = Meter::new(settings, &format!("checkpoint-{phase}"));
    let mut samples = Vec::new();
    let target = settings.scratch.join(format!("{}-checkpoint", E::NAME));
    let remove = |target: &Path| {
        std::fs::remove_dir_all(target)
            .map_err(|error| BenchError::io("removing a checkpoint", error))
    };
    for _ in 0..settings.checkpoints {
        if target.exists() {
            remove(&target)?;
        }
        let mark = meter.start();
        let (result, elapsed) = timed(|| engine.checkpoint(&target));
        meter.stop(mark);
        result?;
        samples.push(Unit::Millis.of(elapsed));
        remove(&target)?;
    }
    Ok(time_row(
        "checkpoint",
        phase,
        Unit::Millis,
        samples,
        meter.finish(),
        u64::from(settings.checkpoints),
        "Instant around a checkpoint into a new directory on the same file system",
    ))
}

/// Brings the database's files back into the page cache after cold reads,
/// so the writes measured next start warm: every file is read once, then
/// the warm-up reads are made again.
fn rewarm<E: Engine>(
    engine: &E,
    reads: &mut Reads,
    settings: &Settings,
    grove_version: &GroveVersion,
) -> Result<(), BenchError> {
    let io = |error| BenchError::io("warming the database", error);
    for path in machine::files_under(engine.dir()).map_err(io)? {
        // The engine deletes files in the background, between listing and opening.
        match std::fs::File::open(&path) {
            Ok(mut file) => {
                std::io::copy(&mut file, &mut std::io::sink()).map_err(io)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io(error)),
        }
    }
    warm_up(engine, reads, settings, grove_version)
}

/// The untimed reads before timed warm ones.
fn warm_up<E: Engine>(
    engine: &E,
    reads: &mut Reads,
    settings: &Settings,
    grove_version: &GroveVersion,
) -> Result<(), BenchError> {
    let mut idle = Meter::idle();
    let kinds = [
        ReadKind::Get,
        ReadKind::PointProof,
        ReadKind::ReferencesProof,
    ];
    for n in 0..settings.warmup_reads {
        kinds[n as usize % kinds.len()].read(
            engine,
            reads,
            settings.documents,
            grove_version,
            &mut idle,
        )?;
    }
    Ok(())
}

/// Runs the whole measurement over a fresh `E` database in
/// `settings.scratch/<engine>`.
pub fn run_engine<E: Engine>(
    settings: &Settings,
    cold_method: ColdMethod,
    grove_version: &GroveVersion,
) -> Result<EngineReport, BenchError> {
    settings.check()?;
    let dir = settings.scratch.join(E::NAME);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|error| BenchError::io("removing an earlier database", error))?;
    }
    std::fs::create_dir_all(&dir)
        .map_err(|error| BenchError::io("creating the database directory", error))?;
    let result = measure::<E>(settings, &dir, cold_method, grove_version);
    if !settings.keep {
        let _ = std::fs::remove_dir_all(&dir);
    }
    result
}

fn measure<E: Engine>(
    settings: &Settings,
    dir: &Path,
    cold_method: ColdMethod,
    grove_version: &GroveVersion,
) -> Result<EngineReport, BenchError> {
    let mut reads = Reads::new(settings)?;
    let mut stream = settings.workload().stream(settings.seed);
    let mut engine = E::open(dir)?;
    engine.settle()?;
    let mut rows = Vec::new();
    let mut phases = Vec::new();

    let setup = next_block(&mut stream);
    let mut transaction = engine.begin_block();
    for op in setup.iter().filter(|op| !matches!(op, Op::Commit)) {
        transaction.apply(op, grove_version)?;
    }
    transaction.commit()?;
    engine.settle()?;

    let load = write_phase(
        &mut engine,
        &mut stream,
        "load",
        settings.documents,
        settings,
        grove_version,
    )?;
    rows.extend(count_rows(&load, E::NAME));
    let load_seconds = load.totals.apply_seconds + load.totals.commit_seconds;
    let both = |a: Option<u64>, b: Option<u64>| a.zip(b).map(|(a, b)| a + b);
    let load_meter = (
        both(load.apply_meter.0, load.commit_meter.0),
        both(load.apply_meter.1, load.commit_meter.1),
    );
    rows.push(time_row(
        "bulk_load",
        "load",
        Unit::Seconds,
        vec![load_seconds],
        load_meter,
        1,
        "Instant around every apply and commit of the load, summed; one sample per run",
    ));
    rows.push(time_row(
        "wall_per_transition",
        "load",
        Unit::Micros,
        vec![load.totals.apply_seconds * 1e6 / load.totals.transitions.max(1) as f64],
        (None, None),
        1,
        "apply time of the whole load over its transitions; one sample per run",
    ));
    rows.push(time_row(
        "block_commit",
        "load",
        Unit::Millis,
        vec![load.totals.commit_seconds * 1e3 / load.totals.blocks.max(1) as f64],
        load.commit_meter,
        load.totals.blocks,
        "commit time of the whole load over its blocks; one sample per run",
    ));
    phases.push(load.totals);

    let root_hash_after_load = engine.root_hash(grove_version)?;
    let disk_after_load = disk_usage(dir)?;
    rows.push(measured_row(
        "size_on_disk",
        "at-size",
        "B",
        disk_after_load.bytes as f64,
        1,
        "lengths of the files in the database directory right after the load",
    ));

    let loaded = settings.documents;
    warm_up(&engine, &mut reads, settings, grove_version)?;

    let mut get_meter = Meter::new(settings, "warm-get");
    let mut get_us = Vec::new();
    for _ in 0..settings.point_reads {
        let (elapsed, _) =
            ReadKind::Get.read(&engine, &mut reads, loaded, grove_version, &mut get_meter)?;
        get_us.push(Unit::Micros.of(elapsed));
    }
    rows.push(time_row(
        "point_get",
        "warm",
        Unit::Micros,
        get_us,
        get_meter.finish(),
        u64::from(settings.point_reads),
        "Instant around GroveDb::get of a random loaded document",
    ));

    for kind in [ReadKind::PointProof, ReadKind::ReferencesProof] {
        let (count, size_id, rows_per_proof) = match kind {
            ReadKind::PointProof => (settings.point_reads, "point_proof_bytes", 1),
            _ => (
                settings.reference_reads,
                "references_proof_bytes",
                usize::from(REFERENCES_PER_PROOF),
            ),
        };
        let first_query = match kind {
            ReadKind::PointProof => reads.point_query(loaded),
            _ => reads.references_query(),
        };
        let (proof, _) = engine.prove(&first_query, grove_version)?;
        check_proof(
            &proof,
            &first_query,
            &root_hash_after_load,
            rows_per_proof,
            grove_version,
        )?;
        let mut meter = Meter::new(settings, &format!("warm-{}", kind.id()));
        let mut times = Vec::new();
        let mut sizes = Vec::new();
        for _ in 0..count {
            let (elapsed, size) =
                kind.read(&engine, &mut reads, loaded, grove_version, &mut meter)?;
            times.push(Unit::Micros.of(elapsed));
            sizes.push(size.unwrap_or(0) as f64);
        }
        rows.push(time_row(
            kind.id(),
            "warm",
            Unit::Micros,
            times,
            meter.finish(),
            u64::from(count),
            match kind {
                ReadKind::PointProof => {
                    "Instant around GroveDb::prove_query of one random loaded document by id"
                }
                _ => {
                    "Instant around GroveDb::prove_query of 100 references in the $ownerId \
                     index of a random owner with at least 100 documents, references followed"
                }
            },
        ));
        rows.push(sampled_count_row(
            size_id,
            "warm",
            "B",
            sizes,
            "length of each proof GroveDb::prove_query returned for the timed reads",
        ));
    }

    rows.push(checkpoints(&engine, settings, "at-size")?);
    engine.flush()?;
    engine.wait_for_background()?;
    let disk_after_flush = disk_usage(dir)?;
    rows.push(measured_row(
        "size_on_disk",
        "flushed",
        "B",
        disk_after_flush.bytes as f64,
        1,
        "lengths of the files in the database directory after a flush, once the engine's \
         background work is idle",
    ));
    rows.push(checkpoints(&engine, settings, "flushed")?);

    let mut reopen_meter = Meter::new(settings, "reopen");
    let mut reopen_ms = Vec::new();
    for _ in 0..settings.reopens {
        drop(engine);
        let mark = reopen_meter.start();
        let (opened, elapsed) = timed(|| E::open(dir));
        reopen_meter.stop(mark);
        engine = opened?;
        engine.settle()?;
        reopen_ms.push(Unit::Millis.of(elapsed));
    }
    rows.push(time_row(
        "reopen",
        "flushed",
        Unit::Millis,
        reopen_ms,
        reopen_meter.finish(),
        u64::from(settings.reopens),
        "Instant around opening the database again after closing it; the OS page cache is warm",
    ));

    let mut cold_open_ms = Vec::new();
    let mut cold_open_meter = Meter::new(settings, "cold-open");
    for kind in [
        ReadKind::Get,
        ReadKind::PointProof,
        ReadKind::ReferencesProof,
    ] {
        let mut meter = Meter::new(settings, &format!("cold-{}", kind.id()));
        let mut times = Vec::new();
        let mut read_bytes = Some(0u64);
        for _ in 0..settings.cold_reads {
            drop(engine);
            if cold_method == ColdMethod::ReopenAndDropFileCache && !machine::drop_dir_cache(dir) {
                return Err(BenchError::Check(
                    "the page cache could not be dropped for a cold read".to_string(),
                ));
            }
            let mark = cold_open_meter.start();
            let (opened, elapsed) = timed(|| E::open(dir));
            cold_open_meter.stop(mark);
            engine = opened?;
            engine.settle()?;
            cold_open_ms.push(Unit::Millis.of(elapsed));
            let before = machine::process_io("read_bytes");
            let (elapsed, _) = kind.read(&engine, &mut reads, loaded, grove_version, &mut meter)?;
            read_bytes = match (read_bytes, before, machine::process_io("read_bytes")) {
                (Some(sum), Some(before), Some(after)) => Some(sum + after.saturating_sub(before)),
                _ => None,
            };
            times.push(Unit::Micros.of(elapsed));
        }
        let mut row = time_row(
            kind.id(),
            "cold",
            Unit::Micros,
            times,
            meter.finish(),
            u64::from(settings.cold_reads),
            "the first read after reopening; see the report's cold_method",
        );
        row.storage_read_bytes_per_sample =
            read_bytes.map(|bytes| bytes as f64 / f64::from(settings.cold_reads.max(1)));
        rows.push(row);
    }
    rows.push(time_row(
        "reopen",
        "cold",
        Unit::Millis,
        cold_open_ms,
        cold_open_meter.finish(),
        u64::from(settings.cold_reads) * 3,
        "Instant around opening the database with its files dropped from the page cache",
    ));

    rewarm(&engine, &mut reads, settings, grove_version)?;

    for (phase, transitions) in [
        ("create", settings.measured_creates),
        ("update", settings.updates),
        ("delete", settings.deletes),
    ] {
        if transitions == 0 {
            continue;
        }
        let run = write_phase(
            &mut engine,
            &mut stream,
            phase,
            transitions,
            settings,
            grove_version,
        )?;
        rows.extend(count_rows(&run, E::NAME));
        phases.push(run.totals.clone());
        rows.extend(window_time_rows(run));
    }
    let root_hash_at_end = engine.root_hash(grove_version)?;

    Ok(EngineReport {
        engine: E::NAME.to_string(),
        root_hash_after_load: hex::encode(root_hash_after_load),
        root_hash_at_end: hex::encode(root_hash_at_end),
        rows,
        phases,
        disk_after_load,
        disk_after_flush,
    })
}

/// When the run started, in seconds since the Unix epoch.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{bench::Counters, workload::PartialBatch};

    /// The acceptance test of STORE-11: every engine count of a write
    /// phase's rows comes from LigninDB's `StoreStats`, each from the
    /// field of its own.
    #[test]
    fn every_engine_count_is_read_from_the_store_stats() {
        let stats = lignin_store::stats::StoreStats {
            log_bytes: 1_000,
            log_groups: 2,
            node_records: 30,
            node_record_bytes: 400,
            other_records: 5,
            other_record_bytes: 60,
            flush_bytes: 7_000,
            cleaner_bytes: 800,
            page_reads: 9,
            cache_hits: 90,
            hashes: 110,
            keyed_lookups: 12,
            ..Default::default()
        };
        let counters = Counters::from(&stats);
        assert_eq!(
            counters,
            Counters {
                log_bytes: 1_000,
                log_groups: 2,
                node_records: 30,
                node_record_bytes: 400,
                other_records: 5,
                other_record_bytes: 60,
                flush_bytes: 7_000,
                compaction_bytes: 800,
                page_reads: Some(9),
                cache_hits: Some(90),
                keyed_lookups: Some(12),
                hashes: Some(110),
            }
        );
        assert_eq!(counters.since(&Counters::default()).keyed_lookups, None);
        assert_eq!(
            counters.since(&Counters::from(&Default::default())),
            counters
        );

        let run = PhaseRun {
            totals: PhaseTotals {
                phase: "update".into(),
                transitions: 2,
                seek_count: 1_000,
                hash_node_calls: 50,
                counters,
                ..Default::default()
            },
            transition_us: Vec::new(),
            commit_ms: Vec::new(),
            apply_meter: (None, None),
            commit_meter: (None, None),
        };
        let rows = count_rows(&run, "lignindb");
        let value = |id: &str| {
            let row = rows
                .iter()
                .find(|row| row.id == id)
                .unwrap_or_else(|| panic!("no row {id}"));
            (row.value, row.source.clone())
        };
        for (id, per_transition) in [
            ("node_records_per_transition", 15.0),
            ("other_records_per_transition", 2.5),
            ("bytes_handed_per_transition", 230.0),
            ("bytes_logged_per_transition", 500.0),
            ("physical_bytes_per_transition", 4_400.0),
            ("flush_bytes_per_transition", 3_500.0),
            ("compaction_bytes_per_transition", 400.0),
            ("keyed_lookups_per_transition", 6.0),
            ("engine_hashes_per_transition", 55.0),
        ] {
            let (found, source) = value(id);
            assert_eq!(found, per_transition, "{id}");
            assert!(
                source.starts_with("lignindb engine counters"),
                "{id}: {source}"
            );
        }
        assert_eq!(value("hashes_per_transition").0, 25.0);

        // An engine that does not count lookups keeps the derivation from
        // the seeks GroveDB charges, and has no row of engine hashes.
        let mut run = run;
        run.totals.counters.keyed_lookups = None;
        run.totals.counters.hashes = None;
        let rows = count_rows(&run, "rocksdb");
        let lookups = rows
            .iter()
            .find(|row| row.id == "keyed_lookups_per_transition")
            .expect("the row");
        assert_eq!(lookups.value, (1_000.0 - 35.0) / 2.0);
        assert!(!rows
            .iter()
            .any(|row| row.id == "engine_hashes_per_transition"));
    }

    /// Reports written before the engine counted lookups and hashes still
    /// read, with neither counted.
    #[test]
    fn counters_without_lookups_or_hashes_still_read() {
        let mut json = serde_json::to_value(Counters::default()).expect("json");
        let fields = json.as_object_mut().expect("an object");
        fields.remove("keyed_lookups");
        fields.remove("hashes");
        let counters: Counters = serde_json::from_value(json).expect("read");
        assert_eq!(counters, Counters::default());
    }

    #[test]
    fn logical_bytes_count_keys_and_elements_of_every_write() {
        let grove_version = GroveVersion::latest();
        let element = Element::new_item(vec![1; 10]);
        let entry = entry_bytes(b"key", &element, grove_version);
        assert_eq!(
            entry,
            3 + element.serialized_size(grove_version).unwrap() as u64
        );
        let put = BatchOp::InsertOrReplace {
            path: vec![],
            key: b"key".to_vec(),
            element: element.clone(),
        };
        let delete = BatchOp::Delete {
            path: vec![],
            key: b"key".to_vec(),
        };
        let insert = Op::Write(Write::Insert {
            path: vec![],
            key: b"key".to_vec(),
            element,
        });
        assert_eq!(logical_bytes(&insert, grove_version), entry);
        assert_eq!(
            logical_bytes(&Op::Batch(vec![put.clone(), delete.clone()]), grove_version),
            entry
        );
        let partial = Op::PartialBatch(PartialBatch {
            ops: vec![put.clone()],
            add_on: vec![put, delete],
            pause_height: None,
        });
        assert_eq!(logical_bytes(&partial, grove_version), 2 * entry);
        let item = Element::new_item(vec![1; 10]);
        for write in [
            Write::InsertIfNotExists {
                path: vec![],
                key: b"key".to_vec(),
                element: item.clone(),
                return_existing: false,
            },
            Write::InsertIfChanged {
                path: vec![],
                key: b"key".to_vec(),
                element: item.clone(),
            },
            Write::IndexedInsert {
                path: vec![b"indexed".to_vec()],
                key: b"key".to_vec(),
                element: item.clone(),
                tree: crate::workload::IndexedTree::Count,
            },
        ] {
            assert_eq!(logical_bytes(&Op::Write(write), grove_version), entry);
        }
        let patch = BatchOp::Patch {
            path: vec![],
            key: b"key".to_vec(),
            element: item,
            change_in_bytes: 0,
        };
        assert_eq!(logical_bytes(&Op::Batch(vec![patch]), grove_version), entry);
        let reference = |non_counted| BatchOp::RefreshReference {
            path: vec![],
            key: b"key".to_vec(),
            reference: grovedb::element::reference_path::ReferencePathType::SiblingReference(
                b"other".to_vec(),
            ),
            max_hop: None,
            flags: None,
            non_counted,
            trusted: true,
        };
        let plain = logical_bytes(&Op::Batch(vec![reference(false)]), grove_version);
        let wrapped = logical_bytes(&Op::Batch(vec![reference(true)]), grove_version);
        assert!(wrapped > plain, "the NonCounted wrapper is stored too");
        assert_eq!(logical_bytes(&Op::Commit, grove_version), 0);
    }

    /// A batch applied with Drive's storage-flag callbacks writes what the
    /// same batch writes under GroveDB's own, and is a transition like it.
    #[test]
    fn a_batch_under_drives_callbacks_is_counted_like_any_other() {
        let grove_version = GroveVersion::latest();
        let flags = vec![0, 0, 3];
        let element = Element::new_item_with_flags(vec![1; 10], Some(flags));
        let entry = entry_bytes(b"key", &element, grove_version);
        let put = BatchOp::Replace {
            path: vec![],
            key: b"key".to_vec(),
            element,
        };
        let delete = BatchOp::Delete {
            path: vec![],
            key: b"key".to_vec(),
        };
        let ops = vec![put.clone(), delete.clone()];
        let partial = PartialBatch {
            ops: vec![put.clone()],
            add_on: vec![put, delete],
            pause_height: None,
        };
        let handover = crate::workload::HandoverBatch {
            ops: ops.clone(),
            full_handover_owners: [[7; 32]].into(),
        };
        let pairs = [
            (Op::FlagsBatch(ops.clone()), Op::Batch(ops.clone()), entry),
            (Op::FlagsHandoverBatch(handover), Op::Batch(ops), entry),
            (
                Op::FlagsPartialBatch(partial.clone()),
                Op::PartialBatch(partial),
                2 * entry,
            ),
        ];
        for (with_drives, with_grovedbs, bytes) in pairs {
            assert_eq!(logical_bytes(&with_drives, grove_version), bytes);
            assert_eq!(logical_bytes(&with_grovedbs, grove_version), bytes);
            assert!(is_transition(&with_drives) && is_transition(&with_grovedbs));
        }

        // A delete writes no key and no element, with either callback.
        let deletes = [
            Write::Delete {
                path: vec![],
                key: b"key".to_vec(),
            },
            Write::SectionalDelete {
                path: vec![],
                key: b"key".to_vec(),
            },
            Write::SectionalDeleteUpTree {
                path: vec![b"tree".to_vec()],
                key: b"key".to_vec(),
                stop_path_height: Some(0),
            },
        ];
        for delete in deletes {
            let delete = Op::Write(delete);
            assert_eq!(logical_bytes(&delete, grove_version), 0);
            assert!(is_transition(&delete));
        }
        let get = Op::Read(Read::Get {
            path: vec![],
            key: b"key".to_vec(),
        });
        let step = Op::Transaction(crate::workload::TransactionOp::SetSavepoint);
        for other in [get, Op::Commit, Op::FlushPendingDrops, step] {
            assert_eq!(logical_bytes(&other, grove_version), 0);
            assert!(!is_transition(&other), "{other:?}");
        }
    }
}
