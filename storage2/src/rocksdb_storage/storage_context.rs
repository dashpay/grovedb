//! Implementation of prefixed storage context.
mod batch;
mod raw_iterator;

use batch::PrefixedRocksDbBatch;
use raw_iterator::PrefixedRocksDbRawIterator;
use rocksdb::{Error, OptimisticTransactionDB, Transaction};

use crate::StorageContext;

fn make_prefixed_key<K: AsRef<[u8]>>(mut prefix: Vec<u8>, key: K) -> Vec<u8> {
    prefix.extend_from_slice(key.as_ref());
    prefix
}

/// Storage context with a prefix applied to be used in a subtree.
/// It is generic over underlying storage which means context could be over
/// storage or a transaction.
pub struct PrefixedRocksDbStorageContext<S> {
    storage: S,
}

impl<'a> From<&'a OptimisticTransactionDB> for PrefixedRocksDbStorageContext<&'a OptimisticTransactionDB> {
    fn from(storage: &'a OptimisticTransactionDB) -> Self {
        PrefixedRocksDbStorageContext { storage }
    }
}

impl<'a> StorageContext<'a> for PrefixedRocksDbStorageContext<&'a OptimisticTransactionDB> {
    type Error = Error;

    type Batch = PrefixedRocksDbBatch<'a>;

    type RawIterator = PrefixedRocksDbRawIterator;

    fn put<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        todo!()
    }

    fn put_aux<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        todo!()
    }

    fn put_root<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        todo!()
    }

    fn put_meta<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error> {
        todo!()
    }

    fn delete<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
        todo!()
    }

    fn delete_aux<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
        todo!()
    }

    fn delete_root<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
        todo!()
    }

    fn delete_meta<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
        todo!()
    }

    fn get<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error> {
        todo!()
    }

    fn get_aux<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error> {
        todo!()
    }

    fn get_root<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error> {
        todo!()
    }

    fn get_meta<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error> {
        todo!()
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

// impl<'a> StorageContext<'a>
//     for PrefixedRocksDbStorageContext<Transaction<'a,
// OptimisticTransactionDB>> {
//     type Error = Error;

//     type Batch = PrefixedRocksDbBatch<'a>;

//     type RawIterator = PrefixedRocksDbRawIterator;

//     fn put<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(),
// Self::Error> {         todo!()
//     }

//     fn put_aux<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(),
// Self::Error> {         todo!()
//     }

//     fn put_root<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(),
// Self::Error> {         todo!()
//     }

//     fn put_meta<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(),
// Self::Error> {         todo!()
//     }

//     fn delete<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
//         todo!()
//     }

//     fn delete_aux<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error> {
//         todo!()
//     }

//     fn delete_root<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error>
// {         todo!()
//     }

//     fn delete_meta<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error>
// {         todo!()
//     }

//     fn get<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>,
// Self::Error> {         todo!()
//     }

//     fn get_aux<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>,
// Self::Error> {         todo!()
//     }

//     fn get_root<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>,
// Self::Error> {         todo!()
//     }

//     fn get_meta<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>,
// Self::Error> {         todo!()
//     }

//     fn new_batch(&self) -> Result<Self::Batch, Self::Error> {
//         todo!()
//     }

//     fn commit_batch(&self, batch: Self::Batch) -> Result<(), Self::Error> {
//         todo!()
//     }

//     fn raw_iter(&self) -> Self::RawIterator {
//         todo!()
//     }
// }
