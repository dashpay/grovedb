#[cfg(test)]
mod proof_tests {
    use grovedb_merkle_mountain_range::MmrTreeProof;
    use grovedb_query::{Query, QueryItem};

    use crate::{proof::*, test_utils::MemStorageContext, BulkAppendTree};

    /// Helper: build a test tree and return it (tree owns the storage).
    fn build_test_tree(
        height: u8,
        values: &[Vec<u8>],
    ) -> (
        [u8; 32], // state_root
        BulkAppendTree<MemStorageContext>,
    ) {
        let mut tree = BulkAppendTree::new(height, MemStorageContext::new()).expect("create tree");

        let mut last_state_root = [0u8; 32];
        for value in values {
            let result = tree.append(value).expect("append value");
            last_state_root = result.state_root;
        }

        (last_state_root, tree)
    }

    /// Helper: encode a u64 as big-endian bytes for use in QueryItem.
    fn pos_bytes(pos: u64) -> Vec<u8> {
        pos.to_be_bytes().to_vec()
    }

    /// Helper: build a range query [start..end).
    fn range_query(start: u64, end: u64) -> Query {
        let mut q = Query::default();
        q.items
            .push(QueryItem::Range(pos_bytes(start)..pos_bytes(end)));
        q
    }

    /// Helper: build a full-range query.
    fn full_range_query() -> Query {
        let mut q = Query::default();
        q.items.push(QueryItem::RangeFull(..));
        q
    }

    #[test]
    fn test_bulk_proof_buffer_only() {
        // height=3, capacity=7. 3 values stay in buffer.
        let height = 3u8;
        let values: Vec<Vec<u8>> = (0..3u32)
            .map(|i| format!("val_{}", i).into_bytes())
            .collect();
        let (state_root, tree) = build_test_tree(height, &values);

        let query = range_query(0, 3);
        let proof = BulkAppendTreeProof::generate(&query, &tree).expect("generate proof");

        // No chunks — empty MMR proof
        assert_eq!(proof.chunk_proof.mmr_size(), 0);
        assert_eq!(proof.buffer_proof.entries.len(), 3);

        let result = proof
            .verify(&state_root, height, tree.total_count)
            .expect("verify proof");
        let vals = result.values_in_range(0, 3).expect("extract range");
        assert_eq!(vals.len(), 3);
        assert_eq!(vals[0], (0, b"val_0".to_vec()));
        assert_eq!(vals[1], (1, b"val_1".to_vec()));
        assert_eq!(vals[2], (2, b"val_2".to_vec()));
    }

    #[test]
    fn test_bulk_proof_chunk_and_buffer() {
        // Height=2, capacity=3, epoch_size=4.
        // 5 values -> 1 chunk (0..4) + 1 buffer (4)
        let height = 2u8;
        let values: Vec<Vec<u8>> = (0..5u32)
            .map(|i| format!("data_{}", i).into_bytes())
            .collect();
        let (state_root, tree) = build_test_tree(height, &values);
        let total_count = tree.total_count;

        assert_eq!(total_count, 5);

        // Query range 0..5 (all data)
        let query = range_query(0, 5);
        let proof = BulkAppendTreeProof::generate(&query, &tree).expect("generate proof");

        assert!(proof.chunk_proof.mmr_size() > 0);
        assert!(!proof.buffer_proof.entries.is_empty());

        let result = proof
            .verify(&state_root, height, total_count)
            .expect("verify proof");
        let vals = result.values_in_range(0, 5).expect("extract range");
        assert_eq!(vals.len(), 5);
        for i in 0..5u32 {
            assert_eq!(vals[i as usize].1, format!("data_{}", i).into_bytes());
        }
    }

    #[test]
    fn test_bulk_proof_multiple_chunks() {
        // Height=2, capacity=3, epoch_size=4.
        // 9 values -> 2 chunks (0..8) + 1 buffer (8)
        let height = 2u8;
        let values: Vec<Vec<u8>> = (0..9u32).map(|i| format!("e_{}", i).into_bytes()).collect();
        let (state_root, tree) = build_test_tree(height, &values);
        let total_count = tree.total_count;

        // Query range 1..8 — overlaps both chunks (0..4 and 4..8)
        let query = range_query(1, 8);
        let proof = BulkAppendTreeProof::generate(&query, &tree).expect("generate proof");

        assert_eq!(proof.chunk_proof.leaves().len(), 2);

        let result = proof
            .verify(&state_root, height, total_count)
            .expect("verify proof");
        let vals = result.values_in_range(1, 8).expect("extract range");
        assert_eq!(vals.len(), 7);
        assert_eq!(vals[0], (1, b"e_1".to_vec()));
        assert_eq!(vals[6], (7, b"e_7".to_vec()));
    }

