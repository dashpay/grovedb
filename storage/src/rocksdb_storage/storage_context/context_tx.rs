//! Storage context implementation with a transaction.
use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostContext, CostsExt, OperationCost,
};
use rocksdb::{ColumnFamily, DBRawIteratorWithThreadMode, Error, WriteBatchWithTransaction};

use super::{make_prefixed_key, PrefixedRocksDbBatch, PrefixedRocksDbRawIterator};
use crate::{
    rocksdb_storage::storage::{Db, Tx, AUX_CF_NAME, META_CF_NAME, ROOTS_CF_NAME},
    StorageContext,
};

/// Storage context with a prefix applied to be used in a subtree to be used in
/// transaction.
pub struct PrefixedRocksDbTransactionContext<'db> {
    storage: &'db Db,
    transaction: &'db Tx<'db>,
    prefix: Vec<u8>,
}

impl<'db> PrefixedRocksDbTransactionContext<'db> {
    /// Create a new prefixed transaction context instance
    pub fn new(storage: &'db Db, transaction: &'db Tx<'db>, prefix: Vec<u8>) -> Self {
        PrefixedRocksDbTransactionContext {
            storage,
            transaction,
            prefix,
        }
    }
}

impl<'db> PrefixedRocksDbTransactionContext<'db> {
    /// Get auxiliary data column family
    fn cf_aux(&self) -> &'db ColumnFamily {
        self.storage
            .cf_handle(AUX_CF_NAME)
            .expect("aux column family must exist")
    }

    /// Get trees roots data column family
    fn cf_roots(&self) -> &'db ColumnFamily {
        self.storage
            .cf_handle(ROOTS_CF_NAME)
            .expect("roots column family must exist")
    }

    /// Get metadata column family
    fn cf_meta(&self) -> &'db ColumnFamily {
        self.storage
            .cf_handle(META_CF_NAME)
            .expect("meta column family must exist")
    }
}

