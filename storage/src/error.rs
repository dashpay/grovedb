//! Cost Errors File

/// Storage and underlying errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Cost Error
    #[error("cost error")]
    CostError(costs::error::Error),
    /// Rocks DB error
    #[error("rocksDB error")]
    RocksDBError(rocksdb::Error),
}
