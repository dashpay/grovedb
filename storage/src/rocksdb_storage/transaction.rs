use rocksdb::{checkpoint::Checkpoint, Error, OptimisticTransactionDB};

use crate::Transaction;
use super::{PrefixedRocksDbStorageError, make_prefixed_key};

pub struct PrefixedRocksDbTransaction<'a> {
    transaction: rocksdb::Transaction<'a, OptimisticTransactionDB>,
    prefix: Vec<u8>,
}
// TODO: Implement snapshots for transactions
impl PrefixedRocksDbTransaction<'_> {
    fn new(db: &OptimisticTransactionDB, prefix: Vec<u8>) -> Self {
        Self { transaction: db.transaction(), prefix }
    }
}

impl Transaction for PrefixedRocksDbTransaction<'_> {
    type Error = PrefixedRocksDbStorageError;

    fn commit(&self) {
        self.transaction.commit();
    }

    fn rollback(&self) {
        self.transaction.rollback();
    }

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
        Ok(self.transaction.get(make_prefixed_key(self.prefix.clone(), key))?)
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
