use std::rc::Rc;

use rocksdb::WriteBatchWithTransaction;

use super::{
    make_prefixed_key, PrefixedRocksDbBatch, PrefixedRocksDbTransaction, RawIteratorVariant,
    RawPrefixedTransactionalIterator, AUX_CF_NAME, META_CF_NAME, ROOTS_CF_NAME,
};
use crate::{
    rocksdb_storage::{
        batch::{OrBatch, PrefixedTransactionalRocksDbBatch},
        OptimisticTransactionDBTransaction,
    },
    Storage,
};

/// RocksDB wrapper to store items with prefixes
#[derive(Clone)]
pub struct PrefixedRocksDbStorage {
    pub(crate) db: Rc<rocksdb::OptimisticTransactionDB>,
    prefix: Vec<u8>,
}

#[derive(thiserror::Error, Debug)]
pub enum PrefixedRocksDbStorageError {
    #[error("column family not found: {0}")]
    ColumnFamilyNotFound(&'static str),
    #[error(transparent)]
    RocksDbError(#[from] rocksdb::Error),
}

impl PrefixedRocksDbStorage {
    /// Wraps RocksDB to prepend prefixes to each operation
    pub fn new(
        db: Rc<rocksdb::OptimisticTransactionDB>,
        prefix: Vec<u8>,
    ) -> Result<Self, PrefixedRocksDbStorageError> {
        Ok(PrefixedRocksDbStorage { prefix, db })
    }

    /// Get auxiliary data column family
    fn cf_aux(&self) -> Result<&rocksdb::ColumnFamily, PrefixedRocksDbStorageError> {
        self.db
            .cf_handle(AUX_CF_NAME)
            .ok_or(PrefixedRocksDbStorageError::ColumnFamilyNotFound(
                AUX_CF_NAME,
            ))
    }

    /// Get trees roots data column family
    fn cf_roots(&self) -> Result<&rocksdb::ColumnFamily, PrefixedRocksDbStorageError> {
        self.db
            .cf_handle(ROOTS_CF_NAME)
            .ok_or(PrefixedRocksDbStorageError::ColumnFamilyNotFound(
                ROOTS_CF_NAME,
            ))
    }

    /// Get metadata column family
    fn cf_meta(&self) -> Result<&rocksdb::ColumnFamily, PrefixedRocksDbStorageError> {
        self.db
            .cf_handle(META_CF_NAME)
            .ok_or(PrefixedRocksDbStorageError::ColumnFamilyNotFound(
                META_CF_NAME,
            ))
    }
}

impl Storage for PrefixedRocksDbStorage {
    type Batch<'a> = OrBatch<'a>;
    type DBTransaction<'a> = OptimisticTransactionDBTransaction<'a>;
    type Error = PrefixedRocksDbStorageError;
    type RawIterator<'a> = RawPrefixedTransactionalIterator<'a>;
    type StorageTransaction<'a> = PrefixedRocksDbTransaction<'a>;

    fn put<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        self.db
            .put(make_prefixed_key(self.prefix.clone(), key), value)?;
        Ok(())
    }

    fn put_aux<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        self.db.put_cf(
            self.cf_aux()?,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )?;
        Ok(())
    }

    fn put_root<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        self.db.put_cf(
            self.cf_roots()?,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )?;
        Ok(())
    }

    fn delete<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
        self.db
            .delete(make_prefixed_key(self.prefix.clone(), key))?;
        Ok(())
    }

    fn delete_aux<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
        self.db
            .delete_cf(self.cf_aux()?, make_prefixed_key(self.prefix.clone(), key))?;
        Ok(())
    }

    fn delete_root<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
        self.db.delete_cf(
            self.cf_roots()?,
            make_prefixed_key(self.prefix.clone(), key),
        )?;
        Ok(())
    }

    fn get<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.db.get(make_prefixed_key(self.prefix.clone(), key))?)
    }

    fn get_aux<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self
            .db
            .get_cf(self.cf_aux()?, make_prefixed_key(self.prefix.clone(), key))?)
    }

    fn get_root<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.db.get_cf(
            self.cf_roots()?,
            make_prefixed_key(self.prefix.clone(), key),
        )?)
    }

    fn put_meta<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        Ok(self.db.put_cf(self.cf_meta()?, key, value)?)
    }

    fn delete_meta<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
        Ok(self.db.delete_cf(self.cf_meta()?, key)?)
    }

    fn get_meta<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.db.get_cf(self.cf_meta()?, key)?)
    }

    fn new_batch<'a: 'b, 'b>(
        &'a self,
        transaction: Option<&'b OptimisticTransactionDBTransaction>,
    ) -> Result<Self::Batch<'b>, Self::Error> {
        match transaction {
            Some(tx) => Ok(OrBatch::TransactionalBatch(
                PrefixedTransactionalRocksDbBatch {
                    prefix: self.prefix.clone(),
                    transaction: tx,
                    cf_aux: self.cf_aux()?,
                    cf_roots: self.cf_roots()?,
                },
            )),
            None => Ok(OrBatch::Batch(PrefixedRocksDbBatch {
                prefix: self.prefix.clone(),
                batch: WriteBatchWithTransaction::<true>::default(),
                cf_aux: self.cf_aux()?,
                cf_roots: self.cf_roots()?,
            })),
        }
    }

    fn commit_batch<'a>(&'a self, batch: Self::Batch<'a>) -> Result<(), Self::Error> {
        // Do nothing if transaction exists, as the transaction must be explicitly
        // committed by its creator
        match batch {
            OrBatch::TransactionalBatch(_) => {}
            OrBatch::Batch(batch) => self.db.write(batch.batch)?,
        }
        Ok(())
    }

    fn flush(&self) -> Result<(), Self::Error> {
        self.db.flush()?;
        Ok(())
    }

    fn raw_iter<'a>(
        &'a self,
        db_transaction: Option<&'a OptimisticTransactionDBTransaction>,
    ) -> Self::RawIterator<'a> {
        let rocksdb_iterator = db_transaction
            .map(|tx| RawIteratorVariant::TransactionIterator(tx.raw_iterator()))
            .unwrap_or_else(|| RawIteratorVariant::StorageIterator(self.db.raw_iterator()));
        RawPrefixedTransactionalIterator {
            rocksdb_iterator,
            prefix: &self.prefix,
        }
    }

    fn transaction<'a>(
        &'a self,
        db_transaction: &'a OptimisticTransactionDBTransaction,
    ) -> Self::StorageTransaction<'a> {
        PrefixedRocksDbTransaction::new(db_transaction, self.prefix.clone(), &self.db)
    }
}
