//! Unit tests for BulkAppendTree.

use std::{cell::RefCell, collections::HashMap};

use super::BulkAppendTree;
use crate::{chunk::deserialize_chunk_blob, BulkAppendError, BulkStore};

/// Simple in-memory store for testing.
struct MemStore {
    data: RefCell<HashMap<Vec<u8>, Vec<u8>>>,
}

impl MemStore {
    fn new() -> Self {
        Self {
            data: RefCell::new(HashMap::new()),
        }
    }
}

impl BulkStore for MemStore {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, String> {
        Ok(self.data.borrow().get(key).cloned())
    }

    fn put(&self, key: &[u8], value: &[u8]) -> Result<(), String> {
        self.data.borrow_mut().insert(key.to_vec(), value.to_vec());
        Ok(())
    }

    fn delete(&self, key: &[u8]) -> Result<(), String> {
        self.data.borrow_mut().remove(key);
        Ok(())
    }
}

#[test]
fn new_tree() {
    let tree = BulkAppendTree::new(2u8).expect("create tree with chunk_power=2");
    assert_eq!(tree.total_count(), 0);
    assert_eq!(tree.chunk_count(), 0);
    assert_eq!(tree.buffer_count(), 0);
    assert_eq!(tree.chunk_power(), 2);
    assert_eq!(tree.mmr_size(), 0);
    assert_eq!(tree.buffer_hash(), [0u8; 32]);
}

#[test]
fn from_state() {
    let tree = BulkAppendTree::from_state(10, 2u8, 3, [1u8; 32]).expect("restore from state");
    assert_eq!(tree.total_count(), 10);
    assert_eq!(tree.chunk_power(), 2);
    assert_eq!(tree.mmr_size(), 3);
    assert_eq!(tree.buffer_hash(), [1u8; 32]);
    assert_eq!(tree.chunk_count(), 2);
    assert_eq!(tree.buffer_count(), 2);
}

#[test]
fn new_tree_invalid_chunk_power() {
    assert!(BulkAppendTree::new(32u8).is_err());
}

#[test]
fn single_append() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(2u8).expect("create tree");

    let result = tree.append(&store, b"hello").expect("append hello");
    assert_eq!(result.global_position, 0);
    assert!(!result.compacted);
    assert_eq!(tree.total_count(), 1);
    assert_eq!(tree.buffer_count(), 1);
    assert_eq!(tree.chunk_count(), 0);

    // Value should be retrievable
    let val = tree.get_value(&store, 0).expect("get value at 0");
    assert_eq!(val, Some(b"hello".to_vec()));
}

#[test]
fn multiple_appends_no_compaction() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(2u8).expect("create tree");

    for i in 0..3 {
        let result = tree.append(&store, &[i]).expect("append entry");
        assert_eq!(result.global_position, i as u64);
        assert!(!result.compacted);
    }
    assert_eq!(tree.total_count(), 3);
    assert_eq!(tree.buffer_count(), 3);
    assert_eq!(tree.chunk_count(), 0);
}

#[test]
fn compaction_trigger() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(2u8).expect("create tree");

    // Fill buffer (4 entries triggers compaction)
    for i in 0..3u8 {
        let r = tree
            .append(&store, &[i])
            .expect("append pre-compaction entry");
        assert!(!r.compacted);
    }
    let result = tree.append(&store, &[3]).expect("append compacting entry");
    assert!(result.compacted);
    assert_eq!(result.global_position, 3);
    assert_eq!(tree.total_count(), 4);
    assert_eq!(tree.buffer_count(), 0);
    assert_eq!(tree.chunk_count(), 1);
    assert_eq!(tree.buffer_hash(), [0u8; 32]); // Reset after compaction
}

#[test]
fn multi_chunk() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(1u8).expect("create tree"); // chunk_power=1, chunk_size=2

    // 5 appends = 2 full chunks + 1 buffer entry
    for i in 0..5u8 {
        tree.append(&store, &[i]).expect("append entry");
    }
    assert_eq!(tree.total_count(), 5);
    assert_eq!(tree.chunk_count(), 2);
    assert_eq!(tree.buffer_count(), 1);
}

#[test]
fn get_value_from_chunk() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(1u8).expect("create tree");

    tree.append(&store, b"a").expect("append a");
    tree.append(&store, b"b").expect("append b"); // compacts
    tree.append(&store, b"c").expect("append c");

    // From chunk
    assert_eq!(
        tree.get_value(&store, 0).expect("get 0"),
        Some(b"a".to_vec())
    );
    assert_eq!(
        tree.get_value(&store, 1).expect("get 1"),
        Some(b"b".to_vec())
    );
    // From buffer
    assert_eq!(
        tree.get_value(&store, 2).expect("get 2"),
        Some(b"c".to_vec())
    );
    // Out of range
    assert_eq!(tree.get_value(&store, 3).expect("get 3"), None);
}

