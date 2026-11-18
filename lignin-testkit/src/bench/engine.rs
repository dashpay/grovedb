//! The interface the harness drives an engine through.
//!
//! The `Engine` trait of SEAM-04 does not exist yet, so the harness has its
//! own small one. An engine opens a database in a directory, applies
//! workload operations block by block through [`crate::workload::apply`],
//! serves reads and proofs, checkpoints, flushes, and reports
//! [`Counters`]: the physical work it did, which GroveDB's
//! `OperationCost` cannot see.
//!
//! [`RocksDbEngine`](super::RocksDbEngine) implements it for today's
//! engine. What LigninDB has to implement is the same trait over
//! `GroveDb<LigninEngine>` (GROVE-01), with its counters read from
//! `lignin-store::stats` (STORE-11):
//!
//! | `Counters` field | RocksDB | LigninDB |
//! | --- | --- | --- |
//! | `log_bytes` | bytes appended to the write-ahead log | bytes of log groups |
//! | `log_groups` | write batches in the log | log groups |
//! | `node_records`, `node_record_bytes` | puts to the default column family | value cells and hash records |
//! | `other_records`, `other_record_bytes` | every other log entry | flat keyspace records |
//! | `flush_bytes` | SST bytes written by flushes | bytes written by the flusher |
//! | `compaction_bytes` | SST bytes written by compactions | bytes written by the cleaner |
//! | `page_reads`, `cache_hits` | not available | buffer pool reads and hits |
//!
//! [`Engine::node_anatomy`] is RocksDB's alone: it splits the whole node
//! records RocksDB is handed into keys, values, hashes and links, which is
//! the rewrite LigninDB exists to remove. LigninDB leaves it `None`.

use std::{path::Path, time::Duration};

use grovedb::PathQuery;
use grovedb_costs::OperationCost;
use grovedb_version::version::GroveVersion;
use serde::{Deserialize, Serialize};

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

    /// The directory the database lives in.
    fn dir(&self) -> &Path;

    /// Starts the write transaction of a block.
    fn begin_block(&self) -> Self::Block<'_>;

    /// Called after opening and after every committed block, outside any
    /// timed region, so the engine can collect counters that would be gone
    /// by the next window. Nothing written before the first call counts.
    fn settle(&mut self) -> Result<(), BenchError>;

    /// Applies a read outside any transaction, through
    /// [`crate::workload::apply`].
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
    /// size on disk does not depend on when a background job ran.
    fn wait_for_background(&mut self) -> Result<(), BenchError>;

    /// The engine's running totals since it was opened.
    fn counters(&mut self) -> Result<Counters, BenchError>;

    /// How the Merk node records written since the engine was opened
    /// divide up, for an engine that writes whole node records. Read after
    /// [`Engine::counters`].
    fn node_anatomy(&self) -> Option<NodeAnatomy> {
        None
    }
}

/// The write transaction of one block.
pub trait Block {
    /// Applies one operation through [`crate::workload::apply`] and returns
    /// what GroveDB charged for it.
    fn apply(&mut self, op: &Op, grove_version: &GroveVersion)
        -> Result<OperationCost, BenchError>;

    /// Commits the block.
    fn commit(self) -> Result<(), BenchError>;
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
