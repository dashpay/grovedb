use std::{cell::RefCell, collections::HashMap};

use super::*;
use crate::{hash::blake3_merge, proof::DenseTreeProof};

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
    fn get_value(&self, position: u16) -> Result<Option<Vec<u8>>, DenseMerkleError> {
        Ok(self.data.borrow().get(&position).cloned())
    }

    fn put_value(&self, position: u16, value: &[u8]) -> Result<(), DenseMerkleError> {
        self.data.borrow_mut().insert(position, value.to_vec());
        Ok(())
    }
}

// ── Existing utility function tests ──────────────────────────────────

#[test]
fn test_dense_merkle_root_single_leaf() {
    let leaf_hash = *blake3::hash(b"only").as_bytes();
    let root = compute_dense_merkle_root(&[leaf_hash]).expect("single leaf root");
    assert_eq!(root, leaf_hash);
}

#[test]
fn test_dense_merkle_root_two_leaves() {
    let h0 = *blake3::hash(b"a").as_bytes();
    let h1 = *blake3::hash(b"b").as_bytes();
    let root = compute_dense_merkle_root(&[h0, h1]).expect("two leaf root");
    let expected = blake3_merge(&h0, &h1);
    assert_eq!(root, expected);
}

#[test]
fn test_dense_merkle_root_four_leaves() {
    let hashes: Vec<[u8; 32]> = (0..4u8).map(|i| *blake3::hash(&[i]).as_bytes()).collect();
    let root = compute_dense_merkle_root(&hashes).expect("four leaf root");

    let left = blake3_merge(&hashes[0], &hashes[1]);
    let right = blake3_merge(&hashes[2], &hashes[3]);
    let expected = blake3_merge(&left, &right);
    assert_eq!(root, expected);
}

#[test]
fn test_dense_merkle_root_deterministic() {
    let hashes: Vec<[u8; 32]> = (0..8u8).map(|i| *blake3::hash(&[i]).as_bytes()).collect();
    let root1 = compute_dense_merkle_root(&hashes).expect("deterministic root 1");
    let root2 = compute_dense_merkle_root(&hashes).expect("deterministic root 2");
    assert_eq!(root1, root2);
}

#[test]
fn test_dense_merkle_root_different_inputs() {
    let h1: Vec<[u8; 32]> = (0..4u8).map(|i| *blake3::hash(&[i]).as_bytes()).collect();
    let h2: Vec<[u8; 32]> = (10..14u8).map(|i| *blake3::hash(&[i]).as_bytes()).collect();
    assert_ne!(
        compute_dense_merkle_root(&h1).expect("root for h1"),
        compute_dense_merkle_root(&h2).expect("root for h2")
    );
}

#[test]
fn test_dense_merkle_root_from_values() {
    let values: Vec<&[u8]> = vec![b"alpha", b"beta", b"gamma", b"delta"];
    let (root, hash_count) =
        compute_dense_merkle_root_from_values(&values).expect("root from values");
    assert_eq!(hash_count, 7); // 4 leaf + 3 internal

    // Should match computing leaf hashes first then calling the other function
    let leaf_hashes: Vec<[u8; 32]> = values.iter().map(|v| *blake3::hash(v).as_bytes()).collect();
    let expected = compute_dense_merkle_root(&leaf_hashes).expect("root from leaf hashes");
    assert_eq!(root, expected);
}

#[test]
fn test_dense_merkle_root_large() {
    let hashes: Vec<[u8; 32]> = (0..1024u32)
        .map(|i| *blake3::hash(&i.to_be_bytes()).as_bytes())
        .collect();
    let root = compute_dense_merkle_root(&hashes).expect("large root");
    // Just verify it produces a non-zero result and is deterministic
    assert_ne!(root, [0u8; 32]);
    assert_eq!(
        root,
        compute_dense_merkle_root(&hashes).expect("large root again")
    );
}

#[test]
fn test_dense_merkle_root_empty_error() {
    let result = compute_dense_merkle_root(&[]);
    assert!(result.is_err());
}

