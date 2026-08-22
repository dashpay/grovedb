//! Unit tests for BulkAppendTree.

use super::BulkAppendTree;
use crate::{chunk::deserialize_chunk_blob, test_utils::MemStorageContext};
use grovedb_version::version::GroveVersion;

#[test]
fn new_tree() {
    let tree =
        BulkAppendTree::new(2u8, MemStorageContext::new()).expect("create tree with height=2");
    assert_eq!(tree.total_count, 0);
    assert_eq!(tree.chunk_count(), 0);
    assert_eq!(tree.buffer_count(), 0);
    assert_eq!(tree.height(), 2);
    assert_eq!(tree.capacity(), 3); // 2^2 - 1 = 3
    assert_eq!(tree.epoch_size(), 4); // capacity + 1 = 2^2
    assert_eq!(tree.mmr_size(), 0);
}

#[test]
fn from_state() {
    let tree =
        BulkAppendTree::from_state(10, 2u8, MemStorageContext::new()).expect("restore from state");
    assert_eq!(tree.total_count, 10);
    assert_eq!(tree.height(), 2);
    assert_eq!(tree.chunk_count(), 2); // 10 / 4 = 2 (epoch_size = 4)
    assert_eq!(tree.buffer_count(), 2); // 10 % 4 = 2
    assert_eq!(tree.mmr_size(), 3); // leaf_count_to_mmr_size(2) = 2*2 - 1 = 3
}

#[test]
fn new_tree_invalid_height() {
    assert!(BulkAppendTree::new(0u8, MemStorageContext::new()).is_err());
    assert!(BulkAppendTree::new(17u8, MemStorageContext::new()).is_err());
}

#[test]
fn from_state_invalid_height() {
    assert!(BulkAppendTree::from_state(0, 0u8, MemStorageContext::new()).is_err());
    assert!(BulkAppendTree::from_state(0, 17u8, MemStorageContext::new()).is_err());
}

#[test]
fn cached_mmr_root_matches_recomputation_across_compactions() {
    // The append fast-path uses a cached MMR root (`last_mmr_root`) instead of
    // recomputing it — and cloning the blob-bearing overlay — on every append.
    // This guards that the cache never diverges from a fresh recomputation,
    // across both compaction and non-compaction appends.
    //
    // height=2 → epoch_size=4, so 20 appends span 5 compaction cycles.
    let mut tree = BulkAppendTree::new(2u8, MemStorageContext::new()).expect("create tree");
    for i in 0..20u8 {
        tree.append(&[i], GroveVersion::latest()).expect("append");
        let fresh = tree.get_mmr_root().expect("recompute mmr root");
        assert_eq!(
            tree.last_mmr_root,
            Some(fresh),
            "cached MMR root diverged from recomputation after {} append(s)",
            i + 1
        );
    }
}

#[test]
fn single_append() {
    let mut tree = BulkAppendTree::new(2u8, MemStorageContext::new()).expect("create tree");

    let result = tree
        .append(b"hello", GroveVersion::latest())
        .expect("append hello");
    assert_eq!(result.global_position, 0);
    assert!(!result.compacted);
    assert_eq!(tree.total_count, 1);
    assert_eq!(tree.buffer_count(), 1);
    assert_eq!(tree.chunk_count(), 0);

    // Value should be retrievable from the buffer (dense tree)
    let val = tree.get_buffer_value(0).expect("get buffer value at 0");
    assert_eq!(val, Some(b"hello".to_vec()));
}

#[test]
fn multiple_appends_no_compaction() {
    let mut tree = BulkAppendTree::new(2u8, MemStorageContext::new()).expect("create tree");

    // Height=2, capacity=3. Append 2 values (no compaction).
    for i in 0..2 {
        let result = tree
            .append(&[i], GroveVersion::latest())
            .expect("append entry");
        assert_eq!(result.global_position, i as u64);
        assert!(!result.compacted);
    }
    assert_eq!(tree.total_count, 2);
    assert_eq!(tree.buffer_count(), 2);
    assert_eq!(tree.chunk_count(), 0);
}

