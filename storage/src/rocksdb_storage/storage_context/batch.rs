//! Prefixed storage batch implementation for RocksDB backend.
use std::convert::Infallible;

use rocksdb::{ColumnFamily, WriteBatchWithTransaction};

use super::{make_prefixed_key, PrefixedRocksDbTransactionContext};
use crate::{Batch, BatchOperation, StorageBatch, StorageContext};

/// Wrapper to RocksDB batch
pub struct PrefixedRocksDbBatch<'db, B> {
    pub(crate) prefix: Vec<u8>,
    pub(crate) batch: B,
    pub(crate) cf_aux: &'db ColumnFamily,
    pub(crate) cf_roots: &'db ColumnFamily,
}

/// Batch with no backing storage that eventually will be merged into
/// multi-context batch.
pub struct PrefixedMultiContextBatchPart {
    pub(crate) prefix: Vec<u8>,
    pub(crate) batch: StorageBatch,
}

/// Batch used in transactional context (because RocksDB transactions doens't
/// support its batches)
pub struct DummyBatch {
    pub operations: Vec<BatchOperation>,
}

impl Default for DummyBatch {
    fn default() -> Self {
        DummyBatch {
            operations: Vec::new(),
        }
    }
}

impl Batch for DummyBatch {
    fn put<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        self.operations.push(BatchOperation::Put {
            key: key.as_ref().to_vec(),
            value: value.to_vec(),
        });
    }

    fn put_aux<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        self.operations.push(BatchOperation::PutAux {
            key: key.as_ref().to_vec(),
            value: value.to_vec(),
        });
    }

    fn put_root<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        self.operations.push(BatchOperation::PutRoot {
            key: key.as_ref().to_vec(),
            value: value.to_vec(),
        });
    }

    fn delete<K: AsRef<[u8]>>(&mut self, key: K) {
        self.operations.push(BatchOperation::Delete {
            key: key.as_ref().to_vec(),
        });
    }

    fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K) {
        self.operations.push(BatchOperation::DeleteAux {
            key: key.as_ref().to_vec(),
        });
    }

    fn delete_root<K: AsRef<[u8]>>(&mut self, key: K) {
        self.operations.push(BatchOperation::DeleteRoot {
            key: key.as_ref().to_vec(),
        });
    }
}

/// Implementation of a batch ouside a transaction
impl<'db> Batch for PrefixedRocksDbBatch<'db, WriteBatchWithTransaction<true>> {
    fn put<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        self.batch
            .put(make_prefixed_key(self.prefix.clone(), key), value);
    }

    fn put_aux<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        self.batch.put_cf(
            self.cf_aux,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        );
    }

    fn put_root<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        self.batch.put_cf(
            self.cf_roots,
            make_prefixed_key(self.prefix.clone(), key),
            value,
        );
    }

    fn delete<K: AsRef<[u8]>>(&mut self, key: K) {
        self.batch
            .delete(make_prefixed_key(self.prefix.clone(), key));
    }

    fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K) {
        self.batch
            .delete_cf(self.cf_aux, make_prefixed_key(self.prefix.clone(), key));
    }

    fn delete_root<K: AsRef<[u8]>>(&mut self, key: K) {
        self.batch
            .delete_cf(self.cf_roots, make_prefixed_key(self.prefix.clone(), key));
    }
}

/// Implementation of a batch inside a transaction.
/// Basically just proxies all calls to the underlying transaction. TODO: that's wrong!
impl<'ctx, 'db> Batch for &'ctx PrefixedRocksDbTransactionContext<'db> {
    fn put<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        StorageContext::put(*self, key, value)
            .unwrap()
            .expect("TODO: batch append should not fail")
    }

    fn put_aux<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        StorageContext::put_aux(*self, key, value)
            .unwrap()
            .expect("TODO: batch append should not fail")
    }

    fn put_root<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        StorageContext::put_root(*self, key, value)
            .unwrap()
            .expect("TODO: batch append should not fail")
    }

    fn delete<K: AsRef<[u8]>>(&mut self, key: K) {
        StorageContext::delete(*self, key)
            .unwrap()
            .expect("TODO: batch append should not fail")
    }

    fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K) {
        StorageContext::delete_aux(*self, key)
            .unwrap()
            .expect("TODO: batch append should not fail")
    }

    fn delete_root<K: AsRef<[u8]>>(&mut self, key: K) {
        StorageContext::delete_root(*self, key)
            .unwrap()
            .expect("TODO: batch append should not fail")
    }
}

/// Implementation of a rocksdb batch ouside a transaction for multi-context
/// batch
impl Batch for PrefixedMultiContextBatchPart {
    fn put<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        self.batch
            .put(make_prefixed_key(self.prefix.clone(), key), value.to_vec());
    }

    fn put_aux<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        self.batch
            .put_aux(make_prefixed_key(self.prefix.clone(), key), value.to_vec());
    }

    fn put_root<K: AsRef<[u8]>>(&mut self, key: K, value: &[u8]) {
        self.batch
            .put_root(make_prefixed_key(self.prefix.clone(), key), value.to_vec());
    }

    fn delete<K: AsRef<[u8]>>(&mut self, key: K) {
        self.batch
            .delete(make_prefixed_key(self.prefix.clone(), key));
    }

    fn delete_aux<K: AsRef<[u8]>>(&mut self, key: K) {
        self.batch
            .delete_aux(make_prefixed_key(self.prefix.clone(), key));
    }

    fn delete_root<K: AsRef<[u8]>>(&mut self, key: K) {
        self.batch
            .delete_root(make_prefixed_key(self.prefix.clone(), key));
    }
}