    #[test]
    fn test_bulk_proof_wrong_state_root_fails() {
        // Height=2, capacity=3, epoch_size=4. 3 values stay in buffer.
        let height = 2u8;
        let values: Vec<Vec<u8>> = (0..3u32).map(|i| format!("x_{}", i).into_bytes()).collect();
        let (_state_root, tree) = build_test_tree(height, &values);
        let total_count = tree.total_count;

        let query = range_query(0, 3);
        let proof = BulkAppendTreeProof::generate(&query, &tree).expect("generate proof");

        let wrong_root = [0xFFu8; 32];
        assert!(proof.verify(&wrong_root, height, total_count).is_err());
    }

    #[test]
    fn test_verify_against_query_buffer_only() {
        let height = 3u8;
        let values: Vec<Vec<u8>> = (0..3u32)
            .map(|i| format!("val_{}", i).into_bytes())
            .collect();
        let (state_root, tree) = build_test_tree(height, &values);
        let total_count = tree.total_count;

        let query = range_query(0, 3);
        let proof = BulkAppendTreeProof::generate(&query, &tree).expect("generate proof");

        let vals: Vec<(u64, Vec<u8>)> = proof
            .verify_against_query(&state_root, height, total_count, &query)
            .expect("verify against query");
        assert_eq!(vals.len(), 3);
        assert_eq!(vals[0], (0, b"val_0".to_vec()));
        assert_eq!(vals[2], (2, b"val_2".to_vec()));
    }

    #[test]
    fn test_verify_against_query_chunks_and_buffer() {
        // height=2, epoch_size=4. 9 values -> 2 chunks + 1 buffer
        let height = 2u8;
        let values: Vec<Vec<u8>> = (0..9u32).map(|i| format!("v_{}", i).into_bytes()).collect();
        let (state_root, tree) = build_test_tree(height, &values);
        let total_count = tree.total_count;

        // Full range query
        let query = full_range_query();
        let proof = BulkAppendTreeProof::generate(&query, &tree).expect("generate proof");

        let vals: Vec<(u64, Vec<u8>)> = proof
            .verify_against_query(&state_root, height, total_count, &query)
            .expect("verify full range");
        assert_eq!(vals.len(), 9);
        for i in 0..9u32 {
            assert_eq!(vals[i as usize].1, format!("v_{}", i).into_bytes());
        }
    }

    #[test]
    fn test_verify_against_query_buffer_subrange() {
        // height=2, epoch_size=4. 6 values -> 1 chunk (0..4) + 2 buffer (4,5)
        // Query only the buffer portion: [4, 6)
        let height = 2u8;
        let values: Vec<Vec<u8>> = (0..6u32).map(|i| format!("d_{}", i).into_bytes()).collect();
        let (state_root, tree) = build_test_tree(height, &values);
        let total_count = tree.total_count;

        let query = range_query(4, 6);
        let proof = BulkAppendTreeProof::generate(&query, &tree).expect("generate proof");

        let vals: Vec<(u64, Vec<u8>)> = proof
            .verify_against_query(&state_root, height, total_count, &query)
            .expect("verify buffer subrange");
        assert_eq!(vals.len(), 2);
        assert_eq!(vals[0], (4, b"d_4".to_vec()));
        assert_eq!(vals[1], (5, b"d_5".to_vec()));
    }

