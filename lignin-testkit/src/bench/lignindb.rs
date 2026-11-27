//! The harness's engine over LigninDB: `GroveDb` on `LigninEngine`, as
//! `Engine::open` sets it up, with the options a node gets by default
//! unless the run sets one.
//!
//! Every count comes from one reading of the engine's stats, taken
//! where a phase begins and ends: [`Counters::from`] of
//! `lignin_store::stats::StoreStats`, but for the cleaner's bytes, which
//! are the sum of the cleaner's own counts in that reading (the details
//! below say why). After a commit the harness reads the two
//! gauges below, and with [`Pace::Block`] it reads the stats while it
//! waits; nothing else is collected between the commits of a phase.
//!
//! The background work counts as idle once a turn of write-back that
//! began after the wait did has ended, no value waits for its place, and
//! five readings in a row, each a fifth of the quiet time after the one
//! before, found no turn of write-back under way and the same counts of
//! what write-back, the cleaner and the reclaimer have written, moved
//! and given back. The quiet time is a second, or three idle times of
//! the engine's loops where that is longer. That is a sample of those
//! counts and no more: the one turn the harness witnesses is
//! write-back's, and it cannot tell whether the cleaner or the reclaimer
//! ran in that time, nor that nothing is left for them. Pages that
//! write-back leaves for a later turn ([`FlushConfig::max_deferred`])
//! wait on: they are written in a later phase, at a table checkpoint or
//! when the database is closed, and counted where they are written.
//!
//! **A turn of the background work that failed ends the run.** A session
//! counts the failed turns of write-back, the cleaner and the reclaimer
//! from 0 (`DbStats::background`), and write-back tells a turn's failure
//! in one step with its end (`Watch::turns_failed`). The harness refuses
//! a reading in which one of those counts is not 0, where it reads the
//! stats: after every commit ([`Engine::settle`]), at each look of its
//! two waits, and where it reads the counters. A failure after the last
//! such reading of a session is not seen.
//!
//! # Details
//!
//! [`Engine::details`] says where the bytes of the [`Counters`] went
//! ([`DETAIL_IDS`]), from the reading [`Engine::counters`] took last, so
//! the two are of one moment:
//!
//! - `log_group_bytes`, `log_page_step_bytes` and `log_mark_bytes` add up
//!   to the bytes logged;
//! - `segment_bytes`, `value_log_bytes`, `regroup_bytes` and
//!   `table_checkpoint_bytes` add up to the flush bytes. `segment_bytes`
//!   is every block that write-back appended, also one whose link the
//!   page table then refused;
//! - `cleaner_block_bytes` and `cleaner_value_bytes` add up to the
//!   compaction bytes, which the harness takes as their sum and not from
//!   `StoreStats::cleaner_bytes`. The store counts a block when the
//!   cleaner appends it. The cleaner counts it once it has pointed the
//!   page at the copy or found the link gone, and the values of a file
//!   once its pass over the file has ended. A copy that is appended and
//!   not counted so yet is in no row until it is, in however many
//!   readings, and the store's count can be ahead of the rows by it.
//!
//! [`Engine::gauges`] says what stood after the commits of a phase
//! ([`GAUGE_IDS`]), read once after every commit, before anything is
//! waited for, each as the mean and the most over the phase's commits:
//! the log that no table checkpoint covers (`Watch::log_held`), which is
//! about what a recovery after a crash at that moment replays (it also
//! reads the headers of the log's files, and what a file holds before
//! the first group that is not covered), and the page versions that wait
//! to be written back as write-back tells them (`Watch::unwritten`,
//! which a pass that is under way has not brought up to date).
//!
//! **What no count holds.** The engine also writes its manifest anew
//! whenever the files it names or their lengths change, a footer at the
//! end of each sealed segment, and a header at the start of each log
//! file, segment and value-log file. None of them is in
//! [`Counters::physical_bytes`] (a table-checkpoint file is counted
//! whole, its header with it). On Linux the
//! `process_write_bytes_per_transition` row is another count of what the
//! process wrote, with them in it. It is a check beside the engine's
//! counts and not their sum plus those bytes: the file system counts the
//! pages it writes, not the appends.
//!
//! # Settings
//!
//! A run hands its [`EngineSettings`] (`--set KEY=VALUE`) to
//! [`Tuning::new`], which makes the [`Options`] the database is opened
//! with. A key that is not given leaves the engine's default. The
//! thresholds of write-back are those of a store with pages of the
//! default size, which is what the harness creates.
//!
//! | Key | What it sets |
//! | --- | --- |
//! | `cache_mb` | [`Options::cache_size`] |
//! | `log_file_mb` | [`Options::log_file_len`] |
//! | `chain_deltas` | [`FlushConfig::max_chain_deltas`] |
//! | `chain_bytes` | [`FlushConfig::max_chain_bytes`] |
//! | `segment_mb` | [`FlushConfig::segment_len`] |
//! | `deferred` | [`FlushConfig::max_deferred`] |
//! | `unwritten` | [`FlushConfig::max_unwritten`] |
//! | `log_held_mb` | [`FlushConfig::max_log_held`] |
//! | `checkpoint_log_mb` | [`FlushConfig::checkpoint_log_bytes`] |
//! | `flush_idle_ms` | [`FlushConfig::idle`] |
//! | `cleaner_target` | [`CleanerConfig::target_dead_percent`] |
//! | `cleaner_value_target` | [`CleanerConfig::value_dead_percent`] |
//! | `cleaner_value_floor` | [`CleanerConfig::value_min_file_dead_percent`] |
//! | `cleaner_value_ceiling` | [`CleanerConfig::value_ceiling_dead_percent`] |
//! | `cleaner_checkpoint_dead` | [`CleanerConfig::checkpoint_dead_percent`] |
//! | `cleaner_seal_dead` | [`CleanerConfig::seal_dead_percent`] |
//! | `cleaner_max_segment` | [`CleanerConfig::max_segment_percent`] |
//! | `cleaner_min_segment_kb` | [`CleanerConfig::min_segment_len`] |
//! | `cleaner_pass_mb` | [`CleanerConfig::max_pass_bytes`] |
//! | `cleaner_segment_mb` | [`CleanerConfig::segment_len`] |
//! | `cleaner_idle_ms` | [`CleanerConfig::idle`] |
//! | `pace` | [`Pace`]: `free` (the default) or `block` |
//!
//! A share is at most 100, and a length or a time is not 0. An idle time
//! is a minute at most, so that the wait for the background work stays
//! within its bound. `chain_deltas` and `deferred` can be 0.
//!
//! # Pace
//!
//! Write-back is woken by every commit, and a turn of it writes the pages
//! that are due as one block each. A harness that applies blocks back to
//! back commits faster than blocks arrive in use, so how many commits a
//! turn finds follows the machine. With [`Pace::Block`],
//! [`Engine::settle`] waits after every commit, outside every timed
//! region, until a turn of write-back that began after the commit has
//! ended and no value waits for its place: write-back has then had a turn
//! for each block by itself, in which it wrote the pages it did not leave
//! for a later turn. The cleaner and the reclaimer are not waited for, so
//! what they write still follows their timing.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use grovedb::{GroveDb, LigninEngine, PathQuery, Transaction};
use grovedb_costs::{CostContext, OperationCost};
use grovedb_version::version::GroveVersion;
use lignin_format::PageSize;
use lignin_store::{cleaner::CleanerConfig, flush::FlushConfig};
use lignindb::{db::DbStats, Options};

