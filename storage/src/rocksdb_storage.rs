//! GroveDB storage layer implemented over RocksDB backend.
mod storage;
mod storage_context;
pub mod test_utils;
#[cfg(test)]
mod tests;

pub use rocksdb::Error;
pub use storage_context::{
    PrefixedRocksDbBatch, PrefixedRocksDbBatchStorageContext,
    PrefixedRocksDbBatchTransactionContext, PrefixedRocksDbRawIterator,
    PrefixedRocksDbStorageContext, PrefixedRocksDbTransactionContext,
};

pub use self::storage::RocksDbStorage;