    #[test]
    fn test_verify_against_query_specific_keys() {
        // height=2, epoch_size=4. 9 values -> 2 chunks + 1 buffer
        // Query specific positions: 1, 5, 8
        let height = 2u8;
        let values: Vec<Vec<u8>> = (0..9u32).map(|i| format!("k_{}", i).into_bytes()).collect();
        let (state_root, tree) = build_test_tree(height, &values);
        let total_count = tree.total_count;

        let query = full_range_query();
        let proof = BulkAppendTreeProof::generate(&query, &tree).expect("generate proof");

        let mut verify_query = Query::default();
        verify_query.items.push(QueryItem::Key(pos_bytes(1)));
        verify_query.items.push(QueryItem::Key(pos_bytes(5)));
        verify_query.items.push(QueryItem::Key(pos_bytes(8)));

        let vals: Vec<(u64, Vec<u8>)> = proof
            .verify_against_query(&state_root, height, total_count, &verify_query)
            .expect("verify specific keys");
        assert_eq!(vals.len(), 3);
        assert_eq!(vals[0], (1, b"k_1".to_vec()));
        assert_eq!(vals[1], (5, b"k_5".to_vec()));
        assert_eq!(vals[2], (8, b"k_8".to_vec()));
    }

    #[test]
    fn test_verify_against_query_empty_result() {
        // Query beyond total_count — positions clamped, returns empty
        let height = 2u8;
        let values: Vec<Vec<u8>> = vec![b"a".to_vec()];
        let (state_root, tree) = build_test_tree(height, &values);
        let total_count = tree.total_count;

        let query = range_query(0, 1);
        let proof = BulkAppendTreeProof::generate(&query, &tree).expect("generate proof");

        let mut far_query = Query::default();
        far_query.items.push(QueryItem::Key(pos_bytes(100)));

        let vals: Vec<(u64, Vec<u8>)> = proof
            .verify_against_query(&state_root, height, total_count, &far_query)
            .expect("query beyond total_count returns empty");
        assert!(vals.is_empty());
    }

    #[test]
    fn test_bulk_proof_encode_decode() {
        let height = 2u8;
        let values: Vec<Vec<u8>> = (0..4u32).map(|i| format!("r_{}", i).into_bytes()).collect();
        let (state_root, tree) = build_test_tree(height, &values);
        let total_count = tree.total_count;

        let query = range_query(0, 4);
        let proof = BulkAppendTreeProof::generate(&query, &tree).expect("generate proof");

        let bytes = proof.encode_to_vec().expect("encode proof");
        let decoded = BulkAppendTreeProof::decode_from_slice(&bytes).expect("decode proof");
        let result = decoded
            .verify(&state_root, height, total_count)
            .expect("verify decoded proof");
        let vals = result.values_in_range(0, 4).expect("extract range");
        assert_eq!(vals.len(), 4);
    }

    #[test]
    fn test_bytes_to_global_position_rejects_invalid_lengths() {
        assert_eq!(super::super::bytes_to_global_position(&[7]).unwrap(), 7);
        assert_eq!(
            super::super::bytes_to_global_position(&[0, 0, 0, 0, 0, 0, 1, 2]).unwrap(),
            258
        );

        assert!(matches!(
            super::super::bytes_to_global_position(&[]).expect_err("empty bytes must fail"),
            BulkAppendError::InvalidInput(_)
        ));
        assert!(matches!(
            super::super::bytes_to_global_position(&[0; 9]).expect_err("9 bytes must fail"),
            BulkAppendError::InvalidInput(_)
        ));
    }

    #[test]
    fn test_query_to_ranges_rejects_subqueries() {
        let mut query = Query::new_range_full();
        query.set_subquery(Query::new());
        let err = super::super::query_to_ranges(&query, 10).expect_err("subquery must fail");
        assert!(matches!(err, BulkAppendError::InvalidInput(_)));
    }

    #[test]
    fn test_query_to_ranges_merges_clamps_and_filters() {
        let mut query = Query::default();
        query
            .items
            .push(QueryItem::Range(pos_bytes(2)..pos_bytes(5))); // [2,5)
        query
            .items
            .push(QueryItem::RangeInclusive(pos_bytes(5)..=pos_bytes(6))); // [5,7)
        query.items.push(QueryItem::RangeTo(..pos_bytes(2))); // [0,2)
        query.items.push(QueryItem::RangeFrom(pos_bytes(8)..)); // [8,10)
        query
            .items
            .push(QueryItem::RangeAfterTo(pos_bytes(6)..pos_bytes(9))); // [7,9)
        query.items.push(QueryItem::Key(pos_bytes(50))); // ignored
        query
            .items
            .push(QueryItem::Range(pos_bytes(9)..pos_bytes(1))); // ignored

        let ranges = super::super::query_to_ranges(&query, 10).expect("query_to_ranges");
        assert_eq!(ranges, vec![(0, 10)]);
    }