use super::{
    engine::{Block, Counters, Detail, Engine, EngineSettings},
    BenchError,
};
use crate::workload::{apply, Op, Outcome, TransactionOp};

/// GroveDB on LigninDB, as `GroveDb::open_with_engine` sets it up, or as
/// the run's settings ask.
pub struct LigninDbEngine {
    db: GroveDb<LigninEngine>,
    dir: PathBuf,
    pace: Pace,
    /// How long the background work must have done nothing to be idle.
    quiet: Duration,
    /// The reading [`Engine::counters`] took last.
    read: Option<DbStats>,
    /// What stood after each commit since the gauges were last read.
    stood: Stood,
}

/// What stood after the commits that were looked at: the bytes of log that
/// no table checkpoint covered, and the page versions that waited.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Stood {
    looks: u64,
    log_held: (u128, u64),
    waiting: (u128, u64),
}

impl Stood {
    fn look(&mut self, log_held: u64, waiting: u64) {
        self.looks += 1;
        self.log_held = (
            self.log_held.0 + u128::from(log_held),
            self.log_held.1.max(log_held),
        );
        self.waiting = (
            self.waiting.0 + u128::from(waiting),
            self.waiting.1.max(waiting),
        );
    }

    /// The mean and the most of each, in the order of [`GAUGE_IDS`].
    fn gauges(&self) -> Vec<Detail> {
        const SOURCE: &str = "lignindb engine: read after every commit of the phase";
        let mean =
            |sum: u128| u64::try_from(sum / u128::from(self.looks.max(1))).unwrap_or(u64::MAX);
        let read = [
            ("B", mean(self.log_held.0)),
            ("B", self.log_held.1),
            ("versions", mean(self.waiting.0)),
            ("versions", self.waiting.1),
        ];
        GAUGE_IDS
            .into_iter()
            .zip(read)
            .map(|(id, (unit, total))| Detail {
                id,
                unit,
                total,
                source: SOURCE,
            })
            .collect()
    }
}

/// The ids of the gauges, in the order [`Engine::gauges`] gives them.
pub const GAUGE_IDS: [&str; 4] = [
    "log_held_mean",
    "log_held_max",
    "page_versions_waiting_mean",
    "page_versions_waiting_max",
];

/// How long [`Engine::wait_for_background`] and a paced commit wait at most.
const BACKGROUND_TIMEOUT: Duration = Duration::from_secs(1800);
/// Readings of the stats, evenly spaced over the quiet time, that must all
/// find nothing waiting and nothing done since the reading before.
const IDLE_POLLS: u32 = 5;
/// The shortest quiet time.
const QUIET: Duration = Duration::from_secs(1);
/// How long a paced commit sleeps between two looks at write-back.
const PACE_POLL_INTERVAL: Duration = Duration::from_micros(250);
/// The longest idle time a setting can give a loop of the engine: the
/// quiet time is three of them, well within [`BACKGROUND_TIMEOUT`].
const MAX_IDLE: Duration = Duration::from_secs(60);

/// When the harness lets the next block begin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pace {
    /// At once: blocks are applied back to back.
    #[default]
    Free,
    /// Once write-back has had a turn that began after the block's
    /// commit.
    Block,
}

/// What a run's [`EngineSettings`] ask of LigninDB.
#[derive(Clone, Debug)]
pub struct Tuning {
    /// How the database is opened.
    pub options: Options,
    /// When the next block begins.
    pub pace: Pace,
}

fn refused(key: &str, what: &str, value: &str) -> BenchError {
    BenchError::Check(format!("the setting {key} takes {what}, not {value:?}"))
}

/// A count or a length, which is not 0.
fn number<T: std::str::FromStr + Default + PartialEq>(
    key: &str,
    value: &str,
) -> Result<T, BenchError> {
    value
        .parse()
        .ok()
        .filter(|number| *number != T::default())
        .ok_or_else(|| refused(key, "a number above 0", value))
}

/// A length given in units of `unit` bytes.
fn bytes(key: &str, value: &str, unit: u64) -> Result<u64, BenchError> {
    number::<u64>(key, value)?
        .checked_mul(unit)
        .ok_or_else(|| refused(key, "a length that fits 64 bits", value))
}

