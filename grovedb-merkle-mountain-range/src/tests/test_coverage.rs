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

// =============================================================================
// mmr.rs: get_root bills the peak-bagging merges
// =============================================================================

/// `get_root` reads the peaks with cost, but folding them into a single root
/// calls `MmrNode::merge` — a blake3 — once per extra peak. Those merges went
/// uncharged, so a multi-peak root looked free beyond its I/O.
#[test]
fn get_root_charges_one_hash_per_peak_merge() {
    // 1 leaf: mmr_size 1 takes the single-element path, no bagging at all.
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    mmr.push(leaf(0)).unwrap().expect("push");
    let ctx = mmr.get_root();
    ctx.value.expect("root");
    assert_eq!(
        ctx.cost.hash_node_calls, 0,
        "a single-element MMR bags nothing"
    );

    // 2 leaves: one perfect peak, so still nothing to fold.
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    for i in 0..2 {
        mmr.push(leaf(i)).unwrap().expect("push");
    }
    let ctx = mmr.get_root();
    ctx.value.expect("root");
    assert_eq!(ctx.cost.hash_node_calls, 0, "one peak needs no merge");

    // 3 leaves: two peaks, so exactly one merge.
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    for i in 0..3 {
        mmr.push(leaf(i)).unwrap().expect("push");
    }
    let ctx = mmr.get_root();
    ctx.value.expect("root");
    assert_eq!(
        ctx.cost.hash_node_calls, 1,
        "two peaks fold with one blake3 merge"
    );

    // 7 leaves: three peaks (4 + 2 + 1), so two merges.
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    for i in 0..7 {
        mmr.push(leaf(i)).unwrap().expect("push");
    }
    let ctx = mmr.get_root();
    ctx.value.expect("root");
    assert_eq!(
        ctx.cost.hash_node_calls, 2,
        "three peaks fold with two blake3 merges"
    );
}

/// `gen_proof` folds right-hand peaks through the same `bag_peaks` helper the
/// root computation uses, so it performs the same blake3 merges and must bill
/// them. Charging only in `get_root` left proof generation free.
#[test]
fn gen_proof_charges_the_peak_bagging_merges() {
    // 7 leaves gives three peaks (4 + 2 + 1). A proof for the first leaf
    // leaves the two right-hand peaks to be bagged: one merge.
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    let mut positions = Vec::new();
    for i in 0..7 {
        positions.push(mmr.push(leaf(i)).unwrap().expect("push"));
    }

    let ctx = mmr.gen_proof(vec![positions[0]]);
    ctx.value.expect("proof");
    assert_eq!(
        ctx.cost.hash_node_calls, 1,
        "bagging two right-hand peaks is one blake3 merge, got {:?}",
        ctx.cost
    );

    // A single perfect peak has nothing to bag.
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    let mut positions = Vec::new();
    for i in 0..4 {
        positions.push(mmr.push(leaf(i)).unwrap().expect("push"));
    }
    let ctx = mmr.gen_proof(vec![positions[0]]);
    ctx.value.expect("proof");
    assert_eq!(
        ctx.cost.hash_node_calls, 0,
        "one peak means no bagging, got {:?}",
        ctx.cost
    );
}

/// `push` merges once per peak it collapses, and those merges are blake3
/// calls. The sibling reads were billed but the hashes they fed were not.
#[test]
fn push_charges_one_hash_per_peak_collapse() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    // Leaf 0: no collapse.
    let ctx = mmr.push(leaf(0));
    ctx.value.expect("push");
    assert_eq!(ctx.cost.hash_node_calls, 0, "first leaf merges nothing");

    // Leaf 1 collapses one pair.
    let ctx = mmr.push(leaf(1));
    ctx.value.expect("push");
    assert_eq!(ctx.cost.hash_node_calls, 1, "one merge, got {:?}", ctx.cost);

    // Leaf 2: no collapse (new peak).
    let ctx = mmr.push(leaf(2));
    ctx.value.expect("push");
    assert_eq!(ctx.cost.hash_node_calls, 0, "got {:?}", ctx.cost);

    // Leaf 3 collapses twice: the pair, then the two 2-leaf peaks.
    let ctx = mmr.push(leaf(3));
    ctx.value.expect("push");
    assert_eq!(
        ctx.cost.hash_node_calls, 2,
        "two merges, got {:?}",
        ctx.cost
    );
}
