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
// mmr.rs: versioned hash charges for the internal blake3 merges
// =============================================================================

/// `get_root` bags peaks with one blake3 merge per extra peak. v0 (shipped)
/// charges none of them; v1 charges `peaks - 1`. The root itself is identical
/// under both, so only the cost may differ.
#[test]
fn get_root_peak_bagging_charge_is_versioned() {
    use grovedb_version::version::{v1::GROVE_V1, v4::GROVE_V4};

    // (leaves, expected v1 merges): 1 and 2 leaves leave a single peak (or
    // take the single-element path), 3 leaves give two peaks, 7 give three.
    for (leaves, expected) in [(1u32, 0u32), (2, 0), (3, 1), (7, 2)] {
        let store = MemStore::default();
        let mut mmr = MMR::new(0, &store);
        for i in 0..leaves {
            mmr.push(leaf(i)).unwrap().expect("push");
        }

        let v0 = mmr.get_root_with_version(&GROVE_V1);
        let v0_root = v0.value.expect("root");
        assert_eq!(
            v0.cost.hash_node_calls, 0,
            "v0 charges no bagging merges ({} leaves)",
            leaves
        );

        let v1 = mmr.get_root_with_version(&GROVE_V4);
        let v1_root = v1.value.expect("root");
        assert_eq!(
            v1.cost.hash_node_calls, expected,
            "v1 charges one hash per fold ({} leaves), got {:?}",
            leaves, v1.cost
        );

        assert_eq!(
            v0_root, v1_root,
            "the root must not depend on the cost version ({} leaves)",
            leaves
        );
    }

    // The un-suffixed entry point must keep the shipped accounting.
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    for i in 0..7 {
        mmr.push(leaf(i)).unwrap().expect("push");
    }
    let ctx = mmr.get_root();
    ctx.value.expect("root");
    assert_eq!(
        ctx.cost.hash_node_calls, 0,
        "bare get_root must stay on v0, got {:?}",
        ctx.cost
    );
}

/// `push` merges once per peak it collapses. v0 billed the sibling reads
/// those merges consume but not the merges; v1 charges them.
#[test]
fn push_peak_collapse_charge_is_versioned() {
    use grovedb_version::version::{v1::GROVE_V1, v4::GROVE_V4};

    // Collapses for the first four leaves: 0, 1, 0, 2.
    let expected = [0u32, 1, 0, 2];

    let store_v0 = MemStore::default();
    let mut mmr_v0 = MMR::new(0, &store_v0);
    let store_v1 = MemStore::default();
    let mut mmr_v1 = MMR::new(0, &store_v1);

    for (i, exp) in expected.iter().enumerate() {
        let c0 = mmr_v0.push_with_version(leaf(i as u32), &GROVE_V1);
        c0.value.expect("push");
        assert_eq!(
            c0.cost.hash_node_calls, 0,
            "v0 charges no merges (leaf {})",
            i
        );

        let c1 = mmr_v1.push_with_version(leaf(i as u32), &GROVE_V4);
        c1.value.expect("push");
        assert_eq!(
            c1.cost.hash_node_calls, *exp,
            "v1 charges one hash per collapse (leaf {}), got {:?}",
            i, c1.cost
        );
    }

    // Same MMR either way.
    assert_eq!(
        mmr_v0.get_root().unwrap().expect("root"),
        mmr_v1.get_root().unwrap().expect("root"),
        "the MMR must not depend on the cost version"
    );

    // The un-suffixed entry point must keep the shipped accounting.
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    mmr.push(leaf(0)).unwrap().expect("push");
    let ctx = mmr.push(leaf(1));
    ctx.value.expect("push");
    assert_eq!(
        ctx.cost.hash_node_calls, 0,
        "bare push must stay on v0, got {:?}",
        ctx.cost
    );
}

/// `gen_proof` folds right-hand peaks through the same `bag_peaks` helper,
/// so it carries the same versioned charge.
#[test]
fn gen_proof_peak_bagging_charge_is_versioned() {
    use grovedb_version::version::{v1::GROVE_V1, v4::GROVE_V4};

    // 7 leaves gives three peaks (4 + 2 + 1); a proof for the first leaf
    // leaves the two right-hand peaks to bag: one merge.
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    let mut positions = Vec::new();
    for i in 0..7 {
        positions.push(mmr.push(leaf(i)).unwrap().expect("push"));
    }

    let v0 = mmr.gen_proof_with_version(vec![positions[0]], &GROVE_V1);
    let v0_proof = v0.value.expect("proof");
    assert_eq!(v0.cost.hash_node_calls, 0, "v0 charges no bagging merges");

    let v1 = mmr.gen_proof_with_version(vec![positions[0]], &GROVE_V4);
    let v1_proof = v1.value.expect("proof");
    assert_eq!(
        v1.cost.hash_node_calls, 1,
        "v1 charges the one fold, got {:?}",
        v1.cost
    );

    assert_eq!(
        v0_proof.proof_items(),
        v1_proof.proof_items(),
        "the proof must not depend on the cost version"
    );

    // A single perfect peak has nothing to bag under either version.
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    let mut positions = Vec::new();
    for i in 0..4 {
        positions.push(mmr.push(leaf(i)).unwrap().expect("push"));
    }
    let ctx = mmr.gen_proof_with_version(vec![positions[0]], &GROVE_V4);
    ctx.value.expect("proof");
    assert_eq!(ctx.cost.hash_node_calls, 0, "one peak means no bagging");
}

/// An unknown version must be rejected rather than silently falling back to
/// one of the implemented charges.
#[test]
fn mmr_cost_dispatch_rejects_unknown_version() {
    use grovedb_version::version::{v4::GROVE_V4, GroveVersion};

    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    for i in 0..3 {
        mmr.push(leaf(i)).unwrap().expect("push");
    }

    let mut bad: GroveVersion = GROVE_V4.clone();
    bad.mmr_versions.cost.get_root = 99;
    assert!(
        matches!(
            mmr.get_root_with_version(&bad).unwrap(),
            Err(Error::VersionError(_))
        ),
        "an unknown get_root charge version must error"
    );

    let mut bad: GroveVersion = GROVE_V4.clone();
    bad.mmr_versions.cost.push = 99;
    assert!(
        matches!(
            mmr.push_with_version(leaf(9), &bad).unwrap(),
            Err(Error::VersionError(_))
        ),
        "an unknown push charge version must error"
    );

    let mut bad: GroveVersion = GROVE_V4.clone();
    bad.mmr_versions.cost.gen_proof = 99;
    assert!(
        matches!(
            mmr.gen_proof_with_version(vec![0], &bad).unwrap(),
            Err(Error::VersionError(_))
        ),
        "an unknown gen_proof charge version must error"
    );
}
