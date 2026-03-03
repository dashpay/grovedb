//! Additional tests targeting uncovered code paths in the MMR crate.
//!
//! Identified via Codecov line-level reports for:
//! - mmr.rs: error paths in find_element_at_position, get_root, gen_proof
//! - mmr_store.rs: MMRBatch overlay break path, IntoIterator, commit
//! - proof.rs: calculate_root_with_new_leaf merge path, verify_incremental
//!   edge cases, calculate_peaks_hashes error paths
//! - error.rs: Display impls for all error variants

use crate::{
    Error, MMR, MMRBatch, MMRStoreReadOps, MMRStoreWriteOps, MerkleProof, MmrNode, MmrTreeProof,
    helper::{leaf_index_to_mmr_size, leaf_index_to_pos},
    mem_store::MemStore,
    proof::take_while_vec,
};
use grovedb_costs::{CostResult, CostsExt, OperationCost};

/// Create an MmrNode leaf from an integer.
fn leaf(i: u32) -> MmrNode {
    MmrNode::leaf(i.to_le_bytes().to_vec())
}

// =============================================================================
// Error Display coverage
// =============================================================================

#[test]
fn error_display_all_variants() {
    let variants: Vec<(Error, &str)> = vec![
        (Error::GetRootOnEmpty, "empty MMR"),
        (Error::InconsistentStore, "Inconsistent"),
        (Error::StoreError("disk".into()), "disk"),
        (Error::NodeProofsNotSupported, "non-leaf"),
        (Error::GenProofForInvalidLeaves, "invalid leaves"),
        (Error::OperationFailed("timeout".into()), "timeout"),
        (Error::InvalidData("corrupt".into()), "corrupt"),
        (Error::InvalidInput("bad arg".into()), "bad arg"),
        (Error::InvalidProof("mismatch".into()), "mismatch"),
    ];
    for (err, expected_substr) in variants {
        let msg = format!("{}", err);
        assert!(
            msg.contains(expected_substr),
            "Display for {:?} should contain '{}', got: {}",
            err,
            expected_substr,
            msg
        );
    }
}

#[test]
fn error_implements_std_error() {
    let err: Box<dyn std::error::Error> = Box::new(Error::InconsistentStore);
    let _msg = format!("{}", err);
}

// =============================================================================
// MMRBatch coverage (mmr_store.rs)
// =============================================================================

#[test]
fn batch_element_at_position_break_path() {
    // Tests the `break` arm in MMRBatch::element_at_position:
    // When pos > start_pos + elems.len(), we break and fall through to the store.
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    // Push 3 leaves → batch has entries at positions 0, 1, 2 (leaf), 3 (leaf), etc.
    mmr.push(leaf(0)).unwrap().expect("push");
    mmr.push(leaf(1)).unwrap().expect("push");
    mmr.push(leaf(2)).unwrap().expect("push");

    // Commit so batch is flushed to store
    mmr.commit().unwrap().expect("commit");

    // Now push one more leaf (position 4, mmr_size was 4 after 3 leaves)
    mmr.push(leaf(3)).unwrap().expect("push");

    // Read position 0 — it's before the batch entry's start_pos,
    // so the batch iterator hits the `break` path and falls through to the store.
    let result = mmr.batch.element_at_position(0);
    let node = result.value.expect("should succeed").expect("should exist");
    assert_eq!(node.hash(), leaf(0).hash());
}

#[test]
fn batch_into_iterator() {
    // Tests the IntoIterator impl for MMRBatch.
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    mmr.push(leaf(10)).unwrap().expect("push");
    mmr.push(leaf(11)).unwrap().expect("push");

    let entries: Vec<(u64, Vec<MmrNode>)> = mmr.batch.into_iter().collect();
    // Two pushes: first push adds [leaf(10)] at pos 0,
    // second push adds [leaf(11), merge] at pos 1.
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0, 0); // first entry at pos 0
    assert_eq!(entries[1].0, 1); // second entry at pos 1
}

#[test]
fn batch_store_accessor() {
    // Tests the MMRBatch::store() accessor.
    let store = MemStore::default();
    let batch = MMRBatch::new(&store);
    let _store_ref = batch.store();
}

// =============================================================================
// MMRBatch commit error path
// =============================================================================

/// A store that fails on write.
struct FailingWriteStore;