fn in_memory(key: &str, value: &str, unit: u64) -> Result<usize, BenchError> {
    usize::try_from(bytes(key, value, unit)?)
        .map_err(|_| refused(key, "a length that fits this machine's memory", value))
}

/// A share in a hundred.
fn percent(key: &str, value: &str) -> Result<u8, BenchError> {
    value
        .parse()
        .ok()
        .filter(|percent| *percent <= 100)
        .ok_or_else(|| refused(key, "a share from 0 to 100", value))
}

/// An idle time, which is not 0 and at most [`MAX_IDLE`].
fn millis(key: &str, value: &str) -> Result<Duration, BenchError> {
    (value.parse().ok().map(Duration::from_millis))
        .filter(|idle| !idle.is_zero() && *idle <= MAX_IDLE)
        .ok_or_else(|| refused(key, "a time from 1 to 60000 ms", value))
}

const MIB: u64 = 1 << 20;

impl Tuning {
    /// The options `settings` give. A key that names no setting of
    /// LigninDB, and a value its setting cannot take, are refused.
    pub fn new(settings: &EngineSettings) -> Result<Self, BenchError> {
        let mut options = Options::default();
        let mut flush = FlushConfig::new(PageSize::DEFAULT);
        let mut cleaner = CleanerConfig::new();
        let mut pace = Pace::Free;
        let (mut flush_set, mut cleaner_set) = (false, false);
        for (key, value) in settings.pairs() {
            match key {
                "cache_mb" => options.cache_size = in_memory(key, value, MIB)?,
                "log_file_mb" => options.log_file_len = Some(bytes(key, value, MIB)?),
                "pace" => {
                    pace = match value {
                        "free" => Pace::Free,
                        "block" => Pace::Block,
                        _ => return Err(refused(key, "free or block", value)),
                    }
                }
                "chain_deltas" | "chain_bytes" | "segment_mb" | "deferred" | "unwritten"
                | "log_held_mb" | "checkpoint_log_mb" | "flush_idle_ms" => {
                    flush_set = true;
                    // No delta at all, and no page left for a later turn,
                    // are thresholds of write-back.
                    let count = || value.parse().map_err(|_| refused(key, "a number", value));
                    match key {
                        "chain_deltas" => flush.max_chain_deltas = count()?,
                        "deferred" => flush.max_deferred = count()?,
                        "chain_bytes" => flush.max_chain_bytes = number(key, value)?,
                        "segment_mb" => flush.segment_len = bytes(key, value, MIB)?,
                        "unwritten" => flush.max_unwritten = number(key, value)?,
                        "log_held_mb" => flush.max_log_held = bytes(key, value, MIB)?,
                        "checkpoint_log_mb" => flush.checkpoint_log_bytes = bytes(key, value, MIB)?,
                        _ => flush.idle = millis(key, value)?,
                    }
                }
                "cleaner_target"
                | "cleaner_value_target"
                | "cleaner_value_floor"
                | "cleaner_value_ceiling"
                | "cleaner_checkpoint_dead"
                | "cleaner_seal_dead"
                | "cleaner_max_segment"
                | "cleaner_min_segment_kb"
                | "cleaner_pass_mb"
                | "cleaner_segment_mb"
                | "cleaner_idle_ms" => {
                    cleaner_set = true;
                    match key {
                        "cleaner_target" => cleaner.target_dead_percent = percent(key, value)?,
                        "cleaner_value_target" => cleaner.value_dead_percent = percent(key, value)?,
                        "cleaner_value_floor" => {
                            cleaner.value_min_file_dead_percent = percent(key, value)?
                        }
                        "cleaner_value_ceiling" => {
                            cleaner.value_ceiling_dead_percent = percent(key, value)?
                        }
                        "cleaner_checkpoint_dead" => {
                            cleaner.checkpoint_dead_percent = percent(key, value)?
                        }
                        "cleaner_seal_dead" => cleaner.seal_dead_percent = percent(key, value)?,
                        "cleaner_max_segment" => cleaner.max_segment_percent = percent(key, value)?,
                        "cleaner_min_segment_kb" => {
                            cleaner.min_segment_len = bytes(key, value, 1 << 10)?
                        }
                        "cleaner_pass_mb" => cleaner.max_pass_bytes = bytes(key, value, MIB)?,
                        "cleaner_segment_mb" => cleaner.segment_len = bytes(key, value, MIB)?,
                        _ => cleaner.idle = millis(key, value)?,
                    }
                }
                _ => {
                    return Err(BenchError::Check(format!(
                        "LigninDB has no setting {key:?}"
                    )));
                }
            }
        }
        options.flush = flush_set.then_some(flush);
        options.cleaner = cleaner_set.then_some(cleaner);
        Ok(Tuning { options, pace })
    }

    /// How long the background work must have done nothing to be idle:
    /// [`QUIET`], or three of the longest idle time of its loops where
    /// that is longer, which is time enough for a turn of a loop that
    /// its idle time wakes.
    fn quiet(&self) -> Duration {
        let flush = self.options.flush.map(|flush| flush.idle);
        let cleaner = self.options.cleaner.map(|cleaner| cleaner.idle);
        let idle = [
            flush.unwrap_or(FlushConfig::new(PageSize::DEFAULT).idle),
            cleaner.unwrap_or(CleanerConfig::new().idle),
            self.options.idle,
        ];
        QUIET.max(3 * idle.into_iter().max().unwrap_or_default())
    }
}

/// What of the stats moves while the background work has something to
/// do: whether a turn of write-back is under way, the values that wait
/// for their place, and everything that write-back, the cleaner and the
/// reclaimer have written, moved and given back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Background {
    waiting: (bool, usize),
    written: [u64; 4],
    cleaned: [u64; 9],
    reclaimed: (lignindb::db::ReclaimStats, u64),
}

