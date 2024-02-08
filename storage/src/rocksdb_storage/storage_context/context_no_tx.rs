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
use rocksdb::{ColumnFamily, OptimisticTransactionDB};

use super::{batch::PrefixedMultiContextBatchPart, make_prefixed_key, PrefixedRocksDbRawIterator};
use crate::{
    error,
    error::Error::RocksDBError,
    rocksdb_storage::storage::{
        NonTransactionalDb, SubtreePrefix, AUX_CF_NAME, META_CF_NAME, ROOTS_CF_NAME,
    },
    StorageBatch, StorageContext,
};

/// Storage context with a prefix applied to be used in a subtree to be used
/// outside of transaction.
pub struct PrefixedPrimaryRocksDbStorageContext<'db> {
    storage: &'db OptimisticTransactionDB,
    prefix: SubtreePrefix,
    batch: Option<&'db StorageBatch>,
}

// TODO: We can just use generic for storage instead of the second struct

/// Storage context with a prefix applied to be used in a subtree to be used
/// outside of transaction.
pub struct PrefixedSecondaryRocksDbStorageContext<'db> {
    pub(in crate::rocksdb_storage) storage: &'db NonTransactionalDb,
    pub(in crate::rocksdb_storage) prefix: SubtreePrefix,
    pub(in crate::rocksdb_storage) batch: Option<&'db StorageBatch>,
}

/// Storage context with a prefix applied to be used in a subtree to be used
/// outside of transaction.
pub enum PrefixedRocksDbStorageContext<'db> {
    /// Primary storage context
    Primary(PrefixedPrimaryRocksDbStorageContext<'db>),
    /// Secondary storage context
    Secondary(PrefixedSecondaryRocksDbStorageContext<'db>),
}

macro_rules! call_with_storage_prefix_and_batch {
    ($self:ident, $storage:ident, $prefix:ident, $batch:ident, $code:block) => {
        match $self {
            PrefixedRocksDbStorageContext::Primary(context) => {
                let $storage = context.storage;
                let $prefix = &context.prefix;
                let $batch = context.batch;
                $code
            }
            PrefixedRocksDbStorageContext::Secondary(context) => {
                let $storage = context.storage;
                let $prefix = &context.prefix;
                let $batch = context.batch;
                $code
            }
        }
    };
}

impl<'db> PrefixedRocksDbStorageContext<'db> {
    /// Create a new prefixed context instance
    pub fn new_primary(
        storage: &'db OptimisticTransactionDB,
        prefix: SubtreePrefix,
        batch: Option<&'db StorageBatch>,
    ) -> Self {
        let context = PrefixedPrimaryRocksDbStorageContext {
            storage,
            prefix,
            batch,
        };

        Self::Primary(context)
    }

    /// Create a new prefixed context instance
    pub fn new_secondary(
        storage: &'db NonTransactionalDb,
        prefix: SubtreePrefix,
        batch: Option<&'db StorageBatch>,
    ) -> Self {
        let context = PrefixedSecondaryRocksDbStorageContext {
            storage,
            prefix,
            batch,
        };

        Self::Secondary(context)
    }
}

impl<'db> PrefixedRocksDbStorageContext<'db> {
    /// Get auxiliary data column family
    fn cf_aux(&self) -> &'db ColumnFamily {
        call_with_storage_prefix_and_batch!(self, storage, _prefix, _batch, {
            storage.cf_handle(AUX_CF_NAME)
        })
        .expect("aux column family must exist")
    }

    /// Get trees roots data column family
    fn cf_roots(&self) -> &'db ColumnFamily {
        call_with_storage_prefix_and_batch!(self, storage, _prefix, _batch, {
            storage.cf_handle(ROOTS_CF_NAME)
        })
        .expect("roots column family must exist")
    }

    /// Get metadata column family
    fn cf_meta(&self) -> &'db ColumnFamily {
        call_with_storage_prefix_and_batch!(self, storage, _prefix, _batch, {
            storage.cf_handle(META_CF_NAME)
        })
        .expect("meta column family must exist")
    }
}

