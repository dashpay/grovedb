//! One run of the harness over one engine.
//!
//! The run applies [`Documents`] in this order and measures as it goes:
//!
//! 1. **Load** (`load`): the contract set-up block, untimed, then
//!    `documents` creates, 20 per block. Bulk load, and every count and
//!    measured row averaged over the whole load.
//! 2. **At size** (`at-size`, `warm`, `settled`, `cold`): the size on disk,
//!    warm point gets and proofs right after the load, checkpoints, a flush,
//!    reopens, the warm reads again in the settled state, then cold reads.
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
    engine::{timed, Block, Counters, Detail, Engine, EngineSettings, Space},
    machine::{self, ColdMethod, Perf, StorageReads},
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
    /// Documents created after the load and before the measured creates,
    /// as a write phase of its own (`churn`), so that the phases after it
    /// begin on a state that writes have already churned.
    pub churn: u64,
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
    /// Count instructions with `perf`. A run is refused at its start
    /// where `perf` counts none then.
    pub perf: bool,
    /// Where the databases are created; each engine gets a subdirectory,
    /// removed at the end unless `keep` is set.
    pub scratch: PathBuf,
    /// Keep the databases.
    pub keep: bool,
    /// What the engines are asked beyond their defaults.
    pub engine: EngineSettings,
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
            churn: 0,
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
            engine: EngineSettings::default(),
        }
    }

    /// The documents the workload creates: the load, the churn and the
    /// measured creates. A sum past `u64::MAX` is refused by
    /// [`Self::check`] and is `u64::MAX` here.
    fn created(&self) -> u64 {
        self.documents
            .saturating_add(self.churn)
            .saturating_add(self.measured_creates)
    }

    /// The documents workload these settings apply.
    pub fn workload(&self) -> Documents {
        Documents {
            documents: self.created(),
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
            churn: self.churn,
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
            engine_settings: self.engine.list(),
        }
    }

    /// Refuses settings the run cannot apply as blocks.
    pub fn check(&self) -> Result<(), BenchError> {
        let per_block = u64::from(Documents::reference().transitions_per_block);
        let phases = [
            ("documents", self.documents),
            ("churn", self.churn),
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
        if self
            .documents
            .checked_add(self.churn)
            .and_then(|sum| sum.checked_add(self.measured_creates))
            .is_none()
        {
            return Err(BenchError::Check(
                "the documents, the churn and the measured creates add up to more than a u64"
                    .to_string(),
            ));
        }
        if self.deletes > self.created() {
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
    /// The engine's details over the phase.
    details: Vec<Detail>,
    /// The engine's gauges over the phase.
    gauges: Vec<Detail>,
    /// What the engine's files held at the phase's start and at its end,
    /// for an engine that counts it.
    space: Option<(Space, Space)>,
    /// The size of the database directory at the phase's end, once the
    /// background work the phase set off is idle.
    size_at_end: u64,
    /// The counters and the details as the reading that ended the phase
    /// gave them, from which a span that follows the phase is counted.
    ended: (Counters, Vec<Detail>),
}

/// What `now` counts more than `then`, detail by detail.
fn details_since(now: Vec<Detail>, then: &[Detail]) -> Result<Vec<Detail>, BenchError> {
    if now.len() != then.len() || now.iter().zip(then).any(|(now, then)| now.id != then.id) {
        return Err(BenchError::Check(
            "an engine's details differ between two readings".to_string(),
        ));
    }
    Ok(now
        .into_iter()
        .zip(then)
        .map(|(now, then)| Detail {
            total: now.total.saturating_sub(then.total),
            ..now
        })
        .collect())
}

/// The next block of `stream`, its commit last. Empty at the end.
pub(super) fn next_block(stream: &mut Stream) -> Vec<Op> {
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
    let details_before = engine.details()?;
    // What was seen before the phase is not of it.
    engine.gauges()?;
    let space_before = engine.space()?;
    let anatomy_before = engine.node_anatomy();
    let written_before = machine::process_io("write_bytes");
    let reads_before = StorageReads::now();
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
    let ended = engine.counters()?;
    totals.counters = ended.since(&counters_before);
    let ended_details = engine.details()?;
    let details = details_since(ended_details.clone(), &details_before)?;
    let gauges = engine.gauges()?;
    let space = space_before.zip(engine.space()?);
    let size_at_end = disk_usage(engine.dir())?.bytes;
    totals.node_anatomy = engine
        .node_anatomy()
        .zip(anatomy_before)
        .map(|(now, then)| now.since(&then));
    totals.process_write_bytes = match (written_before, machine::process_io("write_bytes")) {
        (Some(before), Some(after)) => Some(after.saturating_sub(before)),
        _ => None,
    };
    totals.storage_reads = StorageReads::now().since(&reads_before);
    Ok(PhaseRun {
        totals,
        transition_us,
        commit_ms,
        apply_meter: apply_meter.finish(),
        commit_meter: commit_meter.finish(),
        details,
        gauges,
        space,
        size_at_end,
        ended: (ended, ended_details),
    })
}

const COST_SOURCE: &str = "GroveDB's OperationCost of every operation, summed";

/// The count and measured rows of a write phase, per state transition.
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
                &format!(
                    "{engine} engine counters: searches by key that went on below the root \
                     of a Merk"
                ),
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
    if let (Some(read_in), Some(held)) = (counters.page_reads, counters.cache_hits) {
        rows.push(measured_row(
            "page_reads_per_transition",
            phase,
            "pages",
            read_in as f64,
            n,
            &format!(
                "{engine} engine counters: page versions that were read from storage, \
                 each in full for a load, its background threads' among them"
            ),
        ));
        rows.push(measured_row(
            "pages_asked_per_transition",
            phase,
            "pages",
            (read_in + held) as f64,
            n,
            &format!(
                "{engine} engine counters: page versions read from storage and asks the \
                 buffer pool answered from memory, its background threads' among them"
            ),
        ));
    }
    rows.extend(storage_read_rows(
        "",
        "_per_transition",
        phase,
        &totals.storage_reads,
        n,
    ));
    for detail in &run.details {
        rows.push(measured_row(
            &format!("{}_per_transition", detail.id),
            phase,
            detail.unit,
            detail.total as f64,
            n,
            detail.source,
        ));
    }
    for gauge in &run.gauges {
        rows.push(measured_row(
            gauge.id,
            phase,
            gauge.unit,
            gauge.total as f64,
            1,
            gauge.source,
        ));
    }
    rows.push(measured_row(
        "size_on_disk_at_end",
        phase,
        "B",
        run.size_at_end as f64,
        1,
        "lengths of the files in the database directory at the phase's end, once the \
         background work it set off is idle",
    ));
    rows.extend(space_rows(run.space, phase, engine));
    rows
}

/// The rows of what an engine's files held at a phase's start and end
/// ([`Engine::space`]): the dead share of the segments and of the value
/// log, and the bytes each share is of. None for an engine that does not
/// count it.
fn space_rows(space: Option<(Space, Space)>, phase: &str, engine: &str) -> Vec<Row> {
    let Some((start, end)) = space else {
        return Vec::new();
    };
    let source = format!(
        "{engine} engine: the cleaner's accounting of the segments and the value log, read \
         at the phase's start and at its end"
    );
    let mut rows = Vec::new();
    for (at, space) in [("start", start), ("end", end)] {
        for (id, unit, value) in [
            ("segment_dead_share", "fraction", space.segment_dead_share()),
            ("segments_size", "B", space.segment_bytes as f64),
            ("value_log_dead_share", "fraction", space.value_dead_share()),
            ("value_log_size", "B", space.value_bytes as f64),
        ] {
            rows.push(measured_row(
                &format!("{id}_at_{at}"),
                phase,
                unit,
                value,
                1,
                &source,
            ));
        }
    }
    rows
}

/// The rows of what the operating system counted of the process's reads
/// (`reads`) over `per` units of work: `<prefix>process_read_bytes<suffix>`,
/// `<prefix>process_read_calls<suffix>`, `<prefix>device_reads<suffix>` and
/// `<prefix>device_read_bytes<suffix>`, each for one unit. No row for a
/// count the system did not give, and none where `per` is 0. They are of
/// the whole process, and the last two of its whole cgroup, so they are
/// measured rows.
fn storage_read_rows(
    prefix: &str,
    suffix: &str,
    phase: &str,
    reads: &StorageReads,
    per: u64,
) -> Vec<Row> {
    if per == 0 {
        return Vec::new();
    }
    [
        (
            "process_read_bytes",
            "B",
            reads.process_bytes,
            "/proc/self/io read_bytes of the whole process: bytes it caused to be read from \
             storage, read-ahead included",
        ),
        (
            "process_read_calls",
            "calls",
            reads.process_calls,
            "/proc/self/io syscr of the whole process: its read system calls, whatever they \
             read and wherever it came from",
        ),
        (
            "device_reads",
            "requests",
            reads.device_reads,
            "io.stat rios of the process's cgroup, summed over its devices: the read I/Os \
             the kernel counted for the cgroup",
        ),
        (
            "device_read_bytes",
            "B",
            reads.device_bytes,
            "io.stat rbytes of the process's cgroup, summed over its devices: the bytes the \
             kernel counted as read for the cgroup",
        ),
    ]
    .into_iter()
    .filter_map(|(what, unit, total, source)| {
        let id = format!("{prefix}{what}{suffix}");
        Some(measured_row(&id, phase, unit, total? as f64, per, source))
    })
    .collect()
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
    /// The reads of the rows of the settled state ([`SETTLED`]): a
    /// stream of their own, which begins the same wherever those rows
    /// are measured, by a whole run and by `reads` over what it kept.
    fn settled(settings: &Settings) -> Result<Reads, BenchError> {
        let mut reads = Self::new(settings)?;
        reads.rng = Rng::derive(settings.seed, DOMAIN_BENCH_READS, 1);
        Ok(reads)
    }

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

/// The rows `<read>_pages_asked` and `<read>_pages_missed` of `phase`,
/// for one read of the `reads` reads of the kind `read` that were timed:
/// `missed` is the page versions the engine read from storage meanwhile,
/// each offered to its buffer pool by a load, and `asked` is those and
/// the asks its pool answered from memory. None for an engine that counts
/// neither, and none where no read was timed. The counts are the
/// engine's own, so what its background threads read and asked for
/// meanwhile is in them: the rows are measured, and no counts.
fn page_rows(read: &str, phase: &str, counted: &Counters, reads: u64) -> Vec<Row> {
    let (Some(read_in), Some(held)) = (counted.page_reads, counted.cache_hits) else {
        return Vec::new();
    };
    if reads == 0 {
        return Vec::new();
    }
    let row = |what: &str, pages: u64, source: &str| {
        let id = format!("{read}_pages_{what}");
        measured_row(&id, phase, "pages", pages as f64, reads, source)
    };
    vec![
        row(
            "asked",
            read_in + held,
            "engine: page versions read from storage and asks the buffer pool answered from \
             memory, over the timed reads",
        ),
        row(
            "missed",
            read_in,
            "engine: page versions read from storage over the timed reads, each in full for a load",
        ),
    ]
}

/// The write phase of the creates before the measured ones
/// ([`Settings::churn`]).
pub const CHURN: &str = "churn";

/// The phase of the warm reads that are made right after the load, with
/// whatever each engine's background work has not done yet: the state
/// the plan's recorded values were taken in.
pub const WARM: &str = "warm";

/// The phase of the warm reads in the settled state: after a flush, after
/// the database was closed and opened again, and after a call of
/// [`Engine::wait_for_background`] on the opened engine. A node reads a
/// database in this state.
pub const SETTLED: &str = "settled";

/// The warm rows of `engine` in `phase` ([`WARM`] or [`SETTLED`]): untimed
/// warm-up reads, then the timed point gets, point proofs and proofs of 100
/// references with the proof sizes. The first proof of each kind is
/// checked against `root_hash`. For an engine that counts them, each
/// kind of read of which a read was timed has two rows more, for one
/// read: the page versions it read from storage over the timed reads,
/// and those together with the asks its buffer pool answered from memory
/// ([`page_rows`]).
fn warm_rows<E: Engine>(
    engine: &mut E,
    reads: &mut Reads,
    settings: &Settings,
    root_hash: &[u8; 32],
    grove_version: &GroveVersion,
    phase: &str,
    rows: &mut Vec<Row>,
) -> Result<(), BenchError> {
    let loaded = settings.documents;
    warm_up(engine, reads, settings, grove_version)?;

    let mut get_meter = Meter::new(settings, &format!("{phase}-get"));
    let mut get_us = Vec::new();
    let before = engine.counters()?;
    let reads_before = StorageReads::now();
    for _ in 0..settings.point_reads {
        let (elapsed, _) =
            ReadKind::Get.read(engine, reads, loaded, grove_version, &mut get_meter)?;
        get_us.push(Unit::Micros.of(elapsed));
    }
    let storage_reads = StorageReads::now().since(&reads_before);
    let counted = engine.counters()?.since(&before);
    rows.extend(page_rows(
        "point_get",
        phase,
        &counted,
        u64::from(settings.point_reads),
    ));
    rows.extend(storage_read_rows(
        "point_get_",
        "",
        phase,
        &storage_reads,
        u64::from(settings.point_reads),
    ));
    rows.push(time_row(
        "point_get",
        phase,
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
            root_hash,
            rows_per_proof,
            grove_version,
        )?;
        let mut meter = Meter::new(settings, &format!("{phase}-{}", kind.id()));
        let mut times = Vec::new();
        let mut sizes = Vec::new();
        let before = engine.counters()?;
        let reads_before = StorageReads::now();
        for _ in 0..count {
            let (elapsed, size) = kind.read(engine, reads, loaded, grove_version, &mut meter)?;
            times.push(Unit::Micros.of(elapsed));
            sizes.push(size.unwrap_or(0) as f64);
        }
        let storage_reads = StorageReads::now().since(&reads_before);
        let counted = engine.counters()?.since(&before);
        rows.extend(page_rows(kind.id(), phase, &counted, u64::from(count)));
        rows.extend(storage_read_rows(
            &format!("{}_", kind.id()),
            "",
            phase,
            &storage_reads,
            u64::from(count),
        ));
        rows.push(time_row(
            kind.id(),
            phase,
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
            phase,
            "B",
            sizes,
            "length of each proof GroveDb::prove_query returned for the timed reads",
        ));
    }
    Ok(())
}

/// Measures the warm rows alone over the `E` database that an earlier run
/// kept in `settings.scratch/<engine>` (`run --keep`), opened as it was
/// left, which is after a flush and a close, and after a wait for the
/// background work of the opened engine. So the rows are of the phase
/// [`SETTLED`], and their reads are that phase's own stream
/// (`Reads::settled`): with the seed, the documents, the owners and the
/// numbers of warm-up and timed reads of the run, they ask for the
/// documents and owners that the run's settled rows asked for, in the
/// same order. The settings name the documents that run loaded, which
/// made no creates, updates or deletes after the load, so every document
/// a read asks for is there.
///
/// The report has no size on disk: neither is measured.
pub fn read_engine<E: Engine>(
    settings: &Settings,
    grove_version: &GroveVersion,
) -> Result<EngineReport, BenchError> {
    let dir = kept_database::<E>(settings)?;
    let mut reads = Reads::settled(settings)?;
    let mut engine = E::open_with(&dir, &settings.engine)?;
    engine.settle()?;
    engine.wait_for_background()?;
    let root_hash = engine.root_hash(grove_version)?;
    let mut rows = Vec::new();
    warm_rows(
        &mut engine,
        &mut reads,
        settings,
        &root_hash,
        grove_version,
        SETTLED,
        &mut rows,
    )?;
    Ok(EngineReport {
        engine: E::NAME.to_string(),
        root_hash_after_load: hex::encode(root_hash),
        root_hash_at_end: hex::encode(engine.root_hash(grove_version)?),
        rows,
        phases: Vec::new(),
        disk_after_load: None,
        disk_after_flush: None,
    })
}

/// The database of `E` that an earlier run kept in
/// `settings.scratch/<engine>` (`run --keep`).
fn kept_database<E: Engine>(settings: &Settings) -> Result<PathBuf, BenchError> {
    let dir = settings.scratch.join(E::NAME);
    if !dir.is_dir() {
        return Err(BenchError::Check(format!(
            "no kept database at {}: `run --keep` leaves one",
            dir.display()
        )));
    }
    Ok(dir)
}

/// The writes at size: `measured_creates` creates, then `updates`
/// replacements and `deletes` deletions, as `stream` gives them, with the
/// rows and the totals of every phase that applies a transition.
fn writes_at_size<E: Engine>(
    engine: &mut E,
    stream: &mut Stream,
    settings: &Settings,
    grove_version: &GroveVersion,
    rows: &mut Vec<Row>,
    phases: &mut Vec<PhaseTotals>,
) -> Result<(), BenchError> {
    for (phase, transitions) in [
        (CHURN, settings.churn),
        ("create", settings.measured_creates),
        ("update", settings.updates),
        ("delete", settings.deletes),
    ] {
        if transitions == 0 {
            continue;
        }
        let run = write_phase(engine, stream, phase, transitions, settings, grove_version)?;
        rows.extend(count_rows(&run, E::NAME));
        phases.push(run.totals.clone());
        rows.extend(window_time_rows(run));
    }
    Ok(())
}

/// Measures the writes at size alone, over a copy of the `E` database
/// that an earlier run kept in `settings.scratch/<engine>`: a run with
/// `keep` and with no creates, updates or deletes after its load, as
/// [`read_engine`] takes it. The copy is `settings.scratch/<engine>-writes`.
/// Every call makes it anew and removes it at its end, unless
/// `settings.keep` is set. The kept database is read and never written,
/// so every call starts from the same state.
///
/// The copy is opened with the engine settings of this call, the
/// background work of the opened engine is waited for, and the untimed
/// warm-up reads are made, so that the engine's cache holds what reads of
/// the loaded state left in it. Then the creates, updates and deletes of
/// the settings are applied as a whole run applies them after its load:
/// the stream is that run's, with the blocks of the set-up and of the
/// load passed over. With the seed, the documents, the owners and the
/// three numbers of writes and the churn of a whole run, the root hash at
/// the end is that run's.
///
/// The report has the rows of the write phases, each phase's size at its
/// end among them, and no size of the database after the load or the
/// flush.
pub fn write_engine<E: Engine>(
    settings: &Settings,
    grove_version: &GroveVersion,
) -> Result<EngineReport, BenchError> {
    settings.check()?;
    let kept = kept_database::<E>(settings)?;
    let dir = settings.scratch.join(format!("{}-writes", E::NAME));
    // Held until this call's copy is measured and removed.
    let _lock = lock_copy(&settings.scratch, E::NAME)?;
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|error| BenchError::io("removing an earlier copy", error))?;
    }
    machine::copy_tree(&kept, &dir)
        .map_err(|error| BenchError::io("copying the kept database", error))?;
    let result = measure_writes::<E>(settings, &dir, grove_version);
    if settings.keep {
        return result;
    }
    // A copy that cannot be removed is said, and fails a call that
    // measured: the next call would find it and may not remove it either.
    match (result, std::fs::remove_dir_all(&dir)) {
        (result, Ok(())) => result,
        (Ok(_), Err(error)) => Err(BenchError::io(
            format!("removing the copy {}", dir.display()),
            error,
        )),
        (Err(first), Err(error)) => {
            eprintln!(
                "lignin-bench: the copy {} could not be removed: {error}",
                dir.display()
            );
            Err(first)
        }
    }
}

