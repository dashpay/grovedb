use std::rc::Rc;

use rocksdb::WriteBatchWithTransaction;

use super::{
    make_prefixed_key, DBRawTransactionIterator, PrefixedRocksDbBatch, PrefixedRocksDbTransaction,
    AUX_CF_NAME, META_CF_NAME, ROOTS_CF_NAME,
};
use crate::Storage;

/// RocksDB wrapper to store items with prefixes
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
    type Batch<'a> = PrefixedRocksDbBatch<'a>;
    type Error = PrefixedRocksDbStorageError;
    type RawIterator<'a> = DBRawTransactionIterator<'a>;
    type StorageTransaction<'a> = PrefixedRocksDbTransaction<'a>;

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        self.db
            .put(make_prefixed_key(self.prefix.clone(), key), value)?;
        Ok(())
    }

    fn put_aux(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        self.db.put_cf(
            self.cf_aux()?,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )?;
        Ok(())
    }

    fn put_root(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        self.db.put_cf(
            self.cf_roots()?,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )?;
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<(), Self::Error> {
        self.db
            .delete(make_prefixed_key(self.prefix.clone(), key))?;
        Ok(())
    }

    fn delete_aux(&self, key: &[u8]) -> Result<(), Self::Error> {
        self.db
            .delete_cf(self.cf_aux()?, make_prefixed_key(self.prefix.clone(), key))?;
        Ok(())
    }

    fn delete_root(&self, key: &[u8]) -> Result<(), Self::Error> {
        self.db.delete_cf(
            self.cf_roots()?,
            make_prefixed_key(self.prefix.clone(), key),
        )?;
        Ok(())
    }

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.db.get(make_prefixed_key(self.prefix.clone(), key))?)
    }

    fn get_aux(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self
            .db
            .get_cf(self.cf_aux()?, make_prefixed_key(self.prefix.clone(), key))?)
    }

    fn get_root(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.db.get_cf(
            self.cf_roots()?,
            make_prefixed_key(self.prefix.clone(), key),
        )?)
    }

    fn put_meta(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        Ok(self.db.put_cf(self.cf_meta()?, key, value)?)
    }

    fn delete_meta(&self, key: &[u8]) -> Result<(), Self::Error> {
        Ok(self.db.delete_cf(self.cf_meta()?, key)?)
    }

    fn get_meta(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.db.get_cf(self.cf_meta()?, key)?)
    }

    fn new_batch<'a>(&'a self) -> Result<Self::Batch<'a>, Self::Error> {
        Ok(PrefixedRocksDbBatch {
            prefix: self.prefix.clone(),
            batch: WriteBatchWithTransaction::<true>::default(),
            cf_aux: self.cf_aux()?,
            cf_roots: self.cf_roots()?,
        })
    }

    fn commit_batch<'a>(&'a self, batch: Self::Batch<'a>) -> Result<(), Self::Error> {
        self.db.write(batch.batch)?;
        Ok(())
    }

    fn flush(&self) -> Result<(), Self::Error> {
        self.db.flush()?;
        Ok(())
    }

    fn raw_iter<'a>(&'a self) -> Self::RawIterator<'a> {
        self.db.raw_iterator()
    }

    fn transaction<'a>(&'a self) -> Self::StorageTransaction<'a> {
        PrefixedRocksDbTransaction::new(self.db.transaction(), self.prefix.clone(), &self.db)
    }
}
