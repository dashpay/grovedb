#[cfg(test)]
mod proof_tests {
    use grovedb_query::QueryItem;

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
}