/// The lock of the copy that `writes` measures over: the file
/// `<engine>-writes.lock` in the scratch directory, locked for as long as
/// the handle this returns is open. A second call for the same engine and
/// scratch directory would remove the copy under the first, and is
/// refused while the first has the lock. The file stays, empty, and a
/// call that was killed leaves no lock behind: the system lets go of it
/// with the process.
fn lock_copy(scratch: &Path, engine: &str) -> Result<std::fs::File, BenchError> {
    let path = scratch.join(format!("{engine}-writes.lock"));
    let lock = std::fs::File::create(&path)
        .map_err(|error| BenchError::io("creating the lock of the copy", error))?;
    match lock.try_lock() {
        Ok(()) => Ok(lock),
        Err(std::fs::TryLockError::WouldBlock) => Err(BenchError::Check(format!(
            "another `writes` call is measuring over the copy of {engine} in {}:              one call at a time for an engine and a scratch directory",
            scratch.display()
        ))),
        Err(std::fs::TryLockError::Error(error)) => {
            Err(BenchError::io("locking the copy", error))
        }
    }
}

fn measure_writes<E: Engine>(
    settings: &Settings,
    dir: &Path,
    grove_version: &GroveVersion,
) -> Result<EngineReport, BenchError> {
    let mut reads = Reads::new(settings)?;
    let mut stream = settings.workload().stream(settings.seed);
    // The set-up block and the load are in the kept database.
    next_block(&mut stream);
    let mut loaded = 0;
    while loaded < settings.documents {
        let block = next_block(&mut stream);
        if block.is_empty() {
            break;
        }
        loaded += block.iter().filter(|op| is_transition(op)).count() as u64;
    }
    let mut engine = E::open_with(dir, &settings.engine)?;
    engine.settle()?;
    engine.wait_for_background()?;
    let root_hash_after_load = engine.root_hash(grove_version)?;
    warm_up(&engine, &mut reads, settings, grove_version)?;
    let (mut rows, mut phases) = (Vec::new(), Vec::new());
    writes_at_size(
        &mut engine,
        &mut stream,
        settings,
        grove_version,
        &mut rows,
        &mut phases,
    )?;
    let root_hash_at_end = engine.root_hash(grove_version)?;
    Ok(EngineReport {
        engine: E::NAME.to_string(),
        root_hash_after_load: hex::encode(root_hash_after_load),
        root_hash_at_end: hex::encode(root_hash_at_end),
        rows,
        phases,
        disk_after_load: None,
        disk_after_flush: None,
    })
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
    let mut engine = E::open_with(dir, &settings.engine)?;
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
    let (after_load, details_after_load) = load.ended.clone();
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
    warm_rows(
        &mut engine,
        &mut reads,
        settings,
        &root_hash_after_load,
        grove_version,
        WARM,
        &mut rows,
    )?;

    rows.push(checkpoints(&engine, settings, "at-size")?);
    engine.flush()?;
    engine.wait_for_background()?;
    let disk_after_flush = disk_usage(dir)?;
    let flushed_counters = engine.counters()?;
    let settling_details = details_since(engine.details()?, &details_after_load)?;
    rows.push(measured_row(
        "size_on_disk",
        "flushed",
        "B",
        disk_after_flush.bytes as f64,
        1,
        "lengths of the files in the database directory after a flush, once the engine's \
         background work is idle",
    ));
    rows.push(measured_row(
        "physical_bytes_to_flushed",
        "flushed",
        "B",
        flushed_counters
            .physical_bytes()
            .saturating_sub(after_load.physical_bytes()) as f64,
        1,
        "engine counters: log, flush and compaction bytes from the reading that ended the load \
         to the size after the flush: the warm reads, the checkpoints at size, the flush and \
         what the background work did meanwhile",
    ));
    for detail in &settling_details {
        rows.push(measured_row(
            &format!("{}_to_flushed", detail.id),
            "flushed",
            detail.unit,
            detail.total as f64,
            1,
            detail.source,
        ));
    }
    rows.push(checkpoints(&engine, settings, "flushed")?);

    let mut reopen_meter = Meter::new(settings, "reopen");
    let mut reopen_ms = Vec::new();
    for _ in 0..settings.reopens {
        drop(engine);
        let mark = reopen_meter.start();
        let (opened, elapsed) = timed(|| E::open_with(dir, &settings.engine));
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

    // The settled state: the flush is made, the background work is idle
    // again after the checkpoints, the database is opened anew, and the
    // background work of that opened engine is waited for too.
    engine.wait_for_background()?;
    drop(engine);
    engine = E::open_with(dir, &settings.engine)?;
    engine.settle()?;
    engine.wait_for_background()?;
    let mut settled_reads = Reads::settled(settings)?;
    warm_rows(
        &mut engine,
        &mut settled_reads,
        settings,
        &root_hash_after_load,
        grove_version,
        SETTLED,
        &mut rows,
    )?;

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
            let (opened, elapsed) = timed(|| E::open_with(dir, &settings.engine));
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

    writes_at_size(
        &mut engine,
        &mut stream,
        settings,
        grove_version,
        &mut rows,
        &mut phases,
    )?;
    let root_hash_at_end = engine.root_hash(grove_version)?;

    Ok(EngineReport {
        engine: E::NAME.to_string(),
        root_hash_after_load: hex::encode(root_hash_after_load),
        root_hash_at_end: hex::encode(root_hash_at_end),
        rows,
        phases,
        disk_after_load: Some(disk_after_load),
        disk_after_flush: Some(disk_after_flush),
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
    /// The page rows are the pool's counts over the timed reads, for one
    /// read: what it was asked for is what it held and what it did not
    /// hold together. An engine that counts neither has no such row, and
    /// neither has a kind of read of which none was timed.
    #[test]
    fn the_page_rows_are_the_pools_counts_for_one_read() {
        let counted = |hits, misses| Counters {
            cache_hits: hits,
            page_reads: misses,
            ..Counters::default()
        };
        let rows = page_rows("point_get", SETTLED, &counted(Some(6), Some(2)), 2);
        let said: Vec<(&str, &str, f64, &str)> = (rows.iter())
            .map(|row| {
                (
                    row.id.as_str(),
                    row.phase.as_str(),
                    row.value,
                    row.unit.as_str(),
                )
            })
            .collect();
        assert_eq!(
            said,
            [
                ("point_get_pages_asked", "settled", 4.0, "pages"),
                ("point_get_pages_missed", "settled", 1.0, "pages"),
            ]
        );
        assert!(rows.iter().all(|row| row.kind == RowKind::Measured));
        // No read was timed: the counts are of no read.
        assert!(page_rows("point_get", SETTLED, &counted(Some(6), Some(2)), 0).is_empty());
        // An engine that counts one of the two, or neither.
        for (hits, misses) in [(None, Some(2)), (Some(6), None), (None, None)] {
            assert!(page_rows("point_get", WARM, &counted(hits, misses), 2).is_empty());
        }
    }

    use super::*;
    use crate::workload::PartialBatch;

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
            details: vec![Detail {
                id: "segment_bytes",
                unit: "B",
                total: 900,
                source: "lignindb engine counters: write-back",
            }],
            gauges: vec![Detail {
                id: "log_held_max",
                unit: "B",
                total: 4_000,
                source: "lignindb engine: the log that no table checkpoint covers",
            }],
            space: Some((
                Space {
                    segment_bytes: 1_000,
                    segment_live_bytes: 800,
                    value_bytes: 0,
                    value_live_bytes: 0,
                },
                Space {
                    segment_bytes: 2_000,
                    segment_live_bytes: 1_500,
                    value_bytes: 400,
                    value_live_bytes: 100,
                },
            )),
            size_at_end: 12_345,
            ended: (Counters::default(), Vec::new()),
        };
        let rows = count_rows(&run, "lignindb");
        let gauge = rows
            .iter()
            .find(|row| row.id == "log_held_max")
            .expect("the gauge's row");
        assert_eq!(
            (gauge.value, gauge.unit.as_str(), gauge.kind),
            (4_000.0, "B", RowKind::Measured),
            "as it is, and not per transition"
        );
        let detail = rows
            .iter()
            .find(|row| row.id == "segment_bytes_per_transition")
            .expect("the detail's row");
        assert_eq!(
            (detail.value, detail.unit.as_str(), detail.kind),
            (450.0, "B", RowKind::Measured)
        );
        assert_eq!(detail.source, "lignindb engine counters: write-back");
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
            ("page_reads_per_transition", 4.5),
            ("pages_asked_per_transition", 49.5),
        ] {
            let (found, source) = value(id);
            assert_eq!(found, per_transition, "{id}");
            assert!(
                source.starts_with("lignindb engine counters"),
                "{id}: {source}"
            );
        }
        assert_eq!(value("hashes_per_transition").0, 25.0);
        assert_eq!(value("size_on_disk_at_end").0, 12_345.0, "as it is");
        for (id, held) in [
            ("segment_dead_share_at_start", 0.2),
            ("segments_size_at_start", 1_000.0),
            ("value_log_dead_share_at_start", 0.0),
            ("value_log_size_at_start", 0.0),
            ("segment_dead_share_at_end", 0.25),
            ("segments_size_at_end", 2_000.0),
            ("value_log_dead_share_at_end", 0.75),
            ("value_log_size_at_end", 400.0),
        ] {
            let (found, source) = value(id);
            assert_eq!(found, held, "{id}");
            assert!(
                source.contains("the cleaner's accounting"),
                "{id}: {source}"
            );
        }

        // An engine that does not count lookups keeps the derivation from
        // the seeks GroveDB charges, and has no row of engine hashes.
        let mut run = run;
        run.totals.counters.keyed_lookups = None;
        run.totals.counters.hashes = None;
        run.totals.counters.page_reads = None;
        run.totals.storage_reads.device_reads = Some(7);
        run.space = None;
        let rows = count_rows(&run, "rocksdb");
        assert!(!rows.iter().any(|row| row.id.contains("dead_share")));
        assert!(rows.iter().any(|row| row.id == "size_on_disk_at_end"));
        assert!(!rows.iter().any(|row| row.id.starts_with("page")));
        let reads = rows
            .iter()
            .find(|row| row.id == "device_reads_per_transition")
            .expect("the row");
        assert_eq!((reads.value, reads.kind), (3.5, RowKind::Measured));
        let lookups = rows
            .iter()
            .find(|row| row.id == "keyed_lookups_per_transition")
            .expect("the row");
        assert_eq!(lookups.value, (1_000.0 - 35.0) / 2.0);
        assert!(!rows
            .iter()
            .any(|row| row.id == "engine_hashes_per_transition"));
    }

    /// What the system counted of the process's reads is a row for one
    /// unit of work for each count it gave, and none for the others.
    #[test]
    fn the_storage_read_rows_are_the_systems_counts_for_one_unit_of_work() {
        let reads = StorageReads {
            process_bytes: Some(8_192),
            process_calls: Some(6),
            device_reads: Some(3),
            device_bytes: None,
        };
        let rows = storage_read_rows("point_get_", "", "settled", &reads, 2);
        let found: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.id.as_str(),
                    row.phase.as_str(),
                    row.value,
                    row.unit.as_str(),
                    row.kind,
                )
            })
            .collect();
        assert_eq!(
            found,
            vec![
                (
                    "point_get_process_read_bytes",
                    "settled",
                    4_096.0,
                    "B",
                    RowKind::Measured
                ),
                (
                    "point_get_process_read_calls",
                    "settled",
                    3.0,
                    "calls",
                    RowKind::Measured
                ),
                (
                    "point_get_device_reads",
                    "settled",
                    1.5,
                    "requests",
                    RowKind::Measured
                ),
            ]
        );
        assert!(storage_read_rows("", "_per_transition", "load", &reads, 0).is_empty());
        assert!(storage_read_rows("", "", "load", &StorageReads::default(), 1).is_empty());
    }

    /// The details of a phase are what the engine counted more at its end
    /// than at its beginning, and two readings that do not name the same
    /// details are refused.
    #[test]
    fn the_details_of_a_phase_are_differences() {
        let reading = |segments, values| {
            vec![
                Detail {
                    id: "segment_bytes",
                    unit: "B",
                    total: segments,
                    source: "write-back",
                },
                Detail {
                    id: "values_appended",
                    unit: "values",
                    total: values,
                    source: "write-back",
                },
            ]
        };
        let since = details_since(reading(700, 9), &reading(200, 4)).unwrap();
        assert_eq!(since, reading(500, 5));
        assert_eq!(
            details_since(reading(1, 1), &reading(2, 2)).unwrap(),
            reading(0, 0)
        );
        assert_eq!(details_since(Vec::new(), &[]).unwrap(), Vec::new());
        let mut other = reading(1, 1);
        other.swap(0, 1);
        for then in [&other[..], &reading(1, 1)[..1]] {
            assert_eq!(
                details_since(reading(1, 1), then).unwrap_err().to_string(),
                "an engine's details differ between two readings"
            );
        }
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
