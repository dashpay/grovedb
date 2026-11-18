//! The storage contexts, their batches and their cursors, on the cost
//! model.

#[path = "../../../../storage/src/rocksdb_storage/storage_context/flat.rs"]
mod flat;

use std::marker::PhantomData;

use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, ChildrenSizesWithIsSumTree, CostContext,
    CostResult, CostsExt, OperationCost,
};

pub use self::flat::{AuxKeyspace, MetaKeyspace};
use super::storage::{SubtreePrefix, Tx};
use crate::{
    cost_model::{
        rules::{self, CursorMove, Keyspace},
        state::{Cursor, Record, StoredKey, Write},
    },
    error::Error,
    Batch, FlatStore, RawIterator, StorageBatch, StorageContext,
};

pub mod context_immediate {
    //! The immediate storage context, under the path the real crate has it.
    pub use super::PrefixedRocksDbImmediateStorageContext;
}

/// Make prefixed key
pub fn make_prefixed_key<K: AsRef<[u8]>>(prefix: &SubtreePrefix, key: K) -> Vec<u8> {
    let mut prefix_vec = prefix.to_vec();
    prefix_vec.extend_from_slice(key.as_ref());
    prefix_vec
}

fn read(transaction: &Tx, keyspace: Keyspace, prefix: SubtreePrefix, key: &[u8]) -> Option<Record> {
    transaction.get(keyspace, &StoredKey::new(prefix, key))
}

/// A deferred handle to one subtree inside a transaction: reads are charged
/// at the call, writes are staged in the batch the handle was opened with.
pub struct PrefixedRocksDbTransactionContext<'db> {
    transaction: &'db Tx<'db>,
    prefix: SubtreePrefix,
    batch: Option<&'db StorageBatch>,
}

impl<'db> PrefixedRocksDbTransactionContext<'db> {
    pub(super) fn new(
        transaction: &'db Tx<'db>,
        prefix: SubtreePrefix,
        batch: Option<&'db StorageBatch>,
    ) -> Self {
        PrefixedRocksDbTransactionContext {
            transaction,
            prefix,
            batch,
        }
    }

    /// Clears all data in the data keyspace of this prefix.
    pub fn clear(&mut self) -> CostResult<(), Error> {
        FlatStore::clear(self)
    }

    /// Get raw iterator over metadata storage, scoped to this context's
    /// prefix.
    pub fn raw_iter_meta(&self) -> <Self as StorageContext<'db>>::RawIterator {
        cursor(self.transaction, Keyspace::Meta, self.prefix)
    }

    fn staged(&self, operation: &str) -> Result<&'db StorageBatch, Error> {
        self.batch.ok_or_else(|| {
            Error::StorageError(format!(
                "attempted {operation} operation on transactional context without a batch"
            ))
        })
    }

    fn get_from(&self, keyspace: Keyspace, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error> {
        let record = read(self.transaction, keyspace, self.prefix, key);
        let cost = rules::read(keyspace, record.as_ref().map(Record::len));
        Ok(record.map(|record| record.bytes())).wrap_with_cost(cost)
    }

    fn stage(
        &self,
        operation: &str,
        stage: impl FnOnce(&StorageBatch, Vec<u8>),
        key: &[u8],
    ) -> CostResult<(), Error> {
        self.staged(operation)
            .map(|batch| stage(batch, make_prefixed_key(&self.prefix, key)))
            .wrap_with_cost(OperationCost::default())
    }
}

