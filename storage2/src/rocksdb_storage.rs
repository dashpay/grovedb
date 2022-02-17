//! GroveDB storage layer implemented over RocksDB backend.

/// Implementation of Storage trait.
mod storage;
/// Implementation of StorageContext trait.
mod storage_context;

pub use storage::RocksDbStorage;
pub use storage_context::PrefixedRocksDbStorageContext;
