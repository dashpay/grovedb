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
    CostsExt,
};
use rocksdb::{ColumnFamily, OptimisticTransactionDB, WriteBatchWithTransaction};

use super::{make_prefixed_key, PrefixedRocksDbBatch, PrefixedRocksDbRawIterator};
use crate::{
    error,
    error::Error::RocksDBError,
    rocksdb_storage::storage::{
        NonTransactionalDb, SubtreePrefix, Tx, AUX_CF_NAME, META_CF_NAME, ROOTS_CF_NAME,
    },
    StorageContext,
};

/// Primary and secondary storage context with a prefix applied to be used in a
/// subtree to be used in transaction.
pub enum PrefixedRocksDbImmediateStorageContext<'db> {
    /// Primary storage context
    Primary(PrefixedPrimaryRocksDbImmediateStorageContext<'db>),
    /// Secondary storage context
    Secondary(PrefixedSecondaryRocksDbImmediateStorageContext<'db>),
}

/// Transactional storage context with a prefix applied to be used in a
/// subtree to be used in transaction.
pub struct PrefixedPrimaryRocksDbImmediateStorageContext<'db> {
    storage: &'db OptimisticTransactionDB,
    transaction: &'db Tx<'db>,
    prefix: SubtreePrefix,
}

/// Non-transactional storage context with a prefix applied to be used in a
/// subtree to be used in transaction.
pub struct PrefixedSecondaryRocksDbImmediateStorageContext<'db> {
    storage: &'db NonTransactionalDb,
    prefix: SubtreePrefix,
}

macro_rules! call_with_storage_and_prefix {
    ($self:ident, $storage:ident, $prefix:ident, $code:block) => {
        match $self {
            PrefixedRocksDbImmediateStorageContext::Primary(context) => {
                let $storage = context.storage;
                let $prefix = &context.prefix;
                $code
            }
            PrefixedRocksDbImmediateStorageContext::Secondary(context) => {
                let $storage = context.storage;
                let $prefix = &context.prefix;
                $code
            }
        }
    };
}

macro_rules! call_with_storage_or_transaction_and_prefix {
    ($self:ident, $storage:ident, $prefix:ident, $code:block) => {
        match $self {
            PrefixedRocksDbImmediateStorageContext::Primary(context) => {
                let $storage = context.transaction;
                let $prefix = &context.prefix;
                $code
            }
            PrefixedRocksDbImmediateStorageContext::Secondary(context) => {
                let $storage = context.storage;
                let $prefix = &context.prefix;
                $code
            }
        }
    };
}

impl<'db> PrefixedRocksDbImmediateStorageContext<'db> {
    /// Create a new prefixed transaction context instance
    pub fn new_primary(
        storage: &'db OptimisticTransactionDB,
        transaction: &'db Tx<'db>,
        prefix: SubtreePrefix,
    ) -> Self {
        let context = PrefixedPrimaryRocksDbImmediateStorageContext {
            storage,
            transaction,
            prefix,
        };

        Self::Primary(context)
    }

    /// Create a new prefixed context instance
    pub fn new_secondary(storage: &'db NonTransactionalDb, prefix: SubtreePrefix) -> Self {
        let context = PrefixedSecondaryRocksDbImmediateStorageContext { storage, prefix };

        Self::Secondary(context)
    }
}

impl<'db> PrefixedRocksDbImmediateStorageContext<'db> {
    /// Get auxiliary data column family
    fn cf_aux(&self) -> &'db ColumnFamily {
        call_with_storage_and_prefix!(self, storage, _prefix, { storage.cf_handle(AUX_CF_NAME) })
            .expect("aux column family must exist")
    }

    /// Get trees roots data column family
    fn cf_roots(&self) -> &'db ColumnFamily {
        call_with_storage_and_prefix!(self, storage, _prefix, { storage.cf_handle(ROOTS_CF_NAME) })
            .expect("roots column family must exist")
    }

    /// Get metadata column family
    fn cf_meta(&self) -> &'db ColumnFamily {
        call_with_storage_and_prefix!(self, storage, _prefix, { storage.cf_handle(META_CF_NAME) })
            .expect("meta column family must exist")
    }
}

