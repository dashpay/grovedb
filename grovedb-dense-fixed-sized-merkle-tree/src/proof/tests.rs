#[cfg(test)]
mod proof_tests {
    use grovedb_query::Query;

    use crate::{proof::*, test_utils::MemStorageContext, tree::DenseFixedSizedMerkleTree};

    /// Height-3 tree, fully filled (7 positions, values [0]..[6]).
    fn make_tree_h3_full() -> DenseFixedSizedMerkleTree<MemStorageContext> {
        let mut tree = DenseFixedSizedMerkleTree::new(3, MemStorageContext::new())
            .expect("height 3 should be valid");
        for i in 0..7u8 {
            tree.insert(&[i]).unwrap().expect("insert should succeed");
        }
        tree
    }

    /// Height-4 tree, fully filled (15 positions, values [0]..[14] as BE u16).
    fn make_tree_h4_full() -> DenseFixedSizedMerkleTree<MemStorageContext> {
        let mut tree = DenseFixedSizedMerkleTree::new(4, MemStorageContext::new())
            .expect("height 4 should be valid");
        for i in 0u16..15 {
            tree.insert(&i.to_be_bytes())
                .unwrap()
                .expect("insert should succeed");
        }
        tree
    }

    /// Height-3 tree, partially filled (5 of 7 positions).
    fn make_tree_h3_partial() -> DenseFixedSizedMerkleTree<MemStorageContext> {
        let mut tree = DenseFixedSizedMerkleTree::new(3, MemStorageContext::new())
            .expect("height 3 should be valid");
        for i in 0..5u8 {
            tree.insert(&[i]).unwrap().expect("insert should succeed");
        }
        tree
    }

    /// Helper: generate a proof for the query and verify it matches the query
    /// (completeness + soundness). Returns the verified entries.
    fn gen_and_verify(
        tree: &DenseFixedSizedMerkleTree<MemStorageContext>,
        query: &Query,
    ) -> Vec<(u16, Vec<u8>)> {
        let height = tree.height();
        let count = tree.count();
        let proof = DenseTreeProof::generate_for_query(tree, query)
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
        tree: &DenseFixedSizedMerkleTree<MemStorageContext>,
        gen_query: &Query,
        verify_query: &Query,
    ) {
        let height = tree.height();
        let count = tree.count();
        let proof = DenseTreeProof::generate_for_query(tree, gen_query)
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
        let tree = make_tree_h3_full();
        let root = tree.root_hash().unwrap().expect("root hash");
        let proof = DenseTreeProof::generate(&tree, &[4])
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
        let tree = make_tree_h3_full();
        let root = tree.root_hash().unwrap().expect("root hash");
        let proof = DenseTreeProof::generate(&tree, &[1])
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
        let tree = make_tree_h3_full();
        let root = tree.root_hash().unwrap().expect("root hash");
        let proof = DenseTreeProof::generate(&tree, &[0])
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
        let tree = make_tree_h3_full();
        let root = tree.root_hash().unwrap().expect("root hash");
        let proof = DenseTreeProof::generate(&tree, &[3, 5, 6])
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
        let tree = make_tree_h3_full();
        let root = tree.root_hash().unwrap().expect("root hash");
        let proof = DenseTreeProof::generate(&tree, &[0, 1, 2, 3, 4, 5, 6])
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
        let tree = make_tree_h3_full();
        let proof = DenseTreeProof::generate(&tree, &[4])
            .unwrap()
            .expect("generate should succeed");
        let wrong_root = [0xFFu8; 32];
        let result = proof.verify_against_expected_root::<Vec<(u16, Vec<u8>)>>(&wrong_root, 3, 7);
        assert!(result.is_err(), "verification should fail with wrong root");
    }

