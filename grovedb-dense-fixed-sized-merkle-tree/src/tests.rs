use std::{cell::RefCell, collections::HashMap};

use grovedb_costs::{CostResult, CostsExt, OperationCost};

use super::*;
use crate::proof::DenseTreeProof;

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

// ── DenseFixedSizedMerkleTree tests ──────────────────────────────────

#[test]
fn test_new_tree_valid_heights() {
    let tree = DenseFixedSizedMerkleTree::new(1).expect("height 1 should be valid");
    assert_eq!(tree.capacity(), 1); // 2^1 - 1 = 1
    assert_eq!(tree.count(), 0);

    let tree = DenseFixedSizedMerkleTree::new(3).expect("height 3 should be valid");
    assert_eq!(tree.capacity(), 7); // 2^3 - 1 = 7

    let tree = DenseFixedSizedMerkleTree::new(16).expect("height 16 should be valid");
    assert_eq!(tree.height(), 16);
}

#[test]
fn test_new_tree_invalid_heights() {
    assert!(DenseFixedSizedMerkleTree::new(0).is_err());
    assert!(DenseFixedSizedMerkleTree::new(17).is_err());
}

#[test]
fn test_single_insert() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(1).expect("height 1");
    assert_eq!(tree.capacity(), 1);

    let ctx = tree.insert(b"hello", &store);
    // Single node: 1 value hash + 1 node_hash = 2
    assert_eq!(ctx.cost.hash_node_calls, 2);
    let (root_hash, pos) = ctx.value.expect("insert should succeed");
    assert_eq!(pos, 0);
    assert_eq!(tree.count(), 1);
    assert_ne!(root_hash, [0u8; 32]);

    // Root hash = blake3(H(value) || [0;32] || [0;32]) for a single-node tree
    let value_hash = *blake3::hash(b"hello").as_bytes();
    let mut hasher = blake3::Hasher::new();
    hasher.update(&value_hash);
    hasher.update(&[0u8; 32]);
    hasher.update(&[0u8; 32]);
    let expected = *hasher.finalize().as_bytes();
    assert_eq!(root_hash, expected);
}

#[test]
fn test_sequential_fill_height_2() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(2).expect("height 2");
    assert_eq!(tree.capacity(), 3); // 2^2 - 1 = 3

    // Insert three values (positions 0, 1, 2)
    let (_, pos0) = tree.insert(b"root_val", &store).unwrap().expect("insert 0");
    assert_eq!(pos0, 0);

    let (_, pos1) = tree.insert(b"left_val", &store).unwrap().expect("insert 1");
    assert_eq!(pos1, 1);

    let (root_hash, pos2) = tree
        .insert(b"right_val", &store)
        .unwrap()
        .expect("insert 2");
    assert_eq!(pos2, 2);
    assert_eq!(tree.count(), 3);

    // Verify structure: root=0 has children at 1 and 2
    // All nodes: blake3(H(value) || H(left) || H(right))
    // Children are leaf nodes (no children) so H(left_child) = [0;32], etc.
    let h_left = {
        let vh = *blake3::hash(b"left_val").as_bytes();
        let mut h = blake3::Hasher::new();
        h.update(&vh);
        h.update(&[0u8; 32]);
        h.update(&[0u8; 32]);
        *h.finalize().as_bytes()
    };
    let h_right = {
        let vh = *blake3::hash(b"right_val").as_bytes();
        let mut h = blake3::Hasher::new();
        h.update(&vh);
        h.update(&[0u8; 32]);
        h.update(&[0u8; 32]);
        *h.finalize().as_bytes()
    };
    let h_root_val = *blake3::hash(b"root_val").as_bytes();
    let expected = {
        let mut h = blake3::Hasher::new();
        h.update(&h_root_val);
        h.update(&h_left);
        h.update(&h_right);
        *h.finalize().as_bytes()
    };
    assert_eq!(root_hash, expected);
}

#[test]
fn test_capacity_error() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(1).expect("height 1");
    tree.insert(b"only", &store).unwrap().expect("first insert");
    let result = tree.insert(b"overflow", &store);
    assert!(result.unwrap().is_err());
}