impl Background {
    fn of(stats: &DbStats) -> Self {
        let (store, cleaner) = (&stats.store, &stats.cleaner);
        Background {
            waiting: (
                store.watch.turns_begun != store.watch.turns_ended,
                store.watch.pending_value_bytes,
            ),
            written: [
                store.log_bytes,
                store.flush_bytes,
                store.cleaner_bytes,
                store.flush.log_files_dropped,
            ],
            cleaned: [
                cleaner.links_trimmed,
                cleaner.bytes_moved,
                cleaner.bytes_lost,
                cleaner.segments_retired,
                cleaner.segments_dropped,
                cleaner.segments_sealed,
                cleaner.value_bytes_copied,
                cleaner.value_files_closed,
                cleaner.value_files_dropped,
            ],
            reclaimed: (stats.reclaimed, stats.ids_released),
        }
    }

    fn waits(&self) -> bool {
        self.waiting != (false, 0)
    }
}

/// Refuses a reading of the stats in which a turn of the background
/// work has failed in the session. The session counts from 0, and
/// write-back's own count is told with the end of the turn that failed.
fn no_failure(stats: &DbStats) -> Result<(), BenchError> {
    let background = &stats.background;
    let failed = [
        background.write_back_errors,
        background.reclaim_errors,
        background.cleaner_errors,
        stats.store.watch.turns_failed,
    ];
    match failed == [0; 4] {
        true => Ok(()),
        false => Err(BenchError::Check(format!(
            "LigninDB's background work failed during the run: {} turns of write-back, {:?}",
            stats.store.watch.turns_failed, background
        ))),
    }
}

/// The bytes of the copies whose outcome the cleaner has counted: the
/// blocks it moved, those whose link was gone when their copy was made,
/// and the values it copied.
fn cleaner_copied(stats: &DbStats) -> u64 {
    let cleaner = &stats.cleaner;
    cleaner.bytes_moved + cleaner.bytes_lost + cleaner.value_bytes_copied
}

/// The ids of the details, in the order [`Engine::details`] gives them.
pub const DETAIL_IDS: [&str; 53] = [
    "log_group_bytes",
    "log_page_step_bytes",
    "log_page_steps",
    "log_mark_bytes",
    "log_inserts",
    "log_insert_bytes",
    "log_removes",
    "log_values",
    "log_value_bytes",
    "log_values_carried",
    "log_value_bytes_carried",
    "segment_bytes",
    "base_blocks",
    "base_payload_bytes",
    "delta_blocks",
    "delta_payload_bytes",
    "consolidations",
    "flat_blocks",
    "flat_block_bytes",
    "runs_refused",
    "chains_read",
    "chain_blocks_read",
    "versions_held_for_readers",
    "value_log_bytes",
    "values_appended",
    "value_payload_bytes",
    "regroup_bytes",
    "values_regrouped",
    "regroup_copies_left",
    "regroup_left_bytes",
    "values_given_up",
    "table_checkpoint_bytes",
    "table_checkpoints",
    "log_files_dropped",
    "room_waits",
    "cleaner_block_bytes",
    "cleaner_blocks_moved",
    "cleaner_value_bytes",
    "cleaner_values_moved",
    "cleaner_bytes_trimmed",
    "cleaner_bytes_dropped",
    "cleaner_segments_dropped",
    "cleaner_value_files_dropped",
    "cleaner_checkpoints",
    "pages_freed",
    "value_reads",
    "block_reads",
    "block_read_bytes",
    "pool_evictions",
    "pool_overflows",
    "pool_releases",
    "pool_loads_overtaken",
    "pool_misses",
];

/// Where the bytes of [`Counters`] went, and what the background work did
/// for them, as running totals.
fn details(stats: &DbStats) -> Vec<Detail> {
    let (log, flush, cleaner) = (&stats.store.log, &stats.store.flush, &stats.cleaner);
    const LOG: &str = "lignindb engine counters: the log's appends by kind";
    const GROUPS: &str = "lignindb engine counters: what the commit groups carry";
    const FLUSH: &str = "lignindb engine counters: write-back";
    const CLEANER: &str = "lignindb engine counters: the cleaner";
    const RECLAIMER: &str = "lignindb engine counters: the reclaimer";
    const VALUES: &str = "lignindb engine counters: values read from the value log";
    const BLOCKS: &str = "lignindb engine counters: blocks read from segments by their locations";
    const POOL: &str = "lignindb engine counters: the buffer pool's frames";
    let totals = [
        ("B", log.group_bytes, LOG),
        ("B", log.change_bytes, LOG),
        ("records", log.changes, LOG),
        ("B", log.mark_bytes, LOG),
        ("records", flush.insert_records, GROUPS),
        ("B", flush.insert_record_bytes, GROUPS),
        ("records", flush.remove_records, GROUPS),
        ("values", flush.values_logged, GROUPS),
        ("B", flush.value_logged_bytes, GROUPS),
        ("values", stats.commits.values_carried, GROUPS),
        ("B", stats.commits.value_bytes_carried, GROUPS),
        ("B", flush.appended_bytes, FLUSH),
        ("blocks", flush.base_blocks, FLUSH),
        ("B", flush.base_bytes, FLUSH),
        ("blocks", flush.delta_blocks, FLUSH),
        ("B", flush.delta_bytes, FLUSH),
        ("blocks", flush.consolidations, FLUSH),
        ("blocks", flush.flat_blocks, FLUSH),
        ("B", flush.flat_stored_bytes, FLUSH),
        ("blocks", flush.runs_refused, FLUSH),
        ("pages", flush.chains_read, FLUSH),
        ("blocks", flush.chain_blocks_read, FLUSH),
        ("versions", flush.held_for_readers, FLUSH),
        ("B", flush.value_stored_bytes, FLUSH),
        ("values", flush.values_appended, FLUSH),
        ("B", flush.value_bytes, FLUSH),
        ("B", flush.regroup_stored_bytes, FLUSH),
        ("values", flush.values_regrouped, FLUSH),
        ("values", flush.regroup_copies_left, FLUSH),
        ("B", flush.regroup_left_bytes, FLUSH),
        ("values", flush.values_given_up, FLUSH),
        ("B", flush.checkpoint_bytes, FLUSH),
        ("checkpoints", flush.checkpoints, FLUSH),
        ("files", flush.log_files_dropped, FLUSH),
        ("waits", flush.room_waits, FLUSH),
        ("B", cleaner.bytes_moved + cleaner.bytes_lost, CLEANER),
        ("blocks", cleaner.blocks_moved, CLEANER),
        ("B", cleaner.value_bytes_copied, CLEANER),
        ("values", cleaner.values_moved, CLEANER),
        ("B", cleaner.bytes_trimmed, CLEANER),
        ("B", cleaner.bytes_dropped, CLEANER),
        ("files", cleaner.segments_dropped, CLEANER),
        ("files", cleaner.value_files_dropped, CLEANER),
        ("checkpoints", cleaner.checkpoints, CLEANER),
        ("pages", stats.reclaimed.pages_freed, RECLAIMER),
        ("values", stats.store.value_reads, VALUES),
        ("blocks", stats.store.block_reads, BLOCKS),
        ("B", stats.store.block_read_bytes, BLOCKS),
        ("frames", stats.store.pool.evictions, POOL),
        ("frames", stats.store.pool.overflows, POOL),
        ("frames", stats.store.pool.releases, POOL),
        ("loads", stats.store.pool.overtaken, POOL),
        ("asks", stats.store.pool.misses, POOL),
    ];
    DETAIL_IDS
        .into_iter()
        .zip(totals)
        .map(|(id, (unit, total, source))| Detail {
            id,
            unit,
            total,
            source,
        })
        .collect()
}

