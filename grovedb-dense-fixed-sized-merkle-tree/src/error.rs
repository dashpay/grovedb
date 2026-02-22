use thiserror::Error;

/// Errors from dense Merkle tree operations.
#[derive(Debug, Error)]
pub enum DenseMerkleError {
    #[error("invalid data: {0}")]
    InvalidData(String),
    #[error("tree is full (capacity {capacity}, count {count})")]
    TreeFull { capacity: u16, count: u16 },
    #[error("store error: {0}")]
    StoreError(String),
    #[error("invalid proof: {0}")]
    InvalidProof(String),
}