impl<'db> StorageContext<'db> for PrefixedRocksDbTransactionContext<'db> {
    type Batch = PrefixedMultiContextBatchPart;
    type RawIterator = PrefixedRocksDbRawIterator<TransactionCursor<'db>>;

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        let key_len = key.as_ref().len();
        self.stage(
            "put",
            |batch, key| {
                let cost_info = rules::complete_new_key_cost(cost_info, key_len);
                batch.put(key, value.to_vec(), children_sizes, cost_info)
            },
            key.as_ref(),
        )
    }

    fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.stage(
            "put_aux",
            |batch, key| batch.put_aux(key, value.to_vec(), cost_info),
            key.as_ref(),
        )
    }

    fn put_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.stage(
            "put_root",
            |batch, key| batch.put_root(key, value.to_vec(), cost_info),
            key.as_ref(),
        )
    }

    fn put_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.stage(
            "put_meta",
            |batch, key| batch.put_meta(key, value.to_vec(), cost_info),
            key.as_ref(),
        )
    }

    fn delete<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.stage(
            "delete",
            |batch, key| batch.delete(key, cost_info),
            key.as_ref(),
        )
    }

    fn delete_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.stage(
            "delete_aux",
            |batch, key| batch.delete_aux(key, cost_info),
            key.as_ref(),
        )
    }

    fn delete_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.stage(
            "delete_root",
            |batch, key| batch.delete_root(key, cost_info),
            key.as_ref(),
        )
    }

    fn delete_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.stage(
            "delete_meta",
            |batch, key| batch.delete_meta(key, cost_info),
            key.as_ref(),
        )
    }

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.get_from(Keyspace::Data, key.as_ref())
    }

    fn get_aux<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.get_from(Keyspace::Aux, key.as_ref())
    }

    fn get_root<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.get_from(Keyspace::Roots, key.as_ref())
    }

    fn get_meta<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.get_from(Keyspace::Meta, key.as_ref())
    }

    fn new_batch(&self) -> Self::Batch {
        PrefixedMultiContextBatchPart {
            prefix: self.prefix,
            batch: StorageBatch::new(),
        }
    }

    fn commit_batch(&self, batch: Self::Batch) -> CostResult<(), Error> {
        self.staged("commit_batch")
            .map(|existing| existing.merge(batch.batch))
            .wrap_with_cost(OperationCost::default())
    }

    fn raw_iter(&self) -> Self::RawIterator {
        cursor(self.transaction, Keyspace::Data, self.prefix)
    }

    fn raw_iter_aux(&self) -> Self::RawIterator {
        cursor(self.transaction, Keyspace::Aux, self.prefix)
    }
}

/// The writes one Merk commit makes through a deferred handle, merged into
/// the handle's batch when the commit hands them over.
pub struct PrefixedMultiContextBatchPart {
    prefix: SubtreePrefix,
    batch: StorageBatch,
}

impl Batch for PrefixedMultiContextBatchPart {
    fn put<K: AsRef<[u8]>>(
        &mut self,
        key: K,
        value: &[u8],
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error> {
        let cost_info = rules::complete_new_key_cost(cost_info, key.as_ref().len());
        self.batch.put(
            make_prefixed_key(&self.prefix, key),
            value.to_vec(),
            children_sizes,
            cost_info,
        );
        Ok(())
    }

    fn put_aux<K: AsRef<[u8]>>(
        &mut self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error> {
        self.batch.put_aux(
            make_prefixed_key(&self.prefix, key),
            value.to_vec(),
            cost_info,
        );
        Ok(())
    }

    fn put_root<K: AsRef<[u8]>>(
        &mut self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error> {
        self.batch.put_root(
            make_prefixed_key(&self.prefix, key),
            value.to_vec(),
            cost_info,
        );
        Ok(())
    }

    fn delete<K: AsRef<[u8]>>(&mut self, key: K, cost_info: Option<KeyValueStorageCost>) {
        self.batch
            .delete(make_prefixed_key(&self.prefix, key), cost_info);
    }

    fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K, cost_info: Option<KeyValueStorageCost>) {
        self.batch
            .delete_aux(make_prefixed_key(&self.prefix, key), cost_info);
    }

    fn delete_root<K: AsRef<[u8]>>(&mut self, key: K, cost_info: Option<KeyValueStorageCost>) {
        self.batch
            .delete_root(make_prefixed_key(&self.prefix, key), cost_info);
    }
}

/// An immediate handle to one subtree inside a transaction: reads and
/// writes go straight to the transaction and charge nothing, and its batch
/// charges as it is filled.
pub struct PrefixedRocksDbImmediateStorageContext<'db> {
    transaction: &'db Tx<'db>,
    prefix: SubtreePrefix,
}

impl<'db> PrefixedRocksDbImmediateStorageContext<'db> {
    pub(super) fn new(transaction: &'db Tx<'db>, prefix: SubtreePrefix) -> Self {
        PrefixedRocksDbImmediateStorageContext {
            transaction,
            prefix,
        }
    }

    /// Get raw iterator over metadata storage, scoped to this context's
    /// prefix.
    pub fn raw_iter_meta(&self) -> <Self as StorageContext<'db>>::RawIterator {
        cursor(self.transaction, Keyspace::Meta, self.prefix)
    }

