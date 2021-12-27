use std::rc::Rc;

use rocksdb::WriteBatchWithTransaction;

use super::{
    make_prefixed_key, DBRawTransactionIterator, PrefixedRocksDbBatch, PrefixedRocksDbTransaction,
    AUX_CF_NAME, META_CF_NAME, ROOTS_CF_NAME,
};
use crate::{rocksdb_storage::OptimisticTransactionDBTransaction, Storage, Transaction};

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
    type DBTransaction<'a> = OptimisticTransactionDBTransaction<'a>;
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

    fn transaction<'a>(
        &'a self,
        db_transaction: &'a OptimisticTransactionDBTransaction,
    ) -> Self::StorageTransaction<'a> {
        PrefixedRocksDbTransaction::new(db_transaction, self.prefix.clone(), &self.db)
    }
}

pub struct TransactionalStorage<'a> {
    storage: &'a PrefixedRocksDbStorage,
    transaction: Option<PrefixedRocksDbTransaction<'a>>,
}

impl<'a> TransactionalStorage<'a> {
    pub fn new(
        storage: &'a PrefixedRocksDbStorage,
        db_transaction: Option<&'a OptimisticTransactionDBTransaction>,
    ) -> Self {
        Self {
            storage, transaction: db_transaction.map(|tx| storage.transaction(tx))
        }
    }
}

impl<'b> Storage for TransactionalStorage<'b> {
    type Error = PrefixedRocksDbStorageError;
    type Batch<'a>
        where
            'b: 'a,
    = PrefixedRocksDbBatch<'a>;
    type RawIterator<'a>
        where
            'b: 'a,
    = DBRawTransactionIterator<'a>;
    type StorageTransaction<'a>
        where
            'b: 'a,
    = PrefixedRocksDbTransaction<'a>;
    type DBTransaction<'a>
        where
            'b: 'a,
    = OptimisticTransactionDBTransaction<'a>;

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        match &self.transaction {
            None => self.storage.put(key, value),
            Some(tx) => tx.put(key, value),
        }
    }

    fn put_aux(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        match &self.transaction {
            None => self.storage.put_aux(key, value),
            Some(tx) => tx.put_aux(key, value),
        }
    }

    fn put_root(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        match &self.transaction {
            None => self.storage.put_root(key, value),
            Some(tx) => tx.put_root(key, value),
        }
    }

    fn put_meta(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        match &self.transaction {
            None => self.storage.put_meta(key, value),
            Some(tx) => tx.put_meta(key, value),
        }
    }

    fn delete(&self, key: &[u8]) -> Result<(), Self::Error> {
        match &self.transaction {
            None => self.storage.delete(key),
            Some(tx) => tx.delete(key),
        }
    }

    fn delete_aux(&self, key: &[u8]) -> Result<(), Self::Error> {
        match &self.transaction {
            None => self.storage.delete_aux(key),
            Some(tx) => tx.delete_aux(key),
        }
    }

    fn delete_root(&self, key: &[u8]) -> Result<(), Self::Error> {
        match &self.transaction {
            None => self.storage.delete_root(key),
            Some(tx) => tx.delete_root(key),
        }
    }

    fn delete_meta(&self, key: &[u8]) -> Result<(), Self::Error> {
        match &self.transaction {
            None => self.storage.delete_meta(key),
            Some(tx) => tx.delete_meta(key),
        }
    }

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        match &self.transaction {
            None => self.storage.get(key),
            Some(tx) => tx.get(key),
        }
    }

    fn get_aux(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        match &self.transaction {
            None => self.storage.get_aux(key),
            Some(tx) => tx.get_aux(key),
        }
    }

    fn get_root(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        match &self.transaction {
            None => self.storage.get_root(key),
            Some(tx) => tx.get_root(key),
        }
    }

    fn get_meta(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        match &self.transaction {
            None => self.storage.get_meta(key),
            Some(tx) => tx.get_meta(key),
        }
    }

    fn new_batch<'a>(&'a self) -> Result<Self::Batch<'a>, Self::Error> {
        self.storage.new_batch()
    }

    fn commit_batch<'a>(&'a self, batch: Self::Batch<'a>) -> Result<(), Self::Error> {
        self.storage.commit_batch(batch)
    }

    fn flush(&self) -> Result<(), Self::Error> {
        self.storage.flush()
    }

    fn raw_iter<'a>(&'a self) -> Self::RawIterator<'a> {
        self.storage.raw_iter()
    }

    fn transaction<'a>(&'a self, tx: &'a Self::DBTransaction<'a>) -> Self::StorageTransaction<'a> {
        self.storage.transaction(tx)
    }
}