impl MMRStoreReadOps for &FailingWriteStore {
    fn element_at_position(&self, _pos: u64) -> CostResult<Option<MmrNode>, Error> {
        Ok(None).wrap_with_cost(OperationCost::default())
    }
}

impl MMRStoreWriteOps for &FailingWriteStore {
    fn append(&mut self, _pos: u64, _elems: Vec<MmrNode>) -> CostResult<(), Error> {
        Err(Error::StoreError("write failed".into())).wrap_with_cost(OperationCost::default())
    }
}

#[test]
fn batch_commit_surfaces_store_error() {
    let store = FailingWriteStore;
    let mut mmr = MMR::new(0, &store);

    // Push adds to memory batch (no store write yet)
    mmr.push(leaf(0)).unwrap().expect("push to batch");

    // Commit flushes to store → store returns error
    let result = mmr.commit().unwrap();
    assert!(result.is_err(), "commit should surface store write error");
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("write failed"), "error: {}", msg);
}

// =============================================================================
// MMR get_root error paths (mmr.rs)
// =============================================================================

/// A store where position 0 is missing (returns None).
struct EmptyStore;

impl MMRStoreReadOps for &EmptyStore {
    fn element_at_position(&self, _pos: u64) -> CostResult<Option<MmrNode>, Error> {
        Ok(None).wrap_with_cost(OperationCost::default())
    }
}

impl MMRStoreWriteOps for &EmptyStore {
    fn append(&mut self, _pos: u64, _elems: Vec<MmrNode>) -> CostResult<(), Error> {
        Ok(()).wrap_with_cost(OperationCost::default())
    }
}

#[test]
fn get_root_single_element_missing_returns_inconsistent() {
    // mmr_size=1 but store returns None → InconsistentStore error
    let store = EmptyStore;
    let mmr = MMR::new(1, &store);

    let result = mmr.get_root().unwrap();
    assert_eq!(result, Err(Error::InconsistentStore));
}

/// A store that returns an error on read.
struct ErrorStore;

impl MMRStoreReadOps for &ErrorStore {
    fn element_at_position(&self, _pos: u64) -> CostResult<Option<MmrNode>, Error> {
        Err(Error::StoreError("read error".into())).wrap_with_cost(OperationCost::default())
    }
}

impl MMRStoreWriteOps for &ErrorStore {
    fn append(&mut self, _pos: u64, _elems: Vec<MmrNode>) -> CostResult<(), Error> {
        Ok(()).wrap_with_cost(OperationCost::default())
    }
}

#[test]
fn get_root_single_element_store_error_propagates() {
    // mmr_size=1 but store returns error
    let store = ErrorStore;
    let mmr = MMR::new(1, &store);

    let result = mmr.get_root().unwrap();
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("read error"), "error: {}", msg);
}

#[test]
fn get_root_multi_peak_missing_returns_inconsistent() {
    // mmr_size=4 (3 leaves, 2 peaks). Store is empty → InconsistentStore.
    let store = EmptyStore;
    let mmr = MMR::new(4, &store);

    let result = mmr.get_root().unwrap();
    assert_eq!(result, Err(Error::InconsistentStore));
}

#[test]
fn get_root_multi_peak_store_error_propagates() {
    // mmr_size=4 (3 leaves, 2 peaks). Store returns error.
    let store = ErrorStore;
    let mmr = MMR::new(4, &store);

    let result = mmr.get_root().unwrap();
    assert!(result.is_err());
}

// =============================================================================
// MMR is_empty (mmr.rs)
// =============================================================================

#[test]
fn mmr_is_empty() {
    let store = MemStore::default();
    let mmr = MMR::new(0, &store);
    assert!(mmr.is_empty());

    let mut mmr2 = MMR::new(0, &store);
    mmr2.push(leaf(0)).unwrap().expect("push");
    assert!(!mmr2.is_empty());
}

// =============================================================================
// MMR gen_proof edge cases (mmr.rs)
// =============================================================================

#[test]
fn gen_proof_single_element_mmr() {
    // mmr_size=1, prove position 0 → should return empty proof items
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    mmr.push(leaf(0)).unwrap().expect("push");

    let proof = mmr.gen_proof(vec![0]).unwrap().expect("gen_proof");
    // For a single-element MMR, proof items should be empty
    assert!(proof.proof_items().is_empty());

    let root = mmr.get_root().unwrap().expect("root");
    let valid = proof
        .verify(root, vec![(0, leaf(0))])
        .expect("verify should succeed");
    assert!(valid);
}

