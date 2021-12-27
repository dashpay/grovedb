use rocksdb::OptimisticTransactionDB;

use super::{
    make_prefixed_key, PrefixedRocksDbStorageError, AUX_CF_NAME, META_CF_NAME, ROOTS_CF_NAME,
};
use crate::{Transaction};

pub struct PrefixedRocksDbTransaction<'a> {
    transaction: &'a rocksdb::Transaction<'a, OptimisticTransactionDB>,
    prefix: Vec<u8>,
    pub(crate) db: &'a OptimisticTransactionDB,
}
// TODO: Implement snapshots for transactions
impl<'a> PrefixedRocksDbTransaction<'a> {
    pub fn new(
        transaction: &'a rocksdb::Transaction<'a, OptimisticTransactionDB>,
        prefix: Vec<u8>,
        db: &'a OptimisticTransactionDB,
    ) -> Self {
        Self {
            transaction,
            prefix,
            db,
        }
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

impl Transaction for PrefixedRocksDbTransaction<'_> {
    type Error = PrefixedRocksDbStorageError;

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        self.transaction
            .put(make_prefixed_key(self.prefix.clone(), key), value)?;
        Ok(())
    }

    fn put_aux(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        self.transaction.put_cf(
            self.cf_aux()?,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )?;
        Ok(())
    }

    fn put_root(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        self.transaction.put_cf(
            self.cf_roots()?,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )?;
        Ok(())
    }

    fn put_meta(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        Ok(self.transaction.put_cf(self.cf_meta()?, key, value)?)
    }

    fn delete(&self, key: &[u8]) -> Result<(), Self::Error> {
        self.transaction
            .delete(make_prefixed_key(self.prefix.clone(), key))?;
        Ok(())
    }

    fn delete_aux(&self, key: &[u8]) -> Result<(), Self::Error> {
        self.transaction
            .delete_cf(self.cf_aux()?, make_prefixed_key(self.prefix.clone(), key))?;
        Ok(())
    }

    fn delete_root(&self, key: &[u8]) -> Result<(), Self::Error> {
        self.transaction.delete_cf(
            self.cf_roots()?,
            make_prefixed_key(self.prefix.clone(), key),
        )?;
        Ok(())
    }

    fn delete_meta(&self, key: &[u8]) -> Result<(), Self::Error> {
        Ok(self.transaction.delete_cf(self.cf_meta()?, key)?)
    }

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self
            .transaction
            .get(make_prefixed_key(self.prefix.clone(), key))?)
    }

    fn get_aux(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self
            .transaction
            .get_cf(self.cf_aux()?, make_prefixed_key(self.prefix.clone(), key))?)
    }

    fn get_root(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.transaction.get_cf(
            self.cf_roots()?,
            make_prefixed_key(self.prefix.clone(), key),
        )?)
    }

    fn get_meta(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.transaction.get_cf(self.cf_meta()?, key)?)
    }
}
