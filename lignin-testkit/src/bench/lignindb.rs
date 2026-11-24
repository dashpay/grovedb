//! The harness's engine over LigninDB: `GroveDb` on `LigninEngine`, as
//! `Engine::open` sets it up, with the options a node gets by default
//! unless the run sets one.
//!
//! Every count comes from one reading of the engine's stats
//! ([`Counters::from`] of `lignin_store::stats::StoreStats`), so nothing is
//! collected between commits. The background work is idle once no page
//! version waits to be written back and write-back, the cleaner and the
//! reclaimer have done nothing for a second, or for three of their idle
//! times where that is longer.
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
//!   `table_checkpoint_bytes` add up to the flush bytes;
//! - `cleaner_block_bytes` and `cleaner_value_bytes` add up to the
//!   compaction bytes, in a session whose cleaner met no error.
//!
//! **What no count holds.** The engine also writes its manifest anew
//! whenever the files it names or their lengths change, a footer at the
//! end of each sealed segment, and a header at the start of each file.
//! None of them is in [`Counters::physical_bytes`]; on Linux the
//! `process_write_bytes_per_transition` row shows what they come to.
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
//! | `unwritten` | [`FlushConfig::max_unwritten`] |
//! | `log_held_mb` | [`FlushConfig::max_log_held`] |
//! | `checkpoint_log_mb` | [`FlushConfig::checkpoint_log_bytes`] |
//! | `flush_idle_ms` | [`FlushConfig::idle`] |
//! | `cleaner_target` | [`CleanerConfig::target_dead_percent`] |
//! | `cleaner_value_target` | [`CleanerConfig::value_dead_percent`] |
//! | `cleaner_checkpoint_dead` | [`CleanerConfig::checkpoint_dead_percent`] |
//! | `cleaner_seal_dead` | [`CleanerConfig::seal_dead_percent`] |
//! | `cleaner_max_segment` | [`CleanerConfig::max_segment_percent`] |
//! | `cleaner_min_segment_kb` | [`CleanerConfig::min_segment_len`] |
//! | `cleaner_pass_mb` | [`CleanerConfig::max_pass_bytes`] |
//! | `cleaner_segment_mb` | [`CleanerConfig::segment_len`] |
//! | `cleaner_idle_ms` | [`CleanerConfig::idle`] |
//! | `pace` | [`Pace`]: `free` (the default) or `block` |
//!
//! A share is at most 100, and a length or a time is not 0.
//!
//! # Pace
//!
//! Write-back is woken by every commit and writes each page version that
//! waits as one block. A harness that applies blocks back to back commits
//! faster than blocks arrive in use, so how many commits one block of a
//! page holds follows the machine. With [`Pace::Block`],
//! [`Engine::settle`] waits after every commit, outside every timed
//! region, until a turn of write-back that began after the commit has
//! ended with nothing left to write back: each block's pages are then
//! written by themselves. The cleaner and the reclaimer are not waited
//! for, so what they write still follows their timing.

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
}

/// How long [`Engine::wait_for_background`] and a paced commit wait at most.
const BACKGROUND_TIMEOUT: Duration = Duration::from_secs(1800);
/// Readings of the stats, evenly spaced over the quiet time, that must all
/// find nothing waiting and nothing done since the reading before.
const IDLE_POLLS: u32 = 5;
/// The shortest quiet time.
const QUIET: Duration = Duration::from_secs(1);
/// How long a paced commit sleeps between two looks at write-back.
const PACE_POLL_INTERVAL: Duration = Duration::from_micros(250);

/// When the harness lets the next block begin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pace {
    /// At once: blocks are applied back to back.
    #[default]
    Free,
    /// Once write-back has written the block's pages.
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