#[test]
fn compaction_trigger() {
    let mut tree = BulkAppendTree::new(2u8, MemStorageContext::new()).expect("create tree");

    // Height=2, capacity=3, epoch_size=4. First 3 appends fill the buffer,
    // 4th triggers compaction (try_insert returns None when buffer is full).
    for i in 0..3u8 {
        let r = tree
            .append(&[i], GroveVersion::latest())
            .expect("append pre-compaction entry");
        assert!(!r.compacted);
    }
    let result = tree
        .append(&[3], GroveVersion::latest())
        .expect("append compacting entry");
    assert!(result.compacted);
    assert_eq!(result.global_position, 3);
    assert_eq!(tree.total_count, 4);
    assert_eq!(tree.buffer_count(), 0);
    assert_eq!(tree.chunk_count(), 1);
}

#[test]
fn multi_chunk() {
    // height=1, capacity=1
    let mut tree = BulkAppendTree::new(1u8, MemStorageContext::new()).expect("create tree");

    // Capacity=1, epoch_size=2. Every 2 appends creates one chunk:
    //   append 0 → buffer (count=1), append 1 → compaction (chunk has [0,1])
    //   append 2 → buffer (count=1), append 3 → compaction (chunk has [2,3])
    // 4 appends = 2 chunks + 0 buffer
    for i in 0..4u8 {
        tree.append(&[i], GroveVersion::latest())
            .expect("append entry");
    }
    assert_eq!(tree.total_count, 4);
    assert_eq!(tree.chunk_count(), 2);
    assert_eq!(tree.buffer_count(), 0);
}

#[test]
fn get_chunk_value_from_mmr() {
    // capacity=1, epoch_size=2
    let mut tree = BulkAppendTree::new(1u8, MemStorageContext::new()).expect("create tree");

    // append a → buffer (count=1=capacity, no compaction)
    // append b → try_insert fails (full), compact [a, b] → chunk 0
    // append c → buffer (count=1)
    // append d → try_insert fails, compact [c, d] → chunk 1
    tree.append(b"a", GroveVersion::latest()).expect("append a");
    tree.append(b"b", GroveVersion::latest()).expect("append b");
    tree.append(b"c", GroveVersion::latest()).expect("append c");
    tree.append(b"d", GroveVersion::latest()).expect("append d");

    assert_eq!(tree.chunk_count(), 2);
    assert_eq!(tree.buffer_count(), 0);

    // Read from the chunk MMR: chunk 0 has [a,b], chunk 1 has [c,d]
    let blob0 = tree
        .get_chunk_value(0)
        .expect("get chunk 0")
        .expect("chunk 0 should exist");
    let entries0 = deserialize_chunk_blob(&blob0).expect("deserialize chunk 0");
    assert_eq!(entries0, vec![b"a".to_vec(), b"b".to_vec()]);

    let blob1 = tree
        .get_chunk_value(1)
        .expect("get chunk 1")
        .expect("chunk 1 should exist");
    let entries1 = deserialize_chunk_blob(&blob1).expect("deserialize chunk 1");
    assert_eq!(entries1, vec![b"c".to_vec(), b"d".to_vec()]);

    // Out of range
    assert_eq!(tree.get_chunk_value(2).expect("get chunk 2"), None);
}

#[test]
fn get_buffer_value_from_dense_tree() {
    // capacity=3
    let mut tree = BulkAppendTree::new(2u8, MemStorageContext::new()).expect("create tree");

    tree.append(b"a", GroveVersion::latest()).expect("append a");
    tree.append(b"b", GroveVersion::latest()).expect("append b");

    // Both from the buffer (dense tree)
    assert_eq!(
        tree.get_buffer_value(0).expect("get 0"),
        Some(b"a".to_vec())
    );
    assert_eq!(
        tree.get_buffer_value(1).expect("get 1"),
        Some(b"b".to_vec())
    );
    // Out of range
    assert_eq!(tree.get_buffer_value(2).expect("get 2"), None);
}