#[test]
fn test_dense_merkle_root_non_power_of_two_error() {
    let hashes: Vec<[u8; 32]> = (0..3u8).map(|i| *blake3::hash(&[i]).as_bytes()).collect();
    let result = compute_dense_merkle_root(&hashes);
    assert!(result.is_err());
}

#[test]
fn test_dense_merkle_root_from_values_empty_error() {
    let result = compute_dense_merkle_root_from_values(&[]);
    assert!(result.is_err());
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

    let (root_hash, pos, hash_calls) = tree
        .insert(b"hello", &store)
        .expect("insert should succeed");
    assert_eq!(pos, 0);
    assert_eq!(tree.count(), 1);
    assert_ne!(root_hash, [0u8; 32]);
    // Single node at leaf level: 1 hash call
    assert_eq!(hash_calls, 1);

    // Root hash should be blake3(0x00 || value) for a single-node tree
    // (domain-separated leaf)
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[0x00]);
    hasher.update(b"hello");
    let expected = *hasher.finalize().as_bytes();
    assert_eq!(root_hash, expected);
}

#[test]
fn test_sequential_fill_height_2() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(2).expect("height 2");
    assert_eq!(tree.capacity(), 3); // 2^2 - 1 = 3

    // Insert three values (positions 0, 1, 2)
    let (_, pos0, _) = tree.insert(b"root_val", &store).expect("insert 0");
    assert_eq!(pos0, 0);

    let (_, pos1, _) = tree.insert(b"left_val", &store).expect("insert 1");
    assert_eq!(pos1, 1);

    let (root_hash, pos2, _) = tree.insert(b"right_val", &store).expect("insert 2");
    assert_eq!(pos2, 2);
    assert_eq!(tree.count(), 3);

    // Verify structure: root=0 has children at 1 and 2
    // Root hash = blake3(0x01 || H(root_val) || H(left) || H(right))
    // H(left) = blake3(0x00 || left_val), H(right) = blake3(0x00 || right_val)
    // (both leaves)
    let h_left = {
        let mut h = blake3::Hasher::new();
        h.update(&[0x00]);
        h.update(b"left_val");
        *h.finalize().as_bytes()
    };
    let h_right = {
        let mut h = blake3::Hasher::new();
        h.update(&[0x00]);
        h.update(b"right_val");
        *h.finalize().as_bytes()
    };
    let h_root_val = *blake3::hash(b"root_val").as_bytes();
    let expected = {
        let mut h = blake3::Hasher::new();
        h.update(&[0x01]);
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
    tree.insert(b"only", &store).expect("first insert");
    let result = tree.insert(b"overflow", &store);
    assert!(result.is_err());
}

#[test]
fn test_get_by_position() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(2).expect("height 2");
    tree.insert(b"val0", &store).expect("insert 0");
    tree.insert(b"val1", &store).expect("insert 1");

    assert_eq!(tree.get(0, &store).expect("get 0"), Some(b"val0".to_vec()));
    assert_eq!(tree.get(1, &store).expect("get 1"), Some(b"val1".to_vec()));
    // Position 2 not yet filled
    assert_eq!(tree.get(2, &store).expect("get 2"), None);
    // Beyond capacity
    assert_eq!(tree.get(100, &store).expect("get 100"), None);
}

#[test]
fn test_root_hash_determinism() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(3).expect("height 3");
    tree.insert(b"a", &store).expect("insert a");
    tree.insert(b"b", &store).expect("insert b");

    let (h1, _) = tree.root_hash(&store).expect("root hash 1");
    let (h2, _) = tree.root_hash(&store).expect("root hash 2");
    assert_eq!(h1, h2);
}

#[test]
fn test_empty_tree_root_hash() {
    let store = MemStore::new();
    let tree = DenseFixedSizedMerkleTree::new(3).expect("height 3");
    let (hash, calls) = tree.root_hash(&store).expect("empty root hash");
    assert_eq!(hash, [0u8; 32]);
    assert_eq!(calls, 0);
}