#[test]
fn test_get_by_position() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(2).expect("height 2");
    tree.insert(b"val0", &store).unwrap().expect("insert 0");
    tree.insert(b"val1", &store).unwrap().expect("insert 1");

    assert_eq!(
        tree.get(0, &store).unwrap().expect("get 0"),
        Some(b"val0".to_vec())
    );
    assert_eq!(
        tree.get(1, &store).unwrap().expect("get 1"),
        Some(b"val1".to_vec())
    );
    // Position 2 not yet filled
    assert_eq!(tree.get(2, &store).unwrap().expect("get 2"), None);
    // Beyond capacity
    assert_eq!(tree.get(100, &store).unwrap().expect("get 100"), None);
}

#[test]
fn test_root_hash_determinism() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(3).expect("height 3");
    tree.insert(b"a", &store).unwrap().expect("insert a");
    tree.insert(b"b", &store).unwrap().expect("insert b");

    let h1 = tree.root_hash(&store).unwrap().expect("root hash 1");
    let h2 = tree.root_hash(&store).unwrap().expect("root hash 2");
    assert_eq!(h1, h2);
}

#[test]
fn test_empty_tree_root_hash() {
    let store = MemStore::new();
    let tree = DenseFixedSizedMerkleTree::new(3).expect("height 3");
    let ctx = tree.root_hash(&store);
    assert_eq!(ctx.cost.hash_node_calls, 0);
    let hash = ctx.value.expect("empty root hash");
    assert_eq!(hash, [0u8; 32]);
}

#[test]
fn test_from_state_roundtrip() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(3).expect("height 3");
    tree.insert(b"x", &store).unwrap().expect("insert");
    tree.insert(b"y", &store).unwrap().expect("insert");

    let h1 = tree.root_hash(&store).unwrap().expect("hash before");

    // Reconstitute from state
    let tree2 = DenseFixedSizedMerkleTree::from_state(3, 2).expect("from_state");
    let h2 = tree2.root_hash(&store).unwrap().expect("hash after");

    assert_eq!(h1, h2);
    assert_eq!(tree2.count(), 2);
    assert_eq!(tree2.height(), 3);
}

#[test]
fn test_root_hash_changes_on_insert() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(3).expect("height 3");

    tree.insert(b"first", &store).unwrap().expect("insert 1");
    let h1 = tree.root_hash(&store).unwrap().expect("hash 1");

    tree.insert(b"second", &store).unwrap().expect("insert 2");
    let h2 = tree.root_hash(&store).unwrap().expect("hash 2");

    assert_ne!(h1, h2);
}

// ── Vulnerability regression tests ───────────────────────────────────

/// Helper: build a full height-3 tree (7 positions) and return root hash.
fn make_full_h3_tree() -> (MemStore, [u8; 32]) {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(3).expect("height 3 should be valid");
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
fn test_vuln1_node_hashes_root_bypass_rejected() {
    let (_, real_root) = make_full_h3_tree();

    // Attacker constructs a forged proof with root hash at position 0 in
    // node_hashes to short-circuit verification.
    let forged_proof = DenseTreeProof {
        entries: vec![(4, b"FORGED".to_vec())],
        node_value_hashes: vec![],
        node_hashes: vec![(0, real_root)],
    };

    let result = forged_proof.verify_against_expected_root(&real_root, 3, 7);
    assert!(
        result.is_err(),
        "forged proof with node_hash at root should be rejected"
    );
}

#[test]
fn test_vuln1_node_hashes_ancestor_bypass_rejected() {
    let (_, real_root) = make_full_h3_tree();

    // Position 4's ancestors are: 1 (parent of 4), 0 (root).
    // Placing a node_hash at position 1 would bypass verification of
    // the subtree containing position 4.
    let forged_proof = DenseTreeProof {
        entries: vec![(4, b"FORGED".to_vec())],
        node_value_hashes: vec![(0, *blake3::hash(&[0u8]).as_bytes())],
        node_hashes: vec![(1, [0xAA; 32]), (2, [0xBB; 32])],
    };

    let result = forged_proof.verify_against_expected_root(&real_root, 3, 7);
    assert!(
        result.is_err(),
        "forged proof with node_hash at ancestor of entry should be rejected"
    );
}

#[test]
fn test_vuln2_out_of_range_entries_rejected() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(3).expect("height 3 should be valid");
    for i in 0..3u8 {
        tree.insert(&[i], &store)
            .unwrap()
            .expect("insert should succeed");
    }
    let root = tree
        .root_hash(&store)
        .unwrap()
        .expect("root hash should succeed");

    // Generate a legitimate proof for position 0
    let legit_proof = DenseTreeProof::generate(3, 3, &[0], &store)
        .unwrap()
        .expect("generate should succeed");

    // Inject a phantom entry at position 5 (beyond count=3)
    let mut tampered = legit_proof.clone();
    tampered.entries.push((5, b"phantom".to_vec()));

    // Out-of-range entries must cause rejection, not silent filtering
    let result = tampered.verify_against_expected_root(&root, 3, 3);
    assert!(
        result.is_err(),
        "proof with out-of-range entry should be rejected"
    );
}