#[test]
fn get_chunk_blob() {
    // capacity=1, epoch_size=2
    let mut tree = BulkAppendTree::new(1u8, MemStorageContext::new()).expect("create tree");

    // Need 2 appends to trigger compaction (epoch_size=2)
    tree.append(b"x", GroveVersion::latest()).expect("append x");
    tree.append(b"y", GroveVersion::latest()).expect("append y"); // compacts [x, y]

    let blob = tree.get_chunk_value(0).expect("get chunk 0");
    assert!(blob.is_some());
    let entries = deserialize_chunk_blob(&blob.expect("chunk blob should exist"))
        .expect("deserialize chunk blob");
    assert_eq!(entries, vec![b"x".to_vec(), b"y".to_vec()]);

    // Non-existent chunk
    assert!(tree.get_chunk_value(1).expect("get chunk 1").is_none());
}

#[test]
fn query_buffer_entries() {
    // capacity=3
    let mut tree = BulkAppendTree::new(2u8, MemStorageContext::new()).expect("create tree");

    tree.append(b"a", GroveVersion::latest()).expect("append a");
    tree.append(b"b", GroveVersion::latest()).expect("append b");

    // Query all buffer entries with RangeFull
    let query = grovedb_query::Query::new_range_full();
    let result = tree.query_buffer(&query).expect("query buffer");
    assert_eq!(
        result.entries,
        vec![(0u16, b"a".to_vec()), (1u16, b"b".to_vec())]
    );
}

#[test]
fn query_chunks_from_mmr() {
    // capacity=1, epoch_size=2
    let mut tree = BulkAppendTree::new(1u8, MemStorageContext::new()).expect("create tree");

    // 4 appends → 2 chunks: chunk 0 = [a,b], chunk 1 = [c,d]
    tree.append(b"a", GroveVersion::latest()).expect("append a");
    tree.append(b"b", GroveVersion::latest()).expect("append b");
    tree.append(b"c", GroveVersion::latest()).expect("append c");
    tree.append(b"d", GroveVersion::latest()).expect("append d");

    // Query both chunks
    let result = tree.query_chunks(&[0, 1]).expect("query chunks");
    assert_eq!(result.chunks.len(), 2);
    assert_eq!(result.chunks[0], (0, vec![b"a".to_vec(), b"b".to_vec()]));
    assert_eq!(result.chunks[1], (1, vec![b"c".to_vec(), b"d".to_vec()]));
    assert_ne!(result.mmr_root, [0u8; 32]);

    // Query single chunk
    let result = tree.query_chunks(&[1]).expect("query chunk 1");
    assert_eq!(result.chunks.len(), 1);
    assert_eq!(result.chunks[0], (1, vec![b"c".to_vec(), b"d".to_vec()]));

    // Query out-of-range chunk should fail
    assert!(tree.query_chunks(&[2]).is_err());
}

#[test]
fn leaf_count_to_mmr_size_formula() {
    use super::leaf_count_to_mmr_size;

    assert_eq!(leaf_count_to_mmr_size(0), 0);
    assert_eq!(leaf_count_to_mmr_size(1), 1);
    assert_eq!(leaf_count_to_mmr_size(2), 3);
    assert_eq!(leaf_count_to_mmr_size(3), 4);
    assert_eq!(leaf_count_to_mmr_size(4), 7);
    assert_eq!(leaf_count_to_mmr_size(5), 8);
    assert_eq!(leaf_count_to_mmr_size(6), 10);
    assert_eq!(leaf_count_to_mmr_size(7), 11);
    assert_eq!(leaf_count_to_mmr_size(8), 15);
}

