//! Implementation of prefixed storage context.
mod batch;
mod context_batch_no_tx;
mod context_batch_tx;
mod context_no_tx;
mod context_tx;
mod raw_iterator;

pub use batch::PrefixedRocksDbBatch;
pub use context_batch_no_tx::PrefixedRocksDbBatchStorageContext;
pub use context_batch_tx::PrefixedRocksDbBatchTransactionContext;
pub use context_no_tx::PrefixedRocksDbStorageContext;
pub use context_tx::PrefixedRocksDbTransactionContext;
pub use raw_iterator::PrefixedRocksDbRawIterator;

pub fn make_prefixed_key<K: AsRef<[u8]>>(mut prefix: Vec<u8>, key: K) -> Vec<u8> {
    prefix.extend_from_slice(key.as_ref());
    prefix
}
