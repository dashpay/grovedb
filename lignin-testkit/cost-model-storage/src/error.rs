//! The errors of the storage layer: the enum of `storage/src/error.rs`,
//! with the model's stand-in where that one holds a RocksDB error.

/// Storage and underlying errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Storage Error
    #[error("storage error: {0}")]
    StorageError(String),
    /// Cost Error
    #[error("cost error: {0}")]
    CostError(grovedb_costs::error::Error),
    /// A write or commit was attempted through a snapshot read transaction.
    #[error("snapshot read transaction is read-only: refused {0}")]
    SnapshotReadOnlyTransaction(&'static str),
    /// What the engine under the storage layer refused.
    #[error("rocksDB error: {0}")]
    #[cfg(feature = "rocksdb_storage")]
    RocksDBError(#[from] crate::rocksdb_storage::Error),
}
