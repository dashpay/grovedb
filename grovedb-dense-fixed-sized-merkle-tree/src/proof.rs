//! Inclusion proof generation for the dense fixed-sized Merkle tree.
//!
//! A `DenseTreeProof` proves that specific positions hold specific values,
//! authenticated against the tree's root hash.
//!
//! Because internal nodes hash their OWN value (not just child hashes), the
//! auth path must include ancestor values in addition to sibling subtree
//! hashes.

use std::collections::BTreeSet;

use bincode::{Decode, Encode};

use crate::{DenseMerkleError, DenseTreeStore};

/// An inclusion proof for one or more positions in a dense fixed-sized Merkle
/// tree.
///
/// Fields are `pub(crate)` to prevent external construction of proofs that
/// bypass generation. Use [`generate`](DenseTreeProof::generate) to create
/// proofs and [`decode_from_slice`](DenseTreeProof::decode_from_slice) to
/// deserialize them.
#[derive(Debug, Clone, Encode, Decode)]
pub struct DenseTreeProof {
    /// Height of the tree (capacity = 2^height - 1).
    pub(crate) height: u8,
    /// Number of filled positions.
    pub(crate) count: u64,
    /// The proved (position, value) pairs.
    pub(crate) entries: Vec<(u64, Vec<u8>)>,
    /// Ancestor node values on the auth path that are NOT proved entries.
    pub(crate) node_values: Vec<(u64, Vec<u8>)>,
    /// Precomputed subtree hashes for sibling nodes not in the expanded set.
    pub(crate) node_hashes: Vec<(u64, [u8; 32])>,
}

impl DenseTreeProof {
    /// Number of proved entries in this proof.
    pub fn entries_len(&self) -> usize {
        self.entries.len()
    }

    /// Returns the `(height, count)` stored in this proof.
    ///
    /// Used by callers to cross-validate against authenticated element state.
    pub fn height_and_count(&self) -> (u8, u64) {
        (self.height, self.count)
    }

    /// Generate a proof for the given positions.
    ///
    /// Positions must be < count. Duplicates are deduplicated.
    pub fn generate<S: DenseTreeStore>(
        height: u8,
        count: u64,
        positions: &[u64],
        store: &S,
    ) -> Result<Self, DenseMerkleError> {
        // Validate height before the shift to avoid panic on height >= 64
        crate::error::validate_height(height)?;
        let capacity = (1u64 << height) - 1;

        // Validate positions
        for &pos in positions {
            if pos >= count {
                return Err(DenseMerkleError::InvalidProof(format!(
                    "position {} is out of range (count={})",
                    pos, count
                )));
            }
        }

        // Deduplicate
        let proved_set: BTreeSet<u64> = positions.iter().copied().collect();

        // Build expanded set: proved positions + all ancestors up to root
        let mut expanded: BTreeSet<u64> = proved_set.clone();
        for &pos in &proved_set {
            let mut p = pos;
            while p > 0 {
                p = (p - 1) / 2; // parent
                expanded.insert(p);
            }
        }

        // Collect entries, node_values, node_hashes
        let mut entries: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut node_values: Vec<(u64, Vec<u8>)> = Vec::new();
        let mut node_hashes: Vec<(u64, [u8; 32])> = Vec::new();

        // Use from_state to get a tree object for hash_position
        let tree = crate::tree::DenseFixedSizedMerkleTree::from_state(height, count)?;

        for &pos in &expanded {
            // Get the value for this position
            let value = store.get_value(pos)?.ok_or_else(|| {
                DenseMerkleError::StoreError(format!(
                    "expected value at position {} but found none",
                    pos
                ))
            })?;

            if proved_set.contains(&pos) {
                entries.push((pos, value));
            } else {
                node_values.push((pos, value));
            }

            // For each child of this position that is NOT in the expanded set,
            // compute its hash and include it.
            // Check leaf condition before computing child indices to avoid
            // u64 overflow for large heights.
            let first_leaf = (capacity - 1) / 2;
            if pos < first_leaf {
                let left_child = 2 * pos + 1;
                let right_child = 2 * pos + 2;

                if !expanded.contains(&left_child) {
                    let (hash, _) = tree.hash_position(left_child, store)?;
                    node_hashes.push((left_child, hash));
                }
                if !expanded.contains(&right_child) {
                    let (hash, _) = tree.hash_position(right_child, store)?;
                    node_hashes.push((right_child, hash));
                }
            }
        }

        Ok(DenseTreeProof {
            height,
            count,
            entries,
            node_values,
            node_hashes,
        })
    }

