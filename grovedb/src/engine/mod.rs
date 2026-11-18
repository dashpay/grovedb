//! The seam between GroveDB and the engine that stores it.
//!
//! [`Engine`] is everything [`GroveDb`](crate::GroveDb) asks of a storage
//! engine: opening a database, transactions, snapshot read transactions,
//! savepoints, write scopes, handles to subtrees, the batch of writes that
//! spans subtrees, checkpoints, flush and wipe. `GroveDb<E>` is generic over
//! it, and [`RocksDbEngine`] is the engine it uses unless another is named.
//!
//! A subtree handle is a [`SubtreeHandle`]: the `NodeStore` a Merk runs on,
//! the `FlatStore` of the subtree's data keyspace, and the handles of its aux
//! and meta keyspaces.
//!
//! The contract is written out in `docs/lignindb/engine.md`.

pub mod rocksdb;
#[cfg(test)]
mod tests;

use std::path::Path;

use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, CostContext, CostResult, CostsExt,
    OperationCost,
};
use grovedb_merk::store::NodeStore;
use grovedb_path::SubtreePath;
pub use grovedb_storage::error::{CommitError, WriteScopeError};
use grovedb_storage::{
    flat::{PendingPrefixDrops, SubtreePrefix},
    Error, FlatStore,
};

pub use self::rocksdb::RocksDbEngine;

/// The error of a node store call that GroveDB makes itself, where it used to
/// call the storage context: an error of the storage stays
/// [`Error::StorageError`](crate::Error::StorageError), as it was before the
/// call went through the node store.
pub(crate) fn node_store_error(error: grovedb_merk::Error) -> crate::Error {
    match error {
        grovedb_merk::Error::StorageError(error) => crate::Error::StorageError(error),
        error => error.into(),
    }
}

/// The cursor over the nodes of a subtree handle of engine `E`.
pub type SubtreeCursor<'db, E> = <<E as Engine>::Subtree<'db> as NodeStore<'db>>::Cursor;

/// The handle of one subtree inside one transaction.
///
/// It is the [`NodeStore`] a Merk of that subtree runs on, and the
/// [`FlatStore`] of the subtree's data keyspace, which holds either Merk nodes
/// or the records of an append-only tree, never both. Both traits have a
/// `cursor`, so code that holds a handle names the trait it means.
///
/// The aux and meta keyspaces of the subtree are handles of their own.
pub trait SubtreeHandle<'db>: NodeStore<'db> + FlatStore {
    /// The aux keyspace of the subtree.
    type Aux<'a>: FlatStore
    where
        Self: 'a;

    /// The meta keyspace of the subtree.
    type Meta<'a>: FlatStore
    where
        Self: 'a;

    /// The handle of the subtree's aux keyspace, in the same transaction and
    /// write mode as this handle.
    fn aux(&self) -> Self::Aux<'_>;

    /// The handle of the subtree's meta keyspace, in the same transaction and
    /// write mode as this handle.
    fn meta(&self) -> Self::Meta<'_>;
}

/// The writes of one GroveDB operation, staged across subtrees by the
/// deferred handles opened with it, until [`Engine::apply_batch`] applies them
/// to the transaction together.
///
/// A batch keeps one outcome per key. Inside one operation a put wins over a
/// delete of the same key. Across the boundary [`Self::next_operation`] marks,
/// the later operation wins.
pub trait EngineBatch: Default {
    /// Marks the boundary between two operations that share this batch.
    fn next_operation(&self);

    /// Moves everything staged so far into a new batch and leaves this one
    /// empty, still open for the handles that write into it.
    fn take_pending(&self) -> Self;

    /// Merges `other` into this batch as a later operation: each of its
    /// outcomes replaces what this batch holds for the same key.
    fn merge_overwriting(&self, other: Self);
}

/// Batches whose cost is settled and which wait to be applied to their
/// transaction together, in the order they were settled.
///
/// It is what a partial batch needs: GroveDB settles what is staged so far,
/// reports its cost to the caller, goes on staging, and applies everything at
/// the end.
pub trait WriteGroup: Default {
    /// What the settled batches cost once they are applied. It is not
    /// charged when the group is dropped unapplied.
    fn pending_cost(&self) -> &OperationCost;
}

/// The records of one subtree on their way to another prefix, one record at
/// a time ([`Engine::move_subtree`]).
pub trait SubtreeMove {
    /// Moves the next record of the subtree, or returns `None` when none is
    /// left.
    ///
    /// The record is shown to `inspect` first, as its key and its bytes. If
    /// `inspect` fails, the record stays and the error is returned. `bill`
    /// gives what the record is charged where it lands. What `inspect`
    /// returned is handed back.
    fn move_next<T>(
        &mut self,
        inspect: impl FnOnce(&[u8], &[u8]) -> Result<T, crate::Error>,
        bill: impl FnOnce(&[u8], &[u8]) -> KeyValueStorageCost,
    ) -> CostResult<Option<T>, crate::Error>;
}

