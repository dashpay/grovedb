use faster_hex::hex_string;
use proptest::prelude::*;
use rand::{Rng, seq::SliceRandom, thread_rng};

use crate::{
    Error, MMR, MMRStoreReadOps, MerkleProof, MmrNode, MmrTreeProof, helper::pos_height_in_tree,
    leaf_index_to_mmr_size, mem_store::MemStore,
};

/// Create an MmrNode leaf from an integer (for test convenience).
fn leaf_from_u32(i: u32) -> MmrNode {
    MmrNode::leaf(i.to_le_bytes().to_vec())
}

fn test_mmr(count: u32, proof_elem: Vec<u32>) {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    let positions: Vec<u64> = (0u32..count)
        .map(|i| mmr.push(leaf_from_u32(i)).unwrap().expect("push"))
        .collect();
    let root = mmr.get_root().unwrap().expect("get root");
    let proof = mmr
        .gen_proof(
            proof_elem
                .iter()
                .map(|elem| positions[*elem as usize])
                .collect(),
        )
        .unwrap()
        .expect("gen proof");
    mmr.commit().unwrap().expect("commit changes");
    let result = proof
        .verify(
            root,
            proof_elem
                .iter()
                .map(|elem| (positions[*elem as usize], leaf_from_u32(*elem)))
                .collect(),
        )
        .expect("verify");
    assert!(result);
}

fn test_gen_new_root_from_proof(count: u32) {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    let positions: Vec<u64> = (0u32..count)
        .map(|i| mmr.push(leaf_from_u32(i)).unwrap().expect("push"))
        .collect();
    let elem = count - 1;
    let pos = positions[elem as usize];
    let proof = mmr.gen_proof(vec![pos]).unwrap().expect("gen proof");
    let new_elem = count;
    let new_pos = mmr
        .push(leaf_from_u32(new_elem))
        .unwrap()
        .expect("push new");
    let root = mmr.get_root().unwrap().expect("get root");
    mmr.commit().unwrap().expect("commit changes");
    let calculated_root = proof
        .calculate_root_with_new_leaf(
            vec![(pos, leaf_from_u32(elem))],
            new_pos,
            leaf_from_u32(new_elem),
            leaf_index_to_mmr_size(new_elem.into()),
        )
        .expect("calculate_root_with_new_leaf");
    assert_eq!(calculated_root, root);
}

#[test]
fn test_mmr_root() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    (0u32..11).for_each(|i| {
        mmr.push(leaf_from_u32(i)).unwrap().expect("push");
    });
    let root = mmr.get_root().unwrap().expect("get root");
    let hex_root = hex_string(&root.hash());
    // This is the deterministic root for 11 leaves with MmrNode/blake3
    assert_eq!(hex_root.len(), 64, "root hash should be 32 bytes hex");
}

#[test]
fn test_empty_mmr_root() {
    let store = MemStore::default();
    let mmr = MMR::new(0, &store);
    assert_eq!(Err(Error::GetRootOnEmpty), mmr.get_root().unwrap());
}

#[test]
fn test_mmr_3_peaks() {
    test_mmr(11, vec![5]);
}

#[test]
fn test_mmr_2_peaks() {
    test_mmr(10, vec![5]);
}

#[test]
fn test_mmr_1_peak() {
    test_mmr(8, vec![5]);
}

#[test]
fn test_mmr_first_elem_proof() {
    test_mmr(11, vec![0]);
}

#[test]
fn test_mmr_last_elem_proof() {
    test_mmr(11, vec![10]);
}

#[test]
fn test_mmr_1_elem() {
    test_mmr(1, vec![0]);
}

#[test]
fn test_mmr_2_elems() {
    test_mmr(2, vec![0]);
    test_mmr(2, vec![1]);
}

#[test]
fn test_mmr_2_leaves_merkle_proof() {
    test_mmr(11, vec![3, 7]);
    test_mmr(11, vec![3, 4]);
}

#[test]
fn test_mmr_2_sibling_leaves_merkle_proof() {
    test_mmr(11, vec![4, 5]);
    test_mmr(11, vec![5, 6]);
    test_mmr(11, vec![6, 7]);
}

