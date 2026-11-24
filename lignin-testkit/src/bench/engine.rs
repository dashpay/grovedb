//! The interface the harness drives an engine through.
//!
//! The harness has a small interface of its own, beside GroveDB's `Engine`:
//! what it measures is a whole `GroveDb` with its counters. An engine opens
//! a database in a directory, applies workload operations block by block
//! through [`crate::workload::apply()`], serves reads and proofs,
//! checkpoints, flushes, and reports [`Counters`]: the physical work it
//! did, which GroveDB's `OperationCost` cannot see.
//!
//! [`RocksDbEngine`](super::RocksDbEngine) implements it for RocksDB, and
//! `LigninDbEngine` (`bench/lignindb.rs`) for LigninDB, over
//! `GroveDb<LigninEngine>`, with its counters read from
//! `lignin-store::stats` through `Counters::from(&StoreStats)`, which fills
//! every field:
//!
//! | `Counters` field | RocksDB | LigninDB |
//! | --- | --- | --- |
//! | `log_bytes` | bytes appended to the write-ahead log | bytes appended to the log: commit groups, page steps and checkpoint marks |
//! | `log_groups` | write batches in the log | log groups |
//! | `node_records`, `node_record_bytes` | puts to the default column family | page deltas of tree pages, and their bytes with those of out-of-line values |
//! | `other_records`, `other_record_bytes` | every other log entry | changes of flat pages |
//! | `flush_bytes` | SST bytes written by flushes | bytes written by write-back: blocks, values, table checkpoints |
//! | `compaction_bytes` | SST bytes written by compactions | bytes written by the cleaner |
//! | `page_reads`, `cache_hits` | not available | buffer pool misses and hits |
//! | `keyed_lookups` | not available | searches by key that went on below the root of a Merk |
//! | `hashes` | not available | hashes the tree counted |
//!
//! The rows from `node_records`, `node_record_bytes`, `other_records`,
//! `other_record_bytes`, `keyed_lookups` and `hashes` are count rows, held
//! to equality between two runs of one commit, so those fields count only
//! the work of the writes themselves, never a background job's. The rows
//! from `log_bytes`, `flush_bytes` and `compaction_bytes` are measured rows.
//!
//! [`Engine::node_anatomy`] is RocksDB's alone: it splits the whole node
//! records RocksDB is handed into keys, values, hashes and links, which is
//! the rewrite LigninDB exists to remove. LigninDB leaves it `None`.

use std::{path::Path, time::Duration};

use grovedb::PathQuery;
use grovedb_costs::OperationCost;
use grovedb_version::version::GroveVersion;
use serde::{Deserialize, Serialize};

use lignin_store::stats::StoreStats;

use super::BenchError;
use crate::workload::Op;

/// A database the harness can measure.
///
/// The harness times every call itself, so an implementation does only the
/// work: no logging, no checks that a production caller would not make.
pub trait Engine: Sized {
    /// The open write transaction of one block.
    type Block<'a>: Block
    where
        Self: 'a;

    /// The engine's name in reports, stable across releases.
    const NAME: &'static str;

    /// Opens the database in `dir`, creating it if the directory is empty.
    fn open(dir: &Path) -> Result<Self, BenchError>;

    /// Opens the database in `dir` as the run's `settings` ask. An engine
    /// that takes no setting opens as [`Engine::open`] does, whatever they
    /// are: a run of two engines hands both the same settings.
    fn open_with(dir: &Path, settings: &EngineSettings) -> Result<Self, BenchError> {
        let _ = settings;
        Self::open(dir)
    }

    /// The directory the database lives in.
    fn dir(&self) -> &Path;

    /// Starts the write transaction of a block.
    fn begin_block(&self) -> Self::Block<'_>;

    /// Called after opening and after every committed block, outside any
    /// timed region, so the engine can collect counters that would be gone
    /// by the next window. Nothing written before the first call counts.
    fn settle(&mut self) -> Result<(), BenchError>;

