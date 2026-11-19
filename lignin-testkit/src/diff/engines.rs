//! The two engines the fuzzer compares.
//!
//! [`Reference`] is the engine every answer is measured against: RocksDB,
//! as `GroveDb` runs on it today. [`Candidate`] is the engine under test.
//! Until LigninDB exists it is [`StandIn`]: RocksDB again, behind an
//! [`Engine`] implementation of its own.
//!
//! # Registering an engine
//!
//! The candidate is one line, the type alias [`Candidate`]. The engine it
//! names has to be a [`StreamEngine`], which every engine a workload stream
//! is applied to is: one function that says whether a transaction is a
//! snapshot read transaction. Nothing else in the fuzzer names an engine.
//!
//! # Why a stand-in can differ
//!
//! `GroveDb<StandIn>` is a second instance of all of GroveDB, compiled
//! against the seam and nothing below it, so it reaches RocksDB only
//! through the operations of `Engine`. It differs from the reference in the
//! one way the seam allows an engine to differ and LigninDB will: the
//! transaction of a read that was given none is pinned to the state
//! committed when it starts (RD2 of `docs/lignindb/transactions.md`), where
//! the RocksDB engine starts a write transaction for it. A read that wrote
//! through its transaction is refused on the stand-in and not on the
//! reference, and the fuzzer reports the step.
//!
//! It is also where a defect is planted, so that the fuzzer is shown to
//! find one ([`Fault`]).

#![expect(
    clippy::result_large_err,
    reason = "a database is opened with GroveDB's own error, whatever its size"
)]

use std::{fmt, path::Path, str::FromStr};

use grovedb::{engine::SubtreeCursor, Engine, GroveDb, RocksDbEngine, Transaction};
use grovedb_costs::{CostContext, CostResult};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    flat::{PendingPrefixDropRecord, PendingPrefixDrops, SubtreePrefix},
    Error as StorageError,
};

use super::pair::Side;
use crate::workload::StreamEngine;

/// The engine the candidate is compared with.
pub type Reference = RocksDbEngine;

/// The engine under test. GROVE-10 names LigninDB's engine here.
pub type Candidate = StandIn;

/// A defect planted in a [`StandIn`].
///
/// Each one changes a single cost by a single byte, and only for one size
/// of write: what an engine that works a charge out from logical sizes gets
/// wrong at one boundary. Root hashes, results and proofs stay as they are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fault {
    /// No defect.
    #[default]
    None,
    /// A batch that adds exactly this many bytes is charged for one more.
    AddedByte {
        /// The added bytes of the batches charged wrongly.
        when_added: u32,
    },
    /// A batch whose settling loads exactly this many bytes, for the
    /// deletes it holds that carry no cost, is charged for one more.
    LoadedByte {
        /// The loaded bytes of the batches charged wrongly.
        when_loaded: u64,
    },
    /// The engine panics where a batch adds exactly this many bytes.
    Panic {
        /// The added bytes of the batches the engine panics on.
        when_added: u32,
    },
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fault::None => write!(f, "none"),
            Fault::AddedByte { when_added } => write!(f, "added-byte:{when_added}"),
            Fault::LoadedByte { when_loaded } => write!(f, "loaded-byte:{when_loaded}"),
            Fault::Panic { when_added } => write!(f, "panic:{when_added}"),
        }
    }
}

impl FromStr for Fault {
    type Err = String;

    /// A fault as [`fmt::Display`] writes it: `none`, `added-byte:N`,
    /// `loaded-byte:N` or `panic:N`.
    fn from_str(text: &str) -> Result<Self, String> {
        let unknown = || {
            format!(
                "`{text}` is not a fault: none, added-byte:N, loaded-byte:N or panic:N are faults"
            )
        };
        if text == "none" {
            return Ok(Fault::None);
        }
        let (kind, size) = text.split_once(':').ok_or_else(unknown)?;
        match kind {
            "added-byte" => size
                .parse()
                .map(|when_added| Fault::AddedByte { when_added }),
            "loaded-byte" => size
                .parse()
                .map(|when_loaded| Fault::LoadedByte { when_loaded }),
            "panic" => size.parse().map(|when_added| Fault::Panic { when_added }),
            _ => return Err(unknown()),
        }
        .map_err(|_| unknown())
    }
}