impl LigninDbEngine {
    /// The database underneath.
    pub fn grove(&self) -> &GroveDb<LigninEngine> {
        &self.db
    }

    fn stats(&self) -> DbStats {
        self.db.engine().database().stats()
    }

    /// Waits until a turn of write-back that began after this call has
    /// ended, no pending value is held and no turn of the background work
    /// has failed: the turn has seen every commit that was published
    /// before the call, has placed their values and has written the
    /// pages that it did not leave for a later turn.
    fn wait_for_write_back(&self, deadline: Instant) -> Result<(), BenchError> {
        let first = self.stats();
        no_failure(&first)?;
        let begun = first.store.watch.turns_begun;
        loop {
            let stats = self.stats();
            no_failure(&stats)?;
            let watch = stats.store.watch;
            if watch.turns_ended > begun && watch.pending_value_bytes == 0 {
                return Ok(());
            }
            if watch.failing_pages > 0 || Instant::now() > deadline {
                return Err(BenchError::Check(format!(
                    "LigninDB's write-back has not had its turn: {} bytes of values wait \
                     for their place, {} pages fail",
                    watch.pending_value_bytes, watch.failing_pages
                )));
            }
            std::thread::sleep(PACE_POLL_INTERVAL);
        }
    }
}

impl Engine for LigninDbEngine {
    type Block<'a> = LigninDbBlock<'a>;

    const NAME: &'static str = "lignindb";

    fn open(dir: &Path) -> Result<Self, BenchError> {
        Self::open_with(dir, &EngineSettings::default())
    }

    fn open_with(dir: &Path, settings: &EngineSettings) -> Result<Self, BenchError> {
        let tuning = Tuning::new(settings)?;
        let engine =
            LigninEngine::open_with(dir, &tuning.options).map_err(grovedb::Error::StorageError)?;
        Ok(LigninDbEngine {
            db: GroveDb::from_engine(engine),
            dir: dir.to_path_buf(),
            pace: tuning.pace,
            quiet: tuning.quiet(),
            read: None,
            stood: Stood::default(),
        })
    }

    fn dir(&self) -> &Path {
        &self.dir
    }

    fn begin_block(&self) -> LigninDbBlock<'_> {
        LigninDbBlock {
            db: &self.db,
            transaction: self.db.start_transaction(),
            snapshot: None,
        }
    }

    fn settle(&mut self) -> Result<(), BenchError> {
        let stats = self.stats();
        no_failure(&stats)?;
        let watch = stats.store.watch;
        self.stood.look(watch.log_held, watch.unwritten as u64);
        match self.pace {
            Pace::Free => Ok(()),
            Pace::Block => self.wait_for_write_back(Instant::now() + BACKGROUND_TIMEOUT),
        }
    }

    fn read(&self, op: &Op, grove_version: &GroveVersion) -> Result<OperationCost, BenchError> {
        let CostContext { value, cost } = apply(&self.db, op, None, grove_version);
        value?;
        Ok(cost)
    }

    fn prove(
        &self,
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(Vec<u8>, OperationCost), BenchError> {
        let CostContext { value, cost } = self.db.prove_query(query, None, grove_version);
        Ok((value?, cost))
    }

    fn root_hash(&self, grove_version: &GroveVersion) -> Result<[u8; 32], BenchError> {
        Ok(self.db.root_hash(None, grove_version).value?)
    }

    fn checkpoint(&self, target: &Path) -> Result<(), BenchError> {
        Ok(self.db.create_checkpoint(target)?)
    }

    fn flush(&self) -> Result<(), BenchError> {
        Ok(self.db.flush()?)
    }

    fn wait_for_background(&mut self) -> Result<(), BenchError> {
        let deadline = Instant::now() + BACKGROUND_TIMEOUT;
        // A turn of write-back first, so that what the last commit left
        // to do is not taken for nothing to do before a loop has seen it.
        self.wait_for_write_back(deadline)?;
        let first = self.stats();
        no_failure(&first)?;
        let mut last = Background::of(&first);
        let mut idle_polls = 0;
        // A reading counts for the quiet time only after a whole interval,
        // so the wait ends where the bound leaves no room for one.
        let interval = self.quiet / IDLE_POLLS;
        while idle_polls < IDLE_POLLS {
            if deadline.saturating_duration_since(Instant::now()) < interval {
                return Err(BenchError::Check(format!(
                    "LigninDB's background work did not go idle in {BACKGROUND_TIMEOUT:?}"
                )));
            }
            std::thread::sleep(interval);
            let stats = self.stats();
            no_failure(&stats)?;
            let now = Background::of(&stats);
            idle_polls = if !now.waits() && now == last {
                idle_polls + 1
            } else {
                0
            };
            last = now;
        }
        Ok(())
    }

    fn counters(&mut self) -> Result<Counters, BenchError> {
        let stats = self.stats();
        no_failure(&stats)?;
        let mut counters = Counters::from(&stats.store);
        // The store's own count of the cleaner's bytes and the cleaner's
        // counts of what it copied are told at different moments. The
        // rows are of the cleaner's counts, and so is the headline, so
        // that the two are of one observation.
        counters.compaction_bytes = cleaner_copied(&stats);
        self.read = Some(stats);
        Ok(counters)
    }

    fn gauges(&mut self) -> Result<Vec<Detail>, BenchError> {
        Ok(std::mem::take(&mut self.stood).gauges())
    }

    fn details(&mut self) -> Result<Vec<Detail>, BenchError> {
        let read = self.read.take();
        let stats = read.unwrap_or_else(|| self.stats());
        let details = details(&stats);
        self.read = Some(stats);
        Ok(details)
    }
}

