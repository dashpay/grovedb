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

/// Merge two 32-byte hashes by concatenating and hashing with Blake3.
pub(crate) fn blake3_merge(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut data = [0u8; 64];
    data[..32].copy_from_slice(left);
    data[32..].copy_from_slice(right);
    *blake3::hash(&data).as_bytes()
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

/// Compute the blake3 dense Merkle root from pre-computed leaf hashes.
///
/// The input slice length MUST be a power of 2 and non-empty.
/// Returns the root hash of a complete binary tree built bottom-up by
/// hashing pairs: `blake3(left || right)`.
pub fn compute_dense_merkle_root(leaf_hashes: &[[u8; 32]]) -> Result<[u8; 32], DenseMerkleError> {
    if leaf_hashes.is_empty() {
        return Err(DenseMerkleError::InvalidData(
            "leaf_hashes must not be empty".into(),
        ));
    }
    if !leaf_hashes.len().is_power_of_two() {
        return Err(DenseMerkleError::InvalidData(format!(
            "leaf_hashes length must be a power of 2, got {}",
            leaf_hashes.len()
        )));
    }

    let mut level: Vec<[u8; 32]> = leaf_hashes.to_vec();
    while level.len() > 1 {
        level = level
            .chunks(2)
            .map(|pair| blake3_merge(&pair[0], &pair[1]))
            .collect();
    }
    Ok(level[0])
}

/// Compute the blake3 dense Merkle root from raw values.
///
/// Hashes each value with blake3 first, then builds the dense Merkle tree.
/// The number of values MUST be a power of 2 and non-empty.
///
/// Returns `(root_hash, hash_count)` where `hash_count` is the total number
/// of blake3 hash calls made (n leaf hashes + n-1 internal hashes = 2n-1).
pub fn compute_dense_merkle_root_from_values(
    values: &[&[u8]],
) -> Result<([u8; 32], u32), DenseMerkleError> {
    if values.is_empty() {
        return Err(DenseMerkleError::InvalidData(
            "values must not be empty".into(),
        ));
    }
    if !values.len().is_power_of_two() {
        return Err(DenseMerkleError::InvalidData(format!(
            "values length must be a power of 2, got {}",
            values.len()
        )));
    }

    let leaf_hashes: Vec<[u8; 32]> = values.iter().map(|v| *blake3::hash(v).as_bytes()).collect();
    let n = leaf_hashes.len() as u32;
    let root = compute_dense_merkle_root(&leaf_hashes)?;
    Ok((root, 2 * n - 1)) // n leaf hashes + (n-1) internal hashes
}