#[test]
fn get_chunk_blob() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(1u8).expect("create tree");

    tree.append(&store, b"x").expect("append x");
    tree.append(&store, b"y").expect("append y"); // compacts

    let blob = tree.get_chunk(&store, 0).expect("get chunk 0");
    assert!(blob.is_some());
    let entries = deserialize_chunk_blob(&blob.expect("chunk blob should exist"))
        .expect("deserialize chunk blob");
    assert_eq!(entries, vec![b"x".to_vec(), b"y".to_vec()]);

    // Non-existent chunk
    assert!(tree.get_chunk(&store, 1).expect("get chunk 1").is_none());
}

#[test]
fn get_buffer_entries() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(2u8).expect("create tree");

    tree.append(&store, b"a").expect("append a");
    tree.append(&store, b"b").expect("append b");

    let buf = tree.get_buffer(&store).expect("get buffer");
    assert_eq!(buf, vec![b"a".to_vec(), b"b".to_vec()]);
}

#[test]
fn metadata_serialize_roundtrip() {
    let tree = BulkAppendTree::from_state(100, 3u8, 42, [0xAB; 32]).expect("restore from state");
    let meta = tree.serialize_meta();
    let (mmr_size, buffer_hash) =
        BulkAppendTree::deserialize_meta(&meta).expect("deserialize meta");
    assert_eq!(mmr_size, 42);
    assert_eq!(buffer_hash, [0xAB; 32]);
}

#[test]
fn metadata_deserialize_wrong_size() {
    let err = BulkAppendTree::deserialize_meta(&[0; 10]).expect_err("should fail for wrong size");
    assert!(matches!(err, BulkAppendError::CorruptedData(_)));
}

#[test]
fn state_root_determinism() {
    // Two trees with same data should have same state root
    let store1 = MemStore::new();
    let store2 = MemStore::new();
    let mut tree1 = BulkAppendTree::new(1u8).expect("create tree1");
    let mut tree2 = BulkAppendTree::new(1u8).expect("create tree2");

    for i in 0..5u8 {
        tree1.append(&store1, &[i]).expect("append to tree1");
        tree2.append(&store2, &[i]).expect("append to tree2");
    }

    let root1 = tree1
        .compute_current_state_root(&store1)
        .expect("state root 1");
    let root2 = tree2
        .compute_current_state_root(&store2)
        .expect("state root 2");
    assert_eq!(root1, root2);
}

#[test]
fn hash_count_accuracy() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(2u8).expect("create tree");

    // Non-compacting append: 2 (chain hash) + 1 (state root) = 3
    let r = tree.append(&store, b"a").expect("append a");
    assert_eq!(r.hash_count, 3);

    tree.append(&store, b"b").expect("append b");
    tree.append(&store, b"c").expect("append c");

    // Compacting append: 2 (chain) + dense_merkle + mmr_push + 1 (state root)
    let r = tree.append(&store, b"d").expect("append d (compaction)");
    assert!(r.compacted);
    assert!(r.hash_count > 3); // Should include dense merkle + mmr hashes
}

#[test]
fn append_with_mem_buffer_matches_append() {
    let store1 = MemStore::new();
    let store2 = MemStore::new();
    let mut tree1 = BulkAppendTree::new(1u8).expect("create tree1");
    let mut tree2 = BulkAppendTree::new(1u8).expect("create tree2");
    let mut mem_buf = Vec::new();

    for i in 0..5u8 {
        let r1 = tree1.append(&store1, &[i]).expect("append to tree1");
        let r2 = tree2
            .append_with_mem_buffer(&store2, &[i], &mut mem_buf)
            .expect("append to tree2 with mem buffer");
        assert_eq!(r1.state_root, r2.state_root);
        assert_eq!(r1.global_position, r2.global_position);
        assert_eq!(r1.compacted, r2.compacted);
    }
}

#[test]
fn load_from_store_roundtrip() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(2u8).expect("create tree");

    tree.append(&store, b"hello").expect("append hello");
    tree.append(&store, b"world").expect("append world");

    // Load from store using element fields
    let loaded = BulkAppendTree::load_from_store(&store, 2, 2u8).expect("load from store");
    assert_eq!(loaded.total_count(), 2);
    assert_eq!(loaded.mmr_size(), tree.mmr_size());
    assert_eq!(loaded.buffer_hash(), tree.buffer_hash());

    // Should be able to read values
    let val = loaded.get_value(&store, 0).expect("get value at 0");
    assert_eq!(val, Some(b"hello".to_vec()));
}
