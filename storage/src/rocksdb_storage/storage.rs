//! Impementation for a storage abstraction over RocksDB.
use std::path::Path;

use costs::{cost_return_on_error_no_add, CostContext, CostResult, CostsExt, OperationCost};
use lazy_static::lazy_static;
use rocksdb::{
    ColumnFamily, ColumnFamilyDescriptor, Error, OptimisticTransactionDB, Transaction,
    WriteBatchWithTransaction,
};

use super::{
    PrefixedRocksDbBatchStorageContext, PrefixedRocksDbBatchTransactionContext,
    PrefixedRocksDbStorageContext, PrefixedRocksDbTransactionContext,
};
use crate::{AbstractBatchOperation, Storage, StorageBatch};

/// Name of column family used to store auxiliary data
pub(crate) const AUX_CF_NAME: &str = "aux";
/// Name of column family used to store subtrees roots data
pub(crate) const ROOTS_CF_NAME: &str = "roots";
/// Name of column family used to store metadata
pub(crate) const META_CF_NAME: &str = "meta";

lazy_static! {
    static ref DEFAULT_OPTS: rocksdb::Options = {
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(true);
        opts.increase_parallelism(num_cpus::get() as i32);
        opts.set_allow_mmap_writes(true);
        opts.set_allow_mmap_reads(true);
        opts.create_missing_column_families(true);
        opts.set_atomic_flush(true);
        opts
    };
}

/// Type alias for a database
pub(crate) type Db = OptimisticTransactionDB;

/// Type alias for a transaction
pub(crate) type Tx<'db> = Transaction<'db, Db>;

/// Storage which uses RocksDB as its backend.
pub struct RocksDbStorage {
    db: OptimisticTransactionDB,
}

impl RocksDbStorage {
    /// Create RocksDb storage with default parameters using `path`.
    pub fn default_rocksdb_with_path<P: AsRef<Path>>(path: P) -> Result<Self, Error> {
        let db = Db::open_cf_descriptors(
            &DEFAULT_OPTS,
            &path,
            [
                ColumnFamilyDescriptor::new(AUX_CF_NAME, DEFAULT_OPTS.clone()),
                ColumnFamilyDescriptor::new(ROOTS_CF_NAME, DEFAULT_OPTS.clone()),
                ColumnFamilyDescriptor::new(META_CF_NAME, DEFAULT_OPTS.clone()),
            ],
        )?;

        Ok(RocksDbStorage { db })
    }

    fn build_prefix_body<'a, P>(path: P) -> CostContext<Vec<u8>>
    where
        P: IntoIterator<Item = &'a [u8]>,
    {
        let segments_iter = path.into_iter();
        let mut segments_count: usize = 0;
        let mut res = Vec::new();
        let mut lengthes = Vec::new();

        for s in segments_iter {
            segments_count += 1;
            res.extend_from_slice(s);
            lengthes.extend(s.len().to_ne_bytes());
        }

        res.extend(segments_count.to_ne_bytes());
        res.extend(lengthes);
        res.wrap_with_cost(OperationCost::default()) // TODO: actual cost
    }

    /// A helper method to figure out how many blake3 hashes are needed to build
    /// a prefix
    pub fn build_prefix_hash_count<'a, P>(path: P) -> u16
    where
        P: IntoIterator<Item = &'a [u8]>,
    {
        let body = Self::build_prefix_body(path);
        // the block size of blake3 is 64
        (body.len() as u32 / 64 + 1) as u16
    }

    /// A helper method to build a prefix to rocksdb keys or identify a subtree
    /// in `subtrees` map by tree path;
    pub fn build_prefix<'a, P>(path: P) -> Vec<u8>
    where
        P: IntoIterator<Item = &'a [u8]>,
    {
        let body = Self::build_prefix_body(path);
        blake3::hash(&body).as_bytes().to_vec()
    }
}

impl<'db> Storage<'db> for RocksDbStorage {
    type BatchStorageContext = PrefixedRocksDbBatchStorageContext<'db>;
    type BatchTransactionalStorageContext = PrefixedRocksDbBatchTransactionContext<'db>;
    type Error = Error;
    type StorageContext = PrefixedRocksDbStorageContext<'db>;
    type Transaction = Tx<'db>;
    type TransactionalStorageContext = PrefixedRocksDbTransactionContext<'db>;

    fn start_transaction(&'db self) -> Self::Transaction {
        self.db.transaction()
    }

    fn commit_transaction(
        &self,
        transaction: Self::Transaction,
    ) -> CostContext<Result<(), Self::Error>> {
        // All transaction costs were provided on method calls
        transaction.commit().wrap_with_cost(Default::default())
    }

    fn rollback_transaction(&self, transaction: &Self::Transaction) -> Result<(), Self::Error> {
        transaction.rollback()
    }

    fn flush(&self) -> Result<(), Self::Error> {
        self.db.flush()
    }