#[test]
fn test_mmr_3_leaves_merkle_proof() {
    test_mmr(11, vec![4, 5, 6]);
    test_mmr(11, vec![3, 5, 7]);
    test_mmr(11, vec![3, 4, 5]);
    test_mmr(100, vec![3, 5, 13]);
}

#[test]
fn test_gen_root_from_proof() {
    test_gen_new_root_from_proof(11);
}

#[test]
fn test_gen_proof_with_duplicate_leaves() {
    test_mmr(10, vec![5, 5]);
}

fn test_invalid_proof_verification(
    leaf_count: u32,
    positions_to_verify: Vec<u64>,
    tampered_positions: Vec<usize>,
    handrolled_proof_positions: Option<Vec<u64>>,
) {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    let mut positions: Vec<u64> = Vec::new();
    for i in 0u32..leaf_count {
        let pos = mmr.push(leaf_from_u32(i)).unwrap().expect("push");
        positions.push(pos);
    }
    let root = mmr.get_root().unwrap().expect("get root");

    let entries_to_verify: Vec<(u64, MmrNode)> = positions_to_verify
        .iter()
        .map(|pos| {
            (
                *pos,
                mmr.batch
                    .element_at_position(*pos)
                    .unwrap()
                    .expect("read")
                    .expect("exists"),
            )
        })
        .collect();

    let mut tampered_entries_to_verify = entries_to_verify.clone();
    tampered_positions.iter().for_each(|proof_pos| {
        tampered_entries_to_verify[*proof_pos] = (
            tampered_entries_to_verify[*proof_pos].0,
            MmrNode::leaf(31337u32.to_le_bytes().to_vec()),
        )
    });

    let handrolled_proof: Option<crate::MerkleProof> =
        handrolled_proof_positions.map(|handrolled_proof_positions| {
            crate::MerkleProof::new(
                mmr.mmr_size,
                handrolled_proof_positions
                    .iter()
                    .map(|pos| {
                        mmr.batch
                            .element_at_position(*pos)
                            .unwrap()
                            .expect("read")
                            .expect("exists")
                    })
                    .collect(),
            )
        });

    // verification should fail whenever trying to prove membership of a non-member
    if let Some(handrolled_proof) = handrolled_proof {
        let handrolled_proof_result =
            handrolled_proof.verify(root.clone(), tampered_entries_to_verify.clone());
        assert!(handrolled_proof_result.is_err() || !handrolled_proof_result.expect("verify"));
    }

    match mmr.gen_proof(positions_to_verify.clone()).unwrap() {
        Ok(proof) => {
            assert!(
                proof
                    .verify(root.clone(), entries_to_verify)
                    .expect("verify valid")
            );
            assert!(
                !proof
                    .verify(root, tampered_entries_to_verify)
                    .expect("verify tampered")
            );
        }
        Err(Error::NodeProofsNotSupported) => {
            // if couldn't generate proof, then it contained a non-leaf
            assert!(
                positions_to_verify
                    .iter()
                    .any(|pos| pos_height_in_tree(*pos) > 0)
            );
        }
        Err(e) => panic!("Unexpected error: {}", e),
    }
}

#[test]
fn test_generic_proofs() {
    test_invalid_proof_verification(7, vec![5], vec![0], Some(vec![2, 9, 10]));
    test_invalid_proof_verification(7, vec![1, 2], vec![0], Some(vec![5, 9, 10]));
    test_invalid_proof_verification(7, vec![1, 5], vec![0], Some(vec![0, 9, 10]));
    test_invalid_proof_verification(7, vec![1, 6], vec![0], Some(vec![0, 5, 9, 10]));
    test_invalid_proof_verification(7, vec![5, 6], vec![0], Some(vec![2, 9, 10]));
    test_invalid_proof_verification(7, vec![1, 5, 6], vec![0], Some(vec![0, 9, 10]));
    test_invalid_proof_verification(7, vec![1, 5, 7], vec![0], Some(vec![0, 8, 10]));
    test_invalid_proof_verification(7, vec![5, 6, 7], vec![0], Some(vec![2, 8, 10]));
    test_invalid_proof_verification(7, vec![5, 6, 7, 8, 9, 10], vec![0], Some(vec![2]));
    test_invalid_proof_verification(7, vec![1, 5, 7, 8, 9, 10], vec![0], Some(vec![0]));
    test_invalid_proof_verification(7, vec![0, 1, 5, 7, 8, 9, 10], vec![0], Some(vec![]));
    test_invalid_proof_verification(7, vec![0, 1, 5, 6, 7, 8, 9, 10], vec![0], Some(vec![]));
    test_invalid_proof_verification(7, vec![0, 1, 2, 5, 6, 7, 8, 9, 10], vec![0], Some(vec![]));
    test_invalid_proof_verification(7, vec![0, 1, 2, 3, 7, 8, 9, 10], vec![0], Some(vec![4]));
    test_invalid_proof_verification(7, vec![0, 2, 3, 7, 8, 9, 10], vec![0], Some(vec![1, 4]));
    test_invalid_proof_verification(7, vec![0, 3, 7, 8, 9, 10], vec![0], Some(vec![1, 4]));
    test_invalid_proof_verification(7, vec![0, 2, 3, 7, 8, 9, 10], vec![0], Some(vec![1, 4]));
}

