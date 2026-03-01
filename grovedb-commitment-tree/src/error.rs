use orchard::NOTE_COMMITMENT_TREE_DEPTH;
use thiserror::Error;

/// Errors that can occur during commitment tree operations.
#[derive(Debug, Error)]
pub enum CommitmentTreeError {
    /// The commitment tree has reached its maximum capacity (2^32 leaves).
    #[error("tree is full (max {max} leaves)", max = 1u64 << NOTE_COMMITMENT_TREE_DEPTH)]
    TreeFull,
    /// Data read from storage is invalid or corrupt.
    #[error("invalid frontier data: {0}")]
    InvalidData(String),
    /// A 32-byte value is not a valid Pallas field element.
    #[error("invalid Pallas field element")]
    InvalidFieldElement,
    /// The ciphertext payload length does not match the expected size for the
    /// configured `MemoSize`.
    #[error("invalid payload size: expected {expected}, got {actual}")]
    InvalidPayloadSize {
        /// Expected payload byte length.
        expected: usize,
        /// Actual payload byte length received.
        actual: usize,
    },
}
