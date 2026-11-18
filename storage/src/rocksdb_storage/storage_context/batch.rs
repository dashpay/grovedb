//! Prefixed storage batch implementation for RocksDB backend.

use grovedb_costs::{
    cost_model::{ImmediateBatchCharge, Keyspace},
    storage_cost::key_value_cost::KeyValueStorageCost,
    ChildrenSizesWithIsSumTree,
};
use rocksdb::{ColumnFamily, WriteBatchWithTransaction};

use super::make_prefixed_key;
use crate::{
    rocksdb_storage::storage::{SubtreePrefix, COST_MODEL},
    Batch, StorageBatch,
};

/// Wrapper to RocksDB batch.
/// All calls go to RocksDB batch, but wrapper handles prefixes and column
/// families. Also accumulates costs before commit.
pub struct PrefixedRocksDbBatch<'db> {
    pub(crate) prefix: SubtreePrefix,
    pub(crate) batch: WriteBatchWithTransaction<true>,
    pub(crate) cf_aux: &'db ColumnFamily,
    pub(crate) cf_roots: &'db ColumnFamily,

    /// As a batch to be committed is a RocksDB batch and there is no way to get
    /// what it will do, we collect costs at the moment we append something to
    /// the batch.
    pub(crate) cost_acc: ImmediateBatchCharge,
}

/// Batch with no backing storage_cost (it's not a RocksDB batch, but our own
/// way to represent a set of operations) that eventually will be merged into
/// multi-context batch.
pub struct PrefixedMultiContextBatchPart {
    pub(crate) prefix: SubtreePrefix,
    pub(crate) batch: StorageBatch,
}

/// Implementation of a batch outside a transaction
impl Batch for PrefixedRocksDbBatch<'_> {
    fn put<K: AsRef<[u8]>>(
        &mut self,
        key: K,
        value: &[u8],
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error> {
        let prefixed_key = make_prefixed_key(&self.prefix, key);
        let cost_info = COST_MODEL.complete_new_key_cost(cost_info, prefixed_key.len());

        self.cost_acc.put(
            Keyspace::Data,
            prefixed_key.len(),
            value.len(),
            children_sizes,
            cost_info,
        )?;

        self.batch.put(prefixed_key, value);
        Ok(())
    }

    fn put_aux<K: AsRef<[u8]>>(
        &mut self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error> {
        let prefixed_key = make_prefixed_key(&self.prefix, key);

        self.cost_acc.put(
            Keyspace::Aux,
            prefixed_key.len(),
            value.len(),
            None,
            cost_info,
        )?;

        self.batch.put_cf(self.cf_aux, prefixed_key, value);
        Ok(())
    }

    fn put_root<K: AsRef<[u8]>>(
        &mut self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error> {
        let prefixed_key = make_prefixed_key(&self.prefix, key);

        self.cost_acc.put(
            Keyspace::Roots,
            prefixed_key.len(),
            value.len(),
            None,
            cost_info,
        )?;

        self.batch.put_cf(self.cf_roots, prefixed_key, value);
        Ok(())
    }

    fn delete<K: AsRef<[u8]>>(&mut self, key: K, cost_info: Option<KeyValueStorageCost>) {
        let prefixed_key = make_prefixed_key(&self.prefix, key);

        self.cost_acc.delete(cost_info);

        self.batch.delete(prefixed_key);
    }

    fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K, cost_info: Option<KeyValueStorageCost>) {
        let prefixed_key = make_prefixed_key(&self.prefix, key);

        self.cost_acc.delete(cost_info);

        self.batch.delete_cf(self.cf_aux, prefixed_key);
    }

    fn delete_root<K: AsRef<[u8]>>(&mut self, key: K, cost_info: Option<KeyValueStorageCost>) {
        let prefixed_key = make_prefixed_key(&self.prefix, key);

        self.cost_acc.delete(cost_info);

        self.batch.delete_cf(self.cf_roots, prefixed_key);
    }
}

/// Implementation of a rocksdb batch outside a transaction for multi-context
/// batch.
impl Batch for PrefixedMultiContextBatchPart {
    fn put<K: AsRef<[u8]>>(
        &mut self,
        key: K,
        value: &[u8],
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) -> Result<(), grovedb_costs::error::Error> {
        let prefixed_key = make_prefixed_key(&self.prefix, key);
        let cost_info = COST_MODEL.complete_new_key_cost(cost_info, prefixed_key.len());

        self.batch
            .put(prefixed_key, value.to_vec(), children_sizes, cost_info);
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
