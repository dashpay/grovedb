//! Impementation for a storage abstraction over RocksDB.
use std::path::Path;

use lazy_static::lazy_static;
use rocksdb::{
    ColumnFamily, ColumnFamilyDescriptor, Error, OptimisticTransactionDB, Transaction,
    WriteBatchWithTransaction,
};

use super::{
    PrefixedRocksDbBatchStorageContext, PrefixedRocksDbBatchTransactionContext,
    PrefixedRocksDbStorageContext, PrefixedRocksDbTransactionContext,
};
use crate::{BatchOperation, Storage, StorageBatch};

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

    /// A helper method to build a prefix to rocksdb keys or identify a subtree
    /// in `subtrees` map by tree path;
    pub fn build_prefix<'a, P>(path: P) -> Vec<u8>
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
        res = blake3::hash(&res).as_bytes().to_vec();
        res
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

    fn commit_transaction(&self, transaction: Self::Transaction) -> Result<(), Self::Error> {
        transaction.commit()
    }

    fn rollback_transaction(&self, transaction: &Self::Transaction) -> Result<(), Self::Error> {
        transaction.rollback()
    }

    fn flush(&self) -> Result<(), Self::Error> {
        self.db.flush()
    }

    fn get_storage_context<'p, P>(&'db self, path: P) -> Self::StorageContext
    where
        P: IntoIterator<Item = &'p [u8]>,
    {
        let prefix = Self::build_prefix(path);
        PrefixedRocksDbStorageContext::new(&self.db, prefix)
    }

    fn get_transactional_storage_context<'p, P>(
        &'db self,
        path: P,
        transaction: &'db Self::Transaction,
    ) -> Self::TransactionalStorageContext
    where
        P: IntoIterator<Item = &'p [u8]>,
    {
        let prefix = Self::build_prefix(path);
        PrefixedRocksDbTransactionContext::new(&self.db, transaction, prefix)
    }

    fn get_batch_storage_context<'p, P>(
        &'db self,
        path: P,
        batch: &'db StorageBatch,
    ) -> Self::BatchStorageContext
    where
        P: IntoIterator<Item = &'p [u8]>,
    {
        let prefix = Self::build_prefix(path);
        PrefixedRocksDbBatchStorageContext::new(&self.db, prefix, batch)
    }

    fn get_batch_transactional_storage_context<'p, P>(
        &'db self,
        path: P,
        batch: &'db StorageBatch,
        transaction: &'db Self::Transaction,
    ) -> Self::BatchTransactionalStorageContext
    where
        P: IntoIterator<Item = &'p [u8]>,
    {
        let prefix = Self::build_prefix(path);
        PrefixedRocksDbBatchTransactionContext::new(&self.db, transaction, prefix, batch)
    }

    fn commit_multi_context_batch(&self, batch: StorageBatch) -> Result<(), Self::Error> {
        let mut db_batch = WriteBatchWithTransaction::<true>::default();
        for op in batch.into_iter() {
            match op {
                BatchOperation::Put { key, value } => {
                    db_batch.put(key, value);
                }
                BatchOperation::PutAux { key, value } => {
                    db_batch.put_cf(cf_aux(&self.db), key, value);
                }
                BatchOperation::PutRoot { key, value } => {
                    db_batch.put_cf(cf_roots(&self.db), key, value);
                }
                BatchOperation::PutMeta { key, value } => {
                    db_batch.put_cf(cf_meta(&self.db), key, value);
                }
                BatchOperation::Delete { key } => {
                    db_batch.delete(key);
                }
                BatchOperation::DeleteAux { key } => {
                    db_batch.delete_cf(cf_aux(&self.db), key);
                }
                BatchOperation::DeleteRoot { key } => {
                    db_batch.delete_cf(cf_roots(&self.db), key);
                }
                BatchOperation::DeleteMeta { key } => {
                    db_batch.delete_cf(cf_meta(&self.db), key);
                }
            }
        }
        self.db.write(db_batch)?;
        Ok(())
    }

    fn commit_multi_context_batch_with_transaction(
        &self,
        batch: StorageBatch,
        transaction: &'db Self::Transaction,
    ) -> Result<(), Self::Error> {
        transaction.set_savepoint();
        let batch_result: Result<(), Self::Error> = batch.into_iter().try_for_each(|op| match op {
            BatchOperation::Put { key, value } => transaction.put(key, value),
            BatchOperation::PutAux { key, value } => {
                transaction.put_cf(cf_aux(&self.db), key, value)
            }
            BatchOperation::PutRoot { key, value } => {
                transaction.put_cf(cf_roots(&self.db), key, value)
            }
            BatchOperation::PutMeta { key, value } => {
                transaction.put_cf(cf_meta(&self.db), key, value)
            }
            BatchOperation::Delete { key } => transaction.delete(key),
            BatchOperation::DeleteAux { key } => transaction.delete_cf(cf_aux(&self.db), key),
            BatchOperation::DeleteRoot { key } => transaction.delete_cf(cf_roots(&self.db), key),
            BatchOperation::DeleteMeta { key } => transaction.delete_cf(cf_meta(&self.db), key),
        });
        if batch_result.is_err() {
            transaction.rollback_to_savepoint()?;
        }
        batch_result
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