#[test]
fn test_from_state_roundtrip() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(3).expect("height 3");
    tree.insert(b"x", &store).expect("insert");
    tree.insert(b"y", &store).expect("insert");

    let (h1, _) = tree.root_hash(&store).expect("hash before");

    // Reconstitute from state
    let tree2 = DenseFixedSizedMerkleTree::from_state(3, 2).expect("from_state");
    let (h2, _) = tree2.root_hash(&store).expect("hash after");

    assert_eq!(h1, h2);
    assert_eq!(tree2.count(), 2);
    assert_eq!(tree2.height(), 3);
}

#[test]
fn test_root_hash_changes_on_insert() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(3).expect("height 3");

    tree.insert(b"first", &store).expect("insert 1");
    let (h1, _) = tree.root_hash(&store).expect("hash 1");

    tree.insert(b"second", &store).expect("insert 2");
    let (h2, _) = tree.root_hash(&store).expect("hash 2");

    assert_ne!(h1, h2);
}

// ── Vulnerability regression tests ───────────────────────────────────

/// Helper: build a full height-3 tree (7 positions) and return root hash.
fn make_full_h3_tree() -> (MemStore, [u8; 32]) {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(3).expect("height 3 should be valid");
    let mut root = [0u8; 32];
    for i in 0..7u8 {
        let (h, ..) = tree.insert(&[i], &store).expect("insert should succeed");
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
        height: 3,
        count: 7,
        entries: vec![(4, b"FORGED".to_vec())],
        node_value_hashes: vec![],
        node_hashes: vec![(0, real_root)],
    };

    let result = forged_proof.verify(&real_root);
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
        height: 3,
        count: 7,
        entries: vec![(4, b"FORGED".to_vec())],
        node_value_hashes: vec![(0, *blake3::hash(&[0u8]).as_bytes())],
        node_hashes: vec![(1, [0xAA; 32]), (2, [0xBB; 32])],
    };

    let result = forged_proof.verify(&real_root);
    assert!(
        result.is_err(),
        "forged proof with node_hash at ancestor of entry should be rejected"
    );
}

#[test]
fn test_vuln2_out_of_range_entries_filtered() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(3).expect("height 3 should be valid");
    for i in 0..3u8 {
        tree.insert(&[i], &store).expect("insert should succeed");
    }
    let (root, _) = tree.root_hash(&store).expect("root hash should succeed");

    // Generate a legitimate proof for position 0
    let legit_proof =
        DenseTreeProof::generate(3, 3, &[0], &store).expect("generate should succeed");

    // Inject a phantom entry at position 5 (beyond count=3)
    let mut tampered = legit_proof.clone();
    tampered.entries.push((5, b"phantom".to_vec()));

    // The proof may fail verification due to the phantom entry, but if it
    // somehow passes, the phantom entry must NOT appear in results.
    match tampered.verify(&root) {
        Ok(verified) => {
            for (pos, _) in &verified {
                assert!(
                    *pos < 3,
                    "entry at position {} is beyond count=3 and should be filtered",
                    pos
                );
            }
        }
        Err(_) => {} // Also acceptable — the proof is invalid
    }
}

#[test]
fn test_vuln3_duplicate_entries_rejected() {
    let (store, root) = make_full_h3_tree();

    let legit_proof =
        DenseTreeProof::generate(3, 7, &[4], &store).expect("generate should succeed");

    // Inject a duplicate entry at position 4 with different value
    let mut tampered = legit_proof.clone();
    tampered.entries.push((4, b"FAKE".to_vec()));

    let result = tampered.verify(&root);
    assert!(
        result.is_err(),
        "proof with duplicate entries should be rejected"
    );
}

#[test]
fn test_vuln3_duplicate_node_value_hashes_rejected() {
    let (store, root) = make_full_h3_tree();

    let mut proof = DenseTreeProof::generate(3, 7, &[4], &store).expect("generate should succeed");

    // Inject duplicate in node_value_hashes
    if let Some(first) = proof.node_value_hashes.first().cloned() {
        proof.node_value_hashes.push(first);
    }

    let result = proof.verify(&root);
    // Should fail if there are duplicate node_value_hashes
    if proof.node_value_hashes.len() > 1 {
        assert!(
            result.is_err(),
            "proof with duplicate node_value_hashes should be rejected"
        );
    }
}