#[test]
fn state_root_determinism() {
    // Two trees with same data should have same state root
    let mut tree1 = BulkAppendTree::new(2u8, MemStorageContext::new()).expect("create tree1");
    let mut tree2 = BulkAppendTree::new(2u8, MemStorageContext::new()).expect("create tree2");

    for i in 0..5u8 {
        tree1
            .append(&[i], GroveVersion::latest())
            .expect("append to tree1");
        tree2
            .append(&[i], GroveVersion::latest())
            .expect("append to tree2");
    }

    let root1 = tree1
        .compute_current_state_root(GroveVersion::latest())
        .expect("state root 1");
    let root2 = tree2
        .compute_current_state_root(GroveVersion::latest())
        .expect("state root 2");
    assert_eq!(root1, root2);
}

#[test]
fn compute_current_state_root_empty_tree() {
    let tree = BulkAppendTree::new(2u8, MemStorageContext::new()).expect("create tree");
    let root = tree
        .compute_current_state_root(GroveVersion::latest())
        .expect("compute empty tree root");
    assert_ne!(root, [0u8; 32]);
}

#[test]
fn hash_count_accuracy() {
    // capacity=3, epoch_size=4
    let mut tree = BulkAppendTree::new(2u8, MemStorageContext::new()).expect("create tree");

    // Non-compacting append includes dense tree hashing + state root
    let r = tree.append(b"a", GroveVersion::latest()).expect("append a");
    assert!(r.hash_count > 0);

    tree.append(b"b", GroveVersion::latest()).expect("append b");
    tree.append(b"c", GroveVersion::latest()).expect("append c");

    // 4th append triggers compaction: should have more hash calls (dense + mmr +
    // state root)
    let r = tree
        .append(b"d", GroveVersion::latest())
        .expect("append d (compaction)");
    assert!(r.compacted);
    assert!(r.hash_count > 1);
}

#[test]
fn from_state_roundtrip() {
    let mut tree = BulkAppendTree::new(2u8, MemStorageContext::new()).expect("create tree");

    tree.append(b"hello", GroveVersion::latest())
        .expect("append hello");
    tree.append(b"world", GroveVersion::latest())
        .expect("append world");

    let total_count = tree.total_count;
    let mmr_size = tree.mmr_size();
    let buffer_count = tree.buffer_count();

    // Restore from state using element fields — reuse the same stores
    let loaded = BulkAppendTree::from_state(2, 2u8, MemStorageContext::new()).expect("from_state");
    assert_eq!(loaded.total_count, 2);
    assert_eq!(loaded.mmr_size(), mmr_size);
    assert_eq!(loaded.buffer_count(), buffer_count);

    // Note: can't read values from loaded tree since it has fresh stores.
    // In practice, stores would be backed by the same persistent storage.
    let _ = total_count;
}

#[test]
fn compaction_and_continue() {
    // capacity=3, epoch_size=4
    let mut tree = BulkAppendTree::new(2u8, MemStorageContext::new()).expect("create tree");

    // Fill one epoch and continue
    for i in 0..5u8 {
        tree.append(&[i], GroveVersion::latest()).expect("append");
    }
    assert_eq!(tree.total_count, 5);
    assert_eq!(tree.chunk_count(), 1); // 5/4 = 1 full chunk
    assert_eq!(tree.buffer_count(), 1); // 5%4 = 1

    // Chunk 0 has values [0,1,2,3] (epoch_size=4)
    let blob = tree
        .get_chunk_value(0)
        .expect("get chunk 0")
        .expect("chunk 0 should exist");
    let chunk_entries = deserialize_chunk_blob(&blob).expect("deserialize chunk 0");
    for i in 0..4u8 {
        assert_eq!(chunk_entries[i as usize], vec![i]);
    }

    // Buffer has value [4]
    let val = tree.get_buffer_value(0).expect("get buffer value 0");
    assert_eq!(val, Some(vec![4u8]));
}

