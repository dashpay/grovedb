#![feature(generic_associated_types)]

use rocksdb::OptimisticTransactionDB;

pub mod rocksdb_storage;

pub trait Transaction {
    /// Storage error type
    type Error: std::error::Error + Send + Sync + 'static;

    /// Commit data from the transaction
    fn commit(&self) -> Result<(), Self::Error>;

    /// Rollback the transaction
    fn rollback(&self) -> Result<(), Self::Error>;

    /// Put `value` into data storage with `key`
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error>;

    /// Put `value` into auxiliary data storage with `key`
    fn put_aux(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error>;

    /// Put `value` into trees roots storage with `key`
    fn put_root(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error>;

    /// Put `value` into GroveDB metadata storage with `key`
    fn put_meta(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error>;

    /// Delete entry with `key` from data storage
    fn delete(&self, key: &[u8]) -> Result<(), Self::Error>;

    /// Delete entry with `key` from auxiliary data storage
    fn delete_aux(&self, key: &[u8]) -> Result<(), Self::Error>;

    /// Delete entry with `key` from trees roots storage
    fn delete_root(&self, key: &[u8]) -> Result<(), Self::Error>;

    /// Delete entry with `key` from GroveDB metadata storage
    fn delete_meta(&self, key: &[u8]) -> Result<(), Self::Error>;

    /// Get entry by `key` from data storage
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Get entry by `key` from auxiliary data storage
    fn get_aux(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Get entry by `key` from trees roots storage
    fn get_root(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Get entry by `key` from GroveDB metadata storage
    fn get_meta(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error>;
}

/// `Storage` is able to store and retrieve arbitrary bytes by key
pub trait Storage {
    /// Storage error type
    type Error: std::error::Error + Send + Sync + 'static;
    /// Storage batch type
    type Batch<'a>: Batch
    where
        Self: 'a;
    /// Storage raw iterator type (to iterate over storage without supplying a
    /// key)
    type RawIterator<'a>: RawIterator
    where
        Self: 'a;

    type StorageTransaction: Transaction;

    /// Put `value` into data storage with `key`
    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error>;

    /// Put `value` into auxiliary data storage with `key`
    fn put_aux(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error>;

    /// Put `value` into trees roots storage with `key`
    fn put_root(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error>;

    /// Put `value` into GroveDB metadata storage with `key`
    fn put_meta(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error>;

    /// Delete entry with `key` from data storage
    fn delete(&self, key: &[u8]) -> Result<(), Self::Error>;

    /// Delete entry with `key` from auxiliary data storage
    fn delete_aux(&self, key: &[u8]) -> Result<(), Self::Error>;

    /// Delete entry with `key` from trees roots storage
    fn delete_root(&self, key: &[u8]) -> Result<(), Self::Error>;

    /// Delete entry with `key` from GroveDB metadata storage
    fn delete_meta(&self, key: &[u8]) -> Result<(), Self::Error>;

    /// Get entry by `key` from data storage
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Get entry by `key` from auxiliary data storage
    fn get_aux(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Get entry by `key` from trees roots storage
    fn get_root(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Get entry by `key` from GroveDB metadata storage
    fn get_meta(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Initialize a new batch
    fn new_batch<'a>(&'a self) -> Result<Self::Batch<'a>, Self::Error>;

    /// Commits changes from batch into storage
    fn commit_batch<'a>(&'a self, batch: Self::Batch<'a>) -> Result<(), Self::Error>;

    /// Forces data to be written
    fn flush(&self) -> Result<(), Self::Error>;

    /// Get raw iterator over storage
    fn raw_iter<'a>(&'a self) -> Self::RawIterator<'a>;

    /// Starts DB transaction
    fn transaction(&self) -> Self::StorageTransaction;
}

impl<'b, S: Storage> Storage for &'b S {
    type Batch<'a>
    where
        'b: 'a,
    = S::Batch<'a>;
    type Error = S::Error;
    type RawIterator<'a>
    where
        'b: 'a,
    = S::RawIterator<'a>;

    type StorageTransaction = ();

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        (*self).put(key, value)
    }

    fn put_aux(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        (*self).put_aux(key, value)
    }

    fn put_root(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        (*self).put_root(key, value)
    }

    fn delete(&self, key: &[u8]) -> Result<(), Self::Error> {
        (*self).delete(key)
    }

    fn delete_aux(&self, key: &[u8]) -> Result<(), Self::Error> {
        (*self).delete_aux(key)
    }

    fn delete_root(&self, key: &[u8]) -> Result<(), Self::Error> {
        (*self).delete_root(key)
    }

    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        (*self).get(key)
    }

    fn get_aux(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        (*self).get_aux(key)
    }

    fn get_root(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        (*self).get_root(key)
    }

    fn put_meta(&self, key: &[u8], value: &[u8]) -> Result<(), Self::Error> {
        (*self).put_meta(key, value)
    }

    fn delete_meta(&self, key: &[u8]) -> Result<(), Self::Error> {
        (*self).delete_meta(key)
    }

    fn get_meta(&self, key: &[u8]) -> Result<Option<Vec<u8>>, Self::Error> {
        (*self).get_meta(key)
    }

    fn new_batch<'a>(&'a self) -> Result<Self::Batch<'a>, Self::Error> {
        (*self).new_batch()
    }

    fn commit_batch<'a>(&'a self, batch: Self::Batch<'a>) -> Result<(), Self::Error> {
        (*self).commit_batch(batch)
    }

    fn flush(&self) -> Result<(), Self::Error> {
        (*self).flush()
    }

    fn raw_iter<'a>(&'a self) -> Self::RawIterator<'a> {
        (*self).raw_iter()
    }
}

pub trait Batch {
    fn put(&mut self, key: &[u8], value: &[u8]);

    fn put_aux(&mut self, key: &[u8], value: &[u8]);

    fn put_root(&mut self, key: &[u8], value: &[u8]);

    fn delete(&mut self, key: &[u8]);

    fn delete_aux(&mut self, key: &[u8]);

    fn delete_root(&mut self, key: &[u8]);
}

pub trait RawIterator {
    fn seek_to_first(&mut self);

    fn seek(&mut self, key: &[u8]);

    fn next(&mut self);

    fn value(&self) -> Option<&[u8]>;

    fn key(&self) -> Option<&[u8]>;

    fn valid(&self) -> bool;
}

/// The `Store` trait allows to store its implementor by key using a storage `S`
/// or to delete it.
pub trait Store
where
    Self: Sized,
{
    /// Error type for a process of object storing
    type Error;

    /// Serialize object into bytes
    fn encode(&self) -> Vec<u8>;

    /// Deserialize object from bytes
    fn decode(bytes: &[u8]) -> Result<Self, Self::Error>;

    /// Persist object into storage
    fn put<S>(&self, storage: S, key: &[u8]) -> Result<(), Self::Error>
    where
        S: Storage,
        Self::Error: From<S::Error>,
    {
        Ok(storage.put(key, &self.encode())?)
    }

    /// Delete object from storage
    fn delete<S>(storage: S, key: &[u8]) -> Result<(), Self::Error>
    where
        S: Storage,
        Self::Error: From<S::Error>,
    {
        Ok(storage.delete(key)?)
    }

    /// Fetch object from storage `S` by `key`
    fn get<S>(storage: S, key: &[u8]) -> Result<Option<Self>, Self::Error>
    where
        S: Storage,
        Self::Error: From<S::Error>,
    {
        Ok(storage.get(key)?.map(|x| Self::decode(&x)).transpose()?)
    }
}
