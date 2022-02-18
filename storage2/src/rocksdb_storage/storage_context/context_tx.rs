use rocksdb::{ColumnFamily, Error, OptimisticTransactionDB, Transaction};

use super::{make_prefixed_key, Db, PrefixedRocksDbBatch, PrefixedRocksDbRawIterator};
use crate::{
    rocksdb_storage::storage::{AUX_CF_NAME, META_CF_NAME, ROOTS_CF_NAME},
    StorageContext,
};

/// Type alias for a transaction
type Tx<'a> = Transaction<'a, OptimisticTransactionDB>;

/// Storage context with a prefix applied to be used in a subtree to be used in
/// transaction.
pub struct PrefixedRocksDbTransactionContext<'a> {
    storage: &'a Db,
    transaction: &'a Tx<'a>,
    prefix: Vec<u8>,
}

impl<'a> PrefixedRocksDbTransactionContext<'a> {
    /// Create a new prefixed transaction context instance
    pub fn new(storage: &'a Db, transaction: &'a Tx<'a>, prefix: Vec<u8>) -> Self {
        PrefixedRocksDbTransactionContext {
            storage,
            transaction,
            prefix,
        }
    }
}

impl<'a> PrefixedRocksDbTransactionContext<'a> {
    /// Get auxiliary data column family
    fn cf_aux(&self) -> &ColumnFamily {
        self.storage
            .cf_handle(AUX_CF_NAME)
            .expect("aux column family must exist")
    }

    /// Get trees roots data column family
    fn cf_roots(&self) -> &ColumnFamily {
        self.storage
            .cf_handle(ROOTS_CF_NAME)
            .expect("roots column family must exist")
    }

    /// Get metadata column family
    fn cf_meta(&self) -> &ColumnFamily {
        self.storage
            .cf_handle(META_CF_NAME)
            .expect("meta column family must exist")
    }
}

impl<'a> StorageContext<'a> for PrefixedRocksDbTransactionContext<'a> {
    type Batch = PrefixedRocksDbBatch<'a>;
    type Error = Error;
    type RawIterator = PrefixedRocksDbRawIterator;

    fn put<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        self.transaction
            .put(make_prefixed_key(self.prefix.clone(), key), value)
    }

    fn put_aux<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        self.transaction.put_cf(
            self.cf_aux(),
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )
    }

    fn put_root<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        self.transaction.put_cf(
            self.cf_roots(),
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )
    }

    fn put_meta<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        self.transaction.put_cf(
            self.cf_meta(),
            make_prefixed_key(self.prefix.clone(), key),
            value,
        )
    }

    fn delete<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
        self.transaction
            .delete(make_prefixed_key(self.prefix.clone(), key))
    }

    fn delete_aux<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
        self.transaction
            .delete_cf(self.cf_aux(), make_prefixed_key(self.prefix.clone(), key))
    }

    fn delete_root<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
        self.transaction
            .delete_cf(self.cf_roots(), make_prefixed_key(self.prefix.clone(), key))
    }

    fn delete_meta<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
        self.transaction.delete_cf(self.cf_meta(), key)
    }

    fn get<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error> {
        self.transaction
            .get(make_prefixed_key(self.prefix.clone(), key))
    }

    fn get_aux<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error> {
        self.transaction
            .get_cf(self.cf_aux(), make_prefixed_key(self.prefix.clone(), key))
    }

    fn get_root<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error> {
        self.transaction
            .get_cf(self.cf_roots(), make_prefixed_key(self.prefix.clone(), key))
    }

    fn get_meta<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error> {
        self.transaction.get_cf(self.cf_meta(), key)
    }

    fn new_batch(&self) -> Result<Self::Batch, Self::Error> {
        todo!()
    }

    fn commit_batch(&self, batch: Self::Batch) -> Result<(), Self::Error> {
        todo!()
    }

    fn raw_iter(&self) -> Self::RawIterator {
        todo!()
    }
}
