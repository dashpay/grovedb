//! Cost Errors File

/// Storage and underlying errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Cost Error
    #[error("cost error: {0}")]
    CostError(costs::error::Error),
    /// Rocks DB error
    #[error("rocksDB error: {0}")]
    #[cfg(feature = "rocksdb_storage")]
    RocksDBError(#[from] rocksdb::Error),
}
