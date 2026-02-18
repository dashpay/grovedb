//! Error types for BulkAppendTree operations.

use thiserror::Error;

/// Errors from BulkAppendTree operations.
#[derive(Debug, Error)]
pub enum BulkAppendError {
    #[error("corrupted data: {0}")]
    CorruptedData(String),
    #[error("storage error: {0}")]
    StorageError(String),
    #[error("MMR error: {0}")]
    MmrError(String),
    #[error("invalid proof: {0}")]
    InvalidProof(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}
