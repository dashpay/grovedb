//! The RocksDB engine: `grovedb-storage` behind the [`Engine`] seam.
//!
//! Every operation is the call on [`RocksDbStorage`], its transaction or its
//! storage contexts that GroveDB made before the seam, with the same
//! arguments. The engine adds one thing of its own, the flag that allows one
//! write scope at a time.

use std::{
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
};

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::key_value_cost::KeyValueStorageCost, CostContext, CostResult, CostsExt,
    OperationCost,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    error::WriteScopeError,
    flat::{PendingPrefixDropRecord, PendingPrefixDrops, SubtreePrefix},
    rocksdb_storage::{
        AuxKeyspace, MetaKeyspace, PrefixedRocksDbImmediateStorageContext,
        PrefixedRocksDbTransactionContext, RocksDbStorage, Tx, WriteBatchWithTransaction,
    },
    Error, RawIterator, Storage, StorageBatch, StorageContext,
};

use super::{Engine, EngineBatch, SubtreeCursor, SubtreeHandle, SubtreeMove, WriteGroup};

/// The RocksDB engine. It is the engine of a `GroveDb` that names no other.
pub struct RocksDbEngine {
    storage: RocksDbStorage,
    /// The transaction whose write scope is open, as its address, or
    /// [`NO_SCOPE`]. The engine allows one scope at a time.
    scope_of: AtomicUsize,
}

const NO_SCOPE: usize = 0;

fn address_of(transaction: &Tx<'_>) -> usize {
    std::ptr::from_ref(transaction).addr()
}

impl RocksDbEngine {
    /// The engine on an open RocksDB storage.
    pub fn new(storage: RocksDbStorage) -> Self {
        RocksDbEngine {
            storage,
            scope_of: AtomicUsize::new(NO_SCOPE),
        }
    }

    /// The RocksDB storage underneath.
    pub fn storage(&self) -> &RocksDbStorage {
        &self.storage
    }

    fn refuse_while_its_scope_is_open(&self, transaction: &Tx<'_>) -> Result<(), Error> {
        if self.scope_of.load(Ordering::Acquire) == address_of(transaction) {
            Err(Error::WriteScope(WriteScopeError::ScopesOpen))
        } else {
            Ok(())
        }
    }
}

impl EngineBatch for StorageBatch {
    fn next_operation(&self) {
        StorageBatch::next_operation(self)
    }

    fn take_pending(&self) -> Self {
        StorageBatch::take_pending(self)
    }

    fn merge_overwriting(&self, other: Self) {
        StorageBatch::merge_overwriting(self, other)
    }
}

/// Settled storage batches as one RocksDB write batch, with the cost that
/// is charged once it is written.
#[derive(Default)]
pub struct RocksDbWriteGroup {
    batch: WriteBatchWithTransaction<true>,
    pending_cost: OperationCost,
}

impl WriteGroup for RocksDbWriteGroup {
    fn pending_cost(&self) -> &OperationCost {
        &self.pending_cost
    }
}

macro_rules! impl_subtree_handle {
    ($context:ident) => {
        impl<'db> SubtreeHandle<'db> for $context<'db> {
            type Aux<'a>
                = AuxKeyspace<'a, Self>
            where
                Self: 'a;
            type Meta<'a>
                = MetaKeyspace<'a, Self>
            where
                Self: 'a;

            fn aux(&self) -> Self::Aux<'_> {
                $context::aux(self)
            }

            fn meta(&self) -> Self::Meta<'_> {
                $context::meta(self)
            }
        }
    };
}

impl_subtree_handle!(PrefixedRocksDbTransactionContext);
impl_subtree_handle!(PrefixedRocksDbImmediateStorageContext);

/// The one write scope the RocksDB engine allows at a time. Its writes go
/// straight to its transaction, and stay there however the scope ends.
pub struct RocksDbWriteScope<'db> {
    transaction: &'db Tx<'db>,
    scope_of: &'db AtomicUsize,
}

impl Drop for RocksDbWriteScope<'_> {
    fn drop(&mut self) {
        self.scope_of.store(NO_SCOPE, Ordering::Release);
    }
}