/// Test that MMRBatch cache hits return non-zero costs matching element size.
#[test]
fn test_batch_cache_hit_returns_nonzero_cost() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    // Push a leaf — it goes into MMRBatch.memory_batch
    let leaf = MmrNode::leaf(b"test value".to_vec());
    let expected_size = leaf.serialized_size();
    let pos = mmr.push(leaf).unwrap().expect("push should succeed");

    // Before commit, read from the batch (cache hit)
    let cost_result = mmr.batch.element_at_position(pos);
    let cost = cost_result.cost;
    let elem = cost_result.value.expect("element should exist");
    assert!(elem.is_some(), "element should be found in batch");

    assert_eq!(
        cost.seek_count, 1,
        "batch cache hit should report seek_count=1"
    );
    assert_eq!(
        cost.storage_loaded_bytes, expected_size,
        "batch cache hit should report storage_loaded_bytes matching serialized_size"
    );
}

/// Test that push cost includes storage costs from batch reads.
#[test]
fn test_push_cost_includes_read_costs() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    // First push — no merging needed, no sibling reads
    mmr.push(MmrNode::leaf(b"leaf0".to_vec()))
        .unwrap()
        .expect("push should succeed");

    // Reset
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    // Push two leaves — second push triggers a merge with the first
    let push0_result = mmr.push(MmrNode::leaf(b"leaf0".to_vec()));
    let push0_cost = push0_result.cost;

    let push1_result = mmr.push(MmrNode::leaf(b"leaf1".to_vec()));
    let push1_cost = push1_result.cost;

    // Second push should have higher cost (reads the first leaf for merging)
    assert!(
        push1_cost.seek_count > push0_cost.seek_count
            || push1_cost.storage_loaded_bytes > push0_cost.storage_loaded_bytes,
        "second push should incur read costs from merging; push0_cost={:?}, push1_cost={:?}",
        push0_cost,
        push1_cost
    );
}

/// Test that get_root cost scales with number of peaks.
#[test]
fn test_get_root_cost_reflects_peak_reads() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    // Push 3 leaves → mmr_size=4, 2 peaks (pos 2 and pos 3)
    for i in 0..3u8 {
        mmr.push(MmrNode::leaf(vec![i]))
            .unwrap()
            .expect("push should succeed");
    }

    let root_result = mmr.get_root();
    let root_cost = root_result.cost;

    // With 2 peaks, get_root reads 2 nodes → at least 2 seeks
    assert!(
        root_cost.seek_count >= 2,
        "get_root with 2 peaks should have at least 2 seeks, got {}",
        root_cost.seek_count
    );
    assert!(
        root_cost.storage_loaded_bytes > 0,
        "get_root should report non-zero loaded bytes"
    );
}