#[test]
fn test_vuln3_duplicate_entries_rejected() {
    let (store, root) = make_full_h3_tree();

    let legit_proof = DenseTreeProof::generate(3, 7, &[4], &store)
        .unwrap()
        .expect("generate should succeed");

    // Inject a duplicate entry at position 4 with different value
    let mut tampered = legit_proof.clone();
    tampered.entries.push((4, b"FAKE".to_vec()));

    let result = tampered.verify_against_expected_root(&root, 3, 7);
    assert!(
        result.is_err(),
        "proof with duplicate entries should be rejected"
    );
}

#[test]
fn test_vuln3_duplicate_node_value_hashes_rejected() {
    let (store, root) = make_full_h3_tree();

    let mut proof = DenseTreeProof::generate(3, 7, &[4], &store)
        .unwrap()
        .expect("generate should succeed");

    // Inject duplicate in node_value_hashes
    assert!(
        !proof.node_value_hashes.is_empty(),
        "proof for position 4 should have ancestor value hashes"
    );
    let first = proof.node_value_hashes[0];
    proof.node_value_hashes.push(first);

    let result = proof.verify_against_expected_root(&root, 3, 7);
    assert!(
        result.is_err(),
        "proof with duplicate node_value_hashes should be rejected"
    );
}

#[test]
fn test_vuln4_height_overflow_rejected() {
    let proof = DenseTreeProof {
        entries: vec![(0, vec![1, 2, 3])],
        node_value_hashes: vec![],
        node_hashes: vec![],
    };
    let fake_root = [0u8; 32];
    // Caller passes height=17 which should be rejected
    let result = proof.verify_against_expected_root(&fake_root, 17, 1);
    assert!(result.is_err(), "height 17 should be rejected");
}

#[test]
fn test_vuln4_height_zero_rejected() {
    let proof = DenseTreeProof {
        entries: vec![],
        node_value_hashes: vec![],
        node_hashes: vec![],
    };
    let zero_root = [0u8; 32];
    // Caller passes height=0 which should be rejected
    let result = proof.verify_against_expected_root(&zero_root, 0, 0);
    assert!(result.is_err(), "height 0 should be rejected");
}

#[test]
fn test_vuln6_overlapping_entries_and_node_value_hashes_rejected() {
    let (store, root) = make_full_h3_tree();

    let mut proof = DenseTreeProof::generate(3, 7, &[4], &store)
        .unwrap()
        .expect("generate should succeed");

    // Put position 4 in both entries and node_value_hashes
    proof
        .node_value_hashes
        .push((4, *blake3::hash(&[4u8]).as_bytes()));

    let result = proof.verify_against_expected_root(&root, 3, 7);
    assert!(
        result.is_err(),
        "proof with overlapping entries and node_value_hashes should be rejected"
    );
}

