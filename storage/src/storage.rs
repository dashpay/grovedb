use std::{
    cell::{Ref, RefCell},
    vec::IntoIter,
};

/// Top-level storage abstraction.
/// Should be able to hold storage connection and to start transaction when
/// needed. All query operations will be exposed using [StorageContext].
pub trait Storage<'db> {
    /// Storage transaction type
    type Transaction;

    /// Storage context type
    /// TODO: add `StorageContext<'db, 'ctx, Error = Self::Error>` bound with
    /// GATs
    type StorageContext;

    /// Storage context type for transactional data
    /// TODO: add `StorageContext<'db, 'ctx, Error = Self::Error>` bound with
    /// GATs
    type TransactionalStorageContext;

    /// Storage context type for mutli-tree batch operations
    type BatchStorageContext;

    /// Storage context type for multi-tree batch operations inside transaction
    type BatchTransactionalStorageContext;

    /// Error type
    type Error: std::error::Error + Send + Sync + 'static;

    /// Starts a new transaction
    fn start_transaction(&'db self) -> Self::Transaction;

    /// Consumes and commits a transaction
    fn commit_transaction(&self, transaction: Self::Transaction) -> Result<(), Self::Error>;

    /// Rollback a transaction
    fn rollback_transaction(&self, transaction: &Self::Transaction) -> Result<(), Self::Error>;

    /// Consumes and applies multi-context batch.
    fn commit_multi_context_batch(&self, batch: StorageBatch) -> Result<(), Self::Error>;

    /// Consumes and applies multi-context batch on transaction.
    fn commit_multi_context_batch_with_transaction(
        &self,
        batch: StorageBatch,
        transaction: &'db Self::Transaction,
    ) -> Result<(), Self::Error>;

    /// Forces data to be written
    fn flush(&self) -> Result<(), Self::Error>;

    /// Make storage context for a subtree with path
    fn get_storage_context<'p, P>(&'db self, path: P) -> Self::StorageContext
    where
        P: IntoIterator<Item = &'p [u8]>;

    /// Make storage context for a subtree on transactional data
    fn get_transactional_storage_context<'p, P>(
        &'db self,
        path: P,
        transaction: &'db Self::Transaction,
    ) -> Self::TransactionalStorageContext
    where
        P: IntoIterator<Item = &'p [u8]>;

    /// Make batch storage context for a subtree with path
    fn get_batch_storage_context<'p, P>(
        &'db self,
        path: P,
        batch: &'db StorageBatch,
    ) -> Self::BatchStorageContext
    where
        P: IntoIterator<Item = &'p [u8]>;

    /// Make batch storage context for a subtree on transactional data
    fn get_batch_transactional_storage_context<'p, P>(
        &'db self,
        path: P,
        batch: &'db StorageBatch,
        transaction: &'db Self::Transaction,
    ) -> Self::BatchTransactionalStorageContext
    where
        P: IntoIterator<Item = &'p [u8]>;
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

/// Database batch (not to be confused with multi-tree operations batch).
pub trait Batch {
    /// Error type for failed operations on the batch.
    type Error: std::error::Error + Send + Sync + 'static;

    /// Appends to the database batch a put operation for a data record.
    fn put<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) -> Result<(), Self::Error>;

    /// Appends to the database batch a put operation for aux storage.
    fn put_aux<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) -> Result<(), Self::Error>;

    /// Appends to the database batch a put operation for subtrees roots
    /// storage.
    fn put_root<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) -> Result<(), Self::Error>;

    /// Appends to the database batch a delete operation for a data record.
    fn delete<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(), Self::Error>;

    /// Appends to the database batch a delete operation for aux storage.
    fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(), Self::Error>;

    /// Appends to the database batch a delete operation for a record in subtree
    /// roots storage.
    fn delete_root<K: AsRef<[u8]>>(&mut self, key: K) -> Result<(), Self::Error>;
}

/// Allows to iterate over database record inside of storage context.
pub trait RawIterator {
    /// Move iterator to first valid record.
    fn seek_to_first(&mut self);

    /// Move iterator to last valid record.
    fn seek_to_last(&mut self);

    /// Move iterator forward until `key` is hit.
    fn seek<K: AsRef<[u8]>>(&mut self, key: K);

