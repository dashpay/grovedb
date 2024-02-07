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

//! Storage context implementation with a transaction.

use error::Error;
use grovedb_costs::{
    storage_cost::key_value_cost::KeyValueStorageCost, ChildrenSizesWithIsSumTree, CostResult,
    CostsExt, OperationCost,
};
use rocksdb::{ColumnFamily, DBRawIteratorWithThreadMode, WriteBatchWithTransaction};

use crate::rocksdb_storage::storage_context::make_prefixed_key;
use crate::rocksdb_storage::PrefixedRocksDbBatch;
use crate::secondary_rocksdb_storage::storage::Db;
use crate::secondary_rocksdb_storage::storage_context::raw_iterator::PrefixedSecondaryRocksDbRawIterator;
use crate::{
    error,
    error::Error::RocksDBError,
    rocksdb_storage::storage::{SubtreePrefix, Tx, AUX_CF_NAME, META_CF_NAME, ROOTS_CF_NAME},
    StorageContext,
};

/// Storage context with a prefix applied to be used in a subtree to be used in
/// transaction.
pub struct PrefixedSecondaryRocksDbImmediateStorageContext<'db> {
    storage: &'db Db,
    prefix: SubtreePrefix,
}

impl<'db> PrefixedSecondaryRocksDbImmediateStorageContext<'db> {
    /// Create a new prefixed transaction context instance
    pub fn new(storage: &'db Db, prefix: SubtreePrefix) -> Self {
        Self { storage, prefix }
    }
}

impl<'db> PrefixedSecondaryRocksDbImmediateStorageContext<'db> {
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

impl<'db> StorageContext<'db> for PrefixedSecondaryRocksDbImmediateStorageContext<'db> {
    type Batch = PrefixedRocksDbBatch<'db>;
    // type RawIterator = PrefixedRocksDbRawIterator<DBRawIteratorWithThreadMode<'db, Tx<'db>>>;
    type RawIterator = PrefixedSecondaryRocksDbRawIterator<DBRawIteratorWithThreadMode<'db, Db>>;

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        _children_sizes: ChildrenSizesWithIsSumTree,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.storage
            .put(make_prefixed_key(&self.prefix, &key), value)
            .map_err(RocksDBError)
            .wrap_with_cost(Default::default())
    }

    fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.storage
            .put_cf(self.cf_aux(), make_prefixed_key(&self.prefix, &key), value)
            .map_err(RocksDBError)
            .wrap_with_cost(Default::default())
    }

    fn put_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.storage
            .put_cf(
                self.cf_roots(),
                make_prefixed_key(&self.prefix, &key),
                value,
            )
            .map_err(RocksDBError)
            .wrap_with_cost(Default::default())
    }

    fn put_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.storage
            .put_cf(self.cf_meta(), make_prefixed_key(&self.prefix, &key), value)
            .map_err(RocksDBError)
            .wrap_with_cost(Default::default())
    }

    fn delete<K: AsRef<[u8]>>(
        &self,
        key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.storage
            .delete(make_prefixed_key(&self.prefix, key))
            .map_err(RocksDBError)
            .wrap_with_cost(Default::default())
    }

    fn delete_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.storage
            .delete_cf(self.cf_aux(), make_prefixed_key(&self.prefix, key))
            .map_err(RocksDBError)
            .wrap_with_cost(Default::default())
    }

    fn delete_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.storage
            .delete_cf(self.cf_roots(), make_prefixed_key(&self.prefix, key))
            .map_err(RocksDBError)
            .wrap_with_cost(Default::default())
    }

    fn delete_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        self.storage
            .delete_cf(self.cf_meta(), make_prefixed_key(&self.prefix, key))
            .map_err(RocksDBError)
            .wrap_with_cost(Default::default())
    }

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.storage
            .get(make_prefixed_key(&self.prefix, key))
            .map_err(RocksDBError)
            .wrap_with_cost(Default::default())
    }

    fn get_aux<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.storage
            .get_cf(self.cf_aux(), make_prefixed_key(&self.prefix, key))
            .map_err(RocksDBError)
            .wrap_with_cost(Default::default())
    }

    fn get_root<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.storage
            .get_cf(self.cf_roots(), make_prefixed_key(&self.prefix, key))
            .map_err(RocksDBError)
            .wrap_with_cost(Default::default())
    }

    fn get_meta<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        self.storage
            .get_cf(self.cf_meta(), make_prefixed_key(&self.prefix, key))
            .map_err(RocksDBError)
            .wrap_with_cost(Default::default())
    }

    fn new_batch(&self) -> Self::Batch {
        PrefixedRocksDbBatch {
            prefix: self.prefix,
            batch: WriteBatchWithTransaction::<true>::default(),
            cf_aux: self.cf_aux(),
            cf_roots: self.cf_roots(),
            cost_acc: Default::default(),
        }
    }

    fn commit_batch(&self, _batch: Self::Batch) -> CostResult<(), Error> {
        // TODO: Implement
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn raw_iter(&self) -> Self::RawIterator {
        PrefixedSecondaryRocksDbRawIterator {
            prefix: self.prefix,
            raw_iterator: self.storage.raw_iterator(),
        }
    }
}
