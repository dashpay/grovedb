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
