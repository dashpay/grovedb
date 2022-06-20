//! Storage context implementation with a transaction.
use costs::{cost_return_on_error, CostContext, CostsExt, OperationCost};
use rocksdb::{ColumnFamily, DBRawIteratorWithThreadMode, Error};

use super::{batch::DummyBatch, make_prefixed_key, PrefixedRocksDbRawIterator};
use crate::{
    rocksdb_storage::storage::{Db, Tx, AUX_CF_NAME, META_CF_NAME, ROOTS_CF_NAME},
    BatchOperation, StorageContext,
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
    type Batch = DummyBatch;
    type Error = Error;
    type RawIterator = PrefixedRocksDbRawIterator<DBRawIteratorWithThreadMode<'db, Tx<'db>>>;

    fn put<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> CostContext<Result<(), Self::Error>> {
        self.transaction
            .put(make_prefixed_key(self.prefix.clone(), key), value)
            .wrap_with_cost(OperationCost {
                storage_written_bytes: key.as_ref().len() + value.len(),
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
                make_prefixed_key(self.prefix.clone(), key),
                value,
            )
            .wrap_with_cost(OperationCost {
                storage_written_bytes: key.as_ref().len() + value.len(),
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
                make_prefixed_key(self.prefix.clone(), key),
                value,
            )
            .wrap_with_cost(OperationCost {
                storage_written_bytes: key.as_ref().len() + value.len(),
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
                make_prefixed_key(self.prefix.clone(), key),
                value,
            )
            .wrap_with_cost(OperationCost {
                storage_written_bytes: key.as_ref().len() + value.len(),
                seek_count: 1,
                ..Default::default()
            })
    }

    fn delete<K: AsRef<[u8]>>(&self, key: K) -> CostContext<Result<(), Self::Error>> {
        let mut cost = OperationCost::default();

        let deleted_len = cost_return_on_error!(&mut cost, self.get(key))
            .map(|x| x.len())
            .unwrap_or(0);

        cost.storage_freed_bytes += deleted_len;
        cost.seek_count += 1;

        self.transaction
            .delete(make_prefixed_key(self.prefix.clone(), key))
            .wrap_with_cost(cost)
    }

    fn delete_aux<K: AsRef<[u8]>>(&self, key: K) -> CostContext<Result<(), Self::Error>> {
        let mut cost = OperationCost::default();

        let deleted_len = cost_return_on_error!(&mut cost, self.get_aux(key))
            .map(|x| x.len())
            .unwrap_or(0);

        cost.storage_freed_bytes += deleted_len;
        cost.seek_count += 1;

        self.transaction
            .delete_cf(self.cf_aux(), make_prefixed_key(self.prefix.clone(), key))
            .wrap_with_cost(cost)
    }

    fn delete_root<K: AsRef<[u8]>>(&self, key: K) -> CostContext<Result<(), Self::Error>> {
        let mut cost = OperationCost::default();

        let deleted_len = cost_return_on_error!(&mut cost, self.get_root(key))
            .map(|x| x.len())
            .unwrap_or(0);

        cost.storage_freed_bytes += deleted_len;
        cost.seek_count += 1;

        self.transaction
            .delete_cf(self.cf_roots(), make_prefixed_key(self.prefix.clone(), key))
            .wrap_with_cost(cost)
    }

    fn delete_meta<K: AsRef<[u8]>>(&self, key: K) -> CostContext<Result<(), Self::Error>> {
        let mut cost = OperationCost::default();

        let deleted_len = cost_return_on_error!(&mut cost, self.get_meta(key))
            .map(|x| x.len())
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
                storage_loaded_bytes: value.ok().flatten().map(|x| x.len()).unwrap_or(0),
                ..Default::default()
            })
    }

    fn get_aux<K: AsRef<[u8]>>(&self, key: K) -> CostContext<Result<Option<Vec<u8>>, Self::Error>> {
        self.transaction
            .get_cf(self.cf_aux(), make_prefixed_key(self.prefix.clone(), key))
            .wrap_fn_cost(|value| OperationCost {
                seek_count: 1,
                storage_loaded_bytes: value.ok().flatten().map(|x| x.len()).unwrap_or(0),
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
                storage_loaded_bytes: value.ok().flatten().map(|x| x.len()).unwrap_or(0),
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
                storage_loaded_bytes: value.ok().flatten().map(|x| x.len()).unwrap_or(0),
                ..Default::default()
            })
    }

    fn new_batch(&self) -> Self::Batch {
        DummyBatch::default()
    }

    fn commit_batch(&self, batch: Self::Batch) -> CostContext<Result<(), Self::Error>> {
        // TODO: this one alters the transaction, but should not on failure
        let mut cost = OperationCost::default();

        for op in batch.operations {
            match op {
                BatchOperation::Put { key, value } => {
                    cost_return_on_error!(&mut cost, self.put(key, &value));
                }
                BatchOperation::PutAux { key, value } => {
                    cost_return_on_error!(&mut cost, self.put_aux(key, &value));
                }
                BatchOperation::PutRoot { key, value } => {
                    cost_return_on_error!(&mut cost, self.put_root(key, &value));
                }
                BatchOperation::PutMeta { key, value } => {
                    cost_return_on_error!(&mut cost, self.put_meta(key, &value));
                }
                BatchOperation::Delete { key } => {
                    cost_return_on_error!(&mut cost, self.delete(key));
                }
                BatchOperation::DeleteAux { key } => {
                    cost_return_on_error!(&mut cost, self.delete_aux(key));
                }
                BatchOperation::DeleteRoot { key } => {
                    cost_return_on_error!(&mut cost, self.delete_root(key));
                }
                BatchOperation::DeleteMeta { key } => {
                    cost_return_on_error!(&mut cost, self.delete_meta(key));
                }
            }
        }
        Ok(()).wrap_with_cost(cost)
    }

    fn raw_iter(&self) -> CostContext<Self::RawIterator> {
        PrefixedRocksDbRawIterator {
            prefix: self.prefix.clone(),
            raw_iterator: self.transaction.raw_iterator(),
        }.wrap_with_cost(Default::default())
    }
}