    /// Move iterator backward until `key` is hit.
    fn seek_for_prev<K: AsRef<[u8]>>(&mut self, key: K);

    /// Move iterator to next record.
    fn next(&mut self);

    /// Move iterator to previous record.
    fn prev(&mut self);

    /// Return value of key-value pair where raw iterator points at.
    fn value(&self) -> Option<&[u8]>;

    /// Return key of key-value pair where raw iterator points at.
    fn key(&self) -> Option<&[u8]>;

    /// Check if raw iterator points into a valid record
    fn valid(&self) -> bool;
}

/// Structure to hold deferred database operations in "batched" storage
/// contexts.
pub struct StorageBatch {
    operations: RefCell<Vec<BatchOperation>>,
}

impl StorageBatch {
    /// Create empty batch.
    pub fn new() -> Self {
        StorageBatch {
            operations: RefCell::new(Vec::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.operations.borrow().len()
    }

    /// Add deferred `put` operation
    pub fn put(&self, key: Vec<u8>, value: Vec<u8>) {
        self.operations
            .borrow_mut()
            .push(BatchOperation::Put { key, value })
    }

    /// Add deferred `put` operation for aux storage
    pub fn put_aux(&self, key: Vec<u8>, value: Vec<u8>) {
        self.operations
            .borrow_mut()
            .push(BatchOperation::PutAux { key, value })
    }

    /// Add deferred `put` operation for subtree roots storage
    pub fn put_root(&self, key: Vec<u8>, value: Vec<u8>) {
        self.operations
            .borrow_mut()
            .push(BatchOperation::PutRoot { key, value })
    }

    /// Add deferred `put` operation for metadata storage
    pub fn put_meta(&self, key: Vec<u8>, value: Vec<u8>) {
        self.operations
            .borrow_mut()
            .push(BatchOperation::PutMeta { key, value })
    }

    /// Add deferred `delete` operation
    pub fn delete(&self, key: Vec<u8>) {
        self.operations
            .borrow_mut()
            .push(BatchOperation::Delete { key })
    }

    /// Add deferred `delete` operation for aux storage
    pub fn delete_aux(&self, key: Vec<u8>) {
        self.operations
            .borrow_mut()
            .push(BatchOperation::DeleteAux { key })
    }

    /// Add deferred `delete` operation for subtree roots storage
    pub fn delete_root(&self, key: Vec<u8>) {
        self.operations
            .borrow_mut()
            .push(BatchOperation::DeleteRoot { key })
    }

    /// Add deferred `delete` operation for metadata storage
    pub fn delete_meta(&self, key: Vec<u8>) {
        self.operations
            .borrow_mut()
            .push(BatchOperation::DeleteMeta { key })
    }

    /// Return borrowed operations vec
    pub fn borrow_ops(&self) -> Ref<Vec<BatchOperation>> {
        self.operations.borrow()
    }

    /// Consume batch to get an iterator over operations
    pub fn into_iter(self) -> IntoIter<BatchOperation> {
        self.operations.into_inner().into_iter()
    }

    /// Merge batch into this one
    pub fn merge(&self, other: StorageBatch) {
        self.operations.borrow_mut().extend(other.into_iter());
    }
}

impl Default for StorageBatch {
    fn default() -> Self {
        Self::new()
    }
}

/// Deferred storage operation.
#[allow(missing_docs)]
pub enum BatchOperation {
    /// Deferred put operation
    Put { key: Vec<u8>, value: Vec<u8> },
    /// Deferred put operation for aux storage
    PutAux { key: Vec<u8>, value: Vec<u8> },
    /// Deferred put operation for roots storage
    PutRoot { key: Vec<u8>, value: Vec<u8> },
    /// Deferred put operation for metadata storage
    PutMeta { key: Vec<u8>, value: Vec<u8> },
    /// Deferred delete operation
    Delete { key: Vec<u8> },
    /// Deferred delete operation for aux storage
    DeleteAux { key: Vec<u8> },
    /// Deferred delete operation for roots storage
    DeleteRoot { key: Vec<u8> },
    /// Deferred delete operation for metadata storage
    DeleteMeta { key: Vec<u8> },
}
