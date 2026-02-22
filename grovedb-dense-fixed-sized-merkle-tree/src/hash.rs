use crate::DenseMerkleError;

/// Validate that height is in the allowed range [1, 16].
pub(crate) fn validate_height(height: u8) -> Result<(), DenseMerkleError> {
    if !(1..=16).contains(&height) {
        return Err(DenseMerkleError::InvalidData(format!(
            "height must be between 1 and 16, got {}",
            height
        )));
    }
    Ok(())
}

/// Compute the hash of a node: `blake3(H(value) || H(left) || H(right))`.
///
/// All nodes use the same scheme — leaf nodes simply have `[0; 32]` for
/// both child hashes.
pub(crate) fn node_hash(
    value_hash: &[u8; 32],
    left_hash: &[u8; 32],
    right_hash: &[u8; 32],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(value_hash);
    hasher.update(left_hash);
    hasher.update(right_hash);
    *hasher.finalize().as_bytes()
}