    fn get_storage_context<'p, P>(&'db self, path: P) -> CostContext<Self::StorageContext>
    where
        P: IntoIterator<Item = &'p [u8]>,
    {
        Self::build_prefix(path).map(|prefix| PrefixedRocksDbStorageContext::new(&self.db, prefix))
    }

    fn get_transactional_storage_context<'p, P>(
        &'db self,
        path: P,
        transaction: &'db Self::Transaction,
    ) -> CostContext<Self::TransactionalStorageContext>
    where
        P: IntoIterator<Item = &'p [u8]>,
    {
        Self::build_prefix(path)
            .map(|prefix| PrefixedRocksDbTransactionContext::new(&self.db, transaction, prefix))
    }

    fn get_batch_storage_context<'p, P>(
        &'db self,
        path: P,
        batch: &'db StorageBatch,
    ) -> CostContext<Self::BatchStorageContext>
    where
        P: IntoIterator<Item = &'p [u8]>,
    {
        Self::build_prefix(path)
            .map(|prefix| PrefixedRocksDbBatchStorageContext::new(&self.db, prefix, batch))
    }

    fn get_batch_transactional_storage_context<'p, P>(
        &'db self,
        path: P,
        batch: &'db StorageBatch,
        transaction: &'db Self::Transaction,
    ) -> CostContext<Self::BatchTransactionalStorageContext>
    where
        P: IntoIterator<Item = &'p [u8]>,
    {
        Self::build_prefix(path).map(|prefix| {
            PrefixedRocksDbBatchTransactionContext::new(&self.db, transaction, prefix, batch)
        })
    }

    fn commit_multi_context_batch(
        &self,
        batch: StorageBatch,
        transaction: Option<&'db Self::Transaction>,
    ) -> CostResult<(), Self::Error> {
        let mut db_batch = WriteBatchWithTransaction::<true>::default();

        let mut cost = OperationCost::default();
        // Until batch is commited these costs are pending (should not be added in case
        // of early termination).
        let mut pending_storage_written_bytes = 0;
        let mut pending_storage_freed_bytes = 0;

        for op in batch.into_iter() {
            match op {
                AbstractBatchOperation::Put { key, value } => {
                    db_batch.put(&key, &value);
                    pending_storage_written_bytes += key.len() + value.len();
                }
                AbstractBatchOperation::PutAux { key, value } => {
                    db_batch.put_cf(cf_aux(&self.db), &key, &value);
                    pending_storage_written_bytes += key.len() + value.len();
                }
                AbstractBatchOperation::PutRoot { key, value } => {
                    db_batch.put_cf(cf_roots(&self.db), &key, &value);
                    pending_storage_written_bytes += key.len() + value.len();
                }
                AbstractBatchOperation::PutMeta { key, value } => {
                    db_batch.put_cf(cf_meta(&self.db), &key, &value);
                    pending_storage_written_bytes += key.len() + value.len();
                }
                AbstractBatchOperation::Delete { key } => {
                    db_batch.delete(&key);

                    // TODO: fix not atomic freed size computation
                    cost.seek_count += 1;
                    let value_len = cost_return_on_error_no_add!(&cost, self.db.get(&key))
                        .map(|x| x.len())
                        .unwrap_or(0);
                    cost.storage_loaded_bytes += value_len;

                    pending_storage_freed_bytes += key.len() + value_len;
                }
                AbstractBatchOperation::DeleteAux { key } => {
                    db_batch.delete_cf(cf_aux(&self.db), &key);

                    // TODO: fix not atomic freed size computation
                    cost.seek_count += 1;
                    let value_len =
                        cost_return_on_error_no_add!(&cost, self.db.get_cf(cf_aux(&self.db), &key))
                            .map(|x| x.len())
                            .unwrap_or(0);
                    cost.storage_loaded_bytes += value_len;

                    pending_storage_freed_bytes += key.len() + value_len;
                }
                AbstractBatchOperation::DeleteRoot { key } => {
                    db_batch.delete_cf(cf_roots(&self.db), &key);

                    // TODO: fix not atomic freed size computation
                    cost.seek_count += 1;
                    let value_len = cost_return_on_error_no_add!(
                        &cost,
                        self.db.get_cf(cf_roots(&self.db), &key)
                    )
                    .map(|x| x.len())
                    .unwrap_or(0);
                    cost.storage_loaded_bytes += value_len;

                    pending_storage_freed_bytes += key.len() + value_len;
                }
                AbstractBatchOperation::DeleteMeta { key } => {
                    db_batch.delete_cf(cf_meta(&self.db), &key);

                    // TODO: fix not atomic freed size computation
                    cost.seek_count += 1;
                    let value_len = cost_return_on_error_no_add!(
                        &cost,
                        self.db.get_cf(cf_meta(&self.db), &key)
                    )
                    .map(|x| x.len())
                    .unwrap_or(0);
                    cost.storage_loaded_bytes += value_len;

                    pending_storage_freed_bytes += key.len() + value_len;
                }
            }
        }

        match transaction {
            None => self.db.write(db_batch),
            Some(transaction) => transaction.rebuild_from_writebatch(&db_batch),
        }
    }
}

/// Get auxiliary data column family
fn cf_aux(storage: &Db) -> &ColumnFamily {
    storage
        .cf_handle(AUX_CF_NAME)
        .expect("aux column family must exist")
}

/// Get trees roots data column family
fn cf_roots(storage: &Db) -> &ColumnFamily {
    storage
        .cf_handle(ROOTS_CF_NAME)
        .expect("roots column family must exist")
}

/// Get metadata column family
fn cf_meta(storage: &Db) -> &ColumnFamily {
    storage
        .cf_handle(META_CF_NAME)
        .expect("meta column family must exist")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_prefix() {
        let path_a = [b"aa".as_ref(), b"b"];
        let path_b = [b"a".as_ref(), b"ab"];
        assert_ne!(
            RocksDbStorage::build_prefix(path_a),
            RocksDbStorage::build_prefix(path_b),
        );
        assert_eq!(
            RocksDbStorage::build_prefix(path_a),
            RocksDbStorage::build_prefix(path_a),
        );
    }
}