#[test]
fn gen_proof_deduplicates_positions() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    for i in 0..4u32 {
        mmr.push(leaf(i)).unwrap().expect("push");
    }
    let root = mmr.get_root().unwrap().expect("root");

    // Prove position 0 twice — should be deduplicated internally
    let proof = mmr.gen_proof(vec![0, 0]).unwrap().expect("gen_proof");
    let valid = proof.verify(root, vec![(0, leaf(0))]).expect("verify");
    assert!(valid);
}

#[test]
fn gen_proof_peak_is_proved_leaf() {
    // When the proved position IS the peak position, gen_proof_for_peak returns
    // immediately.
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    // 3 leaves → mmr_size=4, peaks at [2, 3].
    // Position 3 is a leaf AND a peak.
    let positions: Vec<u64> = (0..3u32)
        .map(|i| mmr.push(leaf(i)).unwrap().expect("push"))
        .collect();
    let root = mmr.get_root().unwrap().expect("root");

    let proof = mmr
        .gen_proof(vec![positions[2]]) // position 3 = leaf & peak
        .unwrap()
        .expect("gen_proof");
    let valid = proof
        .verify(root, vec![(positions[2], leaf(2))])
        .expect("verify");
    assert!(valid);
}

// =============================================================================
// MerkleProof calculate_root_with_new_leaf (proof.rs)
// =============================================================================

#[test]
fn calculate_root_with_new_leaf_merge_path() {
    // Tests the `else` branch where new_pos does NOT trigger a peak merge.
    // With 3 leaves (mmr_size=4), adding leaf 3 creates pos=4 (a new leaf, no merge).
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    let positions: Vec<u64> = (0..3u32)
        .map(|i| mmr.push(leaf(i)).unwrap().expect("push"))
        .collect();

    let last_pos = positions[2];
    let proof = mmr.gen_proof(vec![last_pos]).unwrap().expect("gen_proof");

    // Add leaf 3
    let new_pos = mmr.push(leaf(3)).unwrap().expect("push new");
    let root = mmr.get_root().unwrap().expect("root");

    let new_mmr_size = leaf_index_to_mmr_size(3);
    let calculated = proof
        .calculate_root_with_new_leaf(vec![(last_pos, leaf(2))], new_pos, leaf(3), new_mmr_size)
        .expect("calculate_root_with_new_leaf");

    assert_eq!(calculated, root);
}

#[test]
fn calculate_root_with_new_leaf_peak_merge_path() {
    // Tests the `if next_height > pos_height` branch where the new leaf triggers
    // merging into an existing peak. With 1 leaf (mmr_size=1), adding leaf 1
    // creates a merge (new peak at pos 2).
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    let pos0 = mmr.push(leaf(0)).unwrap().expect("push");
    let proof = mmr.gen_proof(vec![pos0]).unwrap().expect("gen_proof");

    let new_pos = mmr.push(leaf(1)).unwrap().expect("push new");
    let root = mmr.get_root().unwrap().expect("root");

    let new_mmr_size = leaf_index_to_mmr_size(1);
    let calculated = proof
        .calculate_root_with_new_leaf(vec![(pos0, leaf(0))], new_pos, leaf(1), new_mmr_size)
        .expect("calculate_root_with_new_leaf");

    assert_eq!(calculated, root);
}

#[test]
fn calculate_root_with_new_leaf_larger_mmr() {
    // Test with a larger MMR (7 leaves → 8).
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    let positions: Vec<u64> = (0..7u32)
        .map(|i| mmr.push(leaf(i)).unwrap().expect("push"))
        .collect();

    let last_pos = positions[6];
    let proof = mmr.gen_proof(vec![last_pos]).unwrap().expect("gen_proof");

    let new_pos = mmr.push(leaf(7)).unwrap().expect("push new");
    let root = mmr.get_root().unwrap().expect("root");

    let new_mmr_size = leaf_index_to_mmr_size(7);
    let calculated = proof
        .calculate_root_with_new_leaf(vec![(last_pos, leaf(6))], new_pos, leaf(7), new_mmr_size)
        .expect("calculate_root_with_new_leaf");

    assert_eq!(calculated, root);
}

// =============================================================================
// MerkleProof verify_incremental edge cases (proof.rs)
// =============================================================================

