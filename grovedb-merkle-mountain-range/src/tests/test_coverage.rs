//! Tests targeting previously-uncovered code paths in the MMR crate.
//!
//! Each test here covers lines that had zero hits in Codecov before this file.
//! See the Codecov file reports for error.rs, mmr_store.rs, mmr.rs, proof.rs.

use crate::{
    Error, MMR, MMRBatch, MMRStoreReadOps, MMRStoreWriteOps, MerkleProof, MmrNode,
    mem_store::MemStore,
};
use grovedb_costs::{CostResult, CostsExt, OperationCost};

/// Create an MmrNode leaf from an integer.
fn leaf(i: u32) -> MmrNode {
    MmrNode::leaf(i.to_le_bytes().to_vec())
}

// =============================================================================
// error.rs: Display arms for 5 previously-uncovered variants
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

// =============================================================================
// mmr_store.rs: store() accessor, IntoIterator, commit error propagation
// =============================================================================

#[test]
fn batch_store_accessor() {
    let store = MemStore::default();
    let batch = MMRBatch::new(&store);
    let _store_ref = batch.store();
}

#[test]
fn batch_into_iterator() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    mmr.push(leaf(10)).unwrap().expect("push");
    mmr.push(leaf(11)).unwrap().expect("push");

    let entries: Vec<(u64, Vec<MmrNode>)> = mmr.batch.into_iter().collect();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0, 0);
    assert_eq!(entries[1].0, 1);
}

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

    mmr.push(leaf(0)).unwrap().expect("push to batch");

    let result = mmr.commit().unwrap();
    assert!(result.is_err(), "commit should surface store write error");
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("write failed"), "error: {}", msg);
}

// =============================================================================
// mmr.rs: is_empty, get_root error paths (single-element + multi-peak)
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

/// A store where every position returns None.
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
fn get_root_single_element_missing_returns_inconsistent() {
    let store = EmptyStore;
    let mmr = MMR::new(1, &store);
    let result = mmr.get_root().unwrap();
    assert_eq!(result, Err(Error::InconsistentStore));
}

#[test]
fn get_root_single_element_store_error_propagates() {
    let store = ErrorStore;
    let mmr = MMR::new(1, &store);
    let result = mmr.get_root().unwrap();
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("read error"), "error: {}", msg);
}

#[test]
fn get_root_multi_peak_store_error_propagates() {
    let store = ErrorStore;
    let mmr = MMR::new(4, &store);
    let result = mmr.get_root().unwrap();
    assert!(result.is_err());
}

// =============================================================================
// proof.rs: MerkleProof::mmr_size() accessor
// =============================================================================

#[test]
fn merkle_proof_accessors() {
    let proof = MerkleProof::new(42, vec![MmrNode::internal([1u8; 32])]);
    assert_eq!(proof.mmr_size(), 42);
    assert_eq!(proof.proof_items().len(), 1);
}

// =============================================================================
// helper.rs: mmr_node_key, MmrKeySize::default
// =============================================================================

#[test]
fn mmr_node_key_returns_big_endian_bytes() {
    use crate::helper::{MmrKeySize, mmr_node_key, mmr_node_key_sized};

    assert_eq!(mmr_node_key(0), [0u8; 8]);
    assert_eq!(mmr_node_key(1), [0, 0, 0, 0, 0, 0, 0, 1]);
    assert_eq!(mmr_node_key(256), [0, 0, 0, 0, 0, 0, 1, 0]);
    assert_eq!(mmr_node_key(u64::MAX), [0xFF; 8]);

    // MmrKeySize::default() should be U64
    assert_eq!(MmrKeySize::default(), MmrKeySize::U64);

    // Sized key for position 0 with U64 has MSB set
    let key = mmr_node_key_sized(0, MmrKeySize::U64).unwrap();
    assert_eq!(key.as_ref(), &0x8000_0000_0000_0000u64.to_be_bytes());
}

// =============================================================================
// node.rs: into_value, PartialEq hash-only semantics
// =============================================================================

#[test]
fn mmr_node_into_value_consumes_both_variants() {
    let leaf_node = MmrNode::leaf(b"payload".to_vec());
    assert_eq!(leaf_node.into_value(), Some(b"payload".to_vec()));

    let internal = MmrNode::internal([0xABu8; 32]);
    assert_eq!(internal.into_value(), None);
}

