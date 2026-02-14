//! Unit tests for BulkAppendTree.

use std::{cell::RefCell, collections::HashMap};

use super::BulkAppendTree;
use crate::{epoch::deserialize_epoch_blob, BulkAppendError, BulkStore};

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
    let tree = BulkAppendTree::new(4);
    assert_eq!(tree.total_count(), 0);
    assert_eq!(tree.epoch_count(), 0);
    assert_eq!(tree.buffer_count(), 0);
    assert_eq!(tree.epoch_size(), 4);
    assert_eq!(tree.mmr_size(), 0);
    assert_eq!(tree.buffer_hash(), [0u8; 32]);
}

#[test]
fn from_state() {
    let tree = BulkAppendTree::from_state(10, 4, 3, [1u8; 32]);
    assert_eq!(tree.total_count(), 10);
    assert_eq!(tree.epoch_size(), 4);
    assert_eq!(tree.mmr_size(), 3);
    assert_eq!(tree.buffer_hash(), [1u8; 32]);
    assert_eq!(tree.epoch_count(), 2);
    assert_eq!(tree.buffer_count(), 2);
}

#[test]
#[should_panic]
fn new_tree_non_power_of_two() {
    BulkAppendTree::new(3);
}

#[test]
fn single_append() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(4);

    let result = tree.append(&store, b"hello").unwrap();
    assert_eq!(result.global_position, 0);
    assert!(!result.compacted);
    assert_eq!(tree.total_count(), 1);
    assert_eq!(tree.buffer_count(), 1);
    assert_eq!(tree.epoch_count(), 0);

    // Value should be retrievable
    let val = tree.get_value(&store, 0).unwrap();
    assert_eq!(val, Some(b"hello".to_vec()));
}

#[test]
fn multiple_appends_no_compaction() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(4);

    for i in 0..3 {
        let result = tree.append(&store, &[i]).unwrap();
        assert_eq!(result.global_position, i as u64);
        assert!(!result.compacted);
    }
    assert_eq!(tree.total_count(), 3);
    assert_eq!(tree.buffer_count(), 3);
    assert_eq!(tree.epoch_count(), 0);
}

#[test]
fn compaction_trigger() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(4);

    // Fill buffer (4 entries triggers compaction)
    for i in 0..3u8 {
        let r = tree.append(&store, &[i]).unwrap();
        assert!(!r.compacted);
    }
    let result = tree.append(&store, &[3]).unwrap();
    assert!(result.compacted);
    assert_eq!(result.global_position, 3);
    assert_eq!(tree.total_count(), 4);
    assert_eq!(tree.buffer_count(), 0);
    assert_eq!(tree.epoch_count(), 1);
    assert_eq!(tree.buffer_hash(), [0u8; 32]); // Reset after compaction
}

#[test]
fn multi_epoch() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(2); // epoch_size=2 for quick testing

    // 5 appends = 2 full epochs + 1 buffer entry
    for i in 0..5u8 {
        tree.append(&store, &[i]).unwrap();
    }
    assert_eq!(tree.total_count(), 5);
    assert_eq!(tree.epoch_count(), 2);
    assert_eq!(tree.buffer_count(), 1);
}

#[test]
fn get_value_from_epoch() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(2);

    tree.append(&store, b"a").unwrap();
    tree.append(&store, b"b").unwrap(); // compacts
    tree.append(&store, b"c").unwrap();

    // From epoch
    assert_eq!(tree.get_value(&store, 0).unwrap(), Some(b"a".to_vec()));
    assert_eq!(tree.get_value(&store, 1).unwrap(), Some(b"b".to_vec()));
    // From buffer
    assert_eq!(tree.get_value(&store, 2).unwrap(), Some(b"c".to_vec()));
    // Out of range
    assert_eq!(tree.get_value(&store, 3).unwrap(), None);
}