#[test]
fn verify_incremental_with_multiple_peaks_and_reversal() {
    // Tests the reverse_index logic in verify_incremental where prev_pos < cur_pos.
    // Use gen_proof to get proper proof items for the incremental leaves.
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    // Build 8 leaves → mmr_size=15, single peak [14]
    for i in 0u32..8 {
        mmr.push(leaf(i)).unwrap().expect("push");
    }
    mmr.commit().unwrap().expect("commit");
    let prev_root = mmr.get_root().unwrap().expect("prev root");

    // Add 3 incremental leaves → 11 total → mmr_size=19, peaks [14, 17, 18]
    // Previous: 1 peak, current: 3 peaks → exercises reversal logic
    let incremental: Vec<MmrNode> = (8u32..11).map(leaf).collect();
    let new_positions: Vec<u64> = (8u32..11)
        .map(|i| mmr.push(leaf(i)).unwrap().expect("push"))
        .collect();
    mmr.commit().unwrap().expect("commit");
    let current_root = mmr.get_root().unwrap().expect("current root");

    // Generate proof for the incremental leaf positions
    let proof = mmr.gen_proof(new_positions).unwrap().expect("gen_proof");

    let valid = proof
        .verify_incremental(current_root, prev_root, incremental)
        .expect("verify_incremental");
    assert!(valid);
}

// =============================================================================
// MerkleProof calculate_root edge cases (proof.rs)
// =============================================================================

#[test]
fn calculate_root_single_leaf_mmr() {
    // mmr_size=1, single leaf → calculate_peaks_hashes takes the fast path.
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    mmr.push(leaf(42)).unwrap().expect("push");
    mmr.commit().unwrap().expect("commit");

    let root = mmr.get_root().unwrap().expect("root");
    let proof = mmr.gen_proof(vec![0]).unwrap().expect("gen_proof");

    let calculated = proof
        .calculate_root(vec![(0, leaf(42))])
        .expect("calculate_root");
    assert_eq!(calculated, root);
}

#[test]
fn merkle_proof_accessors() {
    let proof = MerkleProof::new(42, vec![MmrNode::internal([1u8; 32])]);
    assert_eq!(proof.mmr_size(), 42);
    assert_eq!(proof.proof_items().len(), 1);
}

// =============================================================================
// MmrTreeProof accessors and edge cases (proof.rs)
// =============================================================================

#[test]
fn mmr_tree_proof_accessors() {
    let proof = MmrTreeProof::new(15, vec![(0, b"data".to_vec())], vec![[0xAAu8; 32]]);
    assert_eq!(proof.mmr_size(), 15);
    assert_eq!(proof.leaves().len(), 1);
    assert_eq!(proof.proof_items().len(), 1);
}

#[test]
fn mmr_tree_proof_encode_decode_empty_proof_items() {
    let proof = MmrTreeProof::new(1, vec![(0, b"leaf".to_vec())], vec![]);
    let bytes = proof.encode_to_vec().expect("encode");
    let decoded = MmrTreeProof::decode_from_slice(&bytes).expect("decode");
    assert_eq!(decoded.mmr_size(), 1);
    assert_eq!(decoded.leaves().len(), 1);
    assert!(decoded.proof_items().is_empty());
}

#[test]
fn mmr_tree_proof_decode_garbage_fails() {
    let result = MmrTreeProof::decode_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("decode"), "error: {}", msg);
}

#[test]
fn mmr_tree_proof_verify_and_get_root_success() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    for i in 0u32..7 {
        mmr.push(leaf(i)).unwrap().expect("push");
    }
    mmr.commit().unwrap().expect("commit");

    let mmr_size = mmr.mmr_size;
    let root = mmr.get_root().unwrap().expect("root").hash();

    let get_node = |pos: u64| -> crate::Result<Option<MmrNode>> {
        (&store)
            .element_at_position(pos)
            .value
            .map_err(|e| Error::StoreError(format!("{}", e)))
    };

    let proof = MmrTreeProof::generate(mmr_size, &[0, 3, 6], get_node).expect("generate");

    let (computed_root, verified) = proof.verify_and_get_root().expect("verify_and_get_root");
    assert_eq!(computed_root, root);
    assert_eq!(verified.len(), 3);
}