    fn write_now(
        &self,
        operation: &'static str,
        keyspace: Keyspace,
        key: &[u8],
        value: Option<&[u8]>,
    ) -> CostResult<(), Error> {
        let write = Write {
            keyspace,
            key: StoredKey::new(self.prefix, key),
            record: value.map(|value| Record::new(keyspace, value)),
        };
        self.transaction
            .write(operation, [write])
            .wrap_with_cost(rules::write_immediate())
    }

    fn get_from(&self, keyspace: Keyspace, key: &[u8]) -> CostResult<Option<Vec<u8>>, Error> {
        Ok(read(self.transaction, keyspace, self.prefix, key).map(|record| record.bytes()))
            .wrap_with_cost(rules::read_immediate())
    }
}

impl<'db> StorageContext<'db> for PrefixedRocksDbImmediateStorageContext<'db> {
    type Batch = PrefixedRocksDbBatch<'db>;
    type RawIterator = PrefixedRocksDbRawIterator<TransactionCursor<'db>>;

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        _children_sizes: ChildrenSizesWithIsSumTree,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.write_now("put", Keyspace::Data, key.as_ref(), Some(value))
    }

    fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.write_now("put", Keyspace::Aux, key.as_ref(), Some(value))
    }

    fn put_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.write_now("put", Keyspace::Roots, key.as_ref(), Some(value))
    }

    fn put_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.write_now("put", Keyspace::Meta, key.as_ref(), Some(value))
    }

    fn delete<K: AsRef<[u8]>>(
        &self,
        key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.write_now("delete", Keyspace::Data, key.as_ref(), None)
    }

    fn delete_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.write_now("delete", Keyspace::Aux, key.as_ref(), None)
    }

    fn delete_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.write_now("delete", Keyspace::Roots, key.as_ref(), None)
    }

    fn delete_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.write_now("delete", Keyspace::Meta, key.as_ref(), None)
    }

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.get_from(Keyspace::Data, key.as_ref())
    }

    fn get_aux<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.get_from(Keyspace::Aux, key.as_ref())
    }

    fn get_root<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.get_from(Keyspace::Roots, key.as_ref())
    }

    fn get_meta<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.get_from(Keyspace::Meta, key.as_ref())
    }

    fn new_batch(&self) -> Self::Batch {
        PrefixedRocksDbBatch {
            prefix: self.prefix,
            writes: Vec::new(),
            cost_acc: OperationCost::default(),
            transaction: PhantomData,
        }
    }

    fn commit_batch(&self, batch: Self::Batch) -> CostResult<(), Error> {
        self.transaction
            .write("batch apply", batch.writes)
            .wrap_with_cost(batch.cost_acc)
    }

    fn raw_iter(&self) -> Self::RawIterator {
        cursor(self.transaction, Keyspace::Data, self.prefix)
    }

    fn raw_iter_aux(&self) -> Self::RawIterator {
        cursor(self.transaction, Keyspace::Aux, self.prefix)
    }
}

/// The batch of an immediate handle. It charges each write as it is added
/// and hands the sum back when it is committed.
pub struct PrefixedRocksDbBatch<'db> {
    prefix: SubtreePrefix,
    writes: Vec<Write>,
    cost_acc: OperationCost,
    transaction: PhantomData<&'db ()>,
}

impl PrefixedRocksDbBatch<'_> {
    fn put_into(
        &mut self,
        keyspace: Keyspace,
        key: &[u8],
        value: &[u8],
        children_sizes: &ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error> {
        let record = Record::new(keyspace, value);
        match rules::immediate_batch_put(
            keyspace,
            key.len(),
            record.len(),
            children_sizes,
            cost_info.as_ref(),
        ) {
            Ok(charge) => charge.add_all_to(&mut self.cost_acc),
            Err((error, owed)) => {
                self.cost_acc += owed;
                return Err(error);
            }
        }
        self.writes.push(Write {
            keyspace,
            key: StoredKey::new(self.prefix, key),
            record: Some(record),
        });
        Ok(())
    }

    fn delete_from(
        &mut self,
        keyspace: Keyspace,
        key: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) {
        rules::immediate_batch_delete(cost_info).add_all_to(&mut self.cost_acc);
        self.writes.push(Write {
            keyspace,
            key: StoredKey::new(self.prefix, key),
            record: None,
        });
    }
}