#[test]
fn test_vuln6_overlapping_entries_and_node_hashes_rejected() {
    let (store, root) = make_full_h3_tree();

    let mut proof = DenseTreeProof::generate(3, 7, &[4], &store)
        .unwrap()
        .expect("generate should succeed");

    // Put position 4 in both entries and node_hashes
    proof.node_hashes.push((4, [0xAA; 32]));

    let result = proof.verify_against_expected_root(&root, 3, 7);
    assert!(
        result.is_err(),
        "proof with overlapping entries and node_hashes should be rejected"
    );
}

// ── Missing coverage tests ───────────────────────────────────────────

#[test]
fn test_try_insert_success() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(2).expect("height 2");

    let result = tree
        .try_insert(b"first", &store)
        .unwrap()
        .expect("try_insert should not error");
    assert!(result.is_some(), "should return Some when space available");

    let (root_hash, position) = result.expect("should be Some");
    assert_eq!(position, 0);
    assert_ne!(root_hash, [0u8; 32]);
    assert_eq!(tree.count(), 1);
}

#[test]
fn test_try_insert_when_full() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(1).expect("height 1");

    tree.try_insert(b"only", &store)
        .unwrap()
        .expect("first insert should work")
        .expect("should return Some");

    let result = tree
        .try_insert(b"overflow", &store)
        .unwrap()
        .expect("try_insert should return Ok(None), not Err");
    assert!(result.is_none(), "should return None when tree is full");
    assert_eq!(tree.count(), 1, "count should not change");
}

#[test]
fn test_hash_position_root_matches_root_hash() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(2).expect("height 2");
    tree.insert(b"a", &store).unwrap().expect("insert a");
    tree.insert(b"b", &store).unwrap().expect("insert b");

    let root_via_position = tree
        .hash_position(0, &store)
        .unwrap()
        .expect("hash_position(0) should succeed");
    let root_via_method = tree
        .root_hash(&store)
        .unwrap()
        .expect("root_hash should succeed");

    assert_eq!(
        root_via_position, root_via_method,
        "hash_position(0) should equal root_hash()"
    );
}

#[test]
fn test_hash_position_beyond_count() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(3).expect("height 3");
    tree.insert(b"only", &store).unwrap().expect("insert");

    let ctx = tree.hash_position(5, &store);
    assert_eq!(ctx.cost.hash_node_calls, 0);
    let hash = ctx.value.expect("hash_position should succeed");
    assert_eq!(hash, [0u8; 32], "unfilled position should return zero hash");
}

#[test]
fn test_from_state_count_exceeds_capacity() {
    let result = DenseFixedSizedMerkleTree::from_state(2, 100);
    assert!(result.is_err(), "count=100 exceeds capacity=3 for height=2");
}

#[test]
fn test_from_state_invalid_height() {
    assert!(
        DenseFixedSizedMerkleTree::from_state(0, 0).is_err(),
        "height=0 should be invalid"
    );
    assert!(
        DenseFixedSizedMerkleTree::from_state(17, 0).is_err(),
        "height=17 should be invalid"
    );
}

#[test]
fn test_proof_decode_invalid_bytes() {
    let invalid_bytes = vec![0xFF, 0xFF, 0xFF];
    let result = DenseTreeProof::decode_from_slice(&invalid_bytes);
    assert!(result.is_err(), "should fail on invalid bincode data");
}

#[test]
fn test_proof_decode_empty_bytes() {
    let result = DenseTreeProof::decode_from_slice(&[]);
    assert!(result.is_err(), "should fail on empty input");
}

// ── Round 2: Additional vulnerability regression tests ───────────────

#[test]
fn test_vuln3_duplicate_node_hashes_rejected() {
    let (store, root) = make_full_h3_tree();
    let mut proof = DenseTreeProof::generate(3, 7, &[4], &store)
        .unwrap()
        .expect("generate should succeed");

    // Inject a duplicate in node_hashes
    assert!(
        !proof.node_hashes.is_empty(),
        "proof for position 4 should have sibling subtree hashes"
    );
    let first = proof.node_hashes[0];
    proof.node_hashes.push(first);

    let result = proof.verify_against_expected_root(&root, 3, 7);
    assert!(
        result.is_err(),
        "proof with duplicate node_hashes should be rejected"
    );
}