#[test]
fn mmr_tree_proof_verify_and_get_root_deduplicates() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    for i in 0u32..4 {
        mmr.push(leaf(i)).unwrap().expect("push");
    }
    mmr.commit().unwrap().expect("commit");

    let mmr_size = mmr.mmr_size;
    let root = mmr.get_root().unwrap().expect("root").hash();

    let get_node = |pos: u64| -> crate::Result<Option<MmrNode>> {
        (&store)
            .element_at_position(pos)
            .value
            .map_err(|e| Error::StoreError(format!("{}", e)))
    };

    let proof = MmrTreeProof::generate(mmr_size, &[1], get_node).expect("generate");

    // Manually construct with duplicate leaves
    let dup = MmrTreeProof::new(
        proof.mmr_size(),
        vec![
            (1, 1u32.to_le_bytes().to_vec()),
            (1, 1u32.to_le_bytes().to_vec()),
        ],
        proof.proof_items().to_vec(),
    );

    let (computed_root, verified) = dup.verify_and_get_root().expect("verify_and_get_root");
    assert_eq!(computed_root, root);
    assert_eq!(verified.len(), 1, "should deduplicate");
}

// =============================================================================
// take_while_vec (proof.rs)
// =============================================================================

#[test]
fn take_while_vec_drains_all() {
    let mut v = vec![1, 2, 3, 4];
    let taken = take_while_vec(&mut v, |x| *x <= 10);
    assert_eq!(taken, vec![1, 2, 3, 4]);
    assert!(v.is_empty());
}

#[test]
fn take_while_vec_drains_partial() {
    let mut v = vec![1, 2, 5, 10];
    let taken = take_while_vec(&mut v, |x| *x < 5);
    assert_eq!(taken, vec![1, 2]);
    assert_eq!(v, vec![5, 10]);
}

#[test]
fn take_while_vec_drains_none() {
    let mut v = vec![10, 20, 30];
    let taken = take_while_vec(&mut v, |x| *x < 5);
    assert!(taken.is_empty());
    assert_eq!(v, vec![10, 20, 30]);
}

#[test]
fn take_while_vec_empty_input() {
    let mut v: Vec<i32> = vec![];
    let taken = take_while_vec(&mut v, |_| true);
    assert!(taken.is_empty());
}

// =============================================================================
// bag_peaks edge cases (mmr.rs)
// =============================================================================

#[test]
fn bag_peaks_empty_returns_none() {
    let result = crate::mmr::bag_peaks(vec![]).expect("should not error");
    assert!(result.is_none());
}

#[test]
fn bag_peaks_single_returns_that_peak() {
    let peak = MmrNode::internal([0xABu8; 32]);
    let result = crate::mmr::bag_peaks(vec![peak.clone()])
        .expect("should not error")
        .expect("should return some");
    assert_eq!(result.hash(), peak.hash());
}

#[test]
fn bag_peaks_two_merges_right_to_left() {
    let left = MmrNode::internal([0xAAu8; 32]);
    let right = MmrNode::internal([0xBBu8; 32]);

    let result = crate::mmr::bag_peaks(vec![left.clone(), right.clone()])
        .expect("should not error")
        .expect("should return some");

    // bag_peaks merges right into left: merge(right, left)
    let expected = MmrNode::merge(&right, &left);
    assert_eq!(result.hash(), expected.hash());
}

// =============================================================================
// MmrNode into_value (node.rs — minor gap)
// =============================================================================

#[test]
fn mmr_node_into_value_leaf() {
    let node = MmrNode::leaf(b"payload".to_vec());
    let value = node.into_value().expect("leaf should have value");
    assert_eq!(value, b"payload");
}

#[test]
fn mmr_node_into_value_internal() {
    let node = MmrNode::internal([0u8; 32]);
    assert!(node.into_value().is_none());
}

// =============================================================================
// MmrNode equality uses hash only (node.rs)
// =============================================================================

#[test]
fn mmr_node_eq_uses_hash_only() {
    // Two nodes with the same hash but different value presence should be equal
    let leaf_node = MmrNode::leaf(b"data".to_vec());
    let internal_node = MmrNode::internal(leaf_node.hash());
    assert_eq!(leaf_node, internal_node);
}

// =============================================================================
// Large MMR proof generation and verification
// =============================================================================

#[test]
fn large_mmr_proof_and_verify() {
    // 100 leaves → exercises multi-level proof with multiple peaks
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    let positions: Vec<u64> = (0..100u32)
        .map(|i| mmr.push(leaf(i)).unwrap().expect("push"))
        .collect();
    let root = mmr.get_root().unwrap().expect("root");

    // Prove scattered positions including first, middle, and last
    let proof_positions = vec![positions[0], positions[49], positions[99]];
    let proof = mmr.gen_proof(proof_positions).unwrap().expect("gen_proof");

    let valid = proof
        .verify(
            root,
            vec![
                (positions[0], leaf(0)),
                (positions[49], leaf(49)),
                (positions[99], leaf(99)),
            ],
        )
        .expect("verify");
    assert!(valid);
}