    #[test]
    fn test_proof_encode_decode_roundtrip() {
        let tree = make_tree_h3_full();
        let root = tree.root_hash().unwrap().expect("root hash");
        let proof = DenseTreeProof::generate(&tree, &[2, 4])
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
        let mut tree = DenseFixedSizedMerkleTree::new(3, MemStorageContext::new())
            .expect("height 3 should be valid");
        for i in 0..3u8 {
            tree.insert(&[i]).unwrap().expect("insert should succeed");
        }
        let root = tree.root_hash().unwrap().expect("root hash should succeed");

        let proof = DenseTreeProof::generate(&tree, &[0, 1, 2])
            .unwrap()
            .expect("generate should succeed");
        let verified = proof
            .verify_against_expected_root::<Vec<(u16, Vec<u8>)>>(&root, 3, 3)
            .expect("verify should succeed");
        assert_eq!(verified.len(), 3);
    }

    #[test]
    fn test_proof_height_1_tree() {
        let mut tree = DenseFixedSizedMerkleTree::new(1, MemStorageContext::new())
            .expect("height 1 should be valid");
        tree.insert(b"hello")
            .unwrap()
            .expect("insert should succeed");
        let root = tree.root_hash().unwrap().expect("root hash should succeed");

        let proof = DenseTreeProof::generate(&tree, &[0])
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
        let mut tree = DenseFixedSizedMerkleTree::new(2, MemStorageContext::new())
            .expect("height 2 should be valid");
        tree.insert(b"root_val")
            .unwrap()
            .expect("insert should succeed");
        tree.insert(b"left_val")
            .unwrap()
            .expect("insert should succeed");
        tree.insert(b"right_val")
            .unwrap()
            .expect("insert should succeed");
        let root = tree.root_hash().unwrap().expect("root hash should succeed");

        let proof = DenseTreeProof::generate(&tree, &[2])
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
        let tree = make_tree_h3_full();
        let result = DenseTreeProof::generate(&tree, &[7]).unwrap();
        assert!(result.is_err(), "position 7 should be out of range");
    }

    #[test]
    fn test_proof_deduplicates_positions() {
        let tree = make_tree_h3_full();
        let root = tree.root_hash().unwrap().expect("root hash");
        let proof = DenseTreeProof::generate(&tree, &[4, 4, 4])
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
        let tree = make_tree_h3_full();
        let query = Query::new_single_key(vec![4]);
        let entries = gen_and_verify(&tree, &query);
        assert_eq!(entries, vec![(4, vec![4u8])]);
    }

    #[test]
    fn test_encoding_2byte_key() {
        let tree = make_tree_h3_full();
        let query = Query::new_single_key(vec![0, 5]);
        let entries = gen_and_verify(&tree, &query);
        assert_eq!(entries, vec![(5, vec![5u8])]);
    }