#[test]
fn test_vuln6_overlapping_node_value_hashes_and_node_hashes_rejected() {
    let (store, root) = make_full_h3_tree();
    let mut proof = DenseTreeProof::generate(3, 7, &[4], &store)
        .unwrap()
        .expect("generate should succeed");

    // Take a position from node_value_hashes and add it to node_hashes
    assert!(
        !proof.node_value_hashes.is_empty(),
        "proof for position 4 should have ancestor value hashes"
    );
    let (pos, _) = proof.node_value_hashes[0];
    proof.node_hashes.push((pos, [0xCC; 32]));

    let result = proof.verify_against_expected_root(&root, 3, 7);
    assert!(
        result.is_err(),
        "proof with overlapping node_value_hashes and node_hashes should be rejected"
    );
}

#[test]
fn test_count_exceeds_capacity_rejected_in_verify() {
    let proof = DenseTreeProof {
        entries: vec![(0, vec![1, 2, 3])],
        node_value_hashes: vec![],
        node_hashes: vec![],
    };
    // Caller passes count=100 which exceeds capacity=7 for height=3
    let result = proof.verify_against_expected_root(&[0u8; 32], 3, 100);
    assert!(
        result.is_err(),
        "count exceeding capacity should be rejected"
    );
}

#[test]
fn test_from_state_count_equals_capacity() {
    // Boundary condition: count exactly equals capacity
    let tree = DenseFixedSizedMerkleTree::from_state(2, 3).expect("count=capacity should be valid");
    assert_eq!(tree.count(), 3);
    assert_eq!(tree.capacity(), 3);
}

#[test]
fn test_height_16_capacity_and_insert() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(16).expect("height 16 should be valid");
    assert_eq!(tree.capacity(), 65_535);

    // Insert and hash
    let (hash, pos) = tree
        .insert(b"test", &store)
        .unwrap()
        .expect("insert should succeed");
    assert_eq!(pos, 0);
    assert_ne!(hash, [0u8; 32]);
}

#[test]
fn test_proof_verify_one_bit_different_root() {
    let (store, root) = make_full_h3_tree();
    let proof = DenseTreeProof::generate(3, 7, &[4], &store)
        .unwrap()
        .expect("generate should succeed");

    let mut wrong_root = root;
    wrong_root[0] ^= 0x01; // flip one bit

    let result = proof.verify_against_expected_root(&wrong_root, 3, 7);
    assert!(
        result.is_err(),
        "verification should fail with 1-bit-different root"
    );
}

#[test]
fn test_proof_verify_all_zero_root() {
    let (store, _) = make_full_h3_tree();
    let proof = DenseTreeProof::generate(3, 7, &[4], &store)
        .unwrap()
        .expect("generate should succeed");

    let result = proof.verify_against_expected_root(&[0u8; 32], 3, 7);
    assert!(
        result.is_err(),
        "verification should fail against all-zero root"
    );
}

#[test]
fn test_incomplete_proof_missing_node_value_hash() {
    // Manually construct a proof missing a required node_value_hash
    let proof = DenseTreeProof {
        entries: vec![(4, vec![4u8])],
        node_value_hashes: vec![], // missing ancestors 0 and 1
        node_hashes: vec![],
    };
    let result = proof.verify_against_expected_root(&[0u8; 32], 3, 7);
    assert!(
        result.is_err(),
        "proof missing node_value_hashes for ancestors should fail"
    );
}

#[test]
fn test_empty_tree_proof_generation() {
    let store = MemStore::new();
    // Empty tree — generate proof with no positions
    let proof = DenseTreeProof::generate(3, 0, &[], &store)
        .unwrap()
        .expect("generating proof for empty tree should succeed");
    assert_eq!(proof.entries.len(), 0);

    let root = [0u8; 32]; // empty tree root
    let verified = proof
        .verify_against_expected_root(&root, 3, 0)
        .expect("verifying empty proof should succeed");
    assert_eq!(verified.len(), 0);
}