impl Batch for PrefixedRocksDbBatch<'_> {
    fn put<K: AsRef<[u8]>>(
        &mut self,
        key: K,
        value: &[u8],
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error> {
        let cost_info = rules::complete_new_key_cost(cost_info, key.as_ref().len());
        self.put_into(
            Keyspace::Data,
            key.as_ref(),
            value,
            &children_sizes,
            cost_info,
        )
    }

    fn put_aux<K: AsRef<[u8]>>(
        &mut self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error> {
        self.put_into(Keyspace::Aux, key.as_ref(), value, &None, cost_info)
    }

    fn put_root<K: AsRef<[u8]>>(
        &mut self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error> {
        self.put_into(Keyspace::Roots, key.as_ref(), value, &None, cost_info)
    }

    fn delete<K: AsRef<[u8]>>(&mut self, key: K, cost_info: Option<KeyValueStorageCost>) {
        self.delete_from(Keyspace::Data, key.as_ref(), cost_info);
    }

    fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K, cost_info: Option<KeyValueStorageCost>) {
        self.delete_from(Keyspace::Aux, key.as_ref(), cost_info);
    }

    fn delete_root<K: AsRef<[u8]>>(&mut self, key: K, cost_info: Option<KeyValueStorageCost>) {
        self.delete_from(Keyspace::Roots, key.as_ref(), cost_info);
    }
}

/// A cursor of the model with the transaction whose writes it sees.
pub struct TransactionCursor<'db> {
    cursor: Cursor,
    transaction: &'db Tx<'db>,
}

fn cursor<'db>(
    transaction: &'db Tx<'db>,
    keyspace: Keyspace,
    prefix: SubtreePrefix,
) -> PrefixedRocksDbRawIterator<TransactionCursor<'db>> {
    PrefixedRocksDbRawIterator {
        prefix,
        raw_iterator: TransactionCursor {
            cursor: transaction.cursor(keyspace),
            transaction,
        },
    }
}

/// A cursor scoped to one subtree.
pub struct PrefixedRocksDbRawIterator<I> {
    prefix: SubtreePrefix,
    raw_iterator: I,
}

impl<I> PrefixedRocksDbRawIterator<I> {
    /// Re-scope this iterator to a different subtree prefix while keeping
    /// the underlying cursor, and therefore the state it sees.
    pub fn retarget(self, prefix: SubtreePrefix) -> Self {
        Self {
            prefix,
            raw_iterator: self.raw_iterator,
        }
    }
}

impl PrefixedRocksDbRawIterator<TransactionCursor<'_>> {
    fn move_to(&mut self, movement: CursorMove, key: &[u8]) -> CostContext<()> {
        let state = self.raw_iterator.transaction.state();
        let cost = self
            .raw_iterator
            .cursor
            .move_to(self.prefix, movement, key, state.overlay());
        ().wrap_with_cost(cost)
    }
}

impl RawIterator for PrefixedRocksDbRawIterator<TransactionCursor<'_>> {
    fn seek_to_first(&mut self) -> CostContext<()> {
        self.move_to(CursorMove::SeekToFirst, &[])
    }

    fn seek_to_last(&mut self) -> CostContext<()> {
        self.move_to(CursorMove::SeekToLast, &[])
    }

    fn seek<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        self.move_to(CursorMove::Seek, key.as_ref())
    }

    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, key: K) -> CostContext<()> {
        self.move_to(CursorMove::SeekForPrev, key.as_ref())
    }

    fn next(&mut self) -> CostContext<()> {
        self.move_to(CursorMove::Next, &[])
    }

    fn prev(&mut self) -> CostContext<()> {
        self.move_to(CursorMove::Prev, &[])
    }

    fn value(&self) -> CostContext<Option<&[u8]>> {
        let (value, cost) = self.raw_iterator.cursor.value(self.prefix);
        value.wrap_with_cost(cost)
    }

    fn key(&self) -> CostContext<Option<&[u8]>> {
        let (key, cost) = self.raw_iterator.cursor.key(self.prefix);
        key.wrap_with_cost(cost)
    }

    fn valid(&self) -> CostContext<bool> {
        let (valid, cost) = self.raw_iterator.cursor.valid(self.prefix);
        valid.wrap_with_cost(cost)
    }
}
