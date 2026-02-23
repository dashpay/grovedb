//! Unit tests for BulkAppendTree.

use super::BulkAppendTree;
use crate::{chunk::deserialize_chunk_blob, test_utils::MemStorageContext};

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
fn single_append() {
    let mut tree = BulkAppendTree::new(2u8, MemStorageContext::new()).expect("create tree");

    let result = tree.append(b"hello").expect("append hello");
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
        let result = tree.append(&[i]).expect("append entry");
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
        let r = tree.append(&[i]).expect("append pre-compaction entry");
        assert!(!r.compacted);
    }
    let result = tree.append(&[3]).expect("append compacting entry");
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
        tree.append(&[i]).expect("append entry");
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
    tree.append(b"a").expect("append a");
    tree.append(b"b").expect("append b");
    tree.append(b"c").expect("append c");
    tree.append(b"d").expect("append d");

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

    tree.append(b"a").expect("append a");
    tree.append(b"b").expect("append b");

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
    tree.append(b"x").expect("append x");
    tree.append(b"y").expect("append y"); // compacts [x, y]

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

    tree.append(b"a").expect("append a");
    tree.append(b"b").expect("append b");

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
    tree.append(b"a").expect("append a");
    tree.append(b"b").expect("append b");
    tree.append(b"c").expect("append c");
    tree.append(b"d").expect("append d");

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
        tree1.append(&[i]).expect("append to tree1");
        tree2.append(&[i]).expect("append to tree2");
    }

    let root1 = tree1.compute_current_state_root().expect("state root 1");
    let root2 = tree2.compute_current_state_root().expect("state root 2");
    assert_eq!(root1, root2);
}

#[test]
fn hash_count_accuracy() {
    // capacity=3, epoch_size=4
    let mut tree = BulkAppendTree::new(2u8, MemStorageContext::new()).expect("create tree");

    // Non-compacting append includes dense tree hashing + state root
    let r = tree.append(b"a").expect("append a");
    assert!(r.hash_count > 0);

    tree.append(b"b").expect("append b");
    tree.append(b"c").expect("append c");

    // 4th append triggers compaction: should have more hash calls (dense + mmr +
    // state root)
    let r = tree.append(b"d").expect("append d (compaction)");
    assert!(r.compacted);
    assert!(r.hash_count > 1);
}

#[test]
fn from_state_roundtrip() {
    let mut tree = BulkAppendTree::new(2u8, MemStorageContext::new()).expect("create tree");

    tree.append(b"hello").expect("append hello");
    tree.append(b"world").expect("append world");

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
        tree.append(&[i]).expect("append");
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
        tree.append(&[i]).expect("append");
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