impl<'db> StorageContext<'db> for PrefixedRocksDbImmediateStorageContext<'db> {
    type Batch = PrefixedRocksDbBatch<'db>;
    type RawIterator = PrefixedRocksDbRawIterator<'db, Tx<'db>, NonTransactionalDb>;

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        _children_sizes: ChildrenSizesWithIsSumTree,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        call_with_storage_or_transaction_and_prefix!(self, storage, prefix, {
            storage.put(make_prefixed_key(prefix, &key), value)
        })
        .map_err(RocksDBError)
        .wrap_with_cost(Default::default())
    }

    fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        call_with_storage_or_transaction_and_prefix!(self, storage, prefix, {
            storage.put_cf(self.cf_aux(), make_prefixed_key(prefix, &key), value)
        })
        .map_err(RocksDBError)
        .wrap_with_cost(Default::default())
    }

    fn put_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        call_with_storage_or_transaction_and_prefix!(self, storage, prefix, {
            storage.put_cf(self.cf_roots(), make_prefixed_key(prefix, &key), value)
        })
        .map_err(RocksDBError)
        .wrap_with_cost(Default::default())
    }

    fn put_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        call_with_storage_or_transaction_and_prefix!(self, storage, prefix, {
            storage.put_cf(self.cf_meta(), make_prefixed_key(prefix, &key), value)
        })
        .map_err(RocksDBError)
        .wrap_with_cost(Default::default())
    }

    fn delete<K: AsRef<[u8]>>(
        &self,
        key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        call_with_storage_or_transaction_and_prefix!(self, storage, prefix, {
            storage.delete(make_prefixed_key(prefix, key))
        })
        .map_err(RocksDBError)
        .wrap_with_cost(Default::default())
    }

    fn delete_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        call_with_storage_or_transaction_and_prefix!(self, storage, prefix, {
            storage.delete_cf(self.cf_aux(), make_prefixed_key(prefix, key))
        })
        .map_err(RocksDBError)
        .wrap_with_cost(Default::default())
    }

    fn delete_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        call_with_storage_or_transaction_and_prefix!(self, storage, prefix, {
            storage.delete_cf(self.cf_roots(), make_prefixed_key(prefix, key))
        })
        .map_err(RocksDBError)
        .wrap_with_cost(Default::default())
    }

    fn delete_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        _cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        call_with_storage_or_transaction_and_prefix!(self, storage, prefix, {
            storage.delete_cf(self.cf_meta(), make_prefixed_key(prefix, key))
        })
        .map_err(RocksDBError)
        .wrap_with_cost(Default::default())
    }

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        call_with_storage_or_transaction_and_prefix!(self, storage, prefix, {
            storage.get(make_prefixed_key(prefix, key))
        })
        .map_err(RocksDBError)
        .wrap_with_cost(Default::default())
    }

    fn get_aux<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        call_with_storage_or_transaction_and_prefix!(self, storage, prefix, {
            storage.get_cf(self.cf_aux(), make_prefixed_key(prefix, key))
        })
        .map_err(RocksDBError)
        .wrap_with_cost(Default::default())
    }

    fn get_root<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        call_with_storage_or_transaction_and_prefix!(self, storage, prefix, {
            storage.get_cf(self.cf_roots(), make_prefixed_key(prefix, key))
        })
        .map_err(RocksDBError)
        .wrap_with_cost(Default::default())
    }

    fn get_meta<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        call_with_storage_or_transaction_and_prefix!(self, storage, prefix, {
            storage.get_cf(self.cf_meta(), make_prefixed_key(prefix, key))
        })
        .map_err(RocksDBError)
        .wrap_with_cost(Default::default())
    }

    fn new_batch(&self) -> Self::Batch {
        call_with_storage_and_prefix!(self, _storage, prefix, {
            PrefixedRocksDbBatch {
                prefix: *prefix,
                batch: WriteBatchWithTransaction::<true>::default(),
                cf_aux: self.cf_aux(),
                cf_roots: self.cf_roots(),
                cost_acc: Default::default(),
            }
        })
    }

    fn commit_batch(&self, batch: Self::Batch) -> CostResult<(), Error> {
        match self {
            PrefixedRocksDbImmediateStorageContext::Primary(db) => db
                .transaction
                .rebuild_from_writebatch(&batch.batch)
                .map_err(RocksDBError)
                .wrap_with_cost(Default::default()),
            PrefixedRocksDbImmediateStorageContext::Secondary(_) => {
                unimplemented!("commit_batch is not supported for secondary storage")
            }
        }
    }

    fn raw_iter(&self) -> Self::RawIterator {
        match self {
            PrefixedRocksDbImmediateStorageContext::Primary(context) => {
                Self::RawIterator::new_primary(context.prefix, context.transaction.raw_iterator())
            }
            PrefixedRocksDbImmediateStorageContext::Secondary(context) => {
                Self::RawIterator::new_secondary(context.prefix, context.storage.raw_iterator())
            }
        }
    }
}