/// RocksDB behind a second implementation of [`Engine`]: what stands in for
/// LigninDB until it exists.
///
/// Every operation is the RocksDB engine's, with two exceptions. A read
/// that was given no transaction gets a snapshot read transaction. And an
/// engine opened with a [`Fault`] has that defect.
pub struct StandIn {
    engine: RocksDbEngine,
    fault: Fault,
}

impl StandIn {
    /// A stand-in over an open RocksDB engine, with `fault` planted.
    pub fn over(engine: RocksDbEngine, fault: Fault) -> Self {
        StandIn { engine, fault }
    }

    /// Opens the database at `path` with `fault` planted.
    pub fn open_with_fault<P: AsRef<Path>>(path: P, fault: Fault) -> Result<Self, StorageError> {
        RocksDbEngine::open(path).map(|engine| Self::over(engine, fault))
    }

    /// The defect this engine has.
    pub fn fault(&self) -> Fault {
        self.fault
    }

    /// This engine with `fault` planted, as one of the two engines of a
    /// comparison.
    pub fn side(fault: Fault) -> Side<Self> {
        let name = match fault {
            Fault::None => "StandIn".to_string(),
            fault => format!("StandIn with {fault}"),
        };
        Side::opened_by(&name, move |dir| {
            Self::open_with_fault(dir, fault)
                .map(GroveDb::from_engine)
                .map_err(Into::into)
        })
    }
}

impl StreamEngine for StandIn {
    fn is_snapshot_read(transaction: &Transaction<'_, Self>) -> bool {
        RocksDbEngine::is_snapshot_read(transaction)
    }
}

impl PendingPrefixDrops for StandIn {
    fn pending_prefix_drop_records(&self) -> Result<Vec<PendingPrefixDropRecord>, StorageError> {
        self.engine.pending_prefix_drop_records()
    }

    fn delete_prefix_ranges(&self, prefixes: &[SubtreePrefix]) -> Result<(), StorageError> {
        self.engine.delete_prefix_ranges(prefixes)
    }

    fn remove_pending_prefix_drop_record(
        &self,
        primary_prefix: &SubtreePrefix,
    ) -> Result<(), StorageError> {
        self.engine
            .remove_pending_prefix_drop_record(primary_prefix)
    }
}

impl Engine for StandIn {
    type Applied = <RocksDbEngine as Engine>::Applied;
    type Batch = <RocksDbEngine as Engine>::Batch;
    type ImmediateSubtree<'db> = <RocksDbEngine as Engine>::ImmediateSubtree<'db>;
    type Subtree<'db> = <RocksDbEngine as Engine>::Subtree<'db>;
    type SubtreeMove<'db> = <RocksDbEngine as Engine>::SubtreeMove<'db>;
    type Transaction<'db> = <RocksDbEngine as Engine>::Transaction<'db>;
    type WriteGroup = <RocksDbEngine as Engine>::WriteGroup;
    type WriteScope<'db> = <RocksDbEngine as Engine>::WriteScope<'db>;

    fn open<P: AsRef<Path>>(path: P) -> Result<Self, StorageError> {
        Self::open_with_fault(path, Fault::None)
    }

    fn open_checkpoint<P: AsRef<Path>>(path: P) -> Result<Self, StorageError> {
        RocksDbEngine::open_checkpoint(path).map(|engine| Self::over(engine, Fault::None))
    }

    fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), StorageError> {
        self.engine.create_checkpoint(path)
    }

    fn flush(&self) -> Result<(), StorageError> {
        self.engine.flush()
    }

    fn wipe(&self) -> Result<(), StorageError> {
        self.engine.wipe()
    }

    fn start_transaction(&self) -> Self::Transaction<'_> {
        self.engine.start_transaction()
    }

    fn start_snapshot_read_transaction(&self) -> Self::Transaction<'_> {
        self.engine.start_snapshot_read_transaction()
    }

    fn start_read_transaction(&self) -> Self::Transaction<'_> {
        self.engine.start_snapshot_read_transaction()
    }

    fn commit_transaction(
        &self,
        transaction: Self::Transaction<'_>,
    ) -> CostResult<(), StorageError> {
        self.engine.commit_transaction(transaction)
    }

    fn rollback_transaction(
        &self,
        transaction: &Self::Transaction<'_>,
    ) -> Result<(), StorageError> {
        self.engine.rollback_transaction(transaction)
    }

    fn set_savepoint(&self, transaction: &Self::Transaction<'_>) -> Result<(), StorageError> {
        self.engine.set_savepoint(transaction)
    }

    fn rollback_to_savepoint(
        &self,
        transaction: &Self::Transaction<'_>,
    ) -> Result<(), StorageError> {
        self.engine.rollback_to_savepoint(transaction)
    }

    fn open_write_scope<'a>(
        &'a self,
        transaction: &'a Self::Transaction<'_>,
    ) -> Result<Self::WriteScope<'a>, StorageError> {
        self.engine.open_write_scope(transaction)
    }

    fn scope_transaction<'s, 'a: 's>(scope: &'s Self::WriteScope<'a>) -> &'s Self::Transaction<'a> {
        RocksDbEngine::scope_transaction(scope)
    }

    fn merge_write_scope(&self, scope: Self::WriteScope<'_>) -> Result<(), StorageError> {
        self.engine.merge_write_scope(scope)
    }

    fn rollback_write_scope(&self, scope: Self::WriteScope<'_>) -> Result<(), StorageError> {
        self.engine.rollback_write_scope(scope)
    }

    fn subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        batch: Option<&'a Self::Batch>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::Subtree<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        self.engine.subtree(path, batch, transaction)
    }

    fn subtree_by_prefix<'a>(
        &'a self,
        prefix: SubtreePrefix,
        batch: Option<&'a Self::Batch>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::Subtree<'a>> {
        self.engine.subtree_by_prefix(prefix, batch, transaction)
    }

    fn immediate_subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::ImmediateSubtree<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        self.engine.immediate_subtree(path, transaction)
    }

    fn immediate_subtree_by_prefix<'a>(
        &'a self,
        prefix: SubtreePrefix,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::ImmediateSubtree<'a>> {
        self.engine.immediate_subtree_by_prefix(prefix, transaction)
    }

    fn settle_batch(
        &self,
        group: &mut Self::WriteGroup,
        batch: Self::Batch,
    ) -> CostResult<(), StorageError> {
        let mut settled = self.engine.settle_batch(group, batch);
        if let Fault::LoadedByte { when_loaded } = self.fault
            && settled.cost.storage_loaded_bytes == when_loaded
        {
            settled.cost.storage_loaded_bytes += 1;
        }
        settled
    }

    fn apply_write_group(
        &self,
        group: Self::WriteGroup,
        transaction: &Self::Transaction<'_>,
    ) -> CostResult<Self::Applied, StorageError> {
        let mut applied = self.engine.apply_write_group(group, transaction);
        let added = &mut applied.cost.storage_cost.added_bytes;
        match self.fault {
            Fault::AddedByte { when_added } if *added == when_added => *added += 1,
            Fault::Panic { when_added } if *added == when_added => {
                panic!("the planted fault: a batch added {when_added} bytes")
            }
            _ => {}
        }
        applied
    }

    fn retarget_cursor<'db>(
        cursor: SubtreeCursor<'db, Self>,
        prefix: SubtreePrefix,
    ) -> SubtreeCursor<'db, Self> {
        RocksDbEngine::retarget_cursor(cursor, prefix)
    }

    fn move_subtree<'a>(
        &'a self,
        from: SubtreePrefix,
        to: SubtreePrefix,
        batch: &'a Self::Batch,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::SubtreeMove<'a>> {
        self.engine.move_subtree(from, to, batch, transaction)
    }
}
