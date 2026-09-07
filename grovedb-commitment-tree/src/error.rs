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
    /// A checkpoint id was not greater than the current maximum checkpoint
    /// id. Checkpoint ids must be strictly increasing; the operation was
    /// refused before any state was modified.
    #[error(
        "checkpoint id {provided} is not greater than the current maximum checkpoint id {max}"
    )]
    CheckpointOutOfOrder {
        /// The checkpoint id supplied by the caller.
        provided: u32,
        /// The current maximum checkpoint id in the tree.
        max: u32,
    },

    /// An unknown grove version was supplied for a versioned accounting
    /// decision.
    #[error("version error: {0}")]
    VersionError(String),
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
