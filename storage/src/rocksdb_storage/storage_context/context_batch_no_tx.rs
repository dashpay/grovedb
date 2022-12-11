use costs::{storage_cost::key_value_cost::KeyValueStorageCost, CostContext, CostsExt, OperationCost, CostResult};
use error::Error;
use rocksdb::{ColumnFamily, DBRawIteratorWithThreadMode};

use super::{batch::PrefixedMultiContextBatchPart, make_prefixed_key, PrefixedRocksDbRawIterator};
use crate::{
    error,
    error::Error::RocksDBError,
    rocksdb_storage::storage::{Db, AUX_CF_NAME, META_CF_NAME, ROOTS_CF_NAME},
    StorageBatch, StorageContext,
};
use crate::storage::ChildrenSizes;

/// Storage context with a prefix applied to be used in a subtree to be used
/// outside of transaction.
pub struct PrefixedRocksDbBatchStorageContext<'db> {
    storage: &'db Db,
    prefix: Vec<u8>,
    batch: &'db StorageBatch,
}

impl<'db> PrefixedRocksDbBatchStorageContext<'db> {
    /// Create a new prefixed storage_cost context instance
    pub fn new(storage: &'db Db, prefix: Vec<u8>, batch: &'db StorageBatch) -> Self {
        PrefixedRocksDbBatchStorageContext {
            storage,
            prefix,
            batch,
        }
    }
}

impl<'db> PrefixedRocksDbBatchStorageContext<'db> {
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

impl<'db> StorageContext<'db> for PrefixedRocksDbBatchStorageContext<'db> {
    type Batch = PrefixedMultiContextBatchPart;
    type RawIterator = PrefixedRocksDbRawIterator<DBRawIteratorWithThreadMode<'db, Db>>;

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        children_sizes: ChildrenSizes,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.batch.put(
            make_prefixed_key(self.prefix.clone(), key),
            value.to_vec(),
            children_sizes,
            cost_info,
        );
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.batch.put_aux(
            make_prefixed_key(self.prefix.clone(), key),
            value.to_vec(),
            cost_info,
        );
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn put_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.batch.put_root(
            make_prefixed_key(self.prefix.clone(), key),
            value.to_vec(),
            cost_info,
        );
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn put_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.batch.put_meta(
            make_prefixed_key(self.prefix.clone(), key),
            value.to_vec(),
            cost_info,
        );
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn delete<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.batch
            .delete(make_prefixed_key(self.prefix.clone(), key), cost_info);
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn delete_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.batch
            .delete_aux(make_prefixed_key(self.prefix.clone(), key), cost_info);
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn delete_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.batch
            .delete_root(make_prefixed_key(self.prefix.clone(), key), cost_info);
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn delete_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.batch
            .delete_meta(make_prefixed_key(self.prefix.clone(), key), cost_info);
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.storage
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

    fn get_aux<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.storage
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
    ) -> CostResult<Option<Vec<u8>>, Error> {
        self.storage
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
    ) -> CostResult<Option<Vec<u8>>, Error> {
        self.storage
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
        }
    }

    fn commit_batch(&self, batch: Self::Batch) -> CostResult<(), Error> {
        self.batch.merge(batch.batch);
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn raw_iter(&self) -> Self::RawIterator {
        PrefixedRocksDbRawIterator {
            prefix: self.prefix.clone(),
            raw_iterator: self.storage.raw_iterator(),
        }
    }
}
