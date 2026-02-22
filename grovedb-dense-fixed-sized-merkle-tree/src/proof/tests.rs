#[cfg(test)]
mod proof_tests {
    use std::{cell::RefCell, collections::HashMap};

    use grovedb_costs::{CostsExt, OperationCost};
    use grovedb_query::Query;

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

    // ---------------------------------------------------------------
    // Original tests, now using generate (positions) directly
    // ---------------------------------------------------------------

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

    // ---------------------------------------------------------------
    // generate_for_query tests
    // ---------------------------------------------------------------

    #[test]
    fn test_query_single_key_1byte() {
        let (store, root) = make_tree_h3_full();
        let query = Query::new_single_key(vec![4]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0], (4, vec![4u8]));
    }

    #[test]
    fn test_query_single_key_2byte() {
        let (store, root) = make_tree_h3_full();
        // Position 5 encoded as 2-byte big-endian
        let query = Query::new_single_key(vec![0, 5]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0], (5, vec![5u8]));
    }

    #[test]
    fn test_query_range_exclusive() {
        let (store, root) = make_tree_h3_full();
        // Range [3..6) → positions 3, 4, 5
        let mut query = Query::new();
        query.insert_range(vec![3]..vec![6]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 3);
        assert_eq!(verified[0], (3, vec![3u8]));
        assert_eq!(verified[1], (4, vec![4u8]));
        assert_eq!(verified[2], (5, vec![5u8]));
    }

    #[test]
    fn test_query_range_inclusive() {
        let (store, root) = make_tree_h3_full();
        // Range [3..=5] → positions 3, 4, 5
        let mut query = Query::new();
        query.insert_range_inclusive(vec![3]..=vec![5]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 3);
        assert_eq!(verified[0], (3, vec![3u8]));
        assert_eq!(verified[1], (4, vec![4u8]));
        assert_eq!(verified[2], (5, vec![5u8]));
    }

    #[test]
    fn test_query_range_full() {
        let (store, root) = make_tree_h3_full();
        let query = Query::new_range_full();
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 7);
        for (pos, val) in &verified {
            assert_eq!(*val, vec![*pos as u8]);
        }
    }

    #[test]
    fn test_query_range_from() {
        let (store, root) = make_tree_h3_full();
        // RangeFrom [5..] → positions 5, 6
        let mut query = Query::new();
        query.insert_range_from(vec![5]..);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 2);
        assert_eq!(verified[0], (5, vec![5u8]));
        assert_eq!(verified[1], (6, vec![6u8]));
    }

    #[test]
    fn test_query_range_to() {
        let (store, root) = make_tree_h3_full();
        // RangeTo [..3) → positions 0, 1, 2
        let mut query = Query::new();
        query.insert_range_to(..vec![3]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 3);
        assert_eq!(verified[0], (0, vec![0u8]));
        assert_eq!(verified[1], (1, vec![1u8]));
        assert_eq!(verified[2], (2, vec![2u8]));
    }

    #[test]
    fn test_query_range_to_inclusive() {
        let (store, root) = make_tree_h3_full();
        // RangeToInclusive [..=2] → positions 0, 1, 2
        let mut query = Query::new();
        query.insert_range_to_inclusive(..=vec![2]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 3);
        assert_eq!(verified[0], (0, vec![0u8]));
        assert_eq!(verified[1], (1, vec![1u8]));
        assert_eq!(verified[2], (2, vec![2u8]));
    }

    #[test]
    fn test_query_range_after() {
        let (store, root) = make_tree_h3_full();
        // RangeAfter (4..) → positions 5, 6
        let mut query = Query::new();
        query.insert_range_after(vec![4]..);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 2);
        assert_eq!(verified[0], (5, vec![5u8]));
        assert_eq!(verified[1], (6, vec![6u8]));
    }

    #[test]
    fn test_query_range_after_to() {
        let (store, root) = make_tree_h3_full();
        // RangeAfterTo (1<..5) → positions 2, 3, 4
        let mut query = Query::new();
        query.insert_range_after_to(vec![1]..vec![5]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 3);
        assert_eq!(verified[0], (2, vec![2u8]));
        assert_eq!(verified[1], (3, vec![3u8]));
        assert_eq!(verified[2], (4, vec![4u8]));
    }

    #[test]
    fn test_query_range_after_to_inclusive() {
        let (store, root) = make_tree_h3_full();
        // RangeAfterToInclusive (1<..=4) → positions 2, 3, 4
        let mut query = Query::new();
        query.insert_range_after_to_inclusive(vec![1]..=vec![4]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 3);
        assert_eq!(verified[0], (2, vec![2u8]));
        assert_eq!(verified[1], (3, vec![3u8]));
        assert_eq!(verified[2], (4, vec![4u8]));
    }

    #[test]
    fn test_query_multiple_disjoint_ranges() {
        let (store, root) = make_tree_h3_full();
        // Two disjoint ranges: [0..2) and [5..7) → positions 0, 1, 5, 6
        let mut query = Query::new();
        query.insert_range(vec![0]..vec![2]);
        query.insert_range(vec![5]..vec![7]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 4);
        assert_eq!(verified[0], (0, vec![0u8]));
        assert_eq!(verified[1], (1, vec![1u8]));
        assert_eq!(verified[2], (5, vec![5u8]));
        assert_eq!(verified[3], (6, vec![6u8]));
    }

    #[test]
    fn test_query_individual_keys_mixed() {
        let (store, root) = make_tree_h3_full();
        // Individual keys: 1, 4, 6
        let mut query = Query::new();
        query.insert_key(vec![1]);
        query.insert_key(vec![4]);
        query.insert_key(vec![6]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 3);
        assert_eq!(verified[0], (1, vec![1u8]));
        assert_eq!(verified[1], (4, vec![4u8]));
        assert_eq!(verified[2], (6, vec![6u8]));
    }

    #[test]
    fn test_query_2byte_encoding() {
        let (store, root) = make_tree_h3_full();
        // Use 2-byte big-endian encoding for all positions
        let mut query = Query::new();
        query.insert_range(vec![0, 3]..vec![0, 6]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 3);
        assert_eq!(verified[0], (3, vec![3u8]));
        assert_eq!(verified[1], (4, vec![4u8]));
        assert_eq!(verified[2], (5, vec![5u8]));
    }

    #[test]
    fn test_query_invalid_byte_length() {
        let (store, _root) = make_tree_h3_full();
        // 3-byte key should fail
        let query = Query::new_single_key(vec![0, 0, 5]);
        let result = DenseTreeProof::generate_for_query(3, 7, &query, &store).unwrap();
        assert!(
            result.is_err(),
            "3-byte position encoding should be rejected"
        );
    }

    #[test]
    fn test_query_empty_byte_key() {
        let (store, _root) = make_tree_h3_full();
        // 0-byte key should fail
        let query = Query::new_single_key(vec![]);
        let result = DenseTreeProof::generate_for_query(3, 7, &query, &store).unwrap();
        assert!(
            result.is_err(),
            "0-byte position encoding should be rejected"
        );
    }

    // ---------------------------------------------------------------
    // bytes_to_position unit tests
    // ---------------------------------------------------------------

    #[test]
    fn test_bytes_to_position_1byte() {
        assert_eq!(
            bytes_to_position(&[0]).expect("1-byte decode should succeed"),
            0
        );
        assert_eq!(
            bytes_to_position(&[255]).expect("1-byte decode should succeed"),
            255
        );
    }

    #[test]
    fn test_bytes_to_position_2byte() {
        assert_eq!(
            bytes_to_position(&[0, 5]).expect("2-byte decode should succeed"),
            5
        );
        assert_eq!(
            bytes_to_position(&[1, 0]).expect("2-byte decode should succeed"),
            256
        );
        assert_eq!(
            bytes_to_position(&[0xFF, 0xFF]).expect("2-byte decode should succeed"),
            65535
        );
    }

    #[test]
    fn test_bytes_to_position_invalid_lengths() {
        assert!(
            bytes_to_position(&[]).is_err(),
            "0 bytes should be rejected"
        );
        assert!(
            bytes_to_position(&[0, 0, 1]).is_err(),
            "3 bytes should be rejected"
        );
        assert!(
            bytes_to_position(&[0, 0, 0, 1]).is_err(),
            "4 bytes should be rejected"
        );
    }

    // ---------------------------------------------------------------
    // query_to_positions unit tests
    // ---------------------------------------------------------------

    #[test]
    fn test_query_to_positions_deduplicates() {
        // Overlapping ranges should deduplicate
        let mut query = Query::new();
        query.insert_range(vec![0]..vec![4]);
        query.insert_range(vec![2]..vec![6]);
        let positions = query_to_positions(&query, 7).expect("query_to_positions should succeed");
        assert_eq!(positions, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_query_to_positions_empty_query() {
        let query = Query::new();
        let positions = query_to_positions(&query, 7).expect("query_to_positions should succeed");
        assert!(positions.is_empty());
    }

    // ---------------------------------------------------------------
    // Clamping: ranges extending beyond count are clamped
    // ---------------------------------------------------------------

    #[test]
    fn test_query_range_clamped_to_count() {
        let (store, root) = make_tree_h3_full();
        // Range [5..100) extends far beyond count=7, should clamp to [5..7)
        let mut query = Query::new();
        query.insert_range(vec![5]..vec![100]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 2);
        assert_eq!(verified[0], (5, vec![5u8]));
        assert_eq!(verified[1], (6, vec![6u8]));
    }

    #[test]
    fn test_query_range_inclusive_clamped_to_count() {
        let (store, root) = make_tree_h3_full();
        // RangeInclusive [5..=100] extends beyond count=7, should clamp to [5..=6]
        let mut query = Query::new();
        query.insert_range_inclusive(vec![5]..=vec![100]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 2);
        assert_eq!(verified[0], (5, vec![5u8]));
        assert_eq!(verified[1], (6, vec![6u8]));
    }

    #[test]
    fn test_query_key_beyond_count_excluded() {
        let (store, root) = make_tree_h3_full();
        // Key at position 10 is beyond count=7, should be silently excluded
        let mut query = Query::new();
        query.insert_key(vec![3]);
        query.insert_key(vec![10]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0], (3, vec![3u8]));
    }

    #[test]
    fn test_query_range_entirely_beyond_count() {
        let (store, _root) = make_tree_h3_full();
        // Range [10..20) is entirely beyond count=7, should produce empty positions
        let mut query = Query::new();
        query.insert_range(vec![10]..vec![20]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        assert_eq!(proof.entries.len(), 0);
    }

    #[test]
    fn test_query_range_to_inclusive_clamped() {
        let (store, root) = make_tree_h3_full();
        // RangeToInclusive [..=50] should clamp to [..=6]
        let mut query = Query::new();
        query.insert_range_to_inclusive(..=vec![50]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 7);
    }

    #[test]
    fn test_query_range_after_to_clamped() {
        let (store, root) = make_tree_h3_full();
        // RangeAfterTo (3<..100) should clamp to (3<..7) → positions 4, 5, 6
        let mut query = Query::new();
        query.insert_range_after_to(vec![3]..vec![100]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 3);
        assert_eq!(verified[0], (4, vec![4u8]));
        assert_eq!(verified[1], (5, vec![5u8]));
        assert_eq!(verified[2], (6, vec![6u8]));
    }

    #[test]
    fn test_query_range_after_to_inclusive_clamped() {
        let (store, root) = make_tree_h3_full();
        // RangeAfterToInclusive (3<..=100) should clamp to (3<..=6) → positions 4, 5, 6
        let mut query = Query::new();
        query.insert_range_after_to_inclusive(vec![3]..=vec![100]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 3);
        assert_eq!(verified[0], (4, vec![4u8]));
        assert_eq!(verified[1], (5, vec![5u8]));
        assert_eq!(verified[2], (6, vec![6u8]));
    }

    #[test]
    fn test_query_range_to_clamped() {
        let (store, root) = make_tree_h3_full();
        // RangeTo [..50) should clamp to [..7) → all 7 positions
        let mut query = Query::new();
        query.insert_range_to(..vec![50]);
        let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let verified = proof.verify(&root).expect("verify should succeed");
        assert_eq!(verified.len(), 7);
    }
}