    #[test]
    fn test_in_ranges_boundaries() {
        let ranges = vec![(2, 4), (6, 9)];
        assert!(!super::super::in_ranges(1, &ranges));
        assert!(super::super::in_ranges(2, &ranges));
        assert!(super::super::in_ranges(3, &ranges));
        assert!(!super::super::in_ranges(4, &ranges));
        assert!(!super::super::in_ranges(5, &ranges));
        assert!(super::super::in_ranges(6, &ranges));
        assert!(super::super::in_ranges(8, &ranges));
        assert!(!super::super::in_ranges(9, &ranges));
    }

    #[test]
    fn test_generate_buffer_only_query_still_anchors_chunk_proof() {
        // height=2, epoch_size=4 -> 5 values = 1 chunk + 1 buffer
        let height = 2u8;
        let values: Vec<Vec<u8>> = (0..5u32).map(|i| format!("z_{}", i).into_bytes()).collect();
        let (state_root, tree) = build_test_tree(height, &values);

        let mut query = Query::default();
        query.items.push(QueryItem::Key(pos_bytes(4))); // only buffer position
        let proof = BulkAppendTreeProof::generate(&query, &tree).expect("generate proof");

        // Chunk anchor for root verification
        assert_eq!(proof.chunk_proof.leaves().len(), 1);
        assert_eq!(proof.chunk_proof.leaves()[0].0, 0);

        let vals: Vec<(u64, Vec<u8>)> = proof
            .verify_against_query(&state_root, height, tree.total_count, &query)
            .expect("verify query");
        assert_eq!(vals, vec![(4, b"z_4".to_vec())]);
    }

    #[test]
    fn test_generate_chunk_only_query_still_anchors_buffer_proof() {
        // height=2, epoch_size=4 -> 5 values = 1 chunk + 1 buffer
        let height = 2u8;
        let values: Vec<Vec<u8>> = (0..5u32).map(|i| format!("c_{}", i).into_bytes()).collect();
        let (state_root, tree) = build_test_tree(height, &values);

        let query = range_query(0, 4); // chunk only
        let proof = BulkAppendTreeProof::generate(&query, &tree).expect("generate proof");

        // Buffer anchor entry (position 0) is needed to verify dense root
        assert_eq!(proof.buffer_proof.entries.len(), 1);
        assert_eq!(proof.buffer_proof.entries[0].0, 0);

        let vals: Vec<(u64, Vec<u8>)> = proof
            .verify_against_query(&state_root, height, tree.total_count, &query)
            .expect("verify query");
        assert_eq!(vals.len(), 4);
        assert_eq!(vals[0], (0, b"c_0".to_vec()));
        assert_eq!(vals[3], (3, b"c_3".to_vec()));
    }

    #[test]
    fn test_verify_and_compute_root_rejects_invalid_height() {
        let height = 2u8;
        let values: Vec<Vec<u8>> = (0..4u32).map(|i| vec![i as u8]).collect();
        let (_state_root, tree) = build_test_tree(height, &values);
        let proof =
            BulkAppendTreeProof::generate(&full_range_query(), &tree).expect("generate proof");

        let err = proof
            .verify_and_compute_root(0, tree.total_count)
            .expect_err("height 0 must fail");
        assert!(matches!(err, BulkAppendError::InvalidProof(_)));
    }

    #[test]
    fn test_verify_and_compute_root_rejects_mmr_size_mismatch() {
        let height = 2u8;
        let values: Vec<Vec<u8>> = (0..4u32).map(|i| vec![i as u8]).collect();
        let (_state_root, tree) = build_test_tree(height, &values);
        let mut proof =
            BulkAppendTreeProof::generate(&full_range_query(), &tree).expect("generate proof");

        proof.chunk_proof = MmrTreeProof::new(
            proof.chunk_proof.mmr_size() + 1,
            proof.chunk_proof.leaves().to_vec(),
            proof.chunk_proof.proof_items().to_vec(),
        );

        let err = proof
            .verify_and_compute_root(height, tree.total_count)
            .expect_err("mmr_size mismatch must fail");
        assert!(matches!(err, BulkAppendError::InvalidProof(_)));
    }

