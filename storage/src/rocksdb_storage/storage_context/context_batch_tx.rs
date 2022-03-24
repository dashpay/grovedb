//! Storage context implementation with a transaction.
use rocksdb::{ColumnFamily, DBRawIteratorWithThreadMode, Error, WriteBatchWithTransaction};

use super::{make_prefixed_key, PrefixedRocksDbBatch, PrefixedRocksDbRawIterator};
use crate::{
    rocksdb_storage::storage::{Db, Tx, AUX_CF_NAME, META_CF_NAME, ROOTS_CF_NAME},
    StorageContext,
};

/// Storage context with a prefix applied to be used in a subtree to be used in
/// transaction.
pub struct PrefixedRocksDbBatchTransactionContext<'db> {
    storage: &'db Db,
    transaction: &'db Tx<'db>,
    prefix: Vec<u8>,
}

impl<'db> PrefixedRocksDbBatchTransactionContext<'db> {
    /// Create a new prefixed transaction context instance
    pub fn new(storage: &'db Db, transaction: &'db Tx<'db>, prefix: Vec<u8>) -> Self {
        PrefixedRocksDbBatchTransactionContext {
            storage,
            transaction,
            prefix,
        }
    }
}

impl<'db> PrefixedRocksDbBatchTransactionContext<'db> {
    /// Get auxiliary data column family
    fn cf_aux(&self) -> &'db ColumnFamily {
        self.storage
            .cf_handle(AUX_CF_NAME)
            .expect("aux column family must exist")
    }

    /// Get trees roots data column family
    fn cf_roots(&self) -> &'db ColumnFamily {
        self.storage
            .cf_handle(ROOTS_CF_NAME)
            .expect("roots column family must exist")
    }

    /// Get metadata column family
    fn cf_meta(&self) -> &'db ColumnFamily {
        self.storage
            .cf_handle(META_CF_NAME)
            .expect("meta column family must exist")
    }
}

impl<'db, 'ctx> StorageContext<'db, 'ctx> for PrefixedRocksDbBatchTransactionContext<'db>
where
    'db: 'ctx,
{
    type Batch = PrefixedRocksDbBatch<'db, WriteBatchWithTransaction<true>>;
    // &'ctx Self;
    type Error = Error;
    type RawIterator = PrefixedRocksDbRawIterator<DBRawIteratorWithThreadMode<'db, Tx<'db>>>;

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
        self.transaction
            .delete_cf(self.cf_meta(), make_prefixed_key(self.prefix.clone(), key))
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
        self.transaction
            .get_cf(self.cf_meta(), make_prefixed_key(self.prefix.clone(), key))
    }

    fn new_batch(&'ctx self) -> Self::Batch {
        todo!()
        // self
    }

    fn commit_batch(&'ctx self, _batch: Self::Batch) -> Result<(), Self::Error> {
        todo!()
        // Ok(())
    }

    fn raw_iter(&self) -> Self::RawIterator {
        PrefixedRocksDbRawIterator {
            prefix: self.prefix.clone(),
            raw_iterator: self.transaction.raw_iterator(),
        }
    }
}
