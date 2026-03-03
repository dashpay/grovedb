use thiserror::Error;

/// Errors from dense Merkle tree operations.
#[derive(Debug, Error)]
pub enum DenseMerkleError {
    /// The input data is malformed or out of range.
    #[error("invalid data: {0}")]
    InvalidData(String),
    /// The tree has reached its maximum capacity and cannot accept more
    /// inserts.
    #[error("tree is full (capacity {capacity}, count {count})")]
    TreeFull {
        /// Maximum number of positions the tree can hold.
        capacity: u16,
        /// Current number of filled positions.
        count: u16,
    },
    /// An error from the underlying storage layer.
    #[error("store error: {0}")]
    StoreError(String),
    /// A proof is structurally invalid or fails verification.
    #[error("invalid proof: {0}")]
    InvalidProof(String),
}
