//! Dense Merkle tree root computation using Blake3.

use crate::{node::blake3_merge, MmrError};

/// Compute the blake3 dense Merkle root from pre-computed leaf hashes.
///
/// The input slice length MUST be a power of 2 and non-empty.
/// Returns the root hash of a complete binary tree built bottom-up by
/// hashing pairs: `blake3(left || right)`.
pub fn compute_dense_merkle_root(leaf_hashes: &[[u8; 32]]) -> Result<[u8; 32], MmrError> {
    if leaf_hashes.is_empty() {
        return Err(MmrError::InvalidData(
            "leaf_hashes must not be empty".into(),
        ));
    }
    if !leaf_hashes.len().is_power_of_two() {
        return Err(MmrError::InvalidData(format!(
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
) -> Result<([u8; 32], u32), MmrError> {
    if values.is_empty() {
        return Err(MmrError::InvalidData("values must not be empty".into()));
    }
    if !values.len().is_power_of_two() {
        return Err(MmrError::InvalidData(format!(
            "values length must be a power of 2, got {}",
            values.len()
        )));
    }

    let leaf_hashes: Vec<[u8; 32]> = values.iter().map(|v| *blake3::hash(v).as_bytes()).collect();
    let n = leaf_hashes.len() as u32;
    let root = compute_dense_merkle_root(&leaf_hashes)?;
    Ok((root, 2 * n - 1)) // n leaf hashes + (n-1) internal hashes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dense_merkle_root_single_leaf() {
        let leaf_hash = *blake3::hash(b"only").as_bytes();
        let root = compute_dense_merkle_root(&[leaf_hash]).expect("single leaf root");
        assert_eq!(root, leaf_hash);
    }

    #[test]
    fn test_dense_merkle_root_two_leaves() {
        let h0 = *blake3::hash(b"a").as_bytes();
        let h1 = *blake3::hash(b"b").as_bytes();
        let root = compute_dense_merkle_root(&[h0, h1]).expect("two leaf root");
        let expected = blake3_merge(&h0, &h1);
        assert_eq!(root, expected);
    }

    #[test]
    fn test_dense_merkle_root_four_leaves() {
        let hashes: Vec<[u8; 32]> = (0..4u8).map(|i| *blake3::hash(&[i]).as_bytes()).collect();
        let root = compute_dense_merkle_root(&hashes).expect("four leaf root");

        let left = blake3_merge(&hashes[0], &hashes[1]);
        let right = blake3_merge(&hashes[2], &hashes[3]);
        let expected = blake3_merge(&left, &right);
        assert_eq!(root, expected);
    }

    #[test]
    fn test_dense_merkle_root_deterministic() {
        let hashes: Vec<[u8; 32]> = (0..8u8).map(|i| *blake3::hash(&[i]).as_bytes()).collect();
        let root1 = compute_dense_merkle_root(&hashes).expect("deterministic root 1");
        let root2 = compute_dense_merkle_root(&hashes).expect("deterministic root 2");
        assert_eq!(root1, root2);
    }

    #[test]
    fn test_dense_merkle_root_different_inputs() {
        let h1: Vec<[u8; 32]> = (0..4u8).map(|i| *blake3::hash(&[i]).as_bytes()).collect();
        let h2: Vec<[u8; 32]> = (10..14u8).map(|i| *blake3::hash(&[i]).as_bytes()).collect();
        assert_ne!(
            compute_dense_merkle_root(&h1).expect("root for h1"),
            compute_dense_merkle_root(&h2).expect("root for h2")
        );
    }

    #[test]
    fn test_dense_merkle_root_from_values() {
        let values: Vec<&[u8]> = vec![b"alpha", b"beta", b"gamma", b"delta"];
        let (root, hash_count) =
            compute_dense_merkle_root_from_values(&values).expect("root from values");
        assert_eq!(hash_count, 7); // 4 leaf + 3 internal

        // Should match computing leaf hashes first then calling the other function
        let leaf_hashes: Vec<[u8; 32]> =
            values.iter().map(|v| *blake3::hash(v).as_bytes()).collect();
        let expected = compute_dense_merkle_root(&leaf_hashes).expect("root from leaf hashes");
        assert_eq!(root, expected);
    }

    #[test]
    fn test_dense_merkle_root_large() {
        let hashes: Vec<[u8; 32]> = (0..1024u32)
            .map(|i| *blake3::hash(&i.to_be_bytes()).as_bytes())
            .collect();
        let root = compute_dense_merkle_root(&hashes).expect("large root");
        // Just verify it produces a non-zero result and is deterministic
        assert_ne!(root, [0u8; 32]);
        assert_eq!(
            root,
            compute_dense_merkle_root(&hashes).expect("large root again")
        );
    }

    #[test]
    fn test_dense_merkle_root_empty_error() {
        let result = compute_dense_merkle_root(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn test_dense_merkle_root_non_power_of_two_error() {
        let hashes: Vec<[u8; 32]> = (0..3u8).map(|i| *blake3::hash(&[i]).as_bytes()).collect();
        let result = compute_dense_merkle_root(&hashes);
        assert!(result.is_err());
    }

    #[test]
    fn test_dense_merkle_root_from_values_empty_error() {
        let result = compute_dense_merkle_root_from_values(&[]);
        assert!(result.is_err());
    }
}