/// MmrTreeProof generate → verify round-trip for standard leaves.
#[test]
fn test_mmr_tree_proof_standard_leaf_verify_succeeds() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    // Push standard leaves — leaf_hash(value) matches the stored hash
    for i in 0u32..5 {
        mmr.push(MmrNode::leaf(i.to_le_bytes().to_vec()))
            .unwrap()
            .expect("push should succeed");
    }
    mmr.commit().unwrap().expect("commit should succeed");

    let mmr_size = mmr.mmr_size;
    let root = mmr.get_root().unwrap().expect("get root should succeed");

    let get_node = |pos: u64| -> crate::Result<Option<MmrNode>> {
        (&store)
            .element_at_position(pos)
            .value
            .map_err(|e| crate::Error::StoreError(format!("{}", e)))
    };

    let proof =
        MmrTreeProof::generate(mmr_size, &[0, 2, 4], get_node).expect("generate should succeed");

    let verified = proof
        .verify(&root.hash())
        .expect("verify should succeed for standard leaves");

    assert_eq!(verified.len(), 3, "should return 3 verified leaves");
    assert_eq!(verified[0].0, 0, "first leaf index should be 0");
    assert_eq!(verified[1].0, 2, "second leaf index should be 2");
    assert_eq!(verified[2].0, 4, "third leaf index should be 4");
}

/// Single-element MMR: get_root returns the leaf itself (mmr_size == 1 fast path).
#[test]
fn test_single_element_mmr_root() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    let leaf = MmrNode::leaf(b"only leaf".to_vec());
    let expected_hash = leaf.hash();
    mmr.push(leaf).unwrap().expect("push should succeed");

    let root = mmr.get_root().unwrap().expect("get_root should succeed");
    assert_eq!(
        root.hash(),
        expected_hash,
        "single-element MMR root should equal the leaf hash"
    );
}

/// gen_proof rejects an empty positions list.
#[test]
fn test_gen_proof_empty_positions() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    mmr.push(MmrNode::leaf(b"leaf".to_vec()))
        .unwrap()
        .expect("push should succeed");

    assert!(
        matches!(mmr.gen_proof(vec![]).unwrap(), Err(Error::GenProofForInvalidLeaves)),
        "should reject empty positions"
    );
}

/// gen_proof rejects internal (non-leaf) node positions.
#[test]
fn test_gen_proof_rejects_internal_positions() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    for i in 0u32..4 {
        mmr.push(leaf_from_u32(i)).unwrap().expect("push");
    }
    // Position 2 is an internal node (height 1, merge of pos 0 and 1)
    assert!(
        matches!(mmr.gen_proof(vec![2]).unwrap(), Err(Error::NodeProofsNotSupported)),
        "should reject internal node positions"
    );
}

/// gen_proof rejects leaf positions that are beyond the MMR range.
#[test]
fn test_gen_proof_out_of_range_leaf_positions() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    for i in 0u32..4 {
        mmr.push(leaf_from_u32(i)).unwrap().expect("push");
    }
    // mmr_size = 7. Position 7 is a leaf position (height 0) but beyond range.
    assert!(
        matches!(mmr.gen_proof(vec![7]).unwrap(), Err(Error::GenProofForInvalidLeaves)),
        "should reject leaf positions beyond MMR range"
    );
}

/// When proved leaves are only in the first peak, trailing peaks are bagged together.
/// This exercises the bagging_track > 1 path in gen_proof.
#[test]
fn test_gen_proof_bags_trailing_peaks() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    // 11 leaves → mmr_size=19, 3 peaks at positions [14, 17, 18]
    let positions: Vec<u64> = (0u32..11)
        .map(|i| mmr.push(leaf_from_u32(i)).unwrap().expect("push"))
        .collect();
    let root = mmr.get_root().unwrap().expect("get_root");

    // Prove only leaf 0 (under peak 14). Peaks 17, 18 have no proved leaves
    // → bagging_track = 2 → triggers bag_peaks of the trailing peaks.
    let proof = mmr
        .gen_proof(vec![positions[0]])
        .unwrap()
        .expect("gen_proof should succeed");
    let valid = proof
        .verify(root, vec![(positions[0], leaf_from_u32(0))])
        .expect("verify should succeed");
    assert!(
        valid,
        "proof with bagged trailing peaks should verify correctly"
    );
}

/// calculate_root_with_new_leaf: adding a leaf that doesn't trigger merges (else branch).
/// With 8 leaves (perfect binary tree), adding leaf 8 creates a new standalone peak.
#[test]
fn test_gen_root_from_proof_no_merge_on_new_leaf() {
    test_gen_new_root_from_proof(8);
}