#[test]
fn gen_proof_all_leaves_single_peak() {
    // 8 leaves → single peak (perfect binary tree). Prove all leaves.
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    let positions: Vec<u64> = (0..8u32)
        .map(|i| mmr.push(leaf(i)).unwrap().expect("push"))
        .collect();
    let root = mmr.get_root().unwrap().expect("root");

    let proof = mmr
        .gen_proof(positions.clone())
        .unwrap()
        .expect("gen_proof");

    let leaves: Vec<(u64, MmrNode)> = positions
        .iter()
        .enumerate()
        .map(|(i, &pos)| (pos, leaf(i as u32)))
        .collect();

    let valid = proof.verify(root, leaves).expect("verify");
    assert!(valid);
}

// =============================================================================
// MMR commit and reopen
// =============================================================================

#[test]
fn mmr_commit_and_reopen_preserves_root() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    for i in 0..15u32 {
        mmr.push(leaf(i)).unwrap().expect("push");
    }

    let root_before = mmr.get_root().unwrap().expect("root before commit");
    let mmr_size = mmr.mmr_size;
    mmr.commit().unwrap().expect("commit");

    // Reopen from the same store
    let mmr2 = MMR::new(mmr_size, &store);
    let root_after = mmr2.get_root().unwrap().expect("root after reopen");
    assert_eq!(root_before.hash(), root_after.hash());
}

// =============================================================================
// MmrTreeProof generate with storage error
// =============================================================================

#[test]
fn mmr_tree_proof_generate_leaf_at_only_position() {
    // mmr_size=1 (single leaf), prove leaf index 0
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    mmr.push(leaf(99)).unwrap().expect("push");
    mmr.commit().unwrap().expect("commit");

    let get_node = |pos: u64| -> crate::Result<Option<MmrNode>> {
        (&store)
            .element_at_position(pos)
            .value
            .map_err(|e| Error::StoreError(format!("{}", e)))
    };

    let proof = MmrTreeProof::generate(1, &[0], get_node).expect("generate");
    let root = mmr.get_root().unwrap().expect("root").hash();
    let verified = proof.verify(&root).expect("verify");
    assert_eq!(verified.len(), 1);
    assert_eq!(verified[0].1, 99u32.to_le_bytes().to_vec());
}

// =============================================================================
// MerkleProof verify with wrong leaves returns false (not error)
// =============================================================================

#[test]
fn merkle_proof_verify_wrong_leaf_returns_false() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    for i in 0..4u32 {
        mmr.push(leaf(i)).unwrap().expect("push");
    }
    let root = mmr.get_root().unwrap().expect("root");

    let proof = mmr.gen_proof(vec![0]).unwrap().expect("gen_proof");

    // Verify with wrong leaf value → should return Ok(false), not error
    let valid = proof
        .verify(root, vec![(0, leaf(999))])
        .expect("verify should succeed, returning false");
    assert!(!valid);
}

// =============================================================================
// helper.rs minor gaps
// =============================================================================

#[test]
fn leaf_index_to_pos_first_few() {
    assert_eq!(leaf_index_to_pos(0), 0);
    assert_eq!(leaf_index_to_pos(1), 1);
    assert_eq!(leaf_index_to_pos(2), 3);
    assert_eq!(leaf_index_to_pos(3), 4);
    assert_eq!(leaf_index_to_pos(4), 7);
    assert_eq!(leaf_index_to_pos(5), 8);
    assert_eq!(leaf_index_to_pos(6), 10);
    assert_eq!(leaf_index_to_pos(7), 11);
}

#[test]
fn leaf_index_to_mmr_size_validates() {
    // 1 leaf → size 1, 2 leaves → size 3, 3 → 4, 4 → 7
    assert_eq!(leaf_index_to_mmr_size(0), 1);
    assert_eq!(leaf_index_to_mmr_size(1), 3);
    assert_eq!(leaf_index_to_mmr_size(2), 4);
    assert_eq!(leaf_index_to_mmr_size(3), 7);
    assert_eq!(leaf_index_to_mmr_size(4), 8);
    assert_eq!(leaf_index_to_mmr_size(7), 15);
}
