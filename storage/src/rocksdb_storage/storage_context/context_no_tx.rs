// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Storage context batch implementation without a transaction

use error::Error;
use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, ChildrenSizesWithIsSumTree, CostResult,
    CostsExt, OperationCost,
};
use rocksdb::{ColumnFamily, DBRawIteratorWithThreadMode};

use super::{batch::PrefixedMultiContextBatchPart, make_prefixed_key, PrefixedRocksDbRawIterator};
use crate::{
    error,
    error::Error::RocksDBError,
    rocksdb_storage::storage::{Db, SubtreePrefix, AUX_CF_NAME, META_CF_NAME, ROOTS_CF_NAME},
    StorageBatch, StorageContext,
};

/// Storage context with a prefix applied to be used in a subtree to be used
/// outside of transaction.
pub struct PrefixedRocksDbStorageContext<'db> {
    storage: &'db Db,
    prefix: SubtreePrefix,
    batch: Option<&'db StorageBatch>,
}

impl<'db> PrefixedRocksDbStorageContext<'db> {
    /// Create a new prefixed storage_cost context instance
    pub fn new(storage: &'db Db, prefix: SubtreePrefix, batch: Option<&'db StorageBatch>) -> Self {
        PrefixedRocksDbStorageContext {
            storage,
            prefix,
            batch,
        }
    }
}

impl<'db> PrefixedRocksDbStorageContext<'db> {
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

impl<'db> StorageContext<'db> for PrefixedRocksDbStorageContext<'db> {
    type Batch = PrefixedMultiContextBatchPart;
    type RawIterator = PrefixedRocksDbRawIterator<DBRawIteratorWithThreadMode<'db, Db>>;

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        if let Some(existing_batch) = self.batch {
            existing_batch.put(
                make_prefixed_key(&self.prefix, key),
                value.to_vec(),
                children_sizes,
                cost_info,
            );
        }
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        if let Some(existing_batch) = self.batch {
            existing_batch.put_aux(
                make_prefixed_key(&self.prefix, key),
                value.to_vec(),
                cost_info,
            );
        }
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn put_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        if let Some(existing_batch) = self.batch {
            existing_batch.put_root(
                make_prefixed_key(&self.prefix, key),
                value.to_vec(),
                cost_info,
            );
        }
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn put_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        if let Some(existing_batch) = self.batch {
            existing_batch.put_meta(
                make_prefixed_key(&self.prefix, key),
                value.to_vec(),
                cost_info,
            );
        }
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn delete<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        if let Some(existing_batch) = self.batch {
            existing_batch.delete(make_prefixed_key(&self.prefix, key), cost_info);
        }
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn delete_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        if let Some(existing_batch) = self.batch {
            existing_batch.delete_aux(make_prefixed_key(&self.prefix, key), cost_info);
        }
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn delete_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        if let Some(existing_batch) = self.batch {
            existing_batch.delete_root(make_prefixed_key(&self.prefix, key), cost_info);
        }
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn delete_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        if let Some(existing_batch) = self.batch {
            existing_batch.delete_meta(make_prefixed_key(&self.prefix, key), cost_info);
        }
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.storage
            .get(make_prefixed_key(&self.prefix, key))
            .map_err(RocksDBError)
            .wrap_fn_cost(|value| OperationCost {
                seek_count: 1,
                storage_loaded_bytes: value
                    .as_ref()
                    .ok()
                    .and_then(Option::as_ref)
                    .map(|x| x.len() as u32)
                    .unwrap_or(0),
                ..Default::default()
            })
    }

    fn get_aux<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.storage
            .get_cf(self.cf_aux(), make_prefixed_key(&self.prefix, key))
            .map_err(RocksDBError)
            .wrap_fn_cost(|value| OperationCost {
                seek_count: 1,
                storage_loaded_bytes: value
                    .as_ref()
                    .ok()
                    .and_then(Option::as_ref)
                    .map(|x| x.len() as u32)
                    .unwrap_or(0),
                ..Default::default()
            })
    }

    fn get_root<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.storage
            .get_cf(self.cf_roots(), make_prefixed_key(&self.prefix, key))
            .map_err(RocksDBError)
            .wrap_fn_cost(|value| OperationCost {
                seek_count: 1,
                storage_loaded_bytes: value
                    .as_ref()
                    .ok()
                    .and_then(Option::as_ref)
                    .map(|x| x.len() as u32)
                    .unwrap_or(0),
                ..Default::default()
            })
    }

    fn get_meta<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.storage
            .get_cf(self.cf_meta(), make_prefixed_key(&self.prefix, key))
            .map_err(RocksDBError)
            .wrap_fn_cost(|value| OperationCost {
                seek_count: 1,
                storage_loaded_bytes: value
                    .as_ref()
                    .ok()
                    .and_then(Option::as_ref)
                    .map(|x| x.len() as u32)
                    .unwrap_or(0),
                ..Default::default()
            })
    }

    fn new_batch(&self) -> Self::Batch {
        PrefixedMultiContextBatchPart {
            prefix: self.prefix,
            batch: StorageBatch::new(),
        }
    }

    fn commit_batch(&self, batch: Self::Batch) -> CostResult<(), Error> {
        if let Some(existing_batch) = self.batch {
            existing_batch.merge(batch.batch);
        }
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn raw_iter(&self) -> Self::RawIterator {
        PrefixedRocksDbRawIterator {
            prefix: self.prefix,
            raw_iterator: self.storage.raw_iterator(),
        }
    }
}