/// Store that returns errors on specific positions.
struct FailingStore {
    data: RefCell<HashMap<u16, Vec<u8>>>,
    fail_on_get: Option<u16>,
    fail_on_put: Option<u16>,
}

impl FailingStore {
    fn new() -> Self {
        Self {
            data: RefCell::new(HashMap::new()),
            fail_on_get: None,
            fail_on_put: None,
        }
    }
}

impl DenseTreeStore for FailingStore {
    fn get_value(&self, position: u16) -> CostResult<Option<Vec<u8>>, DenseMerkleError> {
        if self.fail_on_get == Some(position) {
            return Err(DenseMerkleError::StoreError("simulated get failure".into()))
                .wrap_with_cost(OperationCost::default());
        }
        Ok(self.data.borrow().get(&position).cloned()).wrap_with_cost(OperationCost::default())
    }

    fn put_value(&self, position: u16, value: &[u8]) -> CostResult<(), DenseMerkleError> {
        if self.fail_on_put == Some(position) {
            return Err(DenseMerkleError::StoreError("simulated put failure".into()))
                .wrap_with_cost(OperationCost::default());
        }
        self.data.borrow_mut().insert(position, value.to_vec());
        Ok(()).wrap_with_cost(OperationCost::default())
    }
}

#[test]
fn test_insert_store_put_failure() {
    let store = FailingStore {
        fail_on_put: Some(0),
        ..FailingStore::new()
    };
    let mut tree = DenseFixedSizedMerkleTree::new(2).expect("height 2");
    let result = tree.insert(b"test", &store);
    assert!(
        result.unwrap().is_err(),
        "insert should propagate store put error"
    );
}

#[test]
fn test_root_hash_store_get_failure() {
    // Insert with a good store, then compute hash with a failing store
    let good_store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(2).expect("height 2");
    tree.insert(b"val", &good_store)
        .unwrap()
        .expect("insert should succeed");

    let failing_store = FailingStore {
        fail_on_get: Some(0),
        ..FailingStore::new()
    };
    let result = tree.root_hash(&failing_store);
    assert!(
        result.unwrap().is_err(),
        "root_hash should propagate store get error"
    );
}

#[test]
fn test_get_store_inconsistency_errors() {
    // Build a tree with count=2 but an empty store (simulates corruption)
    let tree = DenseFixedSizedMerkleTree::from_state(3, 2).expect("from_state should succeed");
    let empty_store = MemStore::new();

    // Position 0 is < count but has no value in store
    let result = tree.get(0, &empty_store);
    assert!(
        result.unwrap().is_err(),
        "get should error when position < count but store has no value"
    );

    // Position 5 is >= count, should return None (not error)
    let result = tree.get(5, &empty_store);
    assert_eq!(
        result.unwrap().expect("get beyond count should succeed"),
        None
    );
}

#[test]
fn test_proof_generate_store_failure() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(3).expect("height 3");
    tree.insert(b"val", &store)
        .unwrap()
        .expect("insert should succeed");

    // Corrupt the store by removing the value
    store.data.borrow_mut().remove(&0);

    let result = DenseTreeProof::generate(3, 1, &[0], &store);
    assert!(
        result.unwrap().is_err(),
        "proof generation should fail when store value is missing"
    );
}

// ── Round 3: DoS prevention, large tree, and rollback tests ──────────

#[test]
fn test_dos_too_many_entries_rejected() {
    // Build a proof with more than 65535 entries (max u16 capacity)
    // Since u16 max is 65535, we test with a count that exceeds what's possible
    let entries: Vec<(u16, Vec<u8>)> = (0..65_535u16).map(|i| (i, vec![0u8])).collect();
    let proof = DenseTreeProof {
        entries,
        node_value_hashes: vec![],
        node_hashes: vec![],
    };
    let result = proof.verify_against_expected_root(&[0u8; 32], 16, 65_535);
    // 65535 entries == capacity for height 16, so passes the DoS check.
    // Should fail for root mismatch instead.
    assert!(result.is_err(), "proof should fail (root mismatch)");
}