#[test]
fn test_vuln4_height_overflow_rejected() {
    let proof = DenseTreeProof {
        height: 17,
        count: 1,
        entries: vec![(0, vec![1, 2, 3])],
        node_value_hashes: vec![],
        node_hashes: vec![],
    };
    let fake_root = [0u8; 32];
    let result = proof.verify(&fake_root);
    assert!(result.is_err(), "height 17 should be rejected");
}

#[test]
fn test_vuln4_height_zero_rejected() {
    let proof = DenseTreeProof {
        height: 0,
        count: 0,
        entries: vec![],
        node_value_hashes: vec![],
        node_hashes: vec![],
    };
    let zero_root = [0u8; 32];
    let result = proof.verify(&zero_root);
    assert!(result.is_err(), "height 0 should be rejected");
}

#[test]
fn test_vuln4_decode_invalid_height_rejected() {
    // Craft a proof with height=17, encode it, then decode
    let proof = DenseTreeProof {
        height: 17,
        count: 0,
        entries: vec![],
        node_value_hashes: vec![],
        node_hashes: vec![],
    };
    let bytes = proof
        .encode_to_vec()
        .expect("encoding should succeed even with bad height");
    let result = DenseTreeProof::decode_from_slice(&bytes);
    assert!(result.is_err(), "decode should reject height 17");
}

#[test]
fn test_vuln6_overlapping_entries_and_node_value_hashes_rejected() {
    let (store, root) = make_full_h3_tree();

    let mut proof = DenseTreeProof::generate(3, 7, &[4], &store).expect("generate should succeed");

    // Put position 4 in both entries and node_value_hashes
    proof
        .node_value_hashes
        .push((4, *blake3::hash(&[4u8]).as_bytes()));

    let result = proof.verify(&root);
    assert!(
        result.is_err(),
        "proof with overlapping entries and node_value_hashes should be rejected"
    );
}

#[test]
fn test_vuln6_overlapping_entries_and_node_hashes_rejected() {
    let (store, root) = make_full_h3_tree();

    let mut proof = DenseTreeProof::generate(3, 7, &[4], &store).expect("generate should succeed");

    // Put position 4 in both entries and node_hashes
    proof.node_hashes.push((4, [0xAA; 32]));

    let result = proof.verify(&root);
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
        .expect("try_insert should not error");
    assert!(result.is_some(), "should return Some when space available");

    let (root_hash, position, _) = result.expect("should be Some");
    assert_eq!(position, 0);
    assert_ne!(root_hash, [0u8; 32]);
    assert_eq!(tree.count(), 1);
}

#[test]
fn test_try_insert_when_full() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(1).expect("height 1");

    tree.try_insert(b"only", &store)
        .expect("first insert should work")
        .expect("should return Some");

    let result = tree
        .try_insert(b"overflow", &store)
        .expect("try_insert should return Ok(None), not Err");
    assert!(result.is_none(), "should return None when tree is full");
    assert_eq!(tree.count(), 1, "count should not change");
}

#[test]
fn test_hash_position_root_matches_root_hash() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(2).expect("height 2");
    tree.insert(b"a", &store).expect("insert a");
    tree.insert(b"b", &store).expect("insert b");

    let (root_via_position, _) = tree
        .hash_position(0, &store)
        .expect("hash_position(0) should succeed");
    let (root_via_method, _) = tree.root_hash(&store).expect("root_hash should succeed");

    assert_eq!(
        root_via_position, root_via_method,
        "hash_position(0) should equal root_hash()"
    );
}

#[test]
fn test_hash_position_beyond_count() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(3).expect("height 3");
    tree.insert(b"only", &store).expect("insert");

    let (hash, calls) = tree
        .hash_position(5, &store)
        .expect("hash_position should succeed");
    assert_eq!(hash, [0u8; 32], "unfilled position should return zero hash");
    assert_eq!(calls, 0);
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
    let mut proof = DenseTreeProof::generate(3, 7, &[4], &store).expect("generate should succeed");

    // Inject a duplicate in node_hashes
    if let Some(first) = proof.node_hashes.first().cloned() {
        proof.node_hashes.push(first);
        let result = proof.verify(&root);
        assert!(
            result.is_err(),
            "proof with duplicate node_hashes should be rejected"
        );
    }
}

