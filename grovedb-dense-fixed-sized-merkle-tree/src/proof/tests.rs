#[cfg(test)]
mod proof_tests {
    use std::{cell::RefCell, collections::HashMap};

    use grovedb_costs::{CostsExt, OperationCost};

    use crate::proof::*;

    /// In-memory store for testing.
    struct MemStore {
        data: RefCell<HashMap<u16, Vec<u8>>>,
    }

    impl MemStore {
        fn new() -> Self {
            Self {
                data: RefCell::new(HashMap::new()),
            }
        }
    }

    impl DenseTreeStore for MemStore {
        fn get_value(&self, position: u16) -> CostResult<Option<Vec<u8>>, DenseMerkleError> {
            Ok(self.data.borrow().get(&position).cloned()).wrap_with_cost(OperationCost::default())
        }

        fn put_value(&self, position: u16, value: &[u8]) -> CostResult<(), DenseMerkleError> {
            self.data.borrow_mut().insert(position, value.to_vec());
            Ok(()).wrap_with_cost(OperationCost::default())
        }
    }

    fn make_tree_h3_full() -> (MemStore, [u8; 32]) {
        let store = MemStore::new();
        let mut tree =
            crate::tree::DenseFixedSizedMerkleTree::new(3).expect("height 3 should be valid");
        let mut root = [0u8; 32];
        for i in 0..7u8 {
            let (h, _) = tree
                .insert(&[i], &store)
                .unwrap()
                .expect("insert should succeed");
            root = h;
        }
        (store, root)
    }

    #[test]
    fn test_proof_single_leaf() {
        let (store, root) = make_tree_h3_full();
        let proof = DenseTreeProof::generate(3, 7, &[4], &store)
            .unwrap()
            .expect("generate should succeed");
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
        let proof = DenseTreeProof::generate(3, 7, &[1], &store)
            .unwrap()
            .expect("generate should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0], (1, vec![1u8]));
    }

    #[test]
    fn test_proof_root_node() {
        let (store, root) = make_tree_h3_full();
        let proof = DenseTreeProof::generate(3, 7, &[0], &store)
            .unwrap()
            .expect("generate should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0], (0, vec![0u8]));
    }

    #[test]
    fn test_proof_multiple_positions() {
        let (store, root) = make_tree_h3_full();
        let proof = DenseTreeProof::generate(3, 7, &[3, 5, 6], &store)
            .unwrap()
            .expect("generate should succeed");
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
            .unwrap()
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
        let proof = DenseTreeProof::generate(3, 7, &[4], &store)
            .unwrap()
            .expect("generate should succeed");
        let wrong_root = [0xFFu8; 32];
        let result = proof.verify(&wrong_root);
        assert!(result.is_err(), "verification should fail with wrong root");
    }

    #[test]
    fn test_proof_encode_decode_roundtrip() {
        let (store, root) = make_tree_h3_full();
        let proof = DenseTreeProof::generate(3, 7, &[2, 4], &store)
            .unwrap()
            .expect("generate should succeed");
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
            tree.insert(&[i], &store)
                .unwrap()
                .expect("insert should succeed");
        }
        let root = tree
            .root_hash(&store)
            .unwrap()
            .expect("root hash should succeed");

        let proof = DenseTreeProof::generate(3, 3, &[0, 1, 2], &store)
            .unwrap()
            .expect("generate should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 3);
    }

    #[test]
    fn test_proof_height_1_tree() {
        let store = MemStore::new();
        let mut tree =
            crate::tree::DenseFixedSizedMerkleTree::new(1).expect("height 1 should be valid");
        tree.insert(b"hello", &store)
            .unwrap()
            .expect("insert should succeed");
        let root = tree
            .root_hash(&store)
            .unwrap()
            .expect("root hash should succeed");

        let proof = DenseTreeProof::generate(1, 1, &[0], &store)
            .unwrap()
            .expect("generate should succeed");
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
            .unwrap()
            .expect("insert should succeed");
        tree.insert(b"left_val", &store)
            .unwrap()
            .expect("insert should succeed");
        tree.insert(b"right_val", &store)
            .unwrap()
            .expect("insert should succeed");
        let root = tree
            .root_hash(&store)
            .unwrap()
            .expect("root hash should succeed");

        // Prove leaf position 2
        let proof = DenseTreeProof::generate(2, 3, &[2], &store)
            .unwrap()
            .expect("generate should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0], (2, b"right_val".to_vec()));
    }

    #[test]
    fn test_proof_position_out_of_range() {
        let (store, _root) = make_tree_h3_full();
        let result = DenseTreeProof::generate(3, 7, &[7], &store).unwrap();
        assert!(result.is_err(), "position 7 should be out of range");
    }

    #[test]
    fn test_proof_deduplicates_positions() {
        let (store, root) = make_tree_h3_full();
        let proof = DenseTreeProof::generate(3, 7, &[4, 4, 4], &store)
            .unwrap()
            .expect("generate should succeed");
        assert_eq!(proof.entries.len(), 1);
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 1);
    }
}