    #[test]
    fn test_encoding_2byte_range() {
        let tree = make_tree_h3_full();
        let mut query = Query::new();
        query.insert_range(vec![0, 3]..vec![0, 6]);
        let entries = gen_and_verify(&tree, &query);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].0, 3);
        assert_eq!(entries[2].0, 5);
    }

    #[test]
    fn test_encoding_0byte_rejected() {
        let tree = make_tree_h3_full();
        let query = Query::new_single_key(vec![]);
        let result = DenseTreeProof::generate_for_query(&tree, &query).unwrap();
        assert!(
            result.is_err(),
            "0-byte position encoding should be rejected"
        );
    }

    #[test]
    fn test_encoding_3byte_rejected() {
        let tree = make_tree_h3_full();
        let query = Query::new_single_key(vec![0, 0, 5]);
        let result = DenseTreeProof::generate_for_query(&tree, &query).unwrap();
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
            let tree = make_tree_h3_full();
            let query = Query::new_single_key(vec![4]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries, vec![(4, vec![4u8])]);
        }

        #[test]
        fn inclusion_root_position() {
            let tree = make_tree_h3_full();
            let query = Query::new_single_key(vec![0]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries, vec![(0, vec![0u8])]);
        }

        #[test]
        fn inclusion_last_position() {
            let tree = make_tree_h3_full();
            let query = Query::new_single_key(vec![6]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries, vec![(6, vec![6u8])]);
        }

        #[test]
        fn soundness_extra_positions() {
            let tree = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range(vec![3]..vec![6]);
                q
            };
            let verify_query = Query::new_single_key(vec![4]);
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn completeness_missing_position() {
            let tree = make_tree_h3_full();
            let gen_query = Query::new_single_key(vec![4]);
            let verify_query = Query::new_single_key(vec![5]);
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn beyond_count_clamped() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_key(vec![3]);
            query.insert_key(vec![10]);
            let entries = gen_and_verify(&tree, &query);
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
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![3]..vec![6]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0], (3, vec![3u8]));
            assert_eq!(entries[2], (5, vec![5u8]));
        }

        #[test]
        fn inclusion_from_start() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![0]..vec![3]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].0, 0);
            assert_eq!(entries[2].0, 2);
        }

        #[test]
        fn inclusion_to_end() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![5]..vec![7]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].0, 5);
            assert_eq!(entries[1].0, 6);
        }

        #[test]
        fn single_element_range() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![4]..vec![5]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries, vec![(4, vec![4u8])]);
        }

        #[test]
        fn empty_range_equal_bounds() {
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
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn completeness_narrower_proof() {
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn clamped_beyond_count() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![5]..vec![100]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].0, 5);
            assert_eq!(entries[1].0, 6);
        }

        #[test]
        fn entirely_beyond_count() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![10]..vec![20]);
            let proof = DenseTreeProof::generate_for_query(&tree, &query)
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
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_inclusive(vec![3]..=vec![5]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].0, 3);
            assert_eq!(entries[2].0, 5);
        }

        #[test]
        fn single_element() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_inclusive(vec![4]..=vec![4]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries, vec![(4, vec![4u8])]);
        }

        #[test]
        fn entire_tree() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_inclusive(vec![0]..=vec![6]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 7);
        }

        #[test]
        fn soundness_wider_proof() {
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn completeness_narrower_proof() {
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn clamped_beyond_count() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_inclusive(vec![5]..=vec![100]);
            let entries = gen_and_verify(&tree, &query);
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
            let tree = make_tree_h3_full();
            let query = Query::new_range_full();
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 7);
            for (pos, val) in &entries {
                assert_eq!(*val, vec![*pos as u8]);
            }
        }

        #[test]
        fn completeness_partial_proof_rejected() {
            let tree = make_tree_h3_full();
            let gen_query = Query::new_single_key(vec![4]);
            let verify_query = Query::new_range_full();
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn soundness_full_proof_against_subset() {
            let tree = make_tree_h3_full();
            let gen_query = Query::new_range_full();
            let verify_query = {
                let mut q = Query::new();
                q.insert_range(vec![3]..vec![5]);
                q
            };
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn partial_tree() {
            let tree = make_tree_h3_partial();
            let count = tree.count();
            let query = Query::new_range_full();
            let entries = gen_and_verify(&tree, &query);
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
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_from(vec![5]..);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].0, 5);
            assert_eq!(entries[1].0, 6);
        }

        #[test]
        fn inclusion_from_zero() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_from(vec![0]..);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 7);
        }

        #[test]
        fn inclusion_from_last() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_from(vec![6]..);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries, vec![(6, vec![6u8])]);
        }

        #[test]
        fn soundness_wider_proof() {
            let tree = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range_from(vec![3]..);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range_from(vec![5]..);
                q
            };
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn completeness_narrower_proof() {
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn from_at_count_yields_empty() {
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
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_to(..vec![3]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].0, 0);
            assert_eq!(entries[2].0, 2);
        }

        #[test]
        fn single_element() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_to(..vec![1]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries, vec![(0, vec![0u8])]);
        }

        #[test]
        fn soundness() {
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn completeness() {
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn clamped_beyond_count() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_to(..vec![50]);
            let entries = gen_and_verify(&tree, &query);
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
    // Remaining range types — abbreviated (same pattern as above)
    // =======================================================================

    mod range_to_inclusive {
        use super::*;

        #[test]
        fn inclusion() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_to_inclusive(..=vec![2]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 3);
        }

        #[test]
        fn single_element_to_zero() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_to_inclusive(..=vec![0]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries, vec![(0, vec![0u8])]);
        }

        #[test]
        fn entire_tree() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_to_inclusive(..=vec![6]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 7);
        }

        #[test]
        fn soundness() {
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn completeness() {
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn clamped_beyond_count() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_to_inclusive(..=vec![50]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 7);
        }
    }

    mod range_after {
        use super::*;

        #[test]
        fn inclusion() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after(vec![4]..);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 2);
            assert_eq!(entries[0].0, 5);
            assert_eq!(entries[1].0, 6);
        }

        #[test]
        fn from_zero_excludes_root() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after(vec![0]..);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 6);
            assert_eq!(entries[0].0, 1);
        }

        #[test]
        fn from_last_minus_one() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after(vec![5]..);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries, vec![(6, vec![6u8])]);
        }

        #[test]
        fn from_last_yields_empty() {
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
            let tree = make_tree_h3_full();
            let gen_query = {
                let mut q = Query::new();
                q.insert_range_after(vec![2]..);
                q
            };
            let verify_query = {
                let mut q = Query::new();
                q.insert_range_after(vec![4]..);
                q
            };
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn completeness() {
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }
    }

    mod range_after_to {
        use super::*;

        #[test]
        fn inclusion() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after_to(vec![1]..vec![5]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].0, 2);
            assert_eq!(entries[2].0, 4);
        }

        #[test]
        fn single_element() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after_to(vec![2]..vec![4]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries, vec![(3, vec![3u8])]);
        }

        #[test]
        fn adjacent_bounds_yields_empty() {
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
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn completeness() {
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn clamped_beyond_count() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after_to(vec![3]..vec![100]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].0, 4);
            assert_eq!(entries[2].0, 6);
        }
    }

    mod range_after_to_inclusive {
        use super::*;

        #[test]
        fn inclusion() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after_to_inclusive(vec![1]..=vec![4]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].0, 2);
            assert_eq!(entries[2].0, 4);
        }

        #[test]
        fn single_element() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after_to_inclusive(vec![2]..=vec![3]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries, vec![(3, vec![3u8])]);
        }

        #[test]
        fn same_start_end_yields_empty() {
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
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn completeness() {
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn clamped_beyond_count() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_after_to_inclusive(vec![3]..=vec![100]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].0, 4);
            assert_eq!(entries[2].0, 6);
        }
    }

    // =======================================================================
    // Disjoint ranges
    // =======================================================================

    mod disjoint {
        use super::*;

        #[test]
        fn two_disjoint_ranges() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![0]..vec![2]);
            query.insert_range(vec![5]..vec![7]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 4);
            assert_eq!(entries[0].0, 0);
            assert_eq!(entries[1].0, 1);
            assert_eq!(entries[2].0, 5);
            assert_eq!(entries[3].0, 6);
        }

        #[test]
        fn three_scattered_keys() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_key(vec![0]);
            query.insert_key(vec![3]);
            query.insert_key(vec![6]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 3);
        }

        #[test]
        fn key_plus_range() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_key(vec![0]);
            query.insert_range(vec![4]..vec![7]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 4);
        }

        #[test]
        fn range_from_plus_range_to() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range_to(..vec![2]);
            query.insert_range_from(vec![5]..);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 4);
        }

        #[test]
        fn soundness_disjoint_proof_vs_subset() {
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn completeness_subset_proof_vs_disjoint() {
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn overlapping_items_deduplicate() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![1]..vec![4]);
            query.insert_range(vec![3]..vec![6]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 5);
        }

        #[test]
        fn key_inside_range() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_key(vec![3]);
            query.insert_range(vec![2]..vec![5]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 3);
        }

        #[test]
        fn mixed_range_types() {
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_key(vec![0]);
            query.insert_range_inclusive(vec![2]..=vec![3]);
            query.insert_range_after(vec![4]..);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 5);
        }

        #[test]
        fn three_disjoint_ranges_larger_tree() {
            let tree = make_tree_h4_full();
            let mut query = Query::new();
            query.insert_range(vec![1]..vec![3]);
            query.insert_range(vec![7]..vec![9]);
            query.insert_range(vec![12]..vec![14]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 6);
        }

        #[test]
        fn soundness_wrong_gap() {
            let tree = make_tree_h3_full();
            let gen_query = Query::new_range_full();
            let verify_query = {
                let mut q = Query::new();
                q.insert_range(vec![0]..vec![2]);
                q.insert_range(vec![5]..vec![7]);
                q
            };
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn completeness_missing_second_range() {
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }
    }

    // =======================================================================
    // Partial tree
    // =======================================================================

    mod partial_tree {
        use super::*;

        #[test]
        fn range_clamped_to_count() {
            let tree = make_tree_h3_partial();
            let mut query = Query::new();
            query.insert_range(vec![3]..vec![7]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 2);
        }

        #[test]
        fn range_from_clamped() {
            let tree = make_tree_h3_partial();
            let mut query = Query::new();
            query.insert_range_from(vec![3]..);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 2);
        }

        #[test]
        fn range_full_matches_count() {
            let tree = make_tree_h3_partial();
            let count = tree.count();
            let query = Query::new_range_full();
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), count as usize);
        }

        #[test]
        fn key_beyond_partial_excluded() {
            let tree = make_tree_h3_partial();
            let mut query = Query::new();
            query.insert_key(vec![2]);
            query.insert_key(vec![6]);
            let entries = gen_and_verify(&tree, &query);
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
            let tree = DenseFixedSizedMerkleTree::new(3, MemStorageContext::new())
                .expect("height 3 should be valid");
            let query = Query::new();
            let proof = DenseTreeProof::generate_for_query(&tree, &query)
                .unwrap()
                .expect("should succeed for empty query on empty tree");
            let (_root, entries) = proof
                .verify_for_query::<Vec<(u16, Vec<u8>)>>(&query, 3, 0)
                .expect("should succeed");
            assert!(entries.is_empty());
        }

        #[test]
        fn root_hash_consistency() {
            let tree = make_tree_h3_full();
            let expected_root = tree.root_hash().unwrap().expect("root hash");
            let mut query = Query::new();
            query.insert_range(vec![2]..vec![5]);
            let proof = DenseTreeProof::generate_for_query(&tree, &query)
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
            let tree = make_tree_h3_full();
            let query = Query::new_single_key(vec![4]);
            let proof = DenseTreeProof::generate_for_query(&tree, &query)
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
            let tree = make_tree_h3_full();
            let mut query = Query::new();
            query.insert_range(vec![2]..vec![6]);
            let proof = DenseTreeProof::generate_for_query(&tree, &query)
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
            let tree = make_tree_h3_full();
            let expected_root = tree.root_hash().unwrap().expect("root hash");
            let query = Query::new_range_full();
            let proof = DenseTreeProof::generate_for_query(&tree, &query)
                .unwrap()
                .expect("generate should succeed");
            let result =
                proof.verify_against_expected_root::<Vec<(u16, Vec<u8>)>>(&expected_root, 3, 5);
            assert!(result.is_err(), "wrong count should change root hash");
        }

        #[test]
        fn query_beyond_count_clamped_consistently() {
            let mut tree = DenseFixedSizedMerkleTree::new(4, MemStorageContext::new())
                .expect("height 4 should be valid");
            for i in 0..11u8 {
                tree.insert(&[i]).unwrap().expect("insert should succeed");
            }
            let mut query = Query::new();
            query.insert_range_from(vec![5]..);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 6);
            assert_eq!(entries[0].0, 5);
            assert_eq!(entries[5].0, 10);
        }

        #[test]
        fn completely_disjoint_proof_and_query() {
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn partial_overlap_proof_and_query() {
            let tree = make_tree_h3_full();
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
            gen_and_expect_mismatch(&tree, &gen_query, &verify_query);
        }

        #[test]
        fn larger_tree_verify_for_query() {
            let tree = make_tree_h4_full();
            let mut query = Query::new();
            query.insert_key(vec![0]);
            query.insert_range_inclusive(vec![7]..=vec![10]);
            query.insert_key(vec![14]);
            let entries = gen_and_verify(&tree, &query);
            assert_eq!(entries.len(), 6);
        }
    }
}