    /// Applies a read outside any transaction, through
    /// [`crate::workload::apply()`].
    fn read(&self, op: &Op, grove_version: &GroveVersion) -> Result<OperationCost, BenchError>;

    /// Proves `query` against the committed state.
    fn prove(
        &self,
        query: &PathQuery,
        grove_version: &GroveVersion,
    ) -> Result<(Vec<u8>, OperationCost), BenchError>;

    /// The root hash of the committed state.
    fn root_hash(&self, grove_version: &GroveVersion) -> Result<[u8; 32], BenchError>;

    /// Writes a checkpoint of the committed state into `target`, which must
    /// not exist yet.
    fn checkpoint(&self, target: &Path) -> Result<(), BenchError>;

    /// Makes everything committed durable and lets the engine write it back
    /// to its long-term files.
    fn flush(&self) -> Result<(), BenchError>;

    /// Waits until the engine's background work (RocksDB: flushes and
    /// compactions; LigninDB: the flusher and the cleaner) is idle, so a
    /// size on disk or a phase's background bytes depend less on when a
    /// background job ran. They still do: their rows are
    /// [`RowKind::Measured`](super::RowKind::Measured).
    fn wait_for_background(&mut self) -> Result<(), BenchError>;

    /// The engine's running totals since it was opened.
    fn counters(&mut self) -> Result<Counters, BenchError>;

    /// Counts the engine keeps beside its [`Counters`], as running totals
    /// since it was opened, in the same order at every call: where the
    /// bytes of the counters went, and what the background work did. Each
    /// becomes a measured row of every write phase.
    fn details(&mut self) -> Result<Vec<Detail>, BenchError> {
        Ok(Vec::new())
    }

    /// What the engine has seen of its own state since the last call, as
    /// means and peaks over the commits in between, in the same order at
    /// every call. Each becomes a measured row of a write phase under its
    /// own id, as it is and not per transition.
    fn gauges(&mut self) -> Result<Vec<Detail>, BenchError> {
        Ok(Vec::new())
    }

    /// How the Merk node records written since the engine was opened
    /// divide up, for an engine that writes whole node records. Read after
    /// [`Engine::counters`].
    fn node_anatomy(&self) -> Option<NodeAnatomy> {
        None
    }
}

/// The write transaction of one block.
pub trait Block {
    /// Applies one operation through [`crate::workload::apply()`] and returns
    /// what GroveDB charged for it.
    fn apply(&mut self, op: &Op, grove_version: &GroveVersion)
        -> Result<OperationCost, BenchError>;

    /// Commits the block.
    fn commit(self) -> Result<(), BenchError>;
}

/// A count of an engine beside its [`Counters`] ([`Engine::details`]),
/// or a reading of its state ([`Engine::gauges`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Detail {
    /// The row of a detail is `<id>_per_transition`, and the row of a
    /// gauge is `<id>`.
    pub id: &'static str,
    /// The unit of the count.
    pub unit: &'static str,
    /// The running total.
    pub total: u64,
    /// Where the count comes from.
    pub source: &'static str,
}

/// What a run asks of its engines beyond their defaults, as the keys and
/// values of `--set KEY=VALUE`, in the order given. The engine that takes
/// settings says which keys there are.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineSettings(Vec<(String, String)>);

impl EngineSettings {
    /// Adds `pair`, a `KEY=VALUE`. A key that was set before is set anew.
    pub fn set(&mut self, pair: &str) -> Result<(), BenchError> {
        let (key, value) = pair
            .split_once('=')
            .filter(|(key, value)| !key.is_empty() && !value.is_empty())
            .ok_or_else(|| BenchError::Check(format!("--set takes KEY=VALUE, not {pair:?}")))?;
        self.0.retain(|(set, _)| set != key);
        self.0.push((key.to_string(), value.to_string()));
        Ok(())
    }