impl<'db> StorageContext<'db> for PrefixedRocksDbTransactionContext<'db> {
    type Batch = PrefixedRocksDbBatch<'db>;
    type Error = Error;
    type RawIterator = PrefixedRocksDbRawIterator<DBRawIteratorWithThreadMode<'db, Tx<'db>>>;

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        replaced_value_bytes_count: Option<u16>,
    ) -> CostContext<Result<(), Self::Error>> {
        self.transaction
            .put(make_prefixed_key(self.prefix.clone(), &key), value)
            .wrap_with_cost(OperationCost {
                storage_written_bytes: key.as_ref().len() as u32 + value.len() as u32,
                seek_count: 1,
                ..Default::default()
            })
    }

    fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
    ) -> CostContext<Result<(), Self::Error>> {
        self.transaction
            .put_cf(
                self.cf_aux(),
                make_prefixed_key(self.prefix.clone(), &key),
                value,
            )
            .wrap_with_cost(OperationCost {
                storage_written_bytes: key.as_ref().len() as u32 + value.len() as u32,
                seek_count: 1,
                ..Default::default()
            })
    }

    fn put_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
    ) -> CostContext<Result<(), Self::Error>> {
        self.transaction
            .put_cf(
                self.cf_roots(),
                make_prefixed_key(self.prefix.clone(), &key),
                value,
            )
            .wrap_with_cost(OperationCost {
                storage_written_bytes: key.as_ref().len() as u32 + value.len() as u32,
                seek_count: 1,
                ..Default::default()
            })
    }

    fn put_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
    ) -> CostContext<Result<(), Self::Error>> {
        self.transaction
            .put_cf(
                self.cf_meta(),
                make_prefixed_key(self.prefix.clone(), &key),
                value,
            )
            .wrap_with_cost(OperationCost {
                storage_written_bytes: key.as_ref().len() as u32 + value.len() as u32,
                seek_count: 1,
                ..Default::default()
            })
    }

    fn delete<K: AsRef<[u8]>>(&self, key: K) -> CostContext<Result<(), Self::Error>> {
        let mut cost = OperationCost::default();

        let deleted_len = cost_return_on_error!(&mut cost, self.get(&key))
            .map(|x| x.len() as u32)
            .unwrap_or(0);

        cost.storage_freed_bytes += deleted_len;
        cost.seek_count += 1;

        self.transaction
            .delete(make_prefixed_key(self.prefix.clone(), key))
            .wrap_with_cost(cost)
    }

    fn delete_aux<K: AsRef<[u8]>>(&self, key: K) -> CostContext<Result<(), Self::Error>> {
        let mut cost = OperationCost::default();

        let deleted_len = cost_return_on_error!(&mut cost, self.get_aux(&key))
            .map(|x| x.len() as u32)
            .unwrap_or(0);

        cost.storage_freed_bytes += deleted_len;
        cost.seek_count += 1;

        self.transaction
            .delete_cf(self.cf_aux(), make_prefixed_key(self.prefix.clone(), key))
            .wrap_with_cost(cost)
    }

    fn delete_root<K: AsRef<[u8]>>(&self, key: K) -> CostContext<Result<(), Self::Error>> {
        let mut cost = OperationCost::default();

        let deleted_len = cost_return_on_error!(&mut cost, self.get_root(&key))
            .map(|x| x.len() as u32)
            .unwrap_or(0);

        cost.storage_freed_bytes += deleted_len;
        cost.seek_count += 1;

        self.transaction
            .delete_cf(self.cf_roots(), make_prefixed_key(self.prefix.clone(), key))
            .wrap_with_cost(cost)
    }

    fn delete_meta<K: AsRef<[u8]>>(&self, key: K) -> CostContext<Result<(), Self::Error>> {
        let mut cost = OperationCost::default();

        let deleted_len = cost_return_on_error!(&mut cost, self.get_meta(&key))
            .map(|x| x.len() as u32)
            .unwrap_or(0);

        cost.storage_freed_bytes += deleted_len;
        cost.seek_count += 1;

        self.transaction
            .delete_cf(self.cf_meta(), make_prefixed_key(self.prefix.clone(), key))
            .wrap_with_cost(cost)
    }

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostContext<Result<Option<Vec<u8>>, Self::Error>> {
        self.transaction
            .get(make_prefixed_key(self.prefix.clone(), key))
            .wrap_fn_cost(|value| OperationCost {
                seek_count: 1,
                storage_loaded_bytes: value
                    .as_ref()
                    .ok()
                    .map(Option::as_ref)
                    .flatten()
                    .map(|x| x.len() as u32)
                    .unwrap_or(0),
                ..Default::default()
            })
    }

    fn get_aux<K: AsRef<[u8]>>(&self, key: K) -> CostContext<Result<Option<Vec<u8>>, Self::Error>> {
        self.transaction
            .get_cf(self.cf_aux(), make_prefixed_key(self.prefix.clone(), key))
            .wrap_fn_cost(|value| OperationCost {
                seek_count: 1,
                storage_loaded_bytes: value
                    .as_ref()
                    .ok()
                    .map(Option::as_ref)
                    .flatten()
                    .map(|x| x.len() as u32)
                    .unwrap_or(0),
                ..Default::default()
            })
    }

    fn get_root<K: AsRef<[u8]>>(
        &self,
        key: K,
    ) -> CostContext<Result<Option<Vec<u8>>, Self::Error>> {
        self.transaction
            .get_cf(self.cf_roots(), make_prefixed_key(self.prefix.clone(), key))
            .wrap_fn_cost(|value| OperationCost {
                seek_count: 1,
                storage_loaded_bytes: value
                    .as_ref()
                    .ok()
                    .map(Option::as_ref)
                    .flatten()
                    .map(|x| x.len() as u32)
                    .unwrap_or(0),
                ..Default::default()
            })
    }

    fn get_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
    ) -> CostContext<Result<Option<Vec<u8>>, Self::Error>> {
        self.transaction
            .get_cf(self.cf_meta(), make_prefixed_key(self.prefix.clone(), key))
            .wrap_fn_cost(|value| OperationCost {
                seek_count: 1,
                storage_loaded_bytes: value
                    .as_ref()
                    .ok()
                    .map(Option::as_ref)
                    .flatten()
                    .map(|x| x.len() as u32)
                    .unwrap_or(0),
                ..Default::default()
            })
    }

    fn new_batch(&self) -> Self::Batch {
        PrefixedRocksDbBatch {
            prefix: self.prefix.clone(),
            batch: WriteBatchWithTransaction::<true>::default(),
            cf_aux: self.cf_aux(),
            cf_roots: self.cf_roots(),
            cost_acc: Default::default(),
            delete_keys_for_costs: Default::default(),
            delete_keys_for_costs_aux: Default::default(),
            delete_keys_for_costs_roots: Default::default(),
        }
    }

    fn commit_batch(&self, mut batch: Self::Batch) -> CostContext<Result<(), Self::Error>> {
        let mut cost = OperationCost::default();

        // If deletion cost finalization fails, only cost of this finalization will be
        // returned as no batch will be commited.
        cost_return_on_error!(&mut cost, batch.finalize_deletion_costs(&self.storage));

        // On unsuccessul batch commit only deletion finalization cost will be returned.
        cost_return_on_error_no_add!(
            &cost,
            self.transaction.rebuild_from_writebatch(&batch.batch)
        );

        Ok(()).wrap_with_cost(cost).add_cost(batch.cost_acc)
    }

    fn raw_iter(&self) -> Self::RawIterator {
        PrefixedRocksDbRawIterator {
            prefix: self.prefix.clone(),
            raw_iterator: self.transaction.raw_iterator(),
        }
    }
}