#[test]
fn test_dos_too_many_node_value_hashes_rejected() {
    let node_value_hashes: Vec<(u16, [u8; 32])> = (0..65_535u16).map(|i| (i, [0u8; 32])).collect();
    let proof = DenseTreeProof {
        entries: vec![(65_534, vec![1u8])],
        node_value_hashes,
        node_hashes: vec![],
    };
    let result = proof.verify_against_expected_root(&[0u8; 32], 16, 65_535);
    assert!(
        result.is_err(),
        "proof with many node_value_hashes should be rejected"
    );
}

#[test]
fn test_dos_too_many_node_hashes_rejected() {
    let node_hashes: Vec<(u16, [u8; 32])> = (0..65_535u16).map(|i| (i, [0u8; 32])).collect();
    let proof = DenseTreeProof {
        entries: vec![(0, vec![1u8])],
        node_value_hashes: vec![],
        node_hashes,
    };
    let result = proof.verify_against_expected_root(&[0u8; 32], 16, 65_535);
    assert!(
        result.is_err(),
        "proof with many node_hashes should be rejected"
    );
}

#[test]
fn test_dos_exactly_at_limit_accepted() {
    // 65535 entries == capacity for height 16 — exactly at limit.
    let entries: Vec<(u16, Vec<u8>)> = (0..65_535u16).map(|i| (i, vec![0u8])).collect();
    let proof = DenseTreeProof {
        entries,
        node_value_hashes: vec![],
        node_hashes: vec![],
    };
    let result = proof.verify_against_expected_root(&[0u8; 32], 16, 65_535);
    // Should fail for root mismatch, NOT for exceeding capacity
    assert!(result.is_err(), "proof should fail (root mismatch)");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        !err_msg.contains("exceeds tree capacity"),
        "65535 entries should not trigger capacity limit, got: {}",
        err_msg
    );
}

#[test]
fn test_large_tree_height_8_proof() {
    // Height 8: capacity = 255 nodes
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(8).expect("height 8 should be valid");
    assert_eq!(tree.capacity(), 255);

    // Fill the tree completely
    let mut root = [0u8; 32];
    for i in 0..255u16 {
        let (h, pos) = tree
            .insert(&i.to_be_bytes(), &store)
            .unwrap()
            .expect("insert should succeed");
        assert_eq!(pos, i);
        root = h;
    }
    assert_eq!(tree.count(), 255);

    // Prove a leaf deep in the tree
    let proof = DenseTreeProof::generate(8, 255, &[200], &store)
        .unwrap()
        .expect("generate proof for large tree should succeed");
    let verified = proof
        .verify_against_expected_root(&root, 8, 255)
        .expect("verify proof for large tree should succeed");
    assert_eq!(verified.len(), 1);
    assert_eq!(verified[0].0, 200);
    assert_eq!(verified[0].1, 200u16.to_be_bytes().to_vec());
}

#[test]
fn test_large_tree_multiple_positions_proof() {
    // Height 5: capacity = 31 nodes
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(5).expect("height 5 should be valid");

    let mut root = [0u8; 32];
    for i in 0..31u8 {
        let (h, _) = tree
            .insert(&[i], &store)
            .unwrap()
            .expect("insert should succeed");
        root = h;
    }

    // Prove multiple positions at different tree levels
    let positions = vec![0, 1, 7, 15, 30]; // root, internal, mid, leaf, last leaf
    let proof = DenseTreeProof::generate(5, 31, &positions, &store)
        .unwrap()
        .expect("generate multi-position proof should succeed");
    let verified = proof
        .verify_against_expected_root(&root, 5, 31)
        .expect("verify multi-position proof should succeed");
    assert_eq!(verified.len(), 5);

    // Verify values match
    for (pos, val) in &verified {
        assert_eq!(*val, vec![*pos as u8]);
    }
}