impl<'db> StorageContext<'db> for PrefixedRocksDbStorageContext<'db> {
    type Batch = PrefixedMultiContextBatchPart;
    type RawIterator = PrefixedRocksDbRawIterator<'db, OptimisticTransactionDB, NonTransactionalDb>;

    fn put<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        children_sizes: ChildrenSizesWithIsSumTree,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        call_with_storage_prefix_and_batch!(self, _storage, prefix, batch, {
            if let Some(existing_batch) = batch {
                existing_batch.put(
                    make_prefixed_key(prefix, key),
                    value.to_vec(),
                    children_sizes,
                    cost_info,
                );
            }
        });
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn put_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        call_with_storage_prefix_and_batch!(self, _storage, prefix, batch, {
            if let Some(existing_batch) = batch {
                existing_batch.put_aux(make_prefixed_key(prefix, key), value.to_vec(), cost_info);
            }
        });
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn put_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        call_with_storage_prefix_and_batch!(self, _storage, prefix, batch, {
            if let Some(existing_batch) = batch {
                existing_batch.put_root(make_prefixed_key(prefix, key), value.to_vec(), cost_info);
            }
        });
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn put_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        value: &[u8],
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        call_with_storage_prefix_and_batch!(self, _storage, prefix, batch, {
            if let Some(existing_batch) = batch {
                existing_batch.put_meta(make_prefixed_key(prefix, key), value.to_vec(), cost_info);
            }
        });

        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn delete<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        call_with_storage_prefix_and_batch!(self, _storage, prefix, batch, {
            if let Some(existing_batch) = batch {
                existing_batch.delete(make_prefixed_key(prefix, key), cost_info);
            }
        });

        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn delete_aux<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        call_with_storage_prefix_and_batch!(self, _storage, prefix, batch, {
            if let Some(existing_batch) = batch {
                existing_batch.delete_aux(make_prefixed_key(prefix, key), cost_info);
            }
        });

        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn delete_root<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        call_with_storage_prefix_and_batch!(self, _storage, prefix, batch, {
            if let Some(existing_batch) = batch {
                existing_batch.delete_root(make_prefixed_key(prefix, key), cost_info);
            }
        });

        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn delete_meta<K: AsRef<[u8]>>(
        &self,
        key: K,
        cost_info: Option<KeyValueStorageCost>,
    ) -> CostResult<(), Error> {
        call_with_storage_prefix_and_batch!(self, _storage, prefix, batch, {
            if let Some(existing_batch) = batch {
                existing_batch.delete_meta(make_prefixed_key(prefix, key), cost_info);
            }
        });

        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn get<K: AsRef<[u8]>>(&self, key: K) -> CostResult<Option<Vec<u8>>, Error> {
        call_with_storage_prefix_and_batch!(self, storage, prefix, _batch, {
            storage.get(make_prefixed_key(prefix, key))
        })
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
        call_with_storage_prefix_and_batch!(self, storage, prefix, _batch, {
            storage.get_cf(self.cf_aux(), make_prefixed_key(prefix, key))
        })
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
        call_with_storage_prefix_and_batch!(self, storage, prefix, _batch, {
            storage.get_cf(self.cf_roots(), make_prefixed_key(prefix, key))
        })
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
        call_with_storage_prefix_and_batch!(self, storage, prefix, _batch, {
            storage.get_cf(self.cf_meta(), make_prefixed_key(prefix, key))
        })
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
        call_with_storage_prefix_and_batch!(self, _storage, prefix, _batch, {
            PrefixedMultiContextBatchPart {
                prefix: *prefix,
                batch: StorageBatch::new(),
            }
        })
    }

    fn commit_batch(&self, batch: Self::Batch) -> CostResult<(), Error> {
        call_with_storage_prefix_and_batch!(self, _storage, _prefix, self_batch, {
            if let Some(existing_batch) = self_batch {
                existing_batch.merge(batch.batch);
            }
        });
        Ok(()).wrap_with_cost(OperationCost::default())
    }

    fn raw_iter(&self) -> Self::RawIterator {
        match self {
            PrefixedRocksDbStorageContext::Primary(context) => {
                Self::RawIterator::new_primary(context.prefix, context.storage.raw_iterator())
            }
            PrefixedRocksDbStorageContext::Secondary(context) => {
                Self::RawIterator::new_secondary(context.prefix, context.storage.raw_iterator())
            }
        }
    }
}