#[test]
fn get_epoch_blob() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(2);

    tree.append(&store, b"x").unwrap();
    tree.append(&store, b"y").unwrap(); // compacts

    let blob = tree.get_epoch(&store, 0).unwrap();
    assert!(blob.is_some());
    let entries = deserialize_epoch_blob(&blob.unwrap()).unwrap();
    assert_eq!(entries, vec![b"x".to_vec(), b"y".to_vec()]);

    // Non-existent epoch
    assert!(tree.get_epoch(&store, 1).unwrap().is_none());
}

#[test]
fn get_buffer_entries() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(4);

    tree.append(&store, b"a").unwrap();
    tree.append(&store, b"b").unwrap();

    let buf = tree.get_buffer(&store).unwrap();
    assert_eq!(buf, vec![b"a".to_vec(), b"b".to_vec()]);
}

#[test]
fn metadata_serialize_roundtrip() {
    let tree = BulkAppendTree::from_state(100, 8, 42, [0xAB; 32]);
    let meta = tree.serialize_meta();
    let (mmr_size, buffer_hash) = BulkAppendTree::deserialize_meta(&meta).unwrap();
    assert_eq!(mmr_size, 42);
    assert_eq!(buffer_hash, [0xAB; 32]);
}

#[test]
fn metadata_deserialize_wrong_size() {
    let err = BulkAppendTree::deserialize_meta(&[0; 10]).unwrap_err();
    assert!(matches!(err, BulkAppendError::CorruptedData(_)));
}

#[test]
fn state_root_determinism() {
    // Two trees with same data should have same state root
    let store1 = MemStore::new();
    let store2 = MemStore::new();
    let mut tree1 = BulkAppendTree::new(2);
    let mut tree2 = BulkAppendTree::new(2);

    for i in 0..5u8 {
        tree1.append(&store1, &[i]).unwrap();
        tree2.append(&store2, &[i]).unwrap();
    }

    let root1 = tree1.compute_current_state_root(&store1).unwrap();
    let root2 = tree2.compute_current_state_root(&store2).unwrap();
    assert_eq!(root1, root2);
}

#[test]
fn hash_count_accuracy() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(4);

    // Non-compacting append: 2 (chain hash) + 1 (state root) = 3
    let r = tree.append(&store, b"a").unwrap();
    assert_eq!(r.hash_count, 3);

    tree.append(&store, b"b").unwrap();
    tree.append(&store, b"c").unwrap();

    // Compacting append: 2 (chain) + dense_merkle + mmr_push + 1 (state root)
    let r = tree.append(&store, b"d").unwrap();
    assert!(r.compacted);
    assert!(r.hash_count > 3); // Should include dense merkle + mmr hashes
}

#[test]
fn append_with_mem_buffer_matches_append() {
    let store1 = MemStore::new();
    let store2 = MemStore::new();
    let mut tree1 = BulkAppendTree::new(2);
    let mut tree2 = BulkAppendTree::new(2);
    let mut mem_buf = Vec::new();

    for i in 0..5u8 {
        let r1 = tree1.append(&store1, &[i]).unwrap();
        let r2 = tree2
            .append_with_mem_buffer(&store2, &[i], &mut mem_buf)
            .unwrap();
        assert_eq!(r1.state_root, r2.state_root);
        assert_eq!(r1.global_position, r2.global_position);
        assert_eq!(r1.compacted, r2.compacted);
    }
}

#[test]
fn load_from_store_roundtrip() {
    let store = MemStore::new();
    let mut tree = BulkAppendTree::new(4);

    tree.append(&store, b"hello").unwrap();
    tree.append(&store, b"world").unwrap();

    // Load from store using element fields
    let loaded = BulkAppendTree::load_from_store(&store, 2, 4).unwrap();
    assert_eq!(loaded.total_count(), 2);
    assert_eq!(loaded.mmr_size(), tree.mmr_size());
    assert_eq!(loaded.buffer_hash(), tree.buffer_hash());

    // Should be able to read values
    let val = loaded.get_value(&store, 0).unwrap();
    assert_eq!(val, Some(b"hello".to_vec()));
}