/// Moves the record `cursor` is at, if any, and steps past it: shows it to
/// `inspect`, hands it to `land` and then to `clear`, and returns what
/// `inspect` found. `None` once the records run out. Fails rather than end
/// early when the cursor stopped on an error, which `valid` reports like the
/// end of its records: a move must not leave a partial copy behind.
pub(super) fn move_record<I: RawIterator, T>(
    cursor: &mut I,
    inspect: impl FnOnce(&[u8], &[u8]) -> Result<T, crate::Error>,
    land: impl FnOnce(&[u8], &[u8]) -> CostResult<(), crate::Error>,
    clear: impl FnOnce(&[u8]) -> CostResult<(), crate::Error>,
) -> CostResult<Option<T>, crate::Error> {
    let mut cost = OperationCost::default();
    if !cursor.valid().unwrap_add_cost(&mut cost) {
        cost_return_on_error_no_add!(cost, cursor.status().map_err(crate::Error::StorageError));
        return Ok(None).wrap_with_cost(cost);
    }
    let (Some(key), Some(value)) = (
        cursor.key().unwrap_add_cost(&mut cost),
        cursor.value().unwrap_add_cost(&mut cost),
    ) else {
        return Err(crate::Error::CorruptedData(
            "a raw iterator at a record returned no key or value".to_owned(),
        ))
        .wrap_with_cost(cost);
    };
    let found = cost_return_on_error_no_add!(cost, inspect(key, value));
    cost_return_on_error!(&mut cost, land(key, value));
    cost_return_on_error!(&mut cost, clear(key));
    cursor.next().unwrap_add_cost(&mut cost);
    Ok(Some(found)).wrap_with_cost(cost)
}

/// A subtree that the RocksDB engine moves by copying: each record is put
/// under the new prefix and deleted under the old one.
pub struct RocksDbSubtreeMove<'db> {
    source: PrefixedRocksDbTransactionContext<'db>,
    destination: PrefixedRocksDbTransactionContext<'db>,
    cursor: <PrefixedRocksDbTransactionContext<'db> as StorageContext<'db>>::RawIterator,
}

impl SubtreeMove for RocksDbSubtreeMove<'_> {
    fn move_next<T>(
        &mut self,
        inspect: impl FnOnce(&[u8], &[u8]) -> Result<T, crate::Error>,
        bill: impl FnOnce(&[u8], &[u8]) -> KeyValueStorageCost,
    ) -> CostResult<Option<T>, crate::Error> {
        let Self {
            source,
            destination,
            cursor,
        } = self;
        move_record(
            cursor,
            inspect,
            |key, value| {
                StorageContext::put(&*destination, key, value, None, Some(bill(key, value)))
                    .map_err(Into::into)
            },
            // Billed where it lands. In a swap the other move's put of the
            // same record wins over this delete.
            |key| {
                StorageContext::delete(&*source, key, Some(KeyValueStorageCost::default()))
                    .map_err(Into::into)
            },
        )
    }
}

impl PendingPrefixDrops for RocksDbEngine {
    fn pending_prefix_drop_records(&self) -> Result<Vec<PendingPrefixDropRecord>, Error> {
        self.storage.pending_prefix_drop_records()
    }

    fn delete_prefix_ranges(&self, prefixes: &[SubtreePrefix]) -> Result<(), Error> {
        self.storage.delete_prefix_ranges(prefixes)
    }

    fn remove_pending_prefix_drop_record(
        &self,
        primary_prefix: &SubtreePrefix,
    ) -> Result<(), Error> {
        self.storage
            .remove_pending_prefix_drop_record(primary_prefix)
    }
}

impl Engine for RocksDbEngine {
    type Applied = ();
    type Batch = StorageBatch;
    type ImmediateSubtree<'db> = PrefixedRocksDbImmediateStorageContext<'db>;
    type Subtree<'db> = PrefixedRocksDbTransactionContext<'db>;
    type SubtreeMove<'db> = RocksDbSubtreeMove<'db>;
    type Transaction<'db> = Tx<'db>;
    type WriteGroup = RocksDbWriteGroup;
    type WriteScope<'db> = RocksDbWriteScope<'db>;