/// The write transaction of one block on [`LigninDbEngine`], and a snapshot
/// read transaction while the block holds one open, as
/// [`RocksDbBlock`](super::rocksdb::RocksDbBlock) keeps them.
pub struct LigninDbBlock<'a> {
    db: &'a GroveDb<LigninEngine>,
    transaction: Transaction<'a, LigninEngine>,
    snapshot: Option<Transaction<'a, LigninEngine>>,
}

impl Block for LigninDbBlock<'_> {
    fn apply(
        &mut self,
        op: &Op,
        grove_version: &GroveVersion,
    ) -> Result<OperationCost, BenchError> {
        let transaction = match op {
            Op::Transaction(TransactionOp::SnapshotRead(_)) => {
                self.snapshot.as_ref().ok_or_else(|| {
                    BenchError::Check("a snapshot read needs an open snapshot".to_string())
                })?
            }
            _ => &self.transaction,
        };
        let CostContext { value, cost } = apply(self.db, op, Some(transaction), grove_version);
        match value? {
            Outcome::Abandoned => self.transaction = self.db.start_transaction(),
            Outcome::SnapshotOpens => {
                self.snapshot = Some(self.db.start_snapshot_read_transaction())
            }
            Outcome::SnapshotCloses => self.snapshot = None,
            _ => {}
        }
        Ok(cost)
    }

    fn commit(self) -> Result<(), BenchError> {
        if self.snapshot.is_some() {
            return Err(BenchError::Check(
                "a snapshot read transaction is open at the end of the block; the harness \
                 keeps one only within a block"
                    .to_string(),
            ));
        }
        Ok(self.db.commit_transaction(self.transaction).value?)
    }
}

#[cfg(test)]
mod tests {
    use grovedb::Element;

    use super::*;
    use crate::workload::{Path as GrovePath, Read, Write};

    fn insert(path: GrovePath, key: &[u8], element: Element) -> Op {
        Op::Write(Write::Insert {
            path,
            key: key.to_vec(),
            element,
        })
    }

    #[test]
    fn counters_come_from_the_stats_of_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let grove_version = GroveVersion::latest();
        let mut engine = LigninDbEngine::open(dir.path()).unwrap();
        engine.settle().unwrap();
        let opened = engine.counters().unwrap();

        let mut block = engine.begin_block();
        block
            .apply(
                &insert(vec![], b"tree", Element::empty_tree()),
                grove_version,
            )
            .unwrap();
        block
            .apply(
                &insert(
                    vec![b"tree".to_vec()],
                    b"key",
                    Element::new_item(b"value".to_vec()),
                ),
                grove_version,
            )
            .unwrap();
        block.commit().unwrap();
        engine.flush().unwrap();
        engine.wait_for_background().unwrap();

        let written = engine.counters().unwrap().since(&opened);
        assert_eq!(written.log_groups, 1);
        assert!(written.node_records > 0 && written.log_bytes > 0);
        assert!(written.keyed_lookups.is_some() && written.hashes.is_some());
        assert_eq!(engine.dir(), dir.path());

        // A reading in which a turn of the background work has failed is
        // refused, whichever count tells of it. The cleaner's bytes are
        // those of the copies it has told of, whatever the store has
        // counted of an append since.
        let mut read = engine.stats();
        assert!(no_failure(&read).is_ok());
        let copied = cleaner_copied(&read);
        read.store.cleaner_bytes += 1;
        assert_eq!(cleaner_copied(&read), copied);
        read.cleaner.bytes_lost += 1;
        read.cleaner.bytes_moved += 2;
        read.cleaner.value_bytes_copied += 4;
        assert_eq!(cleaner_copied(&read), copied + 7);
        for failed in 0..4 {
            let mut read = engine.stats();
            match failed {
                0 => read.background.write_back_errors += 1,
                1 => read.background.reclaim_errors += 1,
                2 => read.background.cleaner_errors += 1,
                _ => read.store.watch.turns_failed += 1,
            }
            let error = no_failure(&read).unwrap_err().to_string();
            assert!(error.contains("background work failed"), "{error}");
        }