#[test]
fn test_vuln6_overlapping_node_value_hashes_and_node_hashes_rejected() {
    let (store, root) = make_full_h3_tree();
    let mut proof = DenseTreeProof::generate(3, 7, &[4], &store).expect("generate should succeed");

    // Take a position from node_value_hashes and add it to node_hashes
    if let Some((pos, _)) = proof.node_value_hashes.first().cloned() {
        proof.node_hashes.push((pos, [0xCC; 32]));
        let result = proof.verify(&root);
        assert!(
            result.is_err(),
            "proof with overlapping node_value_hashes and node_hashes should be rejected"
        );
    }
}

#[test]
fn test_count_exceeds_capacity_rejected_in_verify() {
    let proof = DenseTreeProof {
        height: 3,
        count: 100, // capacity is 7 for height=3
        entries: vec![(0, vec![1, 2, 3])],
        node_value_hashes: vec![],
        node_hashes: vec![],
    };
    let result = proof.verify(&[0u8; 32]);
    assert!(
        result.is_err(),
        "count exceeding capacity should be rejected"
    );
}

#[test]
fn test_count_exceeds_capacity_rejected_in_decode() {
    // Craft a proof with count > capacity and encode it
    let proof = DenseTreeProof {
        height: 3,
        count: 100,
        entries: vec![],
        node_value_hashes: vec![],
        node_hashes: vec![],
    };
    let bytes = proof.encode_to_vec().expect("encoding should succeed");
    let result = DenseTreeProof::decode_from_slice(&bytes);
    assert!(result.is_err(), "decode should reject count > capacity");
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
    let (hash, pos, _) = tree.insert(b"test", &store).expect("insert should succeed");
    assert_eq!(pos, 0);
    assert_ne!(hash, [0u8; 32]);
}

#[test]
fn test_proof_verify_one_bit_different_root() {
    let (store, root) = make_full_h3_tree();
    let proof = DenseTreeProof::generate(3, 7, &[4], &store).expect("generate should succeed");

    let mut wrong_root = root;
    wrong_root[0] ^= 0x01; // flip one bit

    let result = proof.verify(&wrong_root);
    assert!(
        result.is_err(),
        "verification should fail with 1-bit-different root"
    );
}

#[test]
fn test_proof_verify_all_zero_root() {
    let (store, _) = make_full_h3_tree();
    let proof = DenseTreeProof::generate(3, 7, &[4], &store).expect("generate should succeed");

    let result = proof.verify(&[0u8; 32]);
    assert!(
        result.is_err(),
        "verification should fail against all-zero root"
    );
}