    #[test]
    fn test_verify_and_compute_root_rejects_non_empty_buffer_when_dense_is_empty() {
        // height=2, epoch_size=4 -> total_count=4 leaves empty buffer
        let height = 2u8;
        let values: Vec<Vec<u8>> = (0..4u32).map(|i| vec![i as u8]).collect();
        let (_state_root, tree) = build_test_tree(height, &values);
        let mut proof =
            BulkAppendTreeProof::generate(&full_range_query(), &tree).expect("generate proof");

        proof.buffer_proof.entries.push((0, b"smuggled".to_vec()));

        let err = proof
            .verify_and_compute_root(height, tree.total_count)
            .expect_err("non-empty buffer proof must fail for dense_count=0");
        assert!(matches!(err, BulkAppendError::InvalidProof(_)));
    }

    #[test]
    fn test_verify_against_query_detects_missing_chunk() {
        // height=2, epoch_size=4 -> total_count=8 => 2 completed chunks
        let height = 2u8;
        let values: Vec<Vec<u8>> = (0..8u32).map(|i| format!("m_{}", i).into_bytes()).collect();
        let (state_root, tree) = build_test_tree(height, &values);

        // Proof only covers chunk 0.
        let proof =
            BulkAppendTreeProof::generate(&range_query(0, 4), &tree).expect("generate proof");

        // Query needs both chunks 0 and 1.
        let err = proof
            .verify_against_query::<Vec<(u64, Vec<u8>)>>(
                &state_root,
                height,
                tree.total_count,
                &range_query(0, 8),
            )
            .expect_err("must fail for missing chunk");
        assert!(matches!(err, BulkAppendError::InvalidProof(_)));
    }

    #[test]
    fn test_verify_against_query_detects_missing_buffer_entries() {
        // height=2, epoch_size=4 -> 6 values = 1 chunk + 2 buffer
        let height = 2u8;
        let values: Vec<Vec<u8>> = (0..6u32).map(|i| format!("b_{}", i).into_bytes()).collect();
        let (state_root, tree) = build_test_tree(height, &values);

        // Proof includes only global position 4 (buffer local pos 0).
        let mut single = Query::default();
        single.items.push(QueryItem::Key(pos_bytes(4)));
        let proof = BulkAppendTreeProof::generate(&single, &tree).expect("generate proof");

        let err = proof
            .verify_against_query::<Vec<(u64, Vec<u8>)>>(
                &state_root,
                height,
                tree.total_count,
                &range_query(4, 6),
            )
            .expect_err("must fail for missing buffer position");
        assert!(matches!(err, BulkAppendError::InvalidProof(_)));
    }

    #[test]
    fn test_generate_rejects_invalid_query_item_encoding() {
        let height = 2u8;
        let values: Vec<Vec<u8>> = vec![b"a".to_vec()];
        let (_state_root, tree) = build_test_tree(height, &values);

        let mut query = Query::default();
        query.items.push(QueryItem::Key(Vec::new())); // invalid: length must be 1..=8

        let err =
            BulkAppendTreeProof::generate(&query, &tree).expect_err("invalid key bytes must fail");
        assert!(matches!(err, BulkAppendError::InvalidInput(_)));
    }

    #[test]
    fn test_decode_from_slice_rejects_invalid_bytes() {
        let err =
            BulkAppendTreeProof::decode_from_slice(&[1, 2, 3]).expect_err("decode should fail");
        assert!(matches!(err, BulkAppendError::CorruptedData(_)));
    }

    #[test]
    fn test_values_in_range_rejects_invalid_height() {
        let result = BulkAppendTreeProofResult {
            chunk_blobs: Vec::new(),
            dense_entries: Vec::new(),
            total_count: 0,
            height: 0,
        };
        let err = result
            .values_in_range(0, 1)
            .expect_err("invalid height must fail");
        assert!(matches!(err, BulkAppendError::InvalidProof(_)));
    }

    #[test]
    fn test_values_in_range_rejects_corrupted_chunk_blob() {
        let result = BulkAppendTreeProofResult {
            chunk_blobs: vec![(0, vec![0xFF, 1, 2, 3])],
            dense_entries: Vec::new(),
            total_count: 4,
            height: 2,
        };
        let err = result
            .values_in_range(0, 4)
            .expect_err("corrupted chunk blob must fail");
        assert!(matches!(err, BulkAppendError::CorruptedData(_)));
    }
}
