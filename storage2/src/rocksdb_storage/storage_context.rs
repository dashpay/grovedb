//! Implementation of prefixed storage context.
mod batch;
mod context_no_tx;
mod context_tx;
mod raw_iterator;

use batch::PrefixedRocksDbBatch;
pub use context_no_tx::PrefixedRocksDbStorageContext;
pub use context_tx::PrefixedRocksDbTransactionContext;
use raw_iterator::PrefixedRocksDbRawIterator;
use rocksdb::OptimisticTransactionDB;

/// Type alias for a database
type Db = OptimisticTransactionDB;

fn make_prefixed_key<K: AsRef<[u8]>>(mut prefix: Vec<u8>, key: K) -> Vec<u8> {
    prefix.extend_from_slice(key.as_ref());
    prefix
}