        let root = engine.root_hash(grove_version).unwrap();
        let read = engine.grove().root_hash(None, grove_version).unwrap();
        assert_eq!(read.unwrap(), root);
        let query = PathQuery::new_single_key(vec![b"tree".to_vec()], b"key".to_vec());
        let (proof, _) = engine.prove(&query, grove_version).unwrap();
        let (proved, _) = GroveDb::verify_query(&proof, &query, grove_version).unwrap();
        assert_eq!(proved, root);
        let target = dir.path().join("checkpoint");
        engine.checkpoint(&target).unwrap();
        assert!(target.is_dir());
    }

    #[test]
    fn a_block_keeps_a_snapshot_only_within_itself() {
        let dir = tempfile::tempdir().unwrap();
        let grove_version = GroveVersion::latest();
        let engine = LigninDbEngine::open(dir.path()).unwrap();
        let mut block = engine.begin_block();
        let read = Read::Get {
            path: vec![],
            key: b"key".to_vec(),
        };
        let refused = block.apply(
            &Op::Transaction(TransactionOp::SnapshotRead(read)),
            grove_version,
        );
        assert!(matches!(refused, Err(BenchError::Check(_))));
        block
            .apply(&Op::Transaction(TransactionOp::OpenSnapshot), grove_version)
            .unwrap();
        assert!(matches!(block.commit(), Err(BenchError::Check(_))));
    }
}

#[cfg(test)]
mod settings_tests {
    use grovedb::Element;

    use super::*;
    use crate::workload::Write;

    fn settings(pairs: &[&str]) -> EngineSettings {
        let mut settings = EngineSettings::default();
        for pair in pairs {
            settings.set(pair).unwrap();
        }
        settings
    }

    #[test]
    fn every_setting_reaches_its_option() {
        let defaults = Tuning::new(&EngineSettings::default()).unwrap();
        assert_eq!(defaults.pace, Pace::Free);
        assert_eq!(defaults.options.cache_size, Options::default().cache_size);
        assert_eq!(defaults.options.log_file_len, None);
        assert_eq!(
            (defaults.options.flush, defaults.options.cleaner),
            (None, None)
        );
        assert_eq!(defaults.quiet(), QUIET);

        let tuning = Tuning::new(&settings(&[
            "cache_mb=3",
            "log_file_mb=5",
            "chain_deltas=0",
            "chain_bytes=9000",
            "segment_mb=11",
            "deferred=0",
            "unwritten=13",
            "log_held_mb=17",
            "checkpoint_log_mb=19",
            "flush_idle_ms=23",
            "cleaner_target=29",
            "cleaner_value_target=0",
            "cleaner_value_floor=31",
            "cleaner_value_ceiling=43",
            "cleaner_checkpoint_dead=37",
            "cleaner_seal_dead=41",
            "cleaner_max_segment=100",
            "cleaner_min_segment_kb=47",
            "cleaner_pass_mb=53",
            "cleaner_segment_mb=59",
            "cleaner_idle_ms=700",
            "pace=block",
            "pace=free",
            "pace=block",
        ]))
        .unwrap();
        assert_eq!(tuning.pace, Pace::Block);
        assert_eq!(tuning.options.cache_size, 3 << 20);
        assert_eq!(tuning.options.log_file_len, Some(5 << 20));
        assert_eq!(
            tuning.options.flush,
            Some(FlushConfig {
                max_chain_deltas: 0,
                max_chain_bytes: 9000,
                segment_len: 11 << 20,
                max_deferred: 0,
                max_unwritten: 13,
                max_log_held: 17 << 20,
                checkpoint_log_bytes: 19 << 20,
                idle: Duration::from_millis(23),
                ..FlushConfig::new(PageSize::DEFAULT)
            })
        );
        assert_eq!(
            tuning.options.cleaner,
            Some(CleanerConfig {
                target_dead_percent: 29,
                value_dead_percent: 0,
                value_min_file_dead_percent: 31,
                value_ceiling_dead_percent: 43,
                checkpoint_dead_percent: 37,
                seal_dead_percent: 41,
                max_segment_percent: 100,
                min_segment_len: 47 << 10,
                max_pass_bytes: 53 << 20,
                segment_len: 59 << 20,
                idle: Duration::from_millis(700),
            })
        );
        // Three turns of the slowest loop, the cleaner's here.
        assert_eq!(tuning.quiet(), Duration::from_millis(2100));

        // A setting of one part leaves the other part the engine's own.
        let one = Tuning::new(&settings(&["cleaner_target=20"])).unwrap();
        assert_eq!(one.options.flush, None);
        assert_eq!(
            one.options
                .cleaner
                .map(|cleaner| cleaner.target_dead_percent),
            Some(20)
        );
    }

    #[test]
    fn a_key_or_a_value_that_no_setting_takes_is_refused() {
        for (pair, message) in [
            (
                "compression=zstd",
                "LigninDB has no setting \"compression\"",
            ),
            (
                "chain_deltas=many",
                "the setting chain_deltas takes a number, not \"many\"",
            ),
            (
                "chain_bytes=0",
                "the setting chain_bytes takes a number above 0, not \"0\"",
            ),
            (
                "segment_mb=0",
                "the setting segment_mb takes a number above 0, not \"0\"",
            ),
            (
                "flush_idle_ms=soon",
                "the setting flush_idle_ms takes a time from 1 to 60000 ms, not \"soon\"",
            ),
            (
                "flush_idle_ms=0",
                "the setting flush_idle_ms takes a time from 1 to 60000 ms, not \"0\"",
            ),
            (
                "cleaner_idle_ms=86400000",
                "the setting cleaner_idle_ms takes a time from 1 to 60000 ms, not \"86400000\"",
            ),
            (
                "cleaner_target=101",
                "the setting cleaner_target takes a share from 0 to 100, not \"101\"",
            ),
            (
                "cleaner_seal_dead=-1",
                "the setting cleaner_seal_dead takes a share from 0 to 100, not \"-1\"",
            ),
            (
                "cache_mb=99999999999999999",
                "the setting cache_mb takes a length that fits 64 bits, not \
                 \"99999999999999999\"",
            ),
            (
                "cleaner_min_segment_kb=99999999999999999",
                "the setting cleaner_min_segment_kb takes a length that fits 64 bits, not \
                 \"99999999999999999\"",
            ),
            (
                "pace=slow",
                "the setting pace takes free or block, not \"slow\"",
            ),
        ] {
            assert_eq!(
                Tuning::new(&settings(&[pair])).unwrap_err().to_string(),
                message
            );
        }
        let dir = tempfile::tempdir().unwrap();
        assert!(LigninDbEngine::open_with(dir.path(), &settings(&["pace=slow"])).is_err());
    }

