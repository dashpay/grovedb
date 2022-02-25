/// Top-level storage abstraction.
/// Should be able to hold storage connection and to start transaction when
/// needed. All query operations will be exposed using [StorageContext].
pub trait Storage<'db> {
    type Transaction;
    type Error: std::error::Error + Send + Sync + 'static;

    /// Starts a new transaction
    fn start_transaction(&'db self) -> Self::Transaction;

    /// Consumes and commits a transaction
    fn commit_transaction(&self, transaction: Self::Transaction) -> Result<(), Self::Error>;

    /// Rollback a transaction
    fn rollback_transaction(&self, transaction: &Self::Transaction) -> Result<(), Self::Error>;

    /// Forces data to be written
    fn flush(&self) -> Result<(), Self::Error>;
}

/// Storage context.
/// Provides operations expected from a database abstracting details such as
/// whether it is a transaction or not.
pub trait StorageContext<'db, 'ctx> {
    /// Storage error type
    type Error: std::error::Error + Send + Sync + 'static;

    /// Storage batch type
    type Batch: Batch;

    /// Storage raw iterator type (to iterate over storage without supplying a
    /// key)
    type RawIterator: RawIterator;

    /// Put `value` into data storage with `key`
    fn put<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error>;

    /// Put `value` into auxiliary data storage with `key`
    fn put_aux<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error>;

    /// Put `value` into trees roots storage with `key`
    fn put_root<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error>;

    /// Put `value` into GroveDB metadata storage with `key`
    fn put_meta<K: AsRef<[u8]>>(&self, key: K, value: &[u8]) -> Result<(), Self::Error>;

    /// Delete entry with `key` from data storage
    fn delete<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error>;

    /// Delete entry with `key` from auxiliary data storage
    fn delete_aux<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error>;

    /// Delete entry with `key` from trees roots storage
    fn delete_root<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error>;

    /// Delete entry with `key` from GroveDB metadata storage
    fn delete_meta<K: AsRef<[u8]>>(&self, key: K) -> Result<(), Self::Error>;

    /// Get entry by `key` from data storage
    fn get<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Get entry by `key` from auxiliary data storage
    fn get_aux<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Get entry by `key` from trees roots storage
    fn get_root<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Get entry by `key` from GroveDB metadata storage
    fn get_meta<K: AsRef<[u8]>>(&self, key: K) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Initialize a new batch
    fn new_batch(&'ctx self) -> Self::Batch;

    /// Commits changes from batch into storage
    fn commit_batch(&'ctx self, batch: Self::Batch) -> Result<(), Self::Error>;

    /// Get raw iterator over storage
    fn raw_iter(&self) -> Self::RawIterator;
}

pub trait Batch {
    type Error: std::error::Error + Send + Sync + 'static;

    fn put<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) -> Result<(), Self::Error>;

    fn put_aux<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) -> Result<(), Self::Error>;

    fn put_root<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) -> Result<(), Self::Error>;

    fn delete<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(), Self::Error>;

    fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(), Self::Error>;

    fn delete_root<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(), Self::Error>;
}

pub trait RawIterator {
    fn seek_to_first(&mut self);

    fn seek_to_last(&mut self);

    fn seek<K: AsRef<[u8]>>(&mut self, key: K);

    fn next(&mut self);

    fn prev(&mut self);

    fn value(&self) -> Option<&[u8]>;

    fn key(&self) -> Option<&[u8]>;

    fn valid(&self) -> bool;
}
