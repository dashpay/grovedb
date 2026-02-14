//! Dense Merkle tree root computation using Blake3.

/// Compute the blake3 dense Merkle root from pre-computed leaf hashes.
///
/// The input slice length MUST be a power of 2.
/// Returns the root hash of a complete binary tree built bottom-up by
/// hashing pairs: `blake3(left || right)`.
///
/// Panics if `leaf_hashes` is empty or not a power of 2.
pub fn compute_dense_merkle_root(leaf_hashes: &[[u8; 32]]) -> [u8; 32] {
    assert!(!leaf_hashes.is_empty(), "leaf_hashes must not be empty");
    assert!(
        leaf_hashes.len().is_power_of_two(),
        "leaf_hashes length must be a power of 2, got {}",
        leaf_hashes.len()
    );

    let mut level: Vec<[u8; 32]> = leaf_hashes.to_vec();
    while level.len() > 1 {
        level = level
            .chunks(2)
            .map(|pair| {
                let mut input = [0u8; 64];
                input[..32].copy_from_slice(&pair[0]);
                input[32..].copy_from_slice(&pair[1]);
                *blake3::hash(&input).as_bytes()
            })
            .collect();
    }
    level[0]
}

/// Compute the blake3 dense Merkle root from raw values.
///
/// Hashes each value with blake3 first, then builds the dense Merkle tree.
/// The number of values MUST be a power of 2.
///
/// Returns `(root_hash, hash_count)` where `hash_count` is the total number
/// of blake3 hash calls made (n leaf hashes + n-1 internal hashes = 2n-1).
pub fn compute_dense_merkle_root_from_values(values: &[&[u8]]) -> ([u8; 32], u32) {
    assert!(!values.is_empty(), "values must not be empty");
    assert!(
        values.len().is_power_of_two(),
        "values length must be a power of 2, got {}",
        values.len()
    );

    let leaf_hashes: Vec<[u8; 32]> = values.iter().map(|v| *blake3::hash(v).as_bytes()).collect();
    let n = leaf_hashes.len() as u32;
    let root = compute_dense_merkle_root(&leaf_hashes);
    (root, 2 * n - 1) // n leaf hashes + (n-1) internal hashes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dense_merkle_root_single_leaf() {
        let leaf_hash = *blake3::hash(b"only").as_bytes();
        let root = compute_dense_merkle_root(&[leaf_hash]);
        assert_eq!(root, leaf_hash);
    }

    #[test]
    fn test_dense_merkle_root_two_leaves() {
        let h0 = *blake3::hash(b"a").as_bytes();
        let h1 = *blake3::hash(b"b").as_bytes();
        let root = compute_dense_merkle_root(&[h0, h1]);

        // Manually compute expected: blake3(h0 || h1)
        let mut input = [0u8; 64];
        input[..32].copy_from_slice(&h0);
        input[32..].copy_from_slice(&h1);
        let expected = *blake3::hash(&input).as_bytes();
        assert_eq!(root, expected);
    }

    #[test]
    fn test_dense_merkle_root_four_leaves() {
        let hashes: Vec<[u8; 32]> = (0..4u8).map(|i| *blake3::hash(&[i]).as_bytes()).collect();
        let root = compute_dense_merkle_root(&hashes);

        // Manual: ((h0, h1), (h2, h3))
        let merge = |a: &[u8; 32], b: &[u8; 32]| -> [u8; 32] {
            let mut input = [0u8; 64];
            input[..32].copy_from_slice(a);
            input[32..].copy_from_slice(b);
            *blake3::hash(&input).as_bytes()
        };
        let left = merge(&hashes[0], &hashes[1]);
        let right = merge(&hashes[2], &hashes[3]);
        let expected = merge(&left, &right);
        assert_eq!(root, expected);
    }

    #[test]
    fn test_dense_merkle_root_deterministic() {
        let hashes: Vec<[u8; 32]> = (0..8u8).map(|i| *blake3::hash(&[i]).as_bytes()).collect();
        let root1 = compute_dense_merkle_root(&hashes);
        let root2 = compute_dense_merkle_root(&hashes);
        assert_eq!(root1, root2);
    }

    #[test]
    fn test_dense_merkle_root_different_inputs() {
        let h1: Vec<[u8; 32]> = (0..4u8).map(|i| *blake3::hash(&[i]).as_bytes()).collect();
        let h2: Vec<[u8; 32]> = (10..14u8).map(|i| *blake3::hash(&[i]).as_bytes()).collect();
        assert_ne!(
            compute_dense_merkle_root(&h1),
            compute_dense_merkle_root(&h2)
        );
    }

    #[test]
    fn test_dense_merkle_root_from_values() {
        let values: Vec<&[u8]> = vec![b"alpha", b"beta", b"gamma", b"delta"];
        let (root, hash_count) = compute_dense_merkle_root_from_values(&values);
        assert_eq!(hash_count, 7); // 4 leaf + 3 internal

        // Should match computing leaf hashes first then calling the other function
        let leaf_hashes: Vec<[u8; 32]> =
            values.iter().map(|v| *blake3::hash(v).as_bytes()).collect();
        let expected = compute_dense_merkle_root(&leaf_hashes);
        assert_eq!(root, expected);
    }

    #[test]
    fn test_dense_merkle_root_large() {
        let hashes: Vec<[u8; 32]> = (0..1024u32)
            .map(|i| *blake3::hash(&i.to_be_bytes()).as_bytes())
            .collect();
        let root = compute_dense_merkle_root(&hashes);
        // Just verify it produces a non-zero result and is deterministic
        assert_ne!(root, [0u8; 32]);
        assert_eq!(root, compute_dense_merkle_root(&hashes));
    }
}
