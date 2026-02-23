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

    /// Height-3 tree, fully filled (7 positions, values [0]..[6]).
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

    /// Height-4 tree, fully filled (15 positions, values [0]..[14] as BE u16).
    fn make_tree_h4_full() -> (MemStore, [u8; 32]) {
        let store = MemStore::new();
        let mut tree =
            crate::tree::DenseFixedSizedMerkleTree::new(4).expect("height 4 should be valid");
        let mut root = [0u8; 32];
        for i in 0u16..15 {
            let (h, _) = tree
                .insert(&i.to_be_bytes(), &store)
                .unwrap()
                .expect("insert should succeed");
            root = h;
        }
        (store, root)
    }

    /// Height-3 tree, partially filled (5 of 7 positions).
    fn make_tree_h3_partial() -> (MemStore, [u8; 32], u16) {
        let store = MemStore::new();
        let mut tree =
            crate::tree::DenseFixedSizedMerkleTree::new(3).expect("height 3 should be valid");
        let mut root = [0u8; 32];
        let count = 5u16;
        for i in 0..count as u8 {
            let (h, _) = tree
                .insert(&[i], &store)
                .unwrap()
                .expect("insert should succeed");
            root = h;
        }
        (store, root, count)
    }

    /// Helper: generate a proof for the query and verify it matches the query
    /// (completeness + soundness). Returns the verified entries.
    fn gen_and_verify(
        height: u8,
        count: u16,
        query: &Query,
        store: &MemStore,
    ) -> Vec<(u16, Vec<u8>)> {
        let proof = DenseTreeProof::generate_for_query(height, count, query, store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let (_root, entries) = proof
            .verify_for_query::<Vec<(u16, Vec<u8>)>>(query, height, count)
            .expect("verify_for_query should succeed");
        entries
    }

    /// Helper: generate a proof for `gen_query`, then assert that
    /// `verify_for_query(verify_query, count)` fails with either "incomplete"
    /// or "unsound" in the error message.
    fn gen_and_expect_mismatch(
        height: u8,
        count: u16,
        gen_query: &Query,
        verify_query: &Query,
        store: &MemStore,
    ) {
        let proof = DenseTreeProof::generate_for_query(height, count, gen_query, store)
            .unwrap()
            .expect("generate_for_query should succeed");
        let result = proof.verify_for_query::<Vec<(u16, Vec<u8>)>>(verify_query, height, count);
        assert!(
            result.is_err(),
            "verify_for_query should fail: gen={:?} vs verify={:?}",
            gen_query.items,
            verify_query.items,
        );
        let err = format!("{}", result.unwrap_err());
        assert!(
            err.contains("incomplete") || err.contains("unsound") || err.contains("unexpected"),
            "error should indicate completeness/soundness failure, got: {}",
            err
        );
    }

    // =======================================================================
    // Low-level: generate (positions) tests
    // =======================================================================

    #[test]
    fn test_proof_single_leaf() {
        let (store, root) = make_tree_h3_full();
        let proof = DenseTreeProof::generate(3, 7, &[4], &store)
            .unwrap()
            .expect("generate should succeed");
        assert_eq!(proof.entries.len(), 1);
        assert_eq!(proof.entries[0].0, 4);

        let verified = proof
            .verify_against_expected_root::<Vec<(u16, Vec<u8>)>>(&root, 3, 7)
            .expect("verify should succeed");
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0], (4, vec![4u8]));
    }

    #[test]
    fn test_proof_internal_node() {
        let (store, root) = make_tree_h3_full();
        let proof = DenseTreeProof::generate(3, 7, &[1], &store)
            .unwrap()
            .expect("generate should succeed");
        let verified = proof
            .verify_against_expected_root::<Vec<(u16, Vec<u8>)>>(&root, 3, 7)
            .expect("verify should succeed");
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0], (1, vec![1u8]));
    }

    #[test]
    fn test_proof_root_node() {
        let (store, root) = make_tree_h3_full();
        let proof = DenseTreeProof::generate(3, 7, &[0], &store)
            .unwrap()
            .expect("generate should succeed");
        let verified = proof
            .verify_against_expected_root::<Vec<(u16, Vec<u8>)>>(&root, 3, 7)
            .expect("verify should succeed");
        assert_eq!(verified.len(), 1);
        assert_eq!(verified[0], (0, vec![0u8]));
    }

    #[test]
    fn test_proof_multiple_positions() {
        let (store, root) = make_tree_h3_full();
        let proof = DenseTreeProof::generate(3, 7, &[3, 5, 6], &store)
            .unwrap()
            .expect("generate should succeed");
        let verified = proof
            .verify_against_expected_root::<Vec<(u16, Vec<u8>)>>(&root, 3, 7)
            .expect("verify should succeed");
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
        let verified = proof
            .verify_against_expected_root::<Vec<(u16, Vec<u8>)>>(&root, 3, 7)
            .expect("verify should succeed");
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
        let result = proof.verify_against_expected_root::<Vec<(u16, Vec<u8>)>>(&wrong_root, 3, 7);
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
        let verified = decoded
            .verify_against_expected_root::<Vec<(u16, Vec<u8>)>>(&root, 3, 7)
            .expect("verify should succeed");
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
        let verified = proof
            .verify_against_expected_root::<Vec<(u16, Vec<u8>)>>(&root, 3, 3)
            .expect("verify should succeed");
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
        let verified = proof
            .verify_against_expected_root::<Vec<(u16, Vec<u8>)>>(&root, 1, 1)
            .expect("verify should succeed");
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
        let verified = proof
            .verify_against_expected_root::<Vec<(u16, Vec<u8>)>>(&root, 2, 3)
            .expect("verify should succeed");
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
        let verified = proof
            .verify_against_expected_root::<Vec<(u16, Vec<u8>)>>(&root, 3, 7)
            .expect("verify should succeed");
        assert_eq!(verified.len(), 1);
    }

    // =======================================================================
    // bytes_to_position unit tests
    // =======================================================================

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

    // =======================================================================
    // query_to_positions unit tests
    // =======================================================================

    #[test]
    fn test_query_to_positions_deduplicates() {
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

    // =======================================================================
    // Byte encoding: 1-byte, 2-byte, invalid
    // =======================================================================

    #[test]
    fn test_encoding_1byte_key() {
        let (store, _) = make_tree_h3_full();
        let query = Query::new_single_key(vec![4]);
        let entries = gen_and_verify(3, 7, &query, &store);
        assert_eq!(entries, vec![(4, vec![4u8])]);
    }

    #[test]
    fn test_encoding_2byte_key() {
        let (store, _) = make_tree_h3_full();
        let query = Query::new_single_key(vec![0, 5]);
        let entries = gen_and_verify(3, 7, &query, &store);
        assert_eq!(entries, vec![(5, vec![5u8])]);
    }

    #[test]
    fn test_encoding_2byte_range() {
        let (store, _) = make_tree_h3_full();
        let mut query = Query::new();
        query.insert_range(vec![0, 3]..vec![0, 6]);
        let entries = gen_and_verify(3, 7, &query, &store);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].0, 3);
        assert_eq!(entries[2].0, 5);
    }

    #[test]
    fn test_encoding_0byte_rejected() {
        let (store, _) = make_tree_h3_full();
        let query = Query::new_single_key(vec![]);
        let result = DenseTreeProof::generate_for_query(3, 7, &query, &store).unwrap();
        assert!(
            result.is_err(),
            "0-byte position encoding should be rejected"
        );
    }

    #[test]
    fn test_encoding_3byte_rejected() {
        let (store, _) = make_tree_h3_full();
        let query = Query::new_single_key(vec![0, 0, 5]);
        let result = DenseTreeProof::generate_for_query(3, 7, &query, &store).unwrap();
        assert!(
            result.is_err(),
            "3-byte position encoding should be rejected"
        );
    }

    // =======================================================================
    // Key — single position
    // =======================================================================

    mod key {
        use super::*;

        #[test]
        fn inclusion_single() {
            let (store, _) = make_tree_h3_full();
            let query = Query::new_single_key(vec![4]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries, vec![(4, vec![4u8])]);
        }

        #[test]
        fn inclusion_root_position() {
            let (store, _) = make_tree_h3_full();
            let query = Query::new_single_key(vec![0]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries, vec![(0, vec![0u8])]);
        }

        #[test]
        fn inclusion_last_position() {
            let (store, _) = make_tree_h3_full();
            let query = Query::new_single_key(vec![6]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries, vec![(6, vec![6u8])]);
        }

        #[test]
        fn soundness_extra_positions() {
            // Proof for {3,4,5} verified against query for just {4} → unsound
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range(vec![3]..vec![6]);
                q
            };
            let verify_query = Query::new_single_key(vec![4]);
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn completeness_missing_position() {
            // Proof for {4} verified against query for {5} → incomplete
            let (store, _) = make_tree_h3_full();
            let gen_query = Query::new_single_key(vec![4]);
            let verify_query = Query::new_single_key(vec![5]);
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn beyond_count_clamped() {
            let (store, _) = make_tree_h3_full();
            // Key at position 10 is beyond count=7, silently excluded
            let mut query = Query::new();
            query.insert_key(vec![3]);
            query.insert_key(vec![10]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries, vec![(3, vec![3u8])]);
        }
    }

    // =======================================================================
    // Range — exclusive end [start..end)
    // =======================================================================

    mod range {
        use super::*;

        #[test]
        fn inclusion_middle() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![3]..vec![6]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0], (3, vec![3u8]));
            assert_eq!(entries[2], (5, vec![5u8]));
        }

        #[test]
        fn inclusion_from_start() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![0]..vec![3]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].0, 0);
            assert_eq!(entries[2].0, 2);
        }

        #[test]
        fn inclusion_to_end() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![5]..vec![7]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].0, 5);
            assert_eq!(entries[1].0, 6);
        }

        #[test]
        fn single_element_range() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![4]..vec![5]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries, vec![(4, vec![4u8])]);
        }

        #[test]
        fn empty_range_equal_bounds() {
            // [5..5) produces zero positions
            let positions = query_to_positions(
                &{
                    let mut q = Query::new();
                    q.insert_range(vec![5]..vec![5]);
                    q
                },
                7,
            )
            .expect("should succeed");
            assert!(positions.is_empty());
        }

        #[test]
        fn soundness_wider_proof() {
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range(vec![2]..vec![6]);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range(vec![3]..vec![5]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn completeness_narrower_proof() {
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range(vec![3]..vec![5]);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range(vec![2]..vec![6]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn clamped_beyond_count() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![5]..vec![100]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].0, 5);
            assert_eq!(entries[1].0, 6);
        }

        #[test]
        fn entirely_beyond_count() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![10]..vec![20]);
            let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
                .unwrap()
                .expect("generate_for_query should succeed");
            assert_eq!(proof.entries.len(), 0);
        }
    }

    // =======================================================================
    // RangeInclusive — [start..=end]
    // =======================================================================

    mod range_inclusive {
        use super::*;

        #[test]
        fn inclusion() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_inclusive(vec![3]..=vec![5]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].0, 3);
            assert_eq!(entries[2].0, 5);
        }

        #[test]
        fn single_element() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_inclusive(vec![4]..=vec![4]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries, vec![(4, vec![4u8])]);
        }

        #[test]
        fn entire_tree() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_inclusive(vec![0]..=vec![6]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 7);
        }

        #[test]
        fn soundness_wider_proof() {
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range_inclusive(vec![1]..=vec![5]);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range_inclusive(vec![2]..=vec![4]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn completeness_narrower_proof() {
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range_inclusive(vec![3]..=vec![4]);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range_inclusive(vec![2]..=vec![5]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn clamped_beyond_count() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_inclusive(vec![5]..=vec![100]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].0, 5);
            assert_eq!(entries[1].0, 6);
        }
    }

    // =======================================================================
    // RangeFull — (..)
    // =======================================================================

    mod range_full {
        use super::*;

        #[test]
        fn inclusion_all_positions() {
            let (store, _) = make_tree_h3_full();
            let query = Query::new_range_full();
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 7);
            for (pos, val) in &entries {
                assert_eq!(*val, vec![*pos as u8]);
            }
        }

        #[test]
        fn completeness_partial_proof_rejected() {
            // Proof for just {4} verified against RangeFull → incomplete
            let (store, _) = make_tree_h3_full();
            let gen_query = Query::new_single_key(vec![4]);
            let verify_query = Query::new_range_full();
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn soundness_full_proof_against_subset() {
            // Proof for RangeFull verified against [3..5) → unsound
            let (store, _) = make_tree_h3_full();
            let gen_query = Query::new_range_full();
            let verify_query = {
                let mut q = Query::new();
                q.insert_range(vec![3]..vec![5]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn partial_tree() {
            let (store, _, count) = make_tree_h3_partial();
            let query = Query::new_range_full();
            let entries = gen_and_verify(3, count, &query, &store);
            assert_eq!(entries.len(), count as usize);
        }
    }

    // =======================================================================
    // RangeFrom — [start..)  (inclusive start, open end clamped to count)
    // =======================================================================

    mod range_from {
        use super::*;

        #[test]
        fn inclusion_from_middle() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_from(vec![5]..);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].0, 5);
            assert_eq!(entries[1].0, 6);
        }

        #[test]
        fn inclusion_from_zero() {
            // [0..) is the same as RangeFull for this tree
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_from(vec![0]..);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 7);
        }

        #[test]
        fn inclusion_from_last() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_from(vec![6]..);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries, vec![(6, vec![6u8])]);
        }

        #[test]
        fn soundness_wider_proof() {
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range_from(vec![3]..);
                q
            };
            // Verify against [5..)
            let verify_query = {
                let mut q = Query::new();
                q.insert_range_from(vec![5]..);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn completeness_narrower_proof() {
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range_from(vec![5]..);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range_from(vec![3]..);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn from_at_count_yields_empty() {
            // [7..) on a tree with count=7 → no positions
            let positions = query_to_positions(
                &{
                    let mut q = Query::new();
                    q.insert_range_from(vec![7]..);
                    q
                },
                7,
            )
            .expect("should succeed");
            assert!(positions.is_empty());
        }
    }

    // =======================================================================
    // RangeTo — [..end)
    // =======================================================================

    mod range_to {
        use super::*;

        #[test]
        fn inclusion() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_to(..vec![3]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].0, 0);
            assert_eq!(entries[2].0, 2);
        }

        #[test]
        fn single_element() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_to(..vec![1]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries, vec![(0, vec![0u8])]);
        }

        #[test]
        fn soundness() {
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range_to(..vec![5]);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range_to(..vec![3]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn completeness() {
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range_to(..vec![3]);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range_to(..vec![5]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn clamped_beyond_count() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_to(..vec![50]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 7);
        }

        #[test]
        fn to_zero_yields_empty() {
            let positions = query_to_positions(
                &{
                    let mut q = Query::new();
                    q.insert_range_to(..vec![0]);
                    q
                },
                7,
            )
            .expect("should succeed");
            assert!(positions.is_empty());
        }
    }

    // =======================================================================
    // RangeToInclusive — [..=end]
    // =======================================================================

    mod range_to_inclusive {
        use super::*;

        #[test]
        fn inclusion() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_to_inclusive(..=vec![2]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].0, 0);
            assert_eq!(entries[2].0, 2);
        }

        #[test]
        fn single_element_to_zero() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_to_inclusive(..=vec![0]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries, vec![(0, vec![0u8])]);
        }

        #[test]
        fn entire_tree() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_to_inclusive(..=vec![6]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 7);
        }

        #[test]
        fn soundness() {
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range_to_inclusive(..=vec![5]);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range_to_inclusive(..=vec![2]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn completeness() {
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range_to_inclusive(..=vec![2]);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range_to_inclusive(..=vec![5]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn clamped_beyond_count() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_to_inclusive(..=vec![50]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 7);
        }
    }

    // =======================================================================
    // RangeAfter — (start..)  (exclusive start, open end clamped to count)
    // =======================================================================

    mod range_after {
        use super::*;

        #[test]
        fn inclusion() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after(vec![4]..);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].0, 5);
            assert_eq!(entries[1].0, 6);
        }

        #[test]
        fn from_zero_excludes_root() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after(vec![0]..);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 6);
            assert_eq!(entries[0].0, 1); // position 0 excluded
        }

        #[test]
        fn from_last_minus_one() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after(vec![5]..);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries, vec![(6, vec![6u8])]);
        }

        #[test]
        fn from_last_yields_empty() {
            // (6..) on count=7 → only position > 6 would be 7, which is >= count
            let positions = query_to_positions(
                &{
                    let mut q = Query::new();
                    q.insert_range_after(vec![6]..);
                    q
                },
                7,
            )
            .expect("should succeed");
            assert!(positions.is_empty());
        }

        #[test]
        fn soundness() {
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range_after(vec![2]..);
                q
            };
            // Verify against (4..)
            let verify_query = {
                let mut q = Query::new();
                q.insert_range_after(vec![4]..);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn completeness() {
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range_after(vec![4]..);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range_after(vec![2]..);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }
    }

    // =======================================================================
    // RangeAfterTo — (start..end)
    // =======================================================================

    mod range_after_to {
        use super::*;

        #[test]
        fn inclusion() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after_to(vec![1]..vec![5]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].0, 2);
            assert_eq!(entries[2].0, 4);
        }

        #[test]
        fn single_element() {
            // (2<..4) → position 3 only
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after_to(vec![2]..vec![4]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries, vec![(3, vec![3u8])]);
        }

        #[test]
        fn adjacent_bounds_yields_empty() {
            // (3<..4) → position 4 is excluded by end, so nothing
            // Actually (3<..4) means start_exclusive=3, end_exclusive=4 → (3+1)..4 = 4..4 =
            // empty
            let positions = query_to_positions(
                &{
                    let mut q = Query::new();
                    q.insert_range_after_to(vec![3]..vec![4]);
                    q
                },
                7,
            )
            .expect("should succeed");
            assert!(positions.is_empty());
        }

        #[test]
        fn soundness() {
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range_after_to(vec![0]..vec![6]);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range_after_to(vec![2]..vec![5]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn completeness() {
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range_after_to(vec![2]..vec![5]);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range_after_to(vec![0]..vec![6]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn clamped_beyond_count() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after_to(vec![3]..vec![100]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].0, 4);
            assert_eq!(entries[2].0, 6);
        }
    }

    // =======================================================================
    // RangeAfterToInclusive — (start..=end]
    // =======================================================================

    mod range_after_to_inclusive {
        use super::*;

        #[test]
        fn inclusion() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after_to_inclusive(vec![1]..=vec![4]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].0, 2);
            assert_eq!(entries[2].0, 4);
        }

        #[test]
        fn single_element() {
            // (2<..=3] → position 3 only
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after_to_inclusive(vec![2]..=vec![3]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries, vec![(3, vec![3u8])]);
        }

        #[test]
        fn same_start_end_yields_empty() {
            // (3<..=3] → start_exclusive=3, end_inclusive=3 → (3+1)..=3 = 4..=3 = empty
            let positions = query_to_positions(
                &{
                    let mut q = Query::new();
                    q.insert_range_after_to_inclusive(vec![3]..=vec![3]);
                    q
                },
                7,
            )
            .expect("should succeed");
            assert!(positions.is_empty());
        }

        #[test]
        fn soundness() {
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range_after_to_inclusive(vec![0]..=vec![5]);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range_after_to_inclusive(vec![2]..=vec![4]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn completeness() {
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range_after_to_inclusive(vec![2]..=vec![4]);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range_after_to_inclusive(vec![0]..=vec![5]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn clamped_beyond_count() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after_to_inclusive(vec![3]..=vec![100]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].0, 4);
            assert_eq!(entries[2].0, 6);
        }
    }

    // =======================================================================
    // Disjoint ranges: multiple QueryItems in a single query
    // =======================================================================

    mod disjoint {
        use super::*;

        #[test]
        fn two_disjoint_ranges() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![0]..vec![2]);
            query.insert_range(vec![5]..vec![7]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 4);
            assert_eq!(entries[0].0, 0);
            assert_eq!(entries[1].0, 1);
            assert_eq!(entries[2].0, 5);
            assert_eq!(entries[3].0, 6);
        }

        #[test]
        fn three_scattered_keys() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_key(vec![0]);
            query.insert_key(vec![3]);
            query.insert_key(vec![6]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].0, 0);
            assert_eq!(entries[1].0, 3);
            assert_eq!(entries[2].0, 6);
        }

        #[test]
        fn key_plus_range() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_key(vec![0]);
            query.insert_range(vec![4]..vec![7]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 4);
            assert_eq!(entries[0].0, 0);
            assert_eq!(entries[1].0, 4);
            assert_eq!(entries[2].0, 5);
            assert_eq!(entries[3].0, 6);
        }

        #[test]
        fn range_from_plus_range_to() {
            // [..2) union [5..) → {0,1,5,6}
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_to(..vec![2]);
            query.insert_range_from(vec![5]..);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 4);
            assert_eq!(entries[0].0, 0);
            assert_eq!(entries[1].0, 1);
            assert_eq!(entries[2].0, 5);
            assert_eq!(entries[3].0, 6);
        }

        #[test]
        fn soundness_disjoint_proof_vs_subset() {
            // Proof for {0,1,5,6}, verify against {0,1} → unsound (extra 5,6)
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range(vec![0]..vec![2]);
                q.insert_range(vec![5]..vec![7]);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range(vec![0]..vec![2]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn completeness_subset_proof_vs_disjoint() {
            // Proof for {0,1}, verify against {0,1,5,6} → incomplete (missing 5,6)
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range(vec![0]..vec![2]);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range(vec![0]..vec![2]);
                q.insert_range(vec![5]..vec![7]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn overlapping_items_deduplicate() {
            // [1..4) union [3..6) should deduplicate to {1,2,3,4,5}
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![1]..vec![4]);
            query.insert_range(vec![3]..vec![6]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 5);
            assert_eq!(entries[0].0, 1);
            assert_eq!(entries[4].0, 5);
        }

        #[test]
        fn key_inside_range() {
            // Key {3} union [2..5) — the key is inside the range, should work fine
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_key(vec![3]);
            query.insert_range(vec![2]..vec![5]);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].0, 2);
            assert_eq!(entries[1].0, 3);
            assert_eq!(entries[2].0, 4);
        }

        #[test]
        fn mixed_range_types() {
            // Combine different range types: Key + RangeInclusive + RangeAfter
            // {0} union [2..=3] union (4..) → {0, 2, 3, 5, 6}
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_key(vec![0]);
            query.insert_range_inclusive(vec![2]..=vec![3]);
            query.insert_range_after(vec![4]..);
            let entries = gen_and_verify(3, 7, &query, &store);
            assert_eq!(entries.len(), 5);
            assert_eq!(entries[0].0, 0);
            assert_eq!(entries[1].0, 2);
            assert_eq!(entries[2].0, 3);
            assert_eq!(entries[3].0, 5);
            assert_eq!(entries[4].0, 6);
        }

        #[test]
        fn three_disjoint_ranges_larger_tree() {
            // Use the h4 tree (15 positions) for more spread-out ranges
            let (store, _) = make_tree_h4_full();
            let mut query = Query::new();
            query.insert_range(vec![1]..vec![3]); // {1,2}
            query.insert_range(vec![7]..vec![9]); // {7,8}
            query.insert_range(vec![12]..vec![14]); // {12,13}
            let entries = gen_and_verify(4, 15, &query, &store);
            assert_eq!(entries.len(), 6);
            assert_eq!(entries[0].0, 1);
            assert_eq!(entries[1].0, 2);
            assert_eq!(entries[2].0, 7);
            assert_eq!(entries[3].0, 8);
            assert_eq!(entries[4].0, 12);
            assert_eq!(entries[5].0, 13);
        }

        #[test]
        fn soundness_wrong_gap() {
            // Proof for {0,1,2,3,4,5,6}, verify against {0,1} + {5,6} → unsound (extra
            // 2,3,4)
            let (store, _) = make_tree_h3_full();
            let gen_query = Query::new_range_full();
            let verify_query = {
                let mut q = Query::new();
                q.insert_range(vec![0]..vec![2]);
                q.insert_range(vec![5]..vec![7]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn completeness_missing_second_range() {
            // Proof for {0,1}, verify against {0,1} + {5,6} → incomplete (missing 5,6)
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range(vec![0]..vec![2]);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range(vec![0]..vec![2]);
                q.insert_range(vec![5]..vec![7]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }
    }

    // =======================================================================
    // Partial tree: positions beyond count don't exist
    // =======================================================================

    mod partial_tree {
        use super::*;

        #[test]
        fn range_clamped_to_count() {
            let (store, _, count) = make_tree_h3_partial(); // count=5
            let mut query = Query::new();
            query.insert_range(vec![3]..vec![7]); // clamped to [3..5)
            let entries = gen_and_verify(3, count, &query, &store);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].0, 3);
            assert_eq!(entries[1].0, 4);
        }

        #[test]
        fn range_from_clamped() {
            let (store, _, count) = make_tree_h3_partial(); // count=5
            let mut query = Query::new();
            query.insert_range_from(vec![3]..);
            let entries = gen_and_verify(3, count, &query, &store);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].0, 3);
            assert_eq!(entries[1].0, 4);
        }

        #[test]
        fn range_full_matches_count() {
            let (store, _, count) = make_tree_h3_partial(); // count=5
            let query = Query::new_range_full();
            let entries = gen_and_verify(3, count, &query, &store);
            assert_eq!(entries.len(), count as usize);
        }

        #[test]
        fn key_beyond_partial_excluded() {
            let (store, _, count) = make_tree_h3_partial(); // count=5
            let mut query = Query::new();
            query.insert_key(vec![2]);
            query.insert_key(vec![6]); // beyond count=5
            let entries = gen_and_verify(3, count, &query, &store);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].0, 2);
        }
    }

    // =======================================================================
    // verify_for_query edge cases
    // =======================================================================

    mod verify_for_query_edges {
        use super::*;

        #[test]
        fn empty_tree_empty_query() {
            let store = MemStore::new();
            let query = Query::new();
            let proof = DenseTreeProof::generate_for_query(3, 0, &query, &store)
                .unwrap()
                .expect("should succeed for empty query on empty tree");
            let (_root, entries) = proof
                .verify_for_query::<Vec<(u16, Vec<u8>)>>(&query, 3, 0)
                .expect("should succeed");
            assert!(entries.is_empty());
        }

        #[test]
        fn root_hash_consistency() {
            // verify_for_query and verify_against_expected_root should produce
            // the same root hash
            let (store, expected_root) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![2]..vec![5]);
            let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
                .unwrap()
                .expect("generate should succeed");

            let (query_root, _) = proof
                .verify_for_query::<Vec<(u16, Vec<u8>)>>(&query, 3, 7)
                .expect("verify_for_query should succeed");
            let entries = proof
                .verify_against_expected_root::<Vec<(u16, Vec<u8>)>>(&expected_root, 3, 7)
                .expect("verify_against_expected_root should succeed");

            assert_eq!(query_root, expected_root);
            assert_eq!(entries.len(), 3);
        }

        #[test]
        fn verify_and_get_root_then_check_query() {
            // Two-step: first get root, then check the same proof against a query
            let (store, _) = make_tree_h3_full();
            let query = Query::new_single_key(vec![4]);
            let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
                .unwrap()
                .expect("generate should succeed");

            let (root, entries1) = proof
                .verify_and_get_root::<Vec<(u16, Vec<u8>)>>(3, 7)
                .expect("verify_and_get_root should succeed");
            let (root2, entries2) = proof
                .verify_for_query::<Vec<(u16, Vec<u8>)>>(&query, 3, 7)
                .expect("verify_for_query should succeed");

            assert_eq!(root, root2);
            assert_eq!(entries1, entries2);
        }

        #[test]
        fn encode_decode_then_verify_for_query() {
            let (store, _) = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![2]..vec![6]);
            let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
                .unwrap()
                .expect("generate should succeed");

            let bytes = proof.encode_to_vec().expect("encode should succeed");
            let decoded = DenseTreeProof::decode_from_slice(&bytes).expect("decode should succeed");

            let (_root, entries) = decoded
                .verify_for_query::<Vec<(u16, Vec<u8>)>>(&query, 3, 7)
                .expect("verify_for_query on decoded proof should succeed");
            assert_eq!(entries.len(), 4);
        }

        #[test]
        fn wrong_count_changes_root_hash() {
            // Passing the wrong count to verify produces a different root hash,
            // so verify_against_expected_root rejects it.
            let (store, expected_root) = make_tree_h3_full();
            let query = Query::new_range_full();
            let proof = DenseTreeProof::generate_for_query(3, 7, &query, &store)
                .unwrap()
                .expect("generate should succeed");
            // Pass wrong count=5 instead of correct count=7
            let result = proof.verify_against_expected_root::<Vec<(u16, Vec<u8>)>>(&expected_root, 3, 5);
            assert!(result.is_err(), "wrong count should change root hash");
        }

        #[test]
        fn query_beyond_count_clamped_consistently() {
            // Query [5..100) on a tree with count=11.
            // Both generate and verify clamp to count=11 → {5..10}.
            let store = MemStore::new();
            let mut tree =
                crate::tree::DenseFixedSizedMerkleTree::new(4).expect("height 4 should be valid");
            for i in 0..11u8 {
                tree.insert(&[i], &store)
                    .unwrap()
                    .expect("insert should succeed");
            }
            let mut query = Query::new();
            query.insert_range_from(vec![5]..);
            let entries = gen_and_verify(4, 11, &query, &store);
            assert_eq!(entries.len(), 6);
            assert_eq!(entries[0].0, 5);
            assert_eq!(entries[5].0, 10);
        }

        #[test]
        fn completely_disjoint_proof_and_query() {
            // Proof covers {0,1}, query asks for {5,6} — both incomplete and unsound
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range(vec![0]..vec![2]);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range(vec![5]..vec![7]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn partial_overlap_proof_and_query() {
            // Proof covers {2,3,4}, query asks for {3,4,5} — both incomplete (5)
            // and unsound (2)
            let (store, _) = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range(vec![2]..vec![5]);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range(vec![3]..vec![6]);
                q
            };
            gen_and_expect_mismatch(3, 7, &gen_query, &verify_query, &store);
        }

        #[test]
        fn larger_tree_verify_for_query() {
            let (store, _) = make_tree_h4_full();
            let mut query = Query::new();
            query.insert_key(vec![0]);
            query.insert_range_inclusive(vec![7]..=vec![10]);
            query.insert_key(vec![14]);
            let entries = gen_and_verify(4, 15, &query, &store);
            assert_eq!(entries.len(), 6); // {0, 7, 8, 9, 10, 14}
            assert_eq!(entries[0].0, 0);
            assert_eq!(entries[1].0, 7);
            assert_eq!(entries[4].0, 10);
            assert_eq!(entries[5].0, 14);
        }
    }
}