    /// Encode to bytes using bincode.
    pub fn encode_to_vec(&self) -> Result<Vec<u8>, DenseMerkleError> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        bincode::encode_to_vec(self, config)
            .map_err(|e| DenseMerkleError::InvalidProof(format!("encode error: {}", e)))
    }

    /// Decode from bytes using bincode.
    ///
    /// Validates that the decoded height is in [1, 63] to prevent overflow.
    pub fn decode_from_slice(bytes: &[u8]) -> Result<Self, DenseMerkleError> {
        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 100 * 1024 * 1024 }>(); // 100MB limit
        let (proof, _): (Self, _) = bincode::decode_from_slice(bytes, config)
            .map_err(|e| DenseMerkleError::InvalidProof(format!("decode error: {}", e)))?;
        if !(1..=63).contains(&proof.height) {
            return Err(DenseMerkleError::InvalidProof(format!(
                "invalid height {} in proof (must be 1..=63)",
                proof.height
            )));
        }
        let capacity = (1u64 << proof.height) - 1;
        if proof.count > capacity {
            return Err(DenseMerkleError::InvalidProof(format!(
                "count {} exceeds capacity {} for height {}",
                proof.count, capacity, proof.height
            )));
        }
        Ok(proof)
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::HashMap};

    use super::*;

    /// In-memory store for testing.
    struct MemStore {
        data: RefCell<HashMap<u64, Vec<u8>>>,
    }

    impl MemStore {
        fn new() -> Self {
            Self {
                data: RefCell::new(HashMap::new()),
            }
        }
    }

    impl DenseTreeStore for MemStore {
        fn get_value(&self, position: u64) -> Result<Option<Vec<u8>>, DenseMerkleError> {
            Ok(self.data.borrow().get(&position).cloned())
        }

        fn put_value(&self, position: u64, value: &[u8]) -> Result<(), DenseMerkleError> {
            self.data.borrow_mut().insert(position, value.to_vec());
            Ok(())
        }
    }

    fn make_tree_h3_full() -> (MemStore, [u8; 32]) {
        let store = MemStore::new();
        let mut tree =
            crate::tree::DenseFixedSizedMerkleTree::new(3).expect("height 3 should be valid");
        let mut root = [0u8; 32];
        for i in 0..7u8 {
            let (h, ..) = tree.insert(&[i], &store).expect("insert should succeed");
            root = h;
        }
        (store, root)
    }

    #[test]
    fn test_proof_single_leaf() {
        let (store, root) = make_tree_h3_full();
        let proof = DenseTreeProof::generate(3, 7, &[4], &store).expect("generate should succeed");
        assert_eq!(proof.entries.len(), 1);
        assert_eq!(proof.entries[0].0, 4);

        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0], (4, vec![4u8]));
    }

    #[test]
    fn test_proof_internal_node() {
        let (store, root) = make_tree_h3_full();
        // Prove position 1 (internal node)
        let proof = DenseTreeProof::generate(3, 7, &[1], &store).expect("generate should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0], (1, vec![1u8]));
    }

    #[test]
    fn test_proof_root_node() {
        let (store, root) = make_tree_h3_full();
        let proof = DenseTreeProof::generate(3, 7, &[0], &store).expect("generate should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0], (0, vec![0u8]));
    }

    #[test]
    fn test_proof_multiple_positions() {
        let (store, root) = make_tree_h3_full();
        let proof =
            DenseTreeProof::generate(3, 7, &[3, 5, 6], &store).expect("generate should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 3);
        // entries are sorted by position
        assert_eq!(verified[0], (3, vec![3u8]));
        assert_eq!(verified[1], (5, vec![5u8]));
        assert_eq!(verified[2], (6, vec![6u8]));
    }

    #[test]
    fn test_proof_all_positions() {
        let (store, root) = make_tree_h3_full();
        let proof = DenseTreeProof::generate(3, 7, &[0, 1, 2, 3, 4, 5, 6], &store)
            .expect("generate should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 7);
        for (pos, val) in &verified {
            assert_eq!(*val, vec![*pos as u8]);
        }
    }

    #[test]
    fn test_proof_wrong_root_fails() {
        let (store, _root) = make_tree_h3_full();
        let proof = DenseTreeProof::generate(3, 7, &[4], &store).expect("generate should succeed");
        let wrong_root = [0xFFu8; 32];
        let result = proof.verify(&wrong_root);
        assert!(result.is_err(), "verification should fail with wrong root");
    }

    #[test]
    fn test_proof_encode_decode_roundtrip() {
        let (store, root) = make_tree_h3_full();
        let proof =
            DenseTreeProof::generate(3, 7, &[2, 4], &store).expect("generate should succeed");
        let bytes = proof.encode_to_vec().expect("encode should succeed");
        let decoded = DenseTreeProof::decode_from_slice(&bytes).expect("decode should succeed");
        let verified = decoded.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 2);
    }

    #[test]
    fn test_proof_partially_filled_tree() {
        let store = MemStore::new();
        let mut tree =
            crate::tree::DenseFixedSizedMerkleTree::new(3).expect("height 3 should be valid");
        // Insert only 3 out of 7 positions
        for i in 0..3u8 {
            tree.insert(&[i], &store).expect("insert should succeed");
        }
        let (root, _) = tree.root_hash(&store).expect("root hash should succeed");

        let proof =
            DenseTreeProof::generate(3, 3, &[0, 1, 2], &store).expect("generate should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 3);
    }

    #[test]
    fn test_proof_height_1_tree() {
        let store = MemStore::new();
        let mut tree =
            crate::tree::DenseFixedSizedMerkleTree::new(1).expect("height 1 should be valid");
        tree.insert(b"hello", &store)
            .expect("insert should succeed");
        let (root, _) = tree.root_hash(&store).expect("root hash should succeed");

        let proof = DenseTreeProof::generate(1, 1, &[0], &store).expect("generate should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0], (0, b"hello".to_vec()));
    }

    #[test]
    fn test_proof_height_2_tree() {
        let store = MemStore::new();
        let mut tree =
            crate::tree::DenseFixedSizedMerkleTree::new(2).expect("height 2 should be valid");
        tree.insert(b"root_val", &store)
            .expect("insert should succeed");
        tree.insert(b"left_val", &store)
            .expect("insert should succeed");
        tree.insert(b"right_val", &store)
            .expect("insert should succeed");
        let (root, _) = tree.root_hash(&store).expect("root hash should succeed");

        // Prove leaf position 2
        let proof = DenseTreeProof::generate(2, 3, &[2], &store).expect("generate should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0], (2, b"right_val".to_vec()));
    }

    #[test]
    fn test_proof_position_out_of_range() {
        let (store, _root) = make_tree_h3_full();
        let result = DenseTreeProof::generate(3, 7, &[7], &store);
        assert!(result.is_err(), "position 7 should be out of range");
    }

    #[test]
    fn test_proof_deduplicates_positions() {
        let (store, root) = make_tree_h3_full();
        let proof =
            DenseTreeProof::generate(3, 7, &[4, 4, 4], &store).expect("generate should succeed");
        assert_eq!(proof.entries.len(), 1);
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 1);
    }
}