#[test]
fn multiple_compaction_cycles() {
    // capacity=3, epoch_size=4
    let mut tree = BulkAppendTree::new(2u8, MemStorageContext::new()).expect("create tree");

    // 8 values = 2 full chunks (8/4 = 2)
    for i in 0..8u8 {
        tree.append(&[i], GroveVersion::latest()).expect("append");
    }
    assert_eq!(tree.total_count, 8);
    assert_eq!(tree.chunk_count(), 2);
    assert_eq!(tree.buffer_count(), 0);

    // Read values from both chunks via query_chunks
    let result = tree.query_chunks(&[0, 1]).expect("query chunks");
    // chunk 0 has [0,1,2,3], chunk 1 has [4,5,6,7]
    let (_, entries0) = &result.chunks[0];
    let (_, entries1) = &result.chunks[1];
    for i in 0..4u8 {
        assert_eq!(entries0[i as usize], vec![i]);
        assert_eq!(entries1[i as usize], vec![i + 4]);
    }
}

#[test]
fn query_chunks_empty_indices_returns_empty_proof() {
    let mut tree = BulkAppendTree::new(1u8, MemStorageContext::new()).expect("create tree");
    tree.append(b"a", GroveVersion::latest()).expect("append a");
    tree.append(b"b", GroveVersion::latest()).expect("append b"); // one completed chunk exists

    let result = tree.query_chunks(&[]).expect("query with empty indices");
    assert!(result.chunks.is_empty());
    assert!(result.mmr_proof_items.is_empty());
    assert_eq!(result.mmr_root, [0u8; 32]);
}

// ── get_range (paginated position-range reads) ───────────────────────

/// Helper: build a tree with `n` single-byte values `[0], [1], ...`.
fn build_range_tree(height: u8, n: u8) -> BulkAppendTree<MemStorageContext> {
    let mut tree = BulkAppendTree::new(height, MemStorageContext::new()).expect("create tree");
    for i in 0..n {
        tree.append(&[i], GroveVersion::latest()).expect("append");
    }
    tree
}

/// Helper: assert a page holds exactly positions `start..end` with value
/// `[pos as u8]` at each.
fn assert_page(page: &super::RangePage, start: u64, end: u64, total_count: u64) {
    assert_eq!(page.total_count, total_count);
    assert_eq!(page.entries.len(), (end - start) as usize);
    for (i, (pos, value)) in page.entries.iter().enumerate() {
        assert_eq!(*pos, start + i as u64);
        assert_eq!(value, &vec![*pos as u8]);
    }
}

#[test]
fn get_range_buffer_only() {
    // height=3, capacity=7: 5 values all in buffer
    let tree = build_range_tree(3, 5);
    assert_eq!(tree.chunk_count(), 0);

    let page = tree.get_range(1, 3).unwrap().expect("get range");
    assert_page(&page, 1, 4, 5);
}

#[test]
fn get_range_single_chunk() {
    // height=2, epoch_size=4: 8 values = 2 full chunks
    let tree = build_range_tree(2, 8);
    assert_eq!(tree.chunk_count(), 2);
    assert_eq!(tree.buffer_count(), 0);

    // Page entirely inside chunk 0
    let page = tree.get_range(1, 2).unwrap().expect("get range");
    assert_page(&page, 1, 3, 8);
}

#[test]
fn get_range_across_chunk_boundary() {
    // height=2, epoch_size=4: 10 values = 2 chunks + 2 buffered
    let tree = build_range_tree(2, 10);
    assert_eq!(tree.chunk_count(), 2);
    assert_eq!(tree.buffer_count(), 2);

    // Page [3, 6) spans the chunk 0 / chunk 1 boundary
    let page = tree.get_range(3, 3).unwrap().expect("get range");
    assert_page(&page, 3, 6, 10);

    // Page [6, 10) spans the chunk 1 / buffer boundary
    let page = tree.get_range(6, 4).unwrap().expect("get range");
    assert_page(&page, 6, 10, 10);
}

#[test]
fn get_range_whole_tree() {
    let tree = build_range_tree(2, 10);
    let page = tree.get_range(0, 100).unwrap().expect("get range");
    assert_page(&page, 0, 10, 10);
}

#[test]
fn get_range_empty_limit() {
    let tree = build_range_tree(2, 10);
    let page = tree.get_range(3, 0).unwrap().expect("get range");
    assert_page(&page, 3, 3, 10);
}

