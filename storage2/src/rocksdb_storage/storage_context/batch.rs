//! Prefixed storage batch implementation for RocksDB backend.
use std::convert::Infallible;

use rocksdb::{ColumnFamily, WriteBatchWithTransaction};

use super::make_prefixed_key;
use crate::Batch;

/// Wrapper to RocksDB batch
pub struct PrefixedRocksDbBatch<'a> {
    pub prefix: Vec<u8>,
    pub batch: WriteBatchWithTransaction<true>,
    pub cf_aux: &'a ColumnFamily,
    pub cf_roots: &'a ColumnFamily,
}

impl<'a> Batch for PrefixedRocksDbBatch<'a> {
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

// /// Wrapper to RocksDB batch
// pub struct PrefixedTransactionalRocksDbBatch<'a> {
//     pub prefix: Vec<u8>,
//     pub cf_aux: &'a ColumnFamily,
//     pub cf_roots: &'a ColumnFamily,
//     pub transaction: &'a rocksdb::Transaction<'a, OptimisticTransactionDB>,
// }

// // TODO: don't ignore errors
// impl<'a> Batch for PrefixedTransactionalRocksDbBatch<'a> {
//     type Error = PrefixedRocksDbStorageError;

//     fn put<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) -> Result<(),
// Self::Error> {         self.transaction
//             .put(make_prefixed_key(self.prefix.clone(), key), value)?;
//         Ok(())
//     }

//     fn put_aux<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) -> Result<(),
// Self::Error> {         self.transaction.put_cf(
//             self.cf_aux,
//             make_prefixed_key(self.prefix.clone(), key),
//             value,
//         )?;
//         Ok(())
//     }

//     fn put_root<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) ->
// Result<(), Self::Error> {         self.transaction.put_cf(
//             self.cf_roots,
//             make_prefixed_key(self.prefix.clone(), key),
//             value,
//         )?;
//         Ok(())
//     }

//     fn delete<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(), Self::Error> {
//         self.transaction
//             .delete(make_prefixed_key(self.prefix.clone(), key))?;
//         Ok(())
//     }

//     fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(),
// Self::Error> {         self.transaction
//             .delete_cf(self.cf_aux, make_prefixed_key(self.prefix.clone(),
// key))?;         Ok(())
//     }

//     fn delete_root<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(),
// Self::Error> {         self.transaction
//             .delete_cf(self.cf_roots, make_prefixed_key(self.prefix.clone(),
// key))?;         Ok(())
//     }
// }

// pub enum OrBatch<'a> {
//     Batch(PrefixedRocksDbBatch<'a>),
//     TransactionalBatch(PrefixedTransactionalRocksDbBatch<'a>),
// }

// impl<'a> Batch for OrBatch<'a> {
//     type Error = PrefixedRocksDbStorageError;

//     fn put<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) -> Result<(),
// Self::Error> {         match self {
//             Self::TransactionalBatch(batch) => batch.put(key, value)?,
//             Self::Batch(batch) => batch.put(key, value).unwrap_or_default(),
//         }
//         Ok(())
//     }

//     fn put_aux<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) -> Result<(),
// Self::Error> {         match self {
//             Self::TransactionalBatch(batch) => batch.put_aux(key, value)?,
//             Self::Batch(batch) => batch.put_aux(key,
// value).unwrap_or_default(),         }
//         Ok(())
//     }

//     fn put_root<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) ->
// Result<(), Self::Error> {         match self {
//             Self::TransactionalBatch(batch) => batch.put_root(key, value)?,
//             Self::Batch(batch) => batch.put_root(key,
// value).unwrap_or_default(),         }
//         Ok(())
//     }

//     fn delete<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(), Self::Error> {
//         match self {
//             Self::TransactionalBatch(batch) => batch.delete(key)?,
//             Self::Batch(batch) => batch.delete(key).unwrap_or_default(),
//         }
//         Ok(())
//     }

//     fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(),
// Self::Error> {         match self {
//             Self::TransactionalBatch(batch) => batch.delete_aux(key)?,
//             Self::Batch(batch) => batch.delete_aux(key).unwrap_or_default(),
//         }
//         Ok(())
//     }

//     fn delete_root<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(),
// Self::Error> {         match self {
//             Self::TransactionalBatch(batch) => batch.delete_root(key)?,
//             Self::Batch(batch) => batch.delete_root(key).unwrap_or_default(),
//         }
//         Ok(())
//     }
// }