    /// The keys with their values.
    pub fn pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    /// Each setting as `KEY=VALUE`, for a report.
    pub fn list(&self) -> Vec<String> {
        self.pairs()
            .map(|(key, value)| format!("{key}={value}"))
            .collect()
    }

    /// Whether nothing is set.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Physical work an engine reports, as running totals since it was opened.
///
/// These are the fields STORE-11's stats struct has to fill for LigninDB;
/// the module documentation says where each comes from for RocksDB.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counters {
    /// Bytes appended to the write-ahead log, framing included.
    pub log_bytes: u64,
    /// Commits the log holds: write batches for RocksDB, log groups for
    /// LigninDB.
    pub log_groups: u64,
    /// Merk node records written.
    pub node_records: u64,
    /// Key and value bytes of those records.
    pub node_record_bytes: u64,
    /// Every other record the log holds: deletes, roots, aux and meta.
    pub other_records: u64,
    /// Key and value bytes of those records.
    pub other_record_bytes: u64,
    /// Bytes written by turning the log into long-term files.
    pub flush_bytes: u64,
    /// Bytes written by rewriting long-term files.
    pub compaction_bytes: u64,
    /// Pages read from storage, where the engine counts them.
    pub page_reads: Option<u64>,
    /// Page reads served by the cache, where the engine counts them.
    pub cache_hits: Option<u64>,
    /// Searches by key that went on below the root of a Merk, where the
    /// engine counts them. GroveDB's
    /// `seek_count` is the same on every engine, so it cannot show the
    /// lookups an engine saves (PERF-03).
    #[serde(default)]
    pub keyed_lookups: Option<u64>,
    /// Hashes computed, where the engine counts them, beside what GroveDB
    /// charges as `hash_node_calls`.
    #[serde(default)]
    pub hashes: Option<u64>,
}

impl From<&StoreStats> for Counters {
    /// The counters of a LigninDB store. Every count of the prediction
    /// table in [`StoreStats`] is named here, so a count added there does
    /// not compile until it is given a place.
    fn from(stats: &StoreStats) -> Self {
        let &StoreStats {
            log_bytes,
            log_groups,
            node_records,
            node_record_bytes,
            other_records,
            other_record_bytes,
            flush_bytes,
            cleaner_bytes,
            page_reads,
            cache_hits,
            hashes,
            keyed_lookups,
            value_reads: _,
            value_read_bytes: _,
            watch: _,
            log: _,
            flush: _,
            pool: _,
        } = stats;
        Counters {
            log_bytes,
            log_groups,
            node_records,
            node_record_bytes,
            other_records,
            other_record_bytes,
            flush_bytes,
            compaction_bytes: cleaner_bytes,
            page_reads: Some(page_reads),
            cache_hits: Some(cache_hits),
            keyed_lookups: Some(keyed_lookups),
            hashes: Some(hashes),
        }
    }
}

impl Counters {
    /// Bytes written to storage: the log, flushes and compactions.
    pub fn physical_bytes(&self) -> u64 {
        self.log_bytes + self.flush_bytes + self.compaction_bytes
    }

    /// What was counted between `earlier` and `self`.
    pub fn since(&self, earlier: &Counters) -> Counters {
        let optional = |now: Option<u64>, then: Option<u64>| match (now, then) {
            (Some(now), Some(then)) => Some(now.saturating_sub(then)),
            _ => None,
        };
        Counters {
            log_bytes: self.log_bytes.saturating_sub(earlier.log_bytes),
            log_groups: self.log_groups.saturating_sub(earlier.log_groups),
            node_records: self.node_records.saturating_sub(earlier.node_records),
            node_record_bytes: self
                .node_record_bytes
                .saturating_sub(earlier.node_record_bytes),
            other_records: self.other_records.saturating_sub(earlier.other_records),
            other_record_bytes: self
                .other_record_bytes
                .saturating_sub(earlier.other_record_bytes),
            flush_bytes: self.flush_bytes.saturating_sub(earlier.flush_bytes),
            compaction_bytes: self
                .compaction_bytes
                .saturating_sub(earlier.compaction_bytes),
            page_reads: optional(self.page_reads, earlier.page_reads),
            cache_hits: optional(self.cache_hits, earlier.cache_hits),
            keyed_lookups: optional(self.keyed_lookups, earlier.keyed_lookups),
            hashes: optional(self.hashes, earlier.hashes),
        }
    }
}

