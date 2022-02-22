//! Prefixed storage batch implementation for RocksDB backend.
use std::convert::Infallible;

use rocksdb::{ColumnFamily, WriteBatchWithTransaction};

use super::{make_prefixed_key, PrefixedRocksDbTransactionContext};
use crate::{Batch, StorageContext};

/// Wrapper to RocksDB batch
pub struct PrefixedRocksDbBatch<'a, B> {
    pub prefix: Vec<u8>,
    pub batch: B,
    pub cf_aux: &'a ColumnFamily,
    pub cf_roots: &'a ColumnFamily,
}

/// Implementation of a batch ouside a transaction
impl<'a> Batch for PrefixedRocksDbBatch<'a, WriteBatchWithTransaction<true>> {
    type Error = Infallible;

    fn put<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        self.batch
            .put(make_prefixed_key(self.prefix.clone(), key), value);
        Ok(())
    }

    fn put_aux<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        self.batch.put_cf(
            self.cf_aux,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        );
        Ok(())
    }

    fn put_root<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        self.batch.put_cf(
            self.cf_roots,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        );
        Ok(())
    }

    fn delete<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(), Self::Error> {
        self.batch
            .delete(make_prefixed_key(self.prefix.clone(), key));
        Ok(())
    }

    fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(), Self::Error> {
        self.batch
            .delete_cf(self.cf_aux, make_prefixed_key(self.prefix.clone(), key));
        Ok(())
    }

    fn delete_root<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(), Self::Error> {
        self.batch
            .delete_cf(self.cf_roots, make_prefixed_key(self.prefix.clone(), key));
        Ok(())
    }
}

/// Implementation of a batch inside a transaction.
/// Basically just proxies all calls to the underlying transaction.
impl<'a> Batch for &'a PrefixedRocksDbTransactionContext<'a> {
    type Error = <PrefixedRocksDbTransactionContext<'a> as StorageContext<'a>>::Error;

    fn put<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        StorageContext::put(*self, key, value)
    }

    fn put_aux<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        StorageContext::put_aux(*self, key, value)
    }

    fn put_root<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        StorageContext::put_root(*self, key, value)
    }

    fn delete<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(), Self::Error> {
        StorageContext::delete(*self, key)
    }

    fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(), Self::Error> {
        StorageContext::delete_aux(*self, key)
    }

    fn delete_root<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(), Self::Error> {
        StorageContext::delete_root(*self, key)
    }
}