fn millis(key: &str, value: &str) -> Result<Duration, BenchError> {
    number(key, value).map(Duration::from_millis)
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
                "chain_deltas" | "chain_bytes" | "segment_mb" | "unwritten" | "log_held_mb"
                | "checkpoint_log_mb" | "flush_idle_ms" => {
                    flush_set = true;
                    match key {
                        // No delta at all is a threshold of write-back.
                        "chain_deltas" => {
                            flush.max_chain_deltas =
                                (value.parse()).map_err(|_| refused(key, "a number", value))?
                        }
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
    /// that is longer, so that each of them has had a turn.
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
/// do: what waits for write-back, and everything that write-back, the
/// cleaner and the reclaimer have written, moved and given back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Background {
    waiting: (usize, usize),
    written: [u64; 4],
    cleaned: [u64; 9],
    reclaimed: (lignindb::db::ReclaimStats, u64),
}

impl Background {
    fn of(stats: &DbStats) -> Self {
        let (store, cleaner) = (&stats.store, &stats.cleaner);
        Background {
            waiting: (store.watch.unwritten, store.watch.pending_value_bytes),
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
        self.waiting != (0, 0)
    }
}

/// The ids of the details, in the order [`Engine::details`] gives them.
pub const DETAIL_IDS: [&str; 33] = [
    "log_group_bytes",
    "log_page_step_bytes",
    "log_page_steps",
    "log_mark_bytes",
    "segment_bytes",
    "base_blocks",
    "base_payload_bytes",
    "delta_blocks",
    "delta_payload_bytes",
    "consolidations",
    "runs_refused",
    "versions_held_for_readers",
    "value_log_bytes",
    "values_appended",
    "value_payload_bytes",
    "regroup_bytes",
    "values_regrouped",
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
];

/// Where the bytes of [`Counters`] went, and what the background work did
/// for them, as running totals.
fn details(stats: &DbStats) -> Vec<Detail> {
    let (log, flush, cleaner) = (&stats.store.log, &stats.store.flush, &stats.cleaner);
    const LOG: &str = "lignindb engine counters: the log's appends by kind";
    const FLUSH: &str = "lignindb engine counters: write-back";
    const CLEANER: &str = "lignindb engine counters: the cleaner";
    const RECLAIMER: &str = "lignindb engine counters: the reclaimer";
    const VALUES: &str = "lignindb engine counters: values read from the value log";
    let totals = [
        ("B", log.group_bytes, LOG),
        ("B", log.change_bytes, LOG),
        ("records", log.changes, LOG),
        ("B", log.mark_bytes, LOG),
        ("B", flush.stored_bytes, FLUSH),
        ("blocks", flush.base_blocks, FLUSH),
        ("B", flush.base_bytes, FLUSH),
        ("blocks", flush.delta_blocks, FLUSH),
        ("B", flush.delta_bytes, FLUSH),
        ("blocks", flush.consolidations, FLUSH),
        ("blocks", flush.runs_refused, FLUSH),
        ("versions", flush.held_for_readers, FLUSH),
        ("B", flush.value_stored_bytes, FLUSH),
        ("values", flush.values_appended, FLUSH),
        ("B", flush.value_bytes, FLUSH),
        ("B", flush.regroup_stored_bytes, FLUSH),
        ("values", flush.values_regrouped, FLUSH),
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
    /// ended with no page version left to write back and no pending value
    /// held: the turn has seen every commit that was published before the
    /// call, and with no commit made meanwhile its count of what is left
    /// is exact.
    fn wait_for_write_back(&self) -> Result<(), BenchError> {
        let deadline = Instant::now() + BACKGROUND_TIMEOUT;
        let begun = self.stats().store.watch.turns_begun;
        loop {
            let watch = self.stats().store.watch;
            let waits = (watch.unwritten, watch.pending_value_bytes) != (0, 0);
            if watch.turns_ended > begun && !waits {
                return Ok(());
            }
            if watch.failing_pages > 0 || Instant::now() > deadline {
                return Err(BenchError::Check(format!(
                    "LigninDB's write-back has not written what it owes: {} page versions \
                     wait, {} pages fail",
                    watch.unwritten, watch.failing_pages
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
        match self.pace {
            Pace::Free => Ok(()),
            Pace::Block => self.wait_for_write_back(),
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
        let mut last = Background::of(&self.stats());
        let mut idle_polls = 0;
        while idle_polls < IDLE_POLLS {
            if Instant::now() > deadline {
                return Err(BenchError::Check(format!(
                    "LigninDB's background work did not go idle in {BACKGROUND_TIMEOUT:?}"
                )));
            }
            std::thread::sleep(self.quiet / IDLE_POLLS);
            let now = Background::of(&self.stats());
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
        let counters = Counters::from(&stats.store);
        self.read = Some(stats);
        Ok(counters)
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
        assert!(!Background::of(&engine.stats()).waits());
        assert_eq!(engine.dir(), dir.path());

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
            "unwritten=13",
            "log_held_mb=17",
            "checkpoint_log_mb=19",
            "flush_idle_ms=23",
            "cleaner_target=29",
            "cleaner_value_target=0",
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
                checkpoint_dead_percent: 37,
                seal_dead_percent: 41,
                max_segment_percent: 100,
                min_segment_len: 47 << 10,
                max_pass_bytes: 53 << 20,
                segment_len: 59 << 20,
                idle: Duration::from_millis(700),
                ..CleanerConfig::new()
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
                "the setting flush_idle_ms takes a number above 0, not \"soon\"",
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

    /// The details are of the reading the counters took, in the order of
    /// their ids, and add up to the counters they split. A paced engine
    /// has settled once a turn of write-back that began after the commit
    /// has left nothing to write back.
    #[test]
    fn details_split_the_counters_of_one_reading() {
        let dir = tempfile::tempdir().unwrap();
        let grove_version = GroveVersion::latest();
        let mut engine = LigninDbEngine::open_with(dir.path(), &settings(&["pace=block"])).unwrap();
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

        let after = engine.counters().unwrap().since(&before);
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
}