/// Times `work`, returning its result and how long it took.
pub(crate) fn timed<T>(work: impl FnOnce() -> T) -> (T, Duration) {
    let start = std::time::Instant::now();
    let result = work();
    (result, start.elapsed())
}

/// How the bytes of whole Merk node records divide up: the record's key,
/// the element, the node's two hashes, its links to its children, and the
/// rest. Shows how much of what is rewritten is unchanged data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeAnatomy {
    /// Records seen.
    pub records: u64,
    /// Records that did not decode as a Merk node; counted as other bytes.
    pub undecoded: u64,
    /// Key bytes: each record's key and the keys its links name.
    pub key_bytes: u64,
    /// Element bytes.
    pub value_bytes: u64,
    /// Each node's key-value hash and value hash.
    pub node_hash_bytes: u64,
    /// The children's hashes the links hold.
    pub link_hash_bytes: u64,
    /// Everything else: lengths, child heights, feature types, aggregates.
    pub other_bytes: u64,
}

impl NodeAnatomy {
    /// Every byte counted.
    pub fn total(&self) -> u64 {
        self.key_bytes
            + self.value_bytes
            + self.node_hash_bytes
            + self.link_hash_bytes
            + self.other_bytes
    }

    /// What was counted between `earlier` and `self`.
    pub fn since(&self, earlier: &NodeAnatomy) -> NodeAnatomy {
        NodeAnatomy {
            records: self.records.saturating_sub(earlier.records),
            undecoded: self.undecoded.saturating_sub(earlier.undecoded),
            key_bytes: self.key_bytes.saturating_sub(earlier.key_bytes),
            value_bytes: self.value_bytes.saturating_sub(earlier.value_bytes),
            node_hash_bytes: self.node_hash_bytes.saturating_sub(earlier.node_hash_bytes),
            link_hash_bytes: self.link_hash_bytes.saturating_sub(earlier.link_hash_bytes),
            other_bytes: self.other_bytes.saturating_sub(earlier.other_bytes),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::RocksDbEngine;

    #[test]
    fn settings_are_keys_with_values_and_the_last_of_a_key_stands() {
        let mut settings = EngineSettings::default();
        assert!(settings.is_empty());
        settings.set("chain_deltas=8").unwrap();
        settings.set("pace=block").unwrap();
        settings.set("chain_deltas=16").unwrap();
        assert!(!settings.is_empty());
        assert_eq!(settings.list(), ["pace=block", "chain_deltas=16"]);
        assert_eq!(
            settings.pairs().collect::<Vec<_>>(),
            [("pace", "block"), ("chain_deltas", "16")]
        );
        for pair in ["", "pace", "=block", "pace="] {
            assert_eq!(
                settings.set(pair).unwrap_err().to_string(),
                format!("--set takes KEY=VALUE, not {pair:?}")
            );
        }
        assert_eq!(settings.list().len(), 2, "a refused pair sets nothing");
    }

    /// An engine that takes no setting opens whatever the run's settings
    /// are, and has no details.
    #[test]
    fn an_engine_without_settings_or_details_ignores_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = EngineSettings::default();
        settings.set("pace=block").unwrap();
        let mut engine = RocksDbEngine::open_with(dir.path(), &settings).unwrap();
        assert_eq!(engine.dir(), dir.path());
        assert_eq!(engine.details().unwrap(), Vec::new());
        assert_eq!(engine.gauges().unwrap(), Vec::new());
    }
}
