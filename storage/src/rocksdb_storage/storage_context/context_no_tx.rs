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

//! Storage context implementation without a transaction

use costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::{
        key_value_cost::KeyValueStorageCost, removal::StorageRemovedBytes::BasicStorageRemoval,
    },
    ChildrenSizesWithIsSumTree, CostResult, CostsExt, OperationCost,
};
use error::Error;
use rocksdb::{ColumnFamily, DBRawIteratorWithThreadMode, WriteBatchWithTransaction};

use super::{make_prefixed_key, PrefixedRocksDbBatch, PrefixedRocksDbRawIterator};
use crate::{
    error,
    error::Error::{CostError, RocksDBError},
    rocksdb_storage::storage::{Db, AUX_CF_NAME, META_CF_NAME, ROOTS_CF_NAME},
    StorageContext,
};

/// Storage context with a prefix applied to be used in a subtree to be used
/// outside of transaction.
pub struct PrefixedRocksDbStorageContext<'db> {
    storage: &'db Db,
    /// ze prefix
    pub prefix: Vec<u8>,
}

impl<'db> PrefixedRocksDbStorageContext<'db> {
    /// Create a new prefixed storage context instance
    pub fn new(storage: &'db Db, prefix: Vec<u8>) -> Self {
        PrefixedRocksDbStorageContext { storage, prefix }
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
    type Batch = PrefixedRocksDbBatch<'db>;
    type RawIterator = PrefixedRocksDbRawIterator<DBRawIteratorWithThreadMode<'db, Db>>;

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost {
            seek_count: 1,
            ..Default::default()
        };
        cost_return_on_error_no_add!(
            &cost,
            cost.add_key_value_storage_costs(
                key.as_ref().len() as u32,
                value.len() as u32,
                children_sizes,
                cost_info,
            )
            .map_err(CostError)
        );
        self.storage
            .put(make_prefixed_key(self.prefix.clone(), &key), value)
            .map_err(RocksDBError)
            .wrap_with_cost(cost)
    }

    fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost {
            seek_count: 1,
            ..Default::default()
        };
        cost_return_on_error_no_add!(
            &cost,
            cost.add_key_value_storage_costs(
                key.as_ref().len() as u32,
                value.len() as u32,
                None,
                cost_info,
            )
            .map_err(CostError)
        );
        self.storage
            .put_cf(
                self.cf_aux(),
                make_prefixed_key(self.prefix.clone(), &key),
                value,
            )
            .map_err(RocksDBError)
            .wrap_with_cost(cost)
    }

    fn put_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost {
            seek_count: 1,
            ..Default::default()
        };
        cost_return_on_error_no_add!(
            &cost,
            cost.add_key_value_storage_costs(
                key.as_ref().len() as u32,
                value.len() as u32,
                None,
                cost_info,
            )
            .map_err(CostError)
        );
        self.storage
            .put_cf(
                self.cf_roots(),
                make_prefixed_key(self.prefix.clone(), &key),
                value,
            )
            .map_err(RocksDBError)
            .wrap_with_cost(cost)
    }

    fn put_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost {
            seek_count: 1,
            ..Default::default()
        };
        cost_return_on_error_no_add!(
            &cost,
            cost.add_key_value_storage_costs(
                key.as_ref().len() as u32,
                value.len() as u32,
                None,
                cost_info,
            )
            .map_err(CostError)
        );
        self.storage
            .put_cf(
                self.cf_meta(),
                make_prefixed_key(self.prefix.clone(), &key),
                value,
            )
            .map_err(RocksDBError)
            .wrap_with_cost(cost)
    }

    fn delete<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        if let Some(cost_info) = cost_info {
            cost.storage_cost.removed_bytes += cost_info.combined_removed_bytes();
            cost.seek_count += 1;
        } else {
            let deleted_len = cost_return_on_error!(&mut cost, self.get(&key))
                .map(|x| x.len() as u32)
                .unwrap_or(0);

            cost.storage_cost.removed_bytes += BasicStorageRemoval(deleted_len);
            cost.seek_count += 2;
        }

        self.storage
            .delete(make_prefixed_key(self.prefix.clone(), key))
            .map_err(RocksDBError)
            .wrap_with_cost(cost)
    }

    fn delete_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        if let Some(cost_info) = cost_info {
            cost.storage_cost.removed_bytes += cost_info.combined_removed_bytes();
            cost.seek_count += 1;
        } else {
            let deleted_len = cost_return_on_error!(&mut cost, self.get_aux(&key))
                .map(|x| x.len() as u32)
                .unwrap_or(0);

            cost.storage_cost.removed_bytes += BasicStorageRemoval(deleted_len);
            cost.seek_count += 2;
        }

        self.storage
            .delete_cf(self.cf_aux(), make_prefixed_key(self.prefix.clone(), key))
            .map_err(RocksDBError)
            .wrap_with_cost(cost)
    }

    fn delete_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        if let Some(cost_info) = cost_info {
            cost.storage_cost.removed_bytes += cost_info.combined_removed_bytes();
            cost.seek_count += 1;
        } else {
            let deleted_len = cost_return_on_error!(&mut cost, self.get_root(&key))
                .map(|x| x.len() as u32)
                .unwrap_or(0);

            cost.storage_cost.removed_bytes += BasicStorageRemoval(deleted_len);
            cost.seek_count += 2;
        }

        self.storage
            .delete_cf(self.cf_roots(), make_prefixed_key(self.prefix.clone(), key))
            .map_err(RocksDBError)
            .wrap_with_cost(cost)
    }

    fn delete_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        if let Some(cost_info) = cost_info {
            cost.storage_cost.removed_bytes += cost_info.combined_removed_bytes();
            cost.seek_count += 1;
        } else {
            let deleted_len = cost_return_on_error!(&mut cost, self.get_meta(&key))
                .map(|x| x.len() as u32)
                .unwrap_or(0);

            cost.storage_cost.removed_bytes += BasicStorageRemoval(deleted_len);
            cost.seek_count += 2;
        }

        self.storage
            .delete_cf(self.cf_meta(), make_prefixed_key(self.prefix.clone(), key))
            .map_err(RocksDBError)
            .wrap_with_cost(cost)
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
                    .and_then(Option::as_ref)
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
                    .and_then(Option::as_ref)
                    .map(|x| x.len() as u32)
                    .unwrap_or(0),
                ..Default::default()
            })
    }

    fn get_root<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.storage
            .get_cf(self.cf_roots(), make_prefixed_key(self.prefix.clone(), key))
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
            .get_cf(self.cf_meta(), make_prefixed_key(self.prefix.clone(), key))
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
        PrefixedRocksDbBatch {
            prefix: self.prefix.clone(),
            batch: WriteBatchWithTransaction::<true>::default(),
            cf_aux: self.cf_aux(),
            cf_roots: self.cf_roots(),
            cost_acc: Default::default(),
        }
    }

    fn commit_batch(&self, batch: Self::Batch) -> CostResult<(), Error> {
        let cost = OperationCost::default();

        // On unsuccessul batch commit only deletion finalization cost will be returned.
        cost_return_on_error_no_add!(&cost, self.storage.write(batch.batch).map_err(RocksDBError));

        Ok(()).wrap_with_cost(cost).add_cost(batch.cost_acc)
    }

    fn raw_iter(&self) -> Self::RawIterator {
        PrefixedRocksDbRawIterator {
            prefix: self.prefix.clone(),
            raw_iterator: self.storage.raw_iterator(),
        }
    }
}