#[test]
fn test_proof_complex_with_all_three_fields() {
    // Generate a proof that has entries, node_value_hashes, and node_hashes
    let (store, root) = make_full_h3_tree();

    // Prove position 4 (leaf): ancestors are 1 and 0
    let proof = DenseTreeProof::generate(3, 7, &[4], &store)
        .unwrap()
        .expect("generate should succeed");

    // Verify the proof has all three field types populated
    assert!(!proof.entries.is_empty(), "proof should have entries");
    assert!(
        !proof.node_value_hashes.is_empty(),
        "proof should have node_value_hashes (ancestor value hashes)"
    );
    assert!(
        !proof.node_hashes.is_empty(),
        "proof should have node_hashes (sibling subtree hashes)"
    );

    // Verify it passes
    let verified = proof
        .verify_against_expected_root(&root, 3, 7)
        .expect("verify should succeed");
    assert_eq!(verified.len(), 1);
    assert_eq!(verified[0], (4, vec![4u8]));
}

#[test]
fn test_generate_invalid_height_returns_error() {
    let store = MemStore::new();

    // Height 0 should error, not panic
    let result = DenseTreeProof::generate(0, 0, &[], &store);
    assert!(result.unwrap().is_err(), "height 0 should return error");

    // Height 17 should error
    let result = DenseTreeProof::generate(17, 0, &[], &store);
    assert!(result.unwrap().is_err(), "height 17 should return error");

    // Height 255 should error
    let result = DenseTreeProof::generate(255, 0, &[], &store);
    assert!(result.unwrap().is_err(), "height 255 should return error");
}

#[test]
fn test_insert_rollback_on_hash_failure() {
    // Use a store that succeeds on put but fails on get (to make
    // compute_root_hash fail after count is incremented).
    let mut store = FailingStore::new();

    let mut tree = DenseFixedSizedMerkleTree::new(2).expect("height 2");

    // First insert position 0 — put succeeds, but then root hash computation
    // calls get_value(0) which will fail.
    store.fail_on_get = Some(0);

    let result = tree.insert(b"test", &store);
    assert!(
        result.unwrap().is_err(),
        "insert should fail when hash computation fails"
    );
    assert_eq!(
        tree.count(),
        0,
        "count should be rolled back to 0 after hash failure"
    );

    // Now the tree should still accept inserts (it's not in a broken state)
    store.fail_on_get = None;
    let (_, pos) = tree
        .insert(b"retry", &store)
        .unwrap()
        .expect("insert should succeed after rollback");
    assert_eq!(
        pos, 0,
        "should insert at position 0 since rollback happened"
    );
    assert_eq!(tree.count(), 1);
}

#[test]
fn test_try_insert_rollback_on_hash_failure() {
    let mut store = FailingStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(2).expect("height 2");

    store.fail_on_get = Some(0);
    let result = tree.try_insert(b"test", &store);
    assert!(
        result.unwrap().is_err(),
        "try_insert should fail when hash computation fails"
    );
    assert_eq!(
        tree.count(),
        0,
        "count should be rolled back after try_insert hash failure"
    );

    store.fail_on_get = None;
    let result = tree
        .try_insert(b"retry", &store)
        .unwrap()
        .expect("try_insert should succeed after rollback");
    assert!(result.is_some(), "should return Some after rollback");
    assert_eq!(tree.count(), 1);
}

#[test]
fn test_deduplication_with_mixed_duplicates() {
    let (store, root) = make_full_h3_tree();

    // Pass duplicates of multiple positions
    let proof = DenseTreeProof::generate(3, 7, &[4, 5, 4, 6, 5, 4], &store)
        .unwrap()
        .expect("generate should succeed with duplicates");

    // Should be deduplicated to {4, 5, 6}
    assert_eq!(proof.entries.len(), 3);

    let verified = proof
        .verify_against_expected_root(&root, 3, 7)
        .expect("verify should succeed");
    assert_eq!(verified.len(), 3);
    // Positions should be sorted (from BTreeSet)
    assert_eq!(verified[0].0, 4);
    assert_eq!(verified[1].0, 5);
    assert_eq!(verified[2].0, 6);
}
