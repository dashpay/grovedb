//! Tests targeting previously-uncovered code paths in the MMR crate.
//!
//! Each test here covers lines that had zero hits in Codecov before this file.
//! See the Codecov file reports for error.rs, mmr_store.rs, mmr.rs, proof.rs.

use crate::{
    mem_store::MemStore, Error, MMRBatch, MMRStoreReadOps, MMRStoreWriteOps, MerkleProof, MmrNode,
    MMR,
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
    use crate::helper::{mmr_node_key, mmr_node_key_sized, MmrKeySize};

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
// proof.rs: unprocessed leaves error path
// =============================================================================

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

/// Exercise push error path when find_element_at_position encounters a
/// store error during merge (mmr.rs Err(e) branch).
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

/// Exercise find_element_at_position returning InconsistentStore when
/// the batch returns Ok(None) for a position that should exist (mmr.rs
/// Ok(None) => InconsistentStore branch).
#[test]
fn push_returns_inconsistent_store_when_merge_element_missing() {
    let store = EmptyStore;
    let mut mmr = MMR::new(1, &store);

    // Push triggers merge with element at position 0, but EmptyStore
    // returns Ok(None) → InconsistentStore
    let result = mmr.push(leaf(1)).unwrap();
    assert_eq!(result, Err(Error::InconsistentStore));
}

/// Exercise the `break` in MMRBatch::element_at_position when the
/// requested position falls past a batch entry's range (mmr_store.rs
/// else-break branch).
#[test]
fn batch_element_at_position_break_falls_through_to_store() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    mmr.push(leaf(0)).unwrap().expect("push");
    // batch has entry (0, [leaf(0)]). Position 5 is past this range,
    // triggering the break and falling through to the store.
    let result = mmr
        .batch
        .element_at_position(5)
        .unwrap()
        .expect("read should succeed");
    assert!(result.is_none(), "position 5 should not exist");
}

/// Exercise verify_and_get_root error mapping when calculate_root fails
/// (proof.rs map_err at verify_and_get_root).
#[test]
fn verify_and_get_root_surfaces_calculate_root_error() {
    use crate::MmrTreeProof;

    // mmr_size=7 (4 leaves), proving leaf 0. Empty proof_items means
    // calculate_peak_root runs out of proof items → error propagates
    // through the map_err in verify_and_get_root.
    let proof = MmrTreeProof::new(7, vec![(0, b"val".to_vec())], vec![]);
    let result = proof.verify_and_get_root();
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("calculation failed"),
        "should map calculate_root error: {}",
        msg
    );
}