#[test]
fn mmr_node_equality_compares_hash_only() {
    // Two nodes with the same hash but different value presence are equal
    let leaf_node = MmrNode::leaf(b"data".to_vec());
    let hash = leaf_node.hash();
    let internal_same_hash = MmrNode::internal(hash);

    // Leaf has value, internal doesn't — but PartialEq only checks hash
    assert_eq!(leaf_node, internal_same_hash);
    assert!(leaf_node.value().is_some());
    assert!(internal_same_hash.value().is_none());
}

// =============================================================================
// proof.rs: decode error, verify with unprocessed leaves, peak-shift
// incremental
// =============================================================================

#[test]
fn mmr_tree_proof_decode_corrupted_data_errors() {
    use crate::MmrTreeProof;

    let result = MmrTreeProof::decode_from_slice(&[0xFF, 0xFF, 0xFF]);
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("decode"),
        "error should mention decode: {}",
        msg
    );
}

/// Verify that MerkleProof::calculate_root rejects leaves at positions
/// beyond the MMR peaks ("unprocessed leaves remain" error path in
/// calculate_peaks_hashes).
#[test]
fn verify_rejects_proof_with_unprocessed_leaves() {
    // mmr_size=7 → peaks at [6]. A leaf at position 7 (height 0) is beyond
    // all peaks, so it remains unprocessed after the peak loop.
    let proof = MerkleProof::new(7, vec![]);
    let node = MmrNode::leaf(b"fake".to_vec());
    let result = proof.calculate_root(vec![(7, node)]);
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("unprocessed leaves"),
        "should mention unprocessed leaves: {}",
        msg
    );
}

/// Exercise verify_incremental when peaks have shifted (prev_pos < cur_pos),
/// triggering the early break at proof.rs line 160.
///
/// Setup: 3 leaves → 2 peaks [2, 3]. Add 1 more → 4 leaves → 1 peak [6].
/// Previous peaks [2, 3] vs current peaks [6]: 2 < 6 triggers the break
/// at i=0, reversing all previous peak hashes for bagging.
#[test]
fn verify_incremental_with_peak_shift() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    // Build initial MMR with 3 leaves → mmr_size=4, peaks at [2, 3]
    for i in 0u32..3 {
        mmr.push(leaf(i)).unwrap().expect("push");
    }
    mmr.commit().unwrap().expect("commit");
    let prev_root = mmr.get_root().unwrap().expect("prev root");

    // Previous peak nodes (at positions 2 and 3)
    let peak_2 = mmr
        .batch
        .element_at_position(2)
        .unwrap()
        .expect("read")
        .expect("exists");
    let peak_3 = mmr
        .batch
        .element_at_position(3)
        .unwrap()
        .expect("read")
        .expect("exists");

    // Add 1 incremental leaf → 4 leaves → mmr_size=7, single peak at [6]
    let incremental = vec![leaf(3)];
    mmr.push(incremental[0].clone()).unwrap().expect("push");
    mmr.commit().unwrap().expect("commit");
    let current_root = mmr.get_root().unwrap().expect("current root");

    // Proof items must be in the order that, after verify_incremental's
    // split+reverse+bag_peaks, reconstructs the correct prev_root.
    // With reverse_index=0 the entire list is reversed before bagging.
    // bag_peaks([peak_2, peak_3]) = merge(peak_3, peak_2) = prev_root.
    let proof = MerkleProof::new(mmr.mmr_size, vec![peak_3, peak_2]);

    let valid = proof
        .verify_incremental(current_root, prev_root, incremental)
        .expect("verify_incremental should succeed");
    assert!(
        valid,
        "incremental verification with peak shift should pass"
    );
}

/// Exercise push error path when find_element_at_position encounters a
/// store error during merge (mmr.rs line 94).
///
/// Use mmr_size=1 (pretend one element already exists in store) with
/// ErrorStore. When push triggers a merge, it reads pos 0 from the store,
/// which returns an error.
#[test]
fn push_propagates_store_read_error_during_merge() {
    let store = ErrorStore;
    let mut mmr = MMR::new(1, &store);

    // Push triggers merge with element at position 0 → store read fails
    let result = mmr.push(leaf(1)).unwrap();
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("read error"),
        "should propagate store error: {}",
        msg
    );
}