#[test]
fn get_range_past_end() {
    let tree = build_range_tree(2, 10);

    // Start exactly at total_count
    let page = tree.get_range(10, 5).unwrap().expect("get range");
    assert!(page.entries.is_empty());
    assert_eq!(page.total_count, 10);

    // Start far past total_count
    let page = tree.get_range(1000, 5).unwrap().expect("get range");
    assert!(page.entries.is_empty());
    assert_eq!(page.total_count, 10);

    // Range that starts inside but extends past the end is clamped
    let page = tree.get_range(8, 100).unwrap().expect("get range");
    assert_page(&page, 8, 10, 10);
}

#[test]
fn get_range_single_entry() {
    let tree = build_range_tree(2, 10);
    let page = tree.get_range(7, 1).unwrap().expect("get range");
    assert_page(&page, 7, 8, 10);
}

#[test]
fn get_range_empty_tree() {
    let tree = build_range_tree(2, 0);
    let page = tree.get_range(0, 10).unwrap().expect("get range");
    assert!(page.entries.is_empty());
    assert_eq!(page.total_count, 0);
}

#[test]
fn get_range_start_saturating_overflow() {
    let tree = build_range_tree(2, 10);
    let page = tree
        .get_range(u64::MAX, u16::MAX)
        .unwrap()
        .expect("get range");
    assert!(page.entries.is_empty());
    assert_eq!(page.total_count, 10);
}

#[test]
fn get_range_paged_scan_covers_everything() {
    // The scanning pattern: walk the whole tree in pages of 3 and check the
    // concatenation matches per-position reads.
    let tree = build_range_tree(2, 11); // 2 chunks + 3 buffered
    let mut cursor = 0u64;
    let mut seen = Vec::new();
    loop {
        let page = tree.get_range(cursor, 3).unwrap().expect("get range");
        if page.entries.is_empty() {
            assert!(cursor >= page.total_count, "empty page only at the end");
            break;
        }
        cursor += page.entries.len() as u64;
        seen.extend(page.entries);
    }
    assert_eq!(seen.len(), 11);
    for (i, (pos, value)) in seen.iter().enumerate() {
        assert_eq!(*pos, i as u64);
        assert_eq!(value, &vec![i as u8]);
    }
}

#[test]
fn get_range_missing_chunk_is_corruption() {
    // Storage claims 2 completed chunks (via from_state) but holds no data:
    // the chunk MMR leaf lookup comes back empty and the read must surface
    // corruption, not silently skip entries.
    let tree = BulkAppendTree::from_state(4, 1, MemStorageContext::new()).expect("from_state");
    assert_eq!(tree.chunk_count(), 2);
    let err = tree
        .get_range(0, 4)
        .unwrap()
        .expect_err("missing chunk blob must error");
    assert!(matches!(err, crate::BulkAppendError::CorruptedData(_)));
}

#[test]
fn get_range_missing_buffer_value_is_corruption() {
    // Storage claims 1 buffered entry (via from_state) but holds no data:
    // the buffer read must surface an error, not silently skip entries.
    let tree = BulkAppendTree::from_state(1, 2, MemStorageContext::new()).expect("from_state");
    assert_eq!(tree.buffer_count(), 1);
    tree.get_range(0, 1)
        .unwrap()
        .expect_err("missing buffer value must error");
}

#[test]
fn get_range_storage_read_failure_is_mmr_error() {
    // A backing store that fails reads must surface as an MMR error from the
    // chunk lookup, not a panic or a silent empty page.
    let ctx = MemStorageContext::new();
    ctx.fail_get.set(true);
    let tree = BulkAppendTree::from_state(4, 1, ctx).expect("from_state");
    assert_eq!(tree.chunk_count(), 2);
    let err = tree
        .get_range(0, 4)
        .unwrap()
        .expect_err("failing storage must error");
    assert!(matches!(err, crate::BulkAppendError::MmrError(_)));
}