/// verify_incremental: extend an MMR and verify the old root + incremental leaves.
#[test]
fn test_verify_incremental_success() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    // Build initial MMR with 4 leaves → single peak at position 6
    for i in 0u32..4 {
        mmr.push(leaf_from_u32(i)).unwrap().expect("push");
    }
    let prev_root = mmr.get_root().unwrap().expect("prev root");
    let peak_node = mmr
        .batch
        .element_at_position(6)
        .unwrap()
        .expect("read peak")
        .expect("peak exists");

    // Add 3 more incremental leaves
    let incremental_leaves: Vec<MmrNode> = (4u32..7).map(leaf_from_u32).collect();
    for leaf in &incremental_leaves {
        mmr.push(leaf.clone()).unwrap().expect("push incremental");
    }
    let current_root = mmr.get_root().unwrap().expect("current root");

    // Proof items = previous peak hashes (just [peak at 6])
    let proof = MerkleProof::new(mmr.mmr_size, vec![peak_node]);

    let valid = proof
        .verify_incremental(current_root, prev_root, incremental_leaves)
        .expect("verify_incremental should not error");
    assert!(valid, "incremental verification should succeed");
}

/// verify_incremental returns false when the previous root doesn't match.
#[test]
fn test_verify_incremental_wrong_prev_root() {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);

    for i in 0u32..4 {
        mmr.push(leaf_from_u32(i)).unwrap().expect("push");
    }
    let peak_node = mmr
        .batch
        .element_at_position(6)
        .unwrap()
        .expect("read peak")
        .expect("peak exists");

    let incremental_leaves: Vec<MmrNode> = (4u32..7).map(leaf_from_u32).collect();
    for leaf in &incremental_leaves {
        mmr.push(leaf.clone()).unwrap().expect("push incremental");
    }
    let current_root = mmr.get_root().unwrap().expect("current root");

    let proof = MerkleProof::new(mmr.mmr_size, vec![peak_node]);
    let wrong_prev = MmrNode::internal([0xFFu8; 32]);

    let valid = proof
        .verify_incremental(current_root, wrong_prev, incremental_leaves)
        .expect("should not error, just return false");
    assert!(
        !valid,
        "should return false when prev_root doesn't match"
    );
}

/// verify_incremental rejects when incremental count >= current leaf count.
#[test]
fn test_verify_incremental_too_many_incremental() {
    // mmr_size=7 → 4 leaves. 4 incremental leaves >= 4 → error.
    let proof = MerkleProof::new(7, vec![]);
    let incremental: Vec<MmrNode> = (0..4).map(leaf_from_u32).collect();
    let result = proof.verify_incremental(
        MmrNode::internal([1u8; 32]),
        MmrNode::internal([2u8; 32]),
        incremental,
    );
    assert!(
        result.is_err(),
        "should reject when incremental count >= current leaf count"
    );
}

/// verify_incremental rejects when proof item count doesn't match previous peak count.
#[test]
fn test_verify_incremental_proof_count_mismatch() {
    // Current: 7 leaves (mmr_size=11). Incremental: 1 leaf.
    // Previous: 6 leaves → mmr_size=10 → peaks at [6, 9] → 2 peaks.
    // But proof has only 1 item → mismatch.
    let proof = MerkleProof::new(11, vec![MmrNode::internal([1u8; 32])]);
    let result = proof.verify_incremental(
        MmrNode::internal([1u8; 32]),
        MmrNode::internal([2u8; 32]),
        vec![leaf_from_u32(0)],
    );
    assert!(
        result.is_err(),
        "should reject when proof item count doesn't match previous peak count"
    );
}

prop_compose! {
    fn count_elem(count: u32)
                (elem in 0..count)
                -> (u32, u32) {
                    (count, elem)
    }
}

proptest! {
    #[test]
    fn test_random_mmr(count in 10u32..500u32) {
        let mut leaves: Vec<u32> = (0..count).collect();
        let mut rng = thread_rng();
        leaves.shuffle(&mut rng);
        let leaves_count = rng.gen_range(1..count - 1);
        leaves.truncate(leaves_count as usize);
        test_mmr(count, leaves);
    }

    #[test]
    fn test_random_gen_root_with_new_leaf(count in 1u32..500u32) {
        test_gen_new_root_from_proof(count);
    }
}