/// A storage engine underneath GroveDB.
///
/// # Transactions
///
/// A write transaction reads its own writes, and for everything else the
/// latest committed state at the moment of the read. A snapshot read
/// transaction reads the state that was committed when it started, and
/// refuses every write and its commit with
/// [`Error::SnapshotReadOnlyTransaction`]. One write transaction commits at a
/// time. A commit that loses to another commit fails with an error for which
/// [`Error::is_commit_conflict`] holds, and [`Error::commit_error`] names the
/// other outcomes an engine can report.
///
/// # Handles
///
/// A subtree handle lives inside one transaction, in one of two write modes.
/// A deferred handle stages its writes in the batch it was opened with, and
/// neither it nor any other handle reads them until that batch is applied. A
/// deferred handle opened with no batch is read-only. An immediate handle
/// writes to the transaction at once and reads its own writes.
///
/// # Threads
///
/// An engine is shared between threads. A transaction, a batch and a handle
/// are not required to be `Send`: one thread at a time writes through a
/// transaction. Parallel writers each write through their own
/// [write scope](Self::WriteScope), and code that sends a scope to another
/// thread asks for `Send` there.
pub trait Engine: PendingPrefixDrops + Sized + Send + Sync + 'static {
    /// A transaction: a write transaction, or a snapshot read transaction.
    type Transaction<'db>
    where
        Self: 'db;

    /// The batch that stages the writes of one operation across subtrees.
    type Batch: EngineBatch;

    /// Settled batches that wait to be applied together.
    type WriteGroup: WriteGroup;

    /// What the application of a batch reports back. An engine that places
    /// nodes when a batch is applied reports their locations here, for the
    /// Merks that stay open.
    type Applied;

    /// A deferred handle of a subtree.
    type Subtree<'db>: SubtreeHandle<'db>
    where
        Self: 'db;

    /// An immediate handle of a subtree.
    type ImmediateSubtree<'db>: SubtreeHandle<'db>
    where
        Self: 'db;

    /// A subtree on its way to another prefix.
    type SubtreeMove<'db>: SubtreeMove
    where
        Self: 'db;

    /// One writer's part of a write transaction.
    type WriteScope<'db>
    where
        Self: 'db;

    /// Opens the database at `path`, and creates it if there is none.
    fn open<P: AsRef<Path>>(path: P) -> Result<Self, Error>;

    /// Opens a checkpoint that [`Self::create_checkpoint`] made at `path`.
    fn open_checkpoint<P: AsRef<Path>>(path: P) -> Result<Self, Error>;

    /// Captures the committed state in a new directory at `path`. An
    /// existing `path` is refused and left as it is.
    fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), Error>;

    /// Makes every commit made so far durable.
    fn flush(&self) -> Result<(), Error>;

    /// Removes every record of every keyspace of every subtree. It takes no
    /// transaction and is not charged.
    fn wipe(&self) -> Result<(), Error>;

    /// Starts a write transaction.
    fn start_transaction(&self) -> Self::Transaction<'_>;

    /// Starts a snapshot read transaction, pinned to the state committed at
    /// this moment.
    fn start_snapshot_read_transaction(&self) -> Self::Transaction<'_>;

    /// Starts the transaction of one read that was given none. Nothing is
    /// written through it and it is not committed. An engine may pin it to
    /// the state committed at this moment, so that the read sees one state.
    fn start_read_transaction(&self) -> Self::Transaction<'_>;

    /// Commits `transaction` and ends it, whether the commit succeeds or
    /// not.
    fn commit_transaction(&self, transaction: Self::Transaction<'_>) -> CostResult<(), Error>;

    /// Discards the writes of `transaction`, which stays usable.
    fn rollback_transaction(&self, transaction: &Self::Transaction<'_>) -> Result<(), Error>;

    /// Sets a savepoint on `transaction`. Savepoints form a stack. It fails
    /// with [`WriteScopeError::ScopesOpen`] while a write scope of the
    /// transaction is open, and so does [`Self::rollback_to_savepoint`].
    fn set_savepoint(&self, transaction: &Self::Transaction<'_>) -> Result<(), Error>;

    /// Undoes every write `transaction` made since its newest savepoint and
    /// removes that savepoint.
    fn rollback_to_savepoint(&self, transaction: &Self::Transaction<'_>) -> Result<(), Error>;

    /// Opens a write scope on `transaction`: the part of it one thread
    /// writes. Scopes that are open together write different subtrees. A
    /// scope that needs what another open scope holds fails with
    /// [`WriteScopeError::Overlap`]; it never waits.
    ///
    /// A scope ends with [`Self::merge_write_scope`] or
    /// [`Self::rollback_write_scope`]. One that is dropped is rolled back,
    /// unless the engine cannot roll a scope back: then its writes stay in
    /// the transaction, as after a refused [`Self::rollback_write_scope`].
    fn open_write_scope<'a>(
        &'a self,
        transaction: &'a Self::Transaction<'_>,
    ) -> Result<Self::WriteScope<'a>, Error>;

    /// The transaction as `scope` sees it: its own writes, then the writes
    /// already merged into the transaction, then the committed state.
    /// Whatever takes a transaction takes this view, and then reads and
    /// writes through the scope.
    fn scope_transaction<'s, 'a: 's>(scope: &'s Self::WriteScope<'a>) -> &'s Self::Transaction<'a>;

    /// Ends `scope` and makes its writes the transaction's.
    fn merge_write_scope(&self, scope: Self::WriteScope<'_>) -> Result<(), Error>;

    /// Ends `scope` and removes its writes from the transaction, which is
    /// then as if the scope had never been opened. An engine that cannot do
    /// this fails with [`WriteScopeError::RollbackUnsupported`], ends the
    /// scope and leaves its writes in the transaction. On such an engine the
    /// savepoint a caller set before it opened the scope is what undoes
    /// them.
    fn rollback_write_scope(&self, scope: Self::WriteScope<'_>) -> Result<(), Error>;

    /// A deferred handle of the subtree at `path`.
    fn subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        batch: Option<&'a Self::Batch>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::Subtree<'a>>
    where
        B: AsRef<[u8]> + 'b;

    /// A deferred handle of the subtree with the given prefix.
    fn subtree_by_prefix<'a>(
        &'a self,
        prefix: SubtreePrefix,
        batch: Option<&'a Self::Batch>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::Subtree<'a>>;

    /// An immediate handle of the subtree at `path`.
    fn immediate_subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::ImmediateSubtree<'a>>
    where
        B: AsRef<[u8]> + 'b;

    /// An immediate handle of the subtree with the given prefix.
    fn immediate_subtree_by_prefix<'a>(
        &'a self,
        prefix: SubtreePrefix,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::ImmediateSubtree<'a>>;

    /// Settles `batch`: works out what its writes cost and adds them to
    /// `group`, after everything the group already holds. Nothing is applied
    /// yet. The cost returned is charged whether or not the group is ever
    /// applied.
    fn settle_batch(
        &self,
        group: &mut Self::WriteGroup,
        batch: Self::Batch,
    ) -> CostResult<(), Error>;

    /// Applies `group` to `transaction`: every write of its batches becomes
    /// visible to every handle of the transaction at once, or none does. The
    /// group's pending cost is charged if the application succeeds.
    fn apply_write_group(
        &self,
        group: Self::WriteGroup,
        transaction: &Self::Transaction<'_>,
    ) -> CostResult<Self::Applied, Error>;

    /// Settles `batch` and applies it to `transaction` in one step.
    fn apply_batch(
        &self,
        batch: Self::Batch,
        transaction: &Self::Transaction<'_>,
    ) -> CostResult<Self::Applied, Error> {
        let mut cost = OperationCost::default();
        let mut group = Self::WriteGroup::default();
        match self
            .settle_batch(&mut group, batch)
            .unwrap_add_cost(&mut cost)
        {
            Ok(()) => self.apply_write_group(group, transaction).add_cost(cost),
            Err(e) => Err(e).wrap_with_cost(cost),
        }
    }

    /// Points `cursor` at the subtree with the given prefix. It keeps the
    /// state it has read from since it was opened, so one cursor can read
    /// several subtrees in one state.
    fn retarget_cursor<'db>(
        cursor: SubtreeCursor<'db, Self>,
        prefix: SubtreePrefix,
    ) -> SubtreeCursor<'db, Self>;

    /// Starts to move every record of the data keyspace of the subtree at
    /// `from` to the subtree at `to`, staged in `batch`. The move is made by
    /// calling [`SubtreeMove::move_next`] until it returns `None`.
    fn move_subtree<'a>(
        &'a self,
        from: SubtreePrefix,
        to: SubtreePrefix,
        batch: &'a Self::Batch,
        transaction: &'a Self::Transaction<'_>,
    ) -> CostContext<Self::SubtreeMove<'a>>;
}