#[test]
fn get_range_wrong_chunk_entry_count_is_corruption() {
    // A completed chunk must hold exactly epoch_size entries: a short blob
    // would silently omit positions and an oversized one would overlap the
    // next chunk. Tamper the MMR overlay so chunk 0's blob deserializes to
    // the wrong entry count and verify the read rejects it.
    for bad_count in [1usize, 3] {
        // epoch_size = 2: append 2 values to complete one genuine chunk.
        let mut tree = BulkAppendTree::new(1u8, MemStorageContext::new()).expect("create tree");
        tree.append(&[0], GroveVersion::latest()).expect("append");
        tree.append(&[1], GroveVersion::latest()).expect("append");
        assert_eq!(tree.chunk_count(), 1);

        let bad_blob =
            crate::serialize_chunk_blob(&(0..bad_count).map(|i| vec![i as u8]).collect::<Vec<_>>())
                .expect("serialize bad blob");
        tree.mmr_overlay = vec![(
            0,
            vec![grovedb_merkle_mountain_range::MmrNode::leaf(bad_blob)],
        )];

        let err = tree
            .get_range(0, 2)
            .unwrap()
            .expect_err("wrong chunk entry count must error");
        match err {
            crate::BulkAppendError::CorruptedData(msg) => {
                assert!(
                    msg.contains(&format!("holds {} entries, expected 2", bad_count)),
                    "unexpected message: {}",
                    msg
                );
            }
            other => panic!("expected CorruptedData, got {:?}", other),
        }
    }
}

/// The hash count a buffered append reports is the dense tree's own figure
/// under every version — the shipped `2 * count` full-buffer walk under
/// GROVE_V3, the fixed model for the buffer's height under GROVE_V4 — and
/// the state roots are identical under both: the records change the work
/// and the charge, not the root.
#[test]
fn buffered_append_hash_count_follows_the_root_maintenance_version() {
    use grovedb_dense_fixed_sized_merkle_tree::V1InsertModel;
    use grovedb_version::version::{v3::GROVE_V3, v4::GROVE_V4};

    let model = V1InsertModel::for_height(4);

    // capacity 15, epoch 16: one full epoch of buffered appends plus the
    // compaction, then a few of the next epoch (slot rewrites).
    let mut v3 = BulkAppendTree::new(4u8, MemStorageContext::new()).expect("v3 tree");
    let mut v4 = BulkAppendTree::new(4u8, MemStorageContext::new()).expect("v4 tree");
    for i in 0..20u8 {
        let r3 = v3.append(&[i; 8], &GROVE_V3).expect("v3 append");
        let r4 = v4.append(&[i; 8], &GROVE_V4).expect("v4 append");
        assert_eq!(r3.state_root, r4.state_root, "position {i}: state root");
        assert_eq!(r3.compacted, r4.compacted);
        let buffer_position = (i % 16) as u32;
        if r3.compacted {
            // No buffer work on a compacting append: the chunk-leaf hash, the
            // MMR push, and the state root — and v4 adds the root bagging
            // the v3 figure omits (`compaction_hash_count`), here nothing.
            assert!(r3.hash_count >= 2, "v3 compaction: {}", r3.hash_count);
        } else {
            let filled = buffer_position + 1;
            assert_eq!(
                r3.hash_count,
                2 * filled + 1,
                "position {i}: v3 walks the {filled} filled positions (+1 state root)"
            );
            assert_eq!(
                r4.hash_count,
                model.hash_node_calls + 1,
                "position {i}: v4 charges the height-4 model (+1 state root), whatever the position"
            );
        }
    }
    assert_eq!(
        v3.compute_current_state_root(&GROVE_V3).expect("v3 root"),
        v4.compute_current_state_root(&GROVE_V4).expect("v4 root")
    );
    // And each tree's root reads the same under the other version's
    // derivation: v3's buffer (no records) walked or read, v4's buffer read
    // from its records or walked.
    assert_eq!(
        v3.compute_current_state_root(&GROVE_V4)
            .expect("v3 tree, v4 read"),
        v4.compute_current_state_root(&GROVE_V3)
            .expect("v4 tree, v3 read")
    );
}