#[test]
fn test_incomplete_proof_missing_node_value_hash() {
    // Manually construct a proof missing a required node_value_hash
    let proof = DenseTreeProof {
        height: 3,
        count: 7,
        entries: vec![(4, vec![4u8])],
        node_value_hashes: vec![], // missing ancestors 0 and 1
        node_hashes: vec![],
    };
    let result = proof.verify(&[0u8; 32]);
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
        .expect("generating proof for empty tree should succeed");
    assert_eq!(proof.entries_len(), 0);

    let root = [0u8; 32]; // empty tree root
    let verified = proof
        .verify(&root)
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
    fn get_value(&self, position: u16) -> Result<Option<Vec<u8>>, DenseMerkleError> {
        if self.fail_on_get == Some(position) {
            return Err(DenseMerkleError::StoreError("simulated get failure".into()));
        }
        Ok(self.data.borrow().get(&position).cloned())
    }

    fn put_value(&self, position: u16, value: &[u8]) -> Result<(), DenseMerkleError> {
        if self.fail_on_put == Some(position) {
            return Err(DenseMerkleError::StoreError("simulated put failure".into()));
        }
        self.data.borrow_mut().insert(position, value.to_vec());
        Ok(())
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
    assert!(result.is_err(), "insert should propagate store put error");
}

#[test]
fn test_root_hash_store_get_failure() {
    // Insert with a good store, then compute hash with a failing store
    let good_store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(2).expect("height 2");
    tree.insert(b"val", &good_store)
        .expect("insert should succeed");

    let failing_store = FailingStore {
        fail_on_get: Some(0),
        ..FailingStore::new()
    };
    let result = tree.root_hash(&failing_store);
    assert!(
        result.is_err(),
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
        result.is_err(),
        "get should error when position < count but store has no value"
    );

    // Position 5 is >= count, should return None (not error)
    let result = tree.get(5, &empty_store);
    assert_eq!(result.expect("get beyond count should succeed"), None);
}

#[test]
fn test_proof_generate_store_failure() {
    let store = MemStore::new();
    let mut tree = DenseFixedSizedMerkleTree::new(3).expect("height 3");
    tree.insert(b"val", &store).expect("insert should succeed");

    // Corrupt the store by removing the value
    store.data.borrow_mut().remove(&0);

    let result = DenseTreeProof::generate(3, 1, &[0], &store);
    assert!(
        result.is_err(),
        "proof generation should fail when store value is missing"
    );
}

#[test]
fn test_compute_dense_merkle_root_from_values_non_power_of_two() {
    let values: Vec<&[u8]> = vec![b"a", b"b", b"c"];
    let result = compute_dense_merkle_root_from_values(&values);
    assert!(result.is_err(), "should reject non-power-of-two length");
}

#[test]
fn test_height_and_count_accessor() {
    let proof = DenseTreeProof {
        height: 5,
        count: 10,
        entries: vec![],
        node_value_hashes: vec![],
        node_hashes: vec![],
    };
    let (h, c) = proof.height_and_count();
    assert_eq!(h, 5);
    assert_eq!(c, 10);
}

// ── Round 3: DoS prevention, large tree, and rollback tests ──────────

#[test]
fn test_dos_too_many_entries_rejected() {
    // Build a proof with more than 65535 entries (max u16 capacity)
    // Since u16 max is 65535, we test with a count that exceeds what's possible
    let entries: Vec<(u16, Vec<u8>)> = (0..65_535u16).map(|i| (i, vec![0u8])).collect();
    let proof = DenseTreeProof {
        height: 16,
        count: 65_535,
        entries,
        node_value_hashes: vec![],
        node_hashes: vec![],
    };
    let result = proof.verify(&[0u8; 32]);
    // With u16 positions, max entries is 65535 which is under 100_000 limit.
    // The proof should fail for root mismatch, not DoS.
    assert!(result.is_err(), "proof should fail (root mismatch)");
}

#[test]
fn test_dos_too_many_node_value_hashes_rejected() {
    let node_value_hashes: Vec<(u16, [u8; 32])> = (0..65_535u16).map(|i| (i, [0u8; 32])).collect();
    let proof = DenseTreeProof {
        height: 16,
        count: 65_535,
        entries: vec![(65_534, vec![1u8])],
        node_value_hashes,
        node_hashes: vec![],
    };
    let result = proof.verify(&[0u8; 32]);
    assert!(
        result.is_err(),
        "proof with many node_value_hashes should be rejected"
    );
}

#[test]
fn test_dos_too_many_node_hashes_rejected() {
    let node_hashes: Vec<(u16, [u8; 32])> = (0..65_535u16).map(|i| (i, [0u8; 32])).collect();
    let proof = DenseTreeProof {
        height: 16,
        count: 65_535,
        entries: vec![(0, vec![1u8])],
        node_value_hashes: vec![],
        node_hashes,
    };
    let result = proof.verify(&[0u8; 32]);
    assert!(
        result.is_err(),
        "proof with many node_hashes should be rejected"
    );
}

#[test]
fn test_dos_exactly_at_limit_accepted() {
    // With u16, max capacity is 65535 (height=16), well under the 100_000 DoS
    // limit. Verify that a proof with all 65535 positions doesn't trigger DoS.
    let entries: Vec<(u16, Vec<u8>)> = (0..65_535u16).map(|i| (i, vec![0u8])).collect();
    let proof = DenseTreeProof {
        height: 16,
        count: 65_535,
        entries,
        node_value_hashes: vec![],
        node_hashes: vec![],
    };
    let result = proof.verify(&[0u8; 32]);
    // Should fail, but NOT with "too many elements" message
    assert!(result.is_err(), "proof should fail (root mismatch)");
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        !err_msg.contains("too many elements"),
        "65535 entries should not trigger DoS limit, got: {}",
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
        let (h, pos, _) = tree
            .insert(&i.to_be_bytes(), &store)
            .expect("insert should succeed");
        assert_eq!(pos, i);
        root = h;
    }
    assert_eq!(tree.count(), 255);

    // Prove a leaf deep in the tree
    let proof = DenseTreeProof::generate(8, 255, &[200], &store)
        .expect("generate proof for large tree should succeed");
    let verified = proof
        .verify(&root)
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
        let (h, ..) = tree.insert(&[i], &store).expect("insert should succeed");
        root = h;
    }

    // Prove multiple positions at different tree levels
    let positions = vec![0, 1, 7, 15, 30]; // root, internal, mid, leaf, last leaf
    let proof = DenseTreeProof::generate(5, 31, &positions, &store)
        .expect("generate multi-position proof should succeed");
    let verified = proof
        .verify(&root)
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
    let proof = DenseTreeProof::generate(3, 7, &[4], &store).expect("generate should succeed");

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
    let verified = proof.verify(&root).expect("verify should succeed");
    assert_eq!(verified.len(), 1);
    assert_eq!(verified[0], (4, vec![4u8]));
}

