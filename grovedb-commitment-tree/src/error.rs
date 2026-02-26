use orchard::NOTE_COMMITMENT_TREE_DEPTH;
use thiserror::Error;

/// Errors that can occur during commitment tree operations.
#[derive(Debug, Error)]
pub enum CommitmentTreeError {
    #[error("tree is full (max {max} leaves)", max = 1u64 << NOTE_COMMITMENT_TREE_DEPTH)]
    TreeFull,
    #[error("invalid frontier data: {0}")]
    InvalidData(String),
    #[error("invalid Pallas field element")]
    InvalidFieldElement,
    #[error("invalid payload size: expected {expected}, got {actual}")]
    InvalidPayloadSize { expected: usize, actual: usize },
}