    fn open<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        RocksDbStorage::default_rocksdb_with_path(path).map(Self::new)
    }

    fn open_checkpoint<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        RocksDbStorage::checkpoint_rocksdb_with_path(path).map(Self::new)
    }

    fn create_checkpoint<P: AsRef<Path>>(&self, path: P) -> Result<(), Error> {
        self.storage.create_checkpoint(path)
    }

    fn flush(&self) -> Result<(), Error> {
        self.storage.flush()
    }

    fn wipe(&self) -> Result<(), Error> {
        self.storage.wipe()
    }

    fn start_transaction(&self) -> Tx<'_> {
        self.storage.start_transaction()
    }

    fn start_snapshot_read_transaction(&self) -> Tx<'_> {
        self.storage.start_snapshot_read_transaction()
    }

    fn start_read_transaction(&self) -> Tx<'_> {
        self.storage.start_transaction()
    }

    fn commit_transaction(&self, transaction: Tx<'_>) -> CostResult<(), Error> {
        self.storage.commit_transaction(transaction)
    }

    fn rollback_transaction(&self, transaction: &Tx<'_>) -> Result<(), Error> {
        self.storage.rollback_transaction(transaction)
    }

    fn set_savepoint(&self, transaction: &Tx<'_>) -> Result<(), Error> {
        self.refuse_while_its_scope_is_open(transaction)?;
        transaction.set_savepoint();
        Ok(())
    }

    fn rollback_to_savepoint(&self, transaction: &Tx<'_>) -> Result<(), Error> {
        self.refuse_while_its_scope_is_open(transaction)?;
        transaction.rollback_to_savepoint()
    }

    fn open_write_scope<'a>(
        &'a self,
        transaction: &'a Tx<'_>,
    ) -> Result<RocksDbWriteScope<'a>, Error> {
        if transaction.is_snapshot_read() {
            return Err(Error::SnapshotReadOnlyTransaction("write scope"));
        }
        self.scope_of
            .compare_exchange(
                NO_SCOPE,
                address_of(transaction),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| Error::WriteScope(WriteScopeError::Overlap))?;
        Ok(RocksDbWriteScope {
            transaction,
            scope_of: &self.scope_of,
        })
    }

    fn scope_transaction<'s, 'a: 's>(scope: &'s RocksDbWriteScope<'a>) -> &'s Tx<'a> {
        scope.transaction
    }

    fn merge_write_scope(&self, scope: RocksDbWriteScope<'_>) -> Result<(), Error> {
        drop(scope);
        Ok(())
    }

    fn rollback_write_scope(&self, scope: RocksDbWriteScope<'_>) -> Result<(), Error> {
        // RocksDB can undo back to a savepoint, but it cannot remove a
        // savepoint without undoing. A scope that set one would leave it on
        // the transaction's stack when it merges, in the way of the caller's
        // own savepoints. The same holds for a scope that is dropped.
        drop(scope);
        Err(Error::WriteScope(WriteScopeError::RollbackUnsupported))
    }

    fn subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        batch: Option<&'a StorageBatch>,
        transaction: &'a Tx<'_>,
    ) -> CostContext<PrefixedRocksDbTransactionContext<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        self.storage
            .get_transactional_storage_context(path, batch, transaction)
    }

    fn subtree_by_prefix<'a>(
        &'a self,
        prefix: SubtreePrefix,
        batch: Option<&'a StorageBatch>,
        transaction: &'a Tx<'_>,
    ) -> CostContext<PrefixedRocksDbTransactionContext<'a>> {
        self.storage
            .get_transactional_storage_context_by_subtree_prefix(prefix, batch, transaction)
    }

    fn immediate_subtree<'a, 'b, B>(
        &'a self,
        path: SubtreePath<'b, B>,
        transaction: &'a Tx<'_>,
    ) -> CostContext<PrefixedRocksDbImmediateStorageContext<'a>>
    where
        B: AsRef<[u8]> + 'b,
    {
        self.storage
            .get_immediate_storage_context(path, transaction)
    }

    fn immediate_subtree_by_prefix<'a>(
        &'a self,
        prefix: SubtreePrefix,
        transaction: &'a Tx<'_>,
    ) -> CostContext<PrefixedRocksDbImmediateStorageContext<'a>> {
        self.storage
            .get_immediate_storage_context_by_subtree_prefix(prefix, transaction)
    }

    fn settle_batch(
        &self,
        group: &mut RocksDbWriteGroup,
        batch: StorageBatch,
    ) -> CostResult<(), Error> {
        let RocksDbWriteGroup {
            batch: write_batch,
            pending_cost,
        } = group;
        self.storage
            .continue_write_batch(write_batch, batch)
            .map_ok(|settled| *pending_cost += settled)
    }

    fn apply_write_group(
        &self,
        group: RocksDbWriteGroup,
        transaction: &Tx<'_>,
    ) -> CostResult<(), Error> {
        self.storage
            .commit_db_write_batch(group.batch, group.pending_cost, Some(transaction))
    }

    fn retarget_cursor<'db>(
        cursor: SubtreeCursor<'db, Self>,
        prefix: SubtreePrefix,
    ) -> SubtreeCursor<'db, Self> {
        cursor.retarget(prefix)
    }

    fn move_subtree<'a>(
        &'a self,
        from: SubtreePrefix,
        to: SubtreePrefix,
        batch: &'a StorageBatch,
        transaction: &'a Tx<'_>,
    ) -> CostContext<RocksDbSubtreeMove<'a>> {
        let mut cost = OperationCost::default();
        let source = self
            .subtree_by_prefix(from, Some(batch), transaction)
            .unwrap_add_cost(&mut cost);
        let destination = self
            .subtree_by_prefix(to, Some(batch), transaction)
            .unwrap_add_cost(&mut cost);
        let mut cursor = source.raw_iter();
        cursor.seek_to_first().unwrap_add_cost(&mut cost);
        RocksDbSubtreeMove {
            source,
            destination,
            cursor,
        }
        .wrap_with_cost(cost)
    }
}
