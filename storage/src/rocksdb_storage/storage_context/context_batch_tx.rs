//! Storage context implementation with a transaction.
use costs::{storage_cost::key_value_cost::KeyValueStorageCost, CostContext, CostsExt, OperationCost, cost_return_on_error};
use error::Error;
use rocksdb::{ColumnFamily, DBRawIteratorWithThreadMode};

use super::{batch::PrefixedMultiContextBatchPart, make_prefixed_key, PrefixedRocksDbRawIterator};
use crate::{Batch, error, error::Error::RocksDBError, RawIterator, rocksdb_storage::storage::{Db, Tx, AUX_CF_NAME, META_CF_NAME, ROOTS_CF_NAME}, StorageBatch, StorageContext};

/// Storage context with a prefix applied to be used in a subtree to be used in
/// transaction.
pub struct PrefixedRocksDbBatchTransactionContext<'db> {
    storage: &'db Db,
    transaction: &'db Tx<'db>,
    prefix: Vec<u8>,
    batch: &'db StorageBatch,
}

impl<'db> PrefixedRocksDbBatchTransactionContext<'db> {
    /// Create a new prefixed transaction context instance
    pub fn new(
        storage: &'db Db,
        transaction: &'db Tx<'db>,
        prefix: Vec<u8>,
        batch: &'db StorageBatch,
    ) -> Self {
        PrefixedRocksDbBatchTransactionContext {
            storage,
            transaction,
            prefix,
            batch,
        }
    }

    /// Clears all the data in the tree at the storage level
    pub fn clear(&mut self) -> CostContext<Result<(), Error>> {
        let mut cost = OperationCost::default();

        let mut iter = self.raw_iter();
        iter.seek_to_first().unwrap_add_cost(&mut cost);

        while iter.valid().unwrap_add_cost(&mut cost) {
            if let Some(key) = iter.key().unwrap_add_cost(&mut cost) {
                cost_return_on_error!(
            &mut cost,
                    //todo: calculate cost
            self.delete(key, None)
        );
            }
            iter.next().unwrap_add_cost(&mut cost);
        }
        Ok(()).wrap_with_cost(cost)
    }
}

impl<'db> PrefixedRocksDbBatchTransactionContext<'db> {
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

impl<'db> StorageContext<'db> for PrefixedRocksDbBatchTransactionContext<'db> {
    type Batch = PrefixedMultiContextBatchPart;
    type Error = Error;
    type RawIterator = PrefixedRocksDbRawIterator<DBRawIteratorWithThreadMode<'db, Tx<'db>>>;

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        children_sizes: Option<(Option<u32>, Option<u32>)>,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostContext<Result<(), Self::Error>> {
        self.batch
            .put(
                make_prefixed_key(self.prefix.clone(), key),
                value.to_vec(),
                children_sizes,
                cost_info,
            )
            .map(Ok)
    }

    fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostContext<Result<(), Self::Error>> {
        self.batch
            .put_aux(
                make_prefixed_key(self.prefix.clone(), key),
                value.to_vec(),
                cost_info,
            )
            .map(Ok)
    }

    fn put_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostContext<Result<(), Self::Error>> {
        self.batch
            .put_root(
                make_prefixed_key(self.prefix.clone(), key),
                value.to_vec(),
                cost_info,
            )
            .map(Ok)
    }

    fn put_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostContext<Result<(), Self::Error>> {
        self.batch
            .put_meta(
                make_prefixed_key(self.prefix.clone(), key),
                value.to_vec(),
                cost_info,
            )
            .map(Ok)
    }

    fn delete<K: AsRef<[u8]>>(&self, key: K, cost_info: Option<KeyValueStorageCost>) -> CostContext<Result<(), Self::Error>> {
        self.batch
            .delete(make_prefixed_key(self.prefix.clone(), key), cost_info)
            .map(Ok)
    }

    fn delete_aux<K: AsRef<[u8]>>(&self, key: K, cost_info: Option<KeyValueStorageCost>) -> CostContext<Result<(), Self::Error>> {
        self.batch
            .delete_aux(make_prefixed_key(self.prefix.clone(), key), cost_info)
            .map(Ok)
    }

    fn delete_root<K: AsRef<[u8]>>(&self, key: K, cost_info: Option<KeyValueStorageCost>) -> CostContext<Result<(), Self::Error>> {
        self.batch
            .delete_root(make_prefixed_key(self.prefix.clone(), key), cost_info)
            .map(Ok)
    }

    fn delete_meta<K: AsRef<[u8]>>(&self, key: K, cost_info: Option<KeyValueStorageCost>) -> CostContext<Result<(), Self::Error>> {
        self.batch
            .delete_meta(make_prefixed_key(self.prefix.clone(), key), cost_info)
            .map(Ok)
    }

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostContext<Result<Option<Vec<u8>>, Self::Error>> {
        self.transaction
            .get(make_prefixed_key(self.prefix.clone(), key))
            .map_err(RocksDBError)
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
            .map_err(RocksDBError)
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
            .map_err(RocksDBError)
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
            .map_err(RocksDBError)
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
        PrefixedMultiContextBatchPart {
            prefix: self.prefix.clone(),
            batch: StorageBatch::new(),
            acc_cost: OperationCost::default(),
        }
    }

    fn commit_batch(&self, batch: Self::Batch) -> CostContext<Result<(), Self::Error>> {
        self.batch.merge(batch.batch).map(Ok)
    }

    fn raw_iter(&self) -> Self::RawIterator {
        PrefixedRocksDbRawIterator {
            prefix: self.prefix.clone(),
            raw_iterator: self.transaction.raw_iterator(),
        }
    }
}