#[test]
fn test_generate_invalid_height_returns_error() {
    let store = MemStore::new();

    // Height 0 should error, not panic
    let result = DenseTreeProof::generate(0, 0, &[], &store);
    assert!(result.is_err(), "height 0 should return error");

    // Height 17 should error
    let result = DenseTreeProof::generate(17, 0, &[], &store);
    assert!(result.is_err(), "height 17 should return error");

    // Height 255 should error
    let result = DenseTreeProof::generate(255, 0, &[], &store);
    assert!(result.is_err(), "height 255 should return error");
}

#[test]
fn test_insert_rollback_on_hash_failure() {
    // Use a store that succeeds on put but fails on get (to make
    // compute_root_hash fail after count is incremented).
    let mut store = FailingStore::new();

    let mut tree = DenseFixedSizedMerkleTree::new(2).expect("height 2");

    // First insert position 0 — put succeeds, but then root hash computation
    // calls get_value(0) which will fail.
    // Actually, we need put to succeed and get to fail.
    // Put at position 0 stores the value, then compute_root_hash calls get(0).
    // We need get(0) to fail AFTER put(0) succeeded.
    // The FailingStore's put stores in data, then get returns from data
    // (unless fail_on_get matches). So set fail_on_get = Some(0).
    store.fail_on_get = Some(0);

    let result = tree.insert(b"test", &store);
    assert!(
        result.is_err(),
        "insert should fail when hash computation fails"
    );
    assert_eq!(
        tree.count(),
        0,
        "count should be rolled back to 0 after hash failure"
    );

    // Now the tree should still accept inserts (it's not in a broken state)
    store.fail_on_get = None;
    let (_, pos, _) = tree
        .insert(b"retry", &store)
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
        result.is_err(),
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
        .expect("try_insert should succeed after rollback");
    assert!(result.is_some(), "should return Some after rollback");
    assert_eq!(tree.count(), 1);
}

#[test]
fn test_deduplication_with_mixed_duplicates() {
    let (store, root) = make_full_h3_tree();

    // Pass duplicates of multiple positions
    let proof = DenseTreeProof::generate(3, 7, &[4, 5, 4, 6, 5, 4], &store)
        .expect("generate should succeed with duplicates");

    // Should be deduplicated to {4, 5, 6}
    assert_eq!(proof.entries.len(), 3);

    let verified = proof.verify(&root).expect("verify should succeed");
    assert_eq!(verified.len(), 3);
    // Positions should be sorted (from BTreeSet)
    assert_eq!(verified[0].0, 4);
    assert_eq!(verified[1].0, 5);
    assert_eq!(verified[2].0, 6);
}
