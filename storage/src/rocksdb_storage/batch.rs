use rocksdb::{ColumnFamily, OptimisticTransactionDB};

use super::make_prefixed_key;
use crate::Batch;

/// Wrapper to RocksDB batch
pub struct PrefixedRocksDbBatch<'a> {
    pub prefix: Vec<u8>,
    pub batch: rocksdb::WriteBatchWithTransaction<true>,
    pub cf_aux: &'a ColumnFamily,
    pub cf_roots: &'a ColumnFamily,
}

impl<'a> Batch for PrefixedRocksDbBatch<'a> {
    fn put<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        self.batch
            .put(make_prefixed_key(self.prefix.clone(), key), value)
    }

    fn put_aux<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        self.batch.put_cf(
            self.cf_aux,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )
    }

    fn put_root<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        self.batch.put_cf(
            self.cf_roots,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )
    }

    fn delete<K: AsRef<[u8]>>(&mut self, key: K) {
        self.batch
            .delete(make_prefixed_key(self.prefix.clone(), key))
    }

    fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K) {
        self.batch
            .delete_cf(self.cf_aux, make_prefixed_key(self.prefix.clone(), key))
    }

    fn delete_root<K: AsRef<[u8]>>(&mut self, key: K) {
        self.batch
            .delete_cf(self.cf_roots, make_prefixed_key(self.prefix.clone(), key))
    }
}

/// Wrapper to RocksDB batch
pub struct PrefixedTransactionalRocksDbBatch<'a> {
    pub prefix: Vec<u8>,
    pub cf_aux: &'a ColumnFamily,
    pub cf_roots: &'a ColumnFamily,
    pub transaction: &'a rocksdb::Transaction<'a, OptimisticTransactionDB>,
}

// TODO: don't ignore errors
impl<'a> Batch for PrefixedTransactionalRocksDbBatch<'a> {
    fn put<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        self.transaction
            .put(make_prefixed_key(self.prefix.clone(), key), value);
    }

    fn put_aux<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        self.transaction.put_cf(
            self.cf_aux,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        );
    }

    fn put_root<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        self.transaction.put_cf(
            self.cf_roots,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        );
    }

    fn delete<K: AsRef<[u8]>>(&mut self, key: K) {
        self.transaction
            .delete(make_prefixed_key(self.prefix.clone(), key));
    }

    fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K) {
        self.transaction
            .delete_cf(self.cf_aux, make_prefixed_key(self.prefix.clone(), key));
    }

    fn delete_root<K: AsRef<[u8]>>(&mut self, key: K) {
        self.transaction
            .delete_cf(self.cf_roots, make_prefixed_key(self.prefix.clone(), key));
    }
}

pub enum OrBatch<'a> {
    Batch(PrefixedRocksDbBatch<'a>),
    TransactionalBatch(PrefixedTransactionalRocksDbBatch<'a>),
}

impl<'a> Batch for OrBatch<'a> {
    fn put<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        match self {
            Self::TransactionalBatch(batch) => batch.put(key, value),
            Self::Batch(batch) => batch.put(key, value),
        }
    }

    fn put_aux<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        match self {
            Self::TransactionalBatch(batch) => batch.put_aux(key, value),
            Self::Batch(batch) => batch.put_aux(key, value),
        }
    }

    fn put_root<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        match self {
            Self::TransactionalBatch(batch) => batch.put_root(key, value),
            Self::Batch(batch) => batch.put_root(key, value),
        }
    }

    fn delete<K: AsRef<[u8]>>(&mut self, key: K) {
        match self {
            Self::TransactionalBatch(batch) => batch.delete(key),
            Self::Batch(batch) => batch.delete(key),
        }
    }

    fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K) {
        match self {
            Self::TransactionalBatch(batch) => batch.delete_aux(key),
            Self::Batch(batch) => batch.delete_aux(key),
        }
    }

    fn delete_root<K: AsRef<[u8]>>(&mut self, key: K) {
        match self {
            Self::TransactionalBatch(batch) => batch.delete_root(key),
            Self::Batch(batch) => batch.delete_root(key),
        }
    }
}