    /// The details are of the reading the counters took, also where the
    /// engine has done more since, in the order of their ids, and add up
    /// to the counters they split (the cleaner has moved nothing here, so
    /// its rows add up as 0). A paced engine has settled once a turn of
    /// write-back that began after the commit has ended: with no page
    /// left for a later turn, nothing waits to be written back then.
    #[test]
    fn details_split_the_counters_of_one_reading() {
        let dir = tempfile::tempdir().unwrap();
        let grove_version = GroveVersion::latest();
        let paced = settings(&["pace=block", "deferred=0"]);
        let mut engine = LigninDbEngine::open_with(dir.path(), &paced).unwrap();
        // Before any reading of the counters the details take their own.
        let fresh = engine.details().unwrap();
        assert_eq!(
            fresh.iter().map(|detail| detail.id).collect::<Vec<_>>(),
            DETAIL_IDS
        );
        engine.settle().unwrap();
        let before = engine.counters().unwrap();
        let details_before = engine.details().unwrap();

        let insert = |path: Vec<Vec<u8>>, key: &[u8], element| {
            Op::Write(Write::Insert {
                path,
                key: key.to_vec(),
                element,
            })
        };
        let mut block = engine.begin_block();
        block
            .apply(
                &insert(vec![], b"tree", Element::empty_tree()),
                grove_version,
            )
            .unwrap();
        block
            .apply(
                &insert(
                    vec![b"tree".to_vec()],
                    b"key",
                    Element::new_item(vec![7; 1000]),
                ),
                grove_version,
            )
            .unwrap();
        block.commit().unwrap();
        let begun = engine.stats().store.watch.turns_begun;
        engine.settle().unwrap();
        let watch = engine.stats().store.watch;
        assert!(
            watch.turns_ended > begun,
            "a turn that began after the commit"
        );
        assert_eq!((watch.unwritten, watch.pending_value_bytes), (0, 0));

        // One look after the open and one after the commit, whose group
        // stood in the log then: no table checkpoint was due.
        let gauges = engine.gauges().unwrap();
        assert_eq!(
            gauges.iter().map(|gauge| gauge.id).collect::<Vec<_>>(),
            GAUGE_IDS
        );
        let (held_mean, held_max) = (gauges[0].total, gauges[1].total);
        assert!(held_max > 1000 && held_mean <= held_max, "{gauges:?}");
        assert!(gauges[2].total <= gauges[3].total, "{gauges:?}");
        assert!(engine
            .gauges()
            .unwrap()
            .iter()
            .all(|gauge| gauge.total == 0));

        let after = engine.counters().unwrap().since(&before);
        // A block between the reading and the details: they are of the
        // reading all the same.
        let mut block = engine.begin_block();
        let more = Element::new_item(vec![9; 2000]);
        block
            .apply(
                &insert(vec![b"tree".to_vec()], b"more", more),
                grove_version,
            )
            .unwrap();
        block.commit().unwrap();
        engine.settle().unwrap();
        let details = engine.details().unwrap();
        let grown = |id: &str| {
            let total = |details: &[Detail]| {
                details
                    .iter()
                    .find(|detail| detail.id == id)
                    .unwrap_or_else(|| panic!("no detail {id}"))
                    .total
            };
            total(&details) - total(&details_before)
        };
        // The value of 1,000 bytes went to the value log with its record
        // header, and its page was written as an image.
        assert_eq!(grown("values_appended"), 1);
        assert!(grown("value_payload_bytes") >= 1000);
        assert!(grown("value_log_bytes") > grown("value_payload_bytes"));
        assert!(grown("log_group_bytes") > 1000);
        assert!(grown("base_blocks") >= 1 && grown("segment_bytes") > 0);
        assert_eq!(
            after.log_bytes,
            grown("log_group_bytes") + grown("log_page_step_bytes") + grown("log_mark_bytes")
        );
        assert_eq!(
            after.flush_bytes,
            grown("segment_bytes")
                + grown("value_log_bytes")
                + grown("regroup_bytes")
                + grown("table_checkpoint_bytes")
        );
        assert_eq!(
            after.compaction_bytes,
            grown("cleaner_block_bytes") + grown("cleaner_value_bytes")
        );
    }

    /// The gauges are the mean and the most of what was looked at.
    #[test]
    fn gauges_are_the_mean_and_the_most_of_the_looks() {
        let mut stood = Stood::default();
        for (log_held, waiting) in [(10, 1), (20, 5), (60, 3)] {
            stood.look(log_held, waiting);
        }
        let read: Vec<(&str, u64)> = (stood.gauges().iter())
            .map(|gauge| (gauge.id, gauge.total))
            .collect();
        assert_eq!(
            read,
            [
                ("log_held_mean", 30),
                ("log_held_max", 60),
                ("page_versions_waiting_mean", 3),
                ("page_versions_waiting_max", 5),
            ]
        );
        assert!(Stood::default()
            .gauges()
            .iter()
            .all(|gauge| gauge.total == 0));
    }
}
