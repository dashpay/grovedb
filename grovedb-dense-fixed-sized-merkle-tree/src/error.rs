use thiserror::Error;

/// Errors from dense Merkle tree operations.
#[derive(Debug, Error)]
pub enum DenseMerkleError {
    #[error("invalid data: {0}")]
    InvalidData(String),
    #[error("tree is full (capacity {capacity}, count {count})")]
    TreeFull { capacity: u64, count: u64 },
    #[error("store error: {0}")]
    StoreError(String),
    #[error("invalid proof: {0}")]
    InvalidProof(String),
}

/// Validate that height is in the allowed range [1, 63].
pub(crate) fn validate_height(height: u8) -> Result<(), DenseMerkleError> {
    if !(1..=63).contains(&height) {
        return Err(DenseMerkleError::InvalidData(format!(
            "height must be between 1 and 63, got {}",
            height
        )));
    }
    Ok(())
}
