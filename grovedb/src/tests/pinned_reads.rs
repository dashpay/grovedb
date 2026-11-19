//! [`PinnedReads`]: the RocksDB engine with one difference. The transaction
//! of a read that was given none is a snapshot read transaction, as on an
//! engine that pins such a read to one state (RD2 of
//! `docs/lignindb/transactions.md`). A read that started two transactions
//! would read two states on such an engine, and a read that wrote would be
//! refused.
//!
//! It counts the transactions it starts, for
//! `one_transaction_per_operation_tests`. It is also a registered engine of
//! the suite (`pinned-reads` in `test-engines.txt`): its handles and
//! transactions are RocksDB's, and the engine itself is a type of its own
//! that gives no way to the storage underneath.

use std::{
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

use grovedb_costs::{CostContext, CostResult};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    flat::{PendingPrefixDropRecord, PendingPrefixDrops, SubtreePrefix},
    Error as StorageError,
};

use crate::{engine::SubtreeCursor, Engine, RocksDbEngine};

pub struct PinnedReads {
    engine: RocksDbEngine,
    pub(crate) reads_started: AtomicUsize,
    pub(crate) writes_started: AtomicUsize,
    pub(crate) snapshots_started: AtomicUsize,
}

impl PinnedReads {
    fn over(engine: RocksDbEngine) -> Self {
        PinnedReads {
            engine,
            reads_started: AtomicUsize::new(0),
            writes_started: AtomicUsize::new(0),
            snapshots_started: AtomicUsize::new(0),
        }
    }
}

impl PendingPrefixDrops for PinnedReads {
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

impl Engine for PinnedReads {
    type Applied = <RocksDbEngine as Engine>::Applied;
    type Batch = <RocksDbEngine as Engine>::Batch;
    type ImmediateSubtree<'db> = <RocksDbEngine as Engine>::ImmediateSubtree<'db>;
    type Subtree<'db> = <RocksDbEngine as Engine>::Subtree<'db>;
    type SubtreeMove<'db> = <RocksDbEngine as Engine>::SubtreeMove<'db>;
    type Transaction<'db> = <RocksDbEngine as Engine>::Transaction<'db>;
    type WriteGroup = <RocksDbEngine as Engine>::WriteGroup;
    type WriteScope<'db> = <RocksDbEngine as Engine>::WriteScope<'db>;

    fn open<P: AsRef<Path>>(path: P) -> Result<Self, StorageError> {
        RocksDbEngine::open(path).map(Self::over)
    }

    fn open_checkpoint<P: AsRef<Path>>(path: P) -> Result<Self, StorageError> {
        RocksDbEngine::open_checkpoint(path).map(Self::over)
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
        self.writes_started.fetch_add(1, Ordering::Relaxed);
        self.engine.start_transaction()
    }

    fn start_snapshot_read_transaction(&self) -> Self::Transaction<'_> {
        self.snapshots_started.fetch_add(1, Ordering::Relaxed);
        self.engine.start_snapshot_read_transaction()
    }

    fn start_read_transaction(&self) -> Self::Transaction<'_> {
        self.reads_started.fetch_add(1, Ordering::Relaxed);
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
        self.engine.settle_batch(group, batch)
    }

    fn apply_write_group(
        &self,
        group: Self::WriteGroup,
        transaction: &Self::Transaction<'_>,
    ) -> CostResult<Self::Applied, StorageError> {
        self.engine.apply_write_group(group, transaction)
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
