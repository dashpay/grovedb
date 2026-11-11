//! A proof verifies only in the one shape `MMR::gen_proof` emits for its
//! leaves.
//!
//! Bagging two peaks is the same hash as merging two siblings, so the root
//! of an MMR with several mountains is also "a parent" whose children are
//! the bag of the right-hand peaks and the peak left of them. A verifier
//! that lets the proof decide how many peaks it walks can therefore be led
//! to rebuild the root inside one mountain, from the leaves of another: the
//! entries verify, at positions they are not stored at.

use std::collections::{BTreeSet, HashMap};

use grovedb_version::version::GroveVersion;

use crate::{
    helper::{get_peaks, parent_offset, pos_height_in_tree},
    leaf_hash, leaf_index_to_pos,
    mem_store::MemStore,
    Error, MMRStoreReadOps, MerkleProof, MmrNode, MmrTreeProof, MMR,
};

fn leaf_value(index: u64) -> Vec<u8> {
    format!("leaf_{index}").into_bytes()
}

/// An MMR of `leaf_count` distinct leaves: its store, size and root hash.
fn build_mmr(leaf_count: u64) -> (MemStore, u64, [u8; 32]) {
    let store = MemStore::default();
    let mut mmr = MMR::new(0, &store);
    for index in 0..leaf_count {
        mmr.push(MmrNode::leaf(leaf_value(index)), GroveVersion::latest())
            .unwrap()
            .expect("push");
    }
    mmr.commit().unwrap().expect("commit");
    let mmr_size = mmr.mmr_size;
    let root = MMR::new(mmr_size, &store)
        .get_root(GroveVersion::latest())
        .unwrap()
        .expect("root")
        .hash();
    (store, mmr_size, root)
}

fn node_hash(store: &MemStore, pos: u64) -> [u8; 32] {
    store
        .element_at_position(pos)
        .unwrap()
        .expect("read node")
        .expect("node exists")
        .hash()
}

fn honest_proof(store: &MemStore, mmr_size: u64, leaf_indices: &[u64]) -> MmrTreeProof {
    MmrTreeProof::generate(mmr_size, leaf_indices, |pos| {
        store
            .element_at_position(pos)
            .unwrap()
            .map_err(|e| Error::OperationFailed(format!("element_at_position: {}", e)))
    })
    .expect("generate proof")
}

/// A proof in a layout the prover does not emit is refused for its layout,
/// not for the root it would compute: `verify_and_get_root` has no root to
/// compare against, so it has to fail on the layout alone.
fn assert_both_verifiers_reject(proof: &MmrTreeProof, root: &[u8; 32], what: &str) {
    if let Ok(leaves) = proof.verify(root) {
        panic!("{what}: verify accepted {leaves:?}");
    }
    match proof.verify_and_get_root() {
        Ok((computed, leaves)) => panic!(
            "{what}: verify_and_get_root computed a root for {leaves:?} (the trusted root: {})",
            &computed == root
        ),
        Err(Error::InvalidProof(_)) => {}
        Err(other) => panic!("{what}: unexpected error {other:?}"),
    }
}

/// Three leaves are two mountains, `[0, 1]` under node 2 and leaf 2 alone,
/// and the root is `merge(leaf 2, node 2)`. That is also what leaf 2's hash
/// and node 2 merge to as a pair of siblings, so the value of leaf 2 used to
/// verify as leaf 0 with node 2 as its only proof item.
#[test]
fn verify_rejects_a_leaf_relocated_from_another_mountain() {
    let (store, mmr_size, root) = build_mmr(3);
    assert_eq!(mmr_size, 4);

    let relocated = MmrTreeProof::new(
        mmr_size,
        vec![(0, leaf_value(2))],
        vec![node_hash(&store, 2)],
    );
    assert_both_verifiers_reject(&relocated, &root, "leaf 2 presented as leaf 0");

    for index in 0..3 {
        let honest = honest_proof(&store, mmr_size, &[index]);
        assert_eq!(
            honest.verify(&root).expect("honest proof verifies"),
            vec![(index, leaf_value(index))]
        );
    }
}

/// Twelve leaves are a mountain of eight and a mountain of four. The four
/// leaves of the second one, presented as leaves 0..4 with the first
/// mountain's peak as the only proof item, used to rebuild the root.
#[test]
fn verify_rejects_a_window_of_leaves_relocated_from_another_mountain() {
    let (store, mmr_size, root) = build_mmr(12);
    let peaks = get_peaks(mmr_size);
    assert_eq!(peaks, vec![14, 21]);

    let window: Vec<(u64, Vec<u8>)> = (0..4).map(|index| (index, leaf_value(index + 8))).collect();
    let relocated = MmrTreeProof::new(mmr_size, window, vec![node_hash(&store, peaks[0])]);
    assert_both_verifiers_reject(&relocated, &root, "leaves 8..12 presented as leaves 0..4");

    let honest = honest_proof(&store, mmr_size, &[0, 1, 2, 3]);
    assert_eq!(
        honest.verify(&root).expect("honest proof verifies"),
        (0..4)
            .map(|index| (index, leaf_value(index)))
            .collect::<Vec<_>>()
    );
}

/// The prover folds the peaks right of the last proved leaf into one item
/// only when there are two or more of them; the same peaks sent one by one
/// bag to the same root, and are a second encoding of the same proof.
#[test]
fn verify_rejects_right_hand_peaks_sent_one_by_one() {
    // Seven leaves: mountains of four, two and one, peaks at 6, 9 and 10.
    let (store, mmr_size, root) = build_mmr(7);
    assert_eq!(get_peaks(mmr_size), vec![6, 9, 10]);

    let honest = honest_proof(&store, mmr_size, &[0]);
    assert_eq!(honest.proof_items().len(), 3);
    honest.verify(&root).expect("honest proof verifies");

    let mut items = honest.proof_items()[..2].to_vec();
    items.push(node_hash(&store, 9));
    items.push(node_hash(&store, 10));
    let unfolded = MmrTreeProof::new(mmr_size, honest.leaves().to_vec(), items);
    assert_both_verifiers_reject(&unfolded, &root, "right-hand peaks sent one by one");
}

/// A proof that runs out of items before the peaks do is refused where it
/// runs out, not bagged from the peaks it did supply.
#[test]
fn verify_rejects_a_proof_that_stops_before_the_last_peak() {
    let (store, mmr_size, root) = build_mmr(7);
    let honest = honest_proof(&store, mmr_size, &[0]);

    let mut items = honest.proof_items().to_vec();
    items.pop();
    let short = MmrTreeProof::new(mmr_size, honest.leaves().to_vec(), items.clone());
    let err = short
        .verify(&root)
        .expect_err("a proof without its right-hand peaks must fail");
    assert!(
        matches!(&err, Error::InvalidProof(msg) if msg.contains("right of the proved leaves")),
        "unexpected error {err:?}"
    );

    // A peak left of the proved leaf is missing: leaf 6 is the last peak, and
    // needs the peaks at 6 and 9 before it.
    let honest_last = honest_proof(&store, mmr_size, &[6]);
    assert_eq!(honest_last.proof_items().len(), 2);
    let short = MmrTreeProof::new(
        mmr_size,
        honest_last.leaves().to_vec(),
        honest_last.proof_items()[..1].to_vec(),
    );
    let err = short
        .verify(&root)
        .expect_err("a proof without a left-hand peak must fail");
    assert!(
        matches!(&err, Error::InvalidProof(msg) if msg.contains("left of a proved leaf")),
        "unexpected error {err:?}"
    );
}

/// The one-leaf MMR takes the same path as every other size: its only leaf
/// is its root, and any proof item is one too many.
#[test]
fn verify_rejects_proof_items_for_the_one_leaf_mmr() {
    let (_store, mmr_size, root) = build_mmr(1);
    assert_eq!(mmr_size, 1);

    let honest = MmrTreeProof::new(1, vec![(0, leaf_value(0))], vec![]);
    assert_eq!(
        honest.verify(&root).expect("honest proof verifies"),
        vec![(0, leaf_value(0))]
    );

    let padded = MmrTreeProof::new(1, vec![(0, leaf_value(0))], vec![[7u8; 32]]);
    assert_both_verifiers_reject(&padded, &root, "one-leaf MMR with a proof item");
}

/// `gen_proof` refuses an empty position list, so no proof proves nothing.
#[test]
fn calculate_root_rejects_a_proof_of_no_leaves() {
    let (store, mmr_size, _root) = build_mmr(3);
    let peaks: Vec<MmrNode> = get_peaks(mmr_size)
        .into_iter()
        .map(|pos| MmrNode::internal(node_hash(&store, pos)))
        .collect();
    let err = MerkleProof::new(mmr_size, peaks)
        .calculate_root(vec![])
        .expect_err("a proof of no leaves must fail");
    assert!(
        matches!(&err, Error::InvalidProof(msg) if msg.contains("no leaves")),
        "unexpected error {err:?}"
    );
}

/// The hash a verifier computes for one leaf, as an expression over that
/// leaf and the proof items in the order it consumes them.
enum Shape {
    Leaf,
    Item(usize),
    Merge(Box<Shape>, Box<Shape>),
}

/// The expression for one leaf at `leaf_pos` under a verifier that walks the
/// first `peaks_walked` peaks and then takes `trailing_items` more items
/// before bagging, and how many items it consumes. `None` when the walk stops
/// before the leaf's own peak.
///
/// The verifier this crate had walked as many peaks as the proof had items
/// for, plus at most one item after the last; the family below covers every
/// such walk, and the canonical shape is one member of it.
fn single_leaf_shape(
    peaks: &[u64],
    leaf_pos: u64,
    peaks_walked: usize,
    trailing_items: usize,
) -> Option<(Shape, usize)> {
    let leaf_peak = peaks.iter().position(|peak_pos| leaf_pos <= *peak_pos)?;
    if peaks_walked <= leaf_peak {
        return None;
    }
    let mut item_count = 0;
    let mut next_item = || {
        item_count += 1;
        Shape::Item(item_count - 1)
    };

    let mut peak_shapes = Vec::new();
    for (index, peak_pos) in peaks.iter().take(peaks_walked).enumerate() {
        if index != leaf_peak {
            peak_shapes.push(next_item());
            continue;
        }
        let (mut pos, mut height, mut shape) = (leaf_pos, 0u8, Shape::Leaf);
        while pos != *peak_pos {
            if pos_height_in_tree(pos + 1) > height {
                // right sibling: the item is on the left
                shape = Shape::Merge(Box::new(next_item()), Box::new(shape));
                pos += 1;
            } else {
                shape = Shape::Merge(Box::new(shape), Box::new(next_item()));
                pos += parent_offset(height);
            }
            height += 1;
        }
        peak_shapes.push(shape);
    }
    for _ in 0..trailing_items {
        peak_shapes.push(next_item());
    }

    // bag right to left, as `bag_peaks` does
    while peak_shapes.len() > 1 {
        let right = peak_shapes.pop().expect("two shapes");
        let left = peak_shapes.pop().expect("two shapes");
        peak_shapes.push(Shape::Merge(Box::new(right), Box::new(left)));
    }
    Some((peak_shapes.pop()?, item_count))
}

/// Choose the proof items that make `shape` evaluate to `target`, if any do.
///
/// Every merge the verifier performs must reproduce a hash the honest tree
/// has, so its two inputs are that hash's two inputs: the items are forced,
/// and the search is exhaustive short of a blake3 collision.
fn solve(
    shape: &Shape,
    target: [u8; 32],
    leaf: [u8; 32],
    merges: &HashMap<[u8; 32], ([u8; 32], [u8; 32])>,
    items: &mut [[u8; 32]],
) -> bool {
    match shape {
        Shape::Leaf => leaf == target,
        Shape::Item(index) => {
            items[*index] = target;
            true
        }
        Shape::Merge(left, right) => match merges.get(&target) {
            Some((left_target, right_target)) => {
                solve(left, *left_target, leaf, merges, items)
                    && solve(right, *right_target, leaf, merges, items)
            }
            None => false,
        },
    }
}

/// Every merge behind the root: the parents inside each mountain, and the
/// steps that bag the peaks.
fn honest_merges(store: &MemStore, mmr_size: u64) -> HashMap<[u8; 32], ([u8; 32], [u8; 32])> {
    let mut merges = HashMap::new();
    for pos in 0..mmr_size {
        let height = pos_height_in_tree(pos);
        if height > 0 {
            let left = node_hash(store, pos - (1u64 << height));
            let right = node_hash(store, pos - 1);
            merges.insert(node_hash(store, pos), (left, right));
        }
    }
    let mut peaks: Vec<MmrNode> = get_peaks(mmr_size)
        .into_iter()
        .map(|pos| MmrNode::internal(node_hash(store, pos)))
        .collect();
    while peaks.len() > 1 {
        let right = peaks.pop().expect("two peaks");
        let left = peaks.pop().expect("two peaks");
        let bagged = MmrNode::merge(&right, &left);
        merges.insert(bagged.hash(), (right.hash(), left.hash()));
        peaks.push(bagged);
    }
    merges
}

/// For every MMR of up to 64 leaves, every leaf value and every index to
/// present it at, search every walk of the peaks for proof items that
/// rebuild the root. The only proofs that verify are the honest ones: the
/// value at its own index, with exactly one list of proof items.
#[test]
fn no_single_leaf_proof_presents_a_leaf_at_another_index() {
    let mut relocations = Vec::new();
    let mut leaf_counts_with_a_relocation = Vec::new();

    for leaf_count in 1..=64u64 {
        let (store, mmr_size, root) = build_mmr(leaf_count);
        let merges = honest_merges(&store, mmr_size);
        let peaks = get_peaks(mmr_size);
        let mut relocated_here = false;

        for stored_index in 0..leaf_count {
            let value = leaf_value(stored_index);
            let value_hash = leaf_hash(&value);

            for claimed_index in 0..leaf_count {
                let claimed_pos = leaf_index_to_pos(claimed_index);
                let mut accepted_proofs = BTreeSet::new();

                for peaks_walked in 1..=peaks.len() {
                    for trailing_items in 0..=1 {
                        let Some((shape, item_count)) =
                            single_leaf_shape(&peaks, claimed_pos, peaks_walked, trailing_items)
                        else {
                            continue;
                        };
                        let mut items = vec![[0u8; 32]; item_count];
                        if !solve(&shape, root, value_hash, &merges, &mut items) {
                            continue;
                        }
                        let proof = MmrTreeProof::new(
                            mmr_size,
                            vec![(claimed_index, value.clone())],
                            items.clone(),
                        );
                        let Ok(leaves) = proof.verify(&root) else {
                            continue;
                        };
                        assert_eq!(leaves, vec![(claimed_index, value.clone())]);
                        accepted_proofs.insert(items);
                    }
                }

                if claimed_index == stored_index {
                    assert_eq!(
                        accepted_proofs.len(),
                        1,
                        "{leaf_count} leaves: leaf {stored_index} must verify with exactly one \
                         list of proof items"
                    );
                } else if !accepted_proofs.is_empty() {
                    relocated_here = true;
                    relocations.push((leaf_count, stored_index, claimed_index));
                }
            }
        }
        if relocated_here {
            leaf_counts_with_a_relocation.push(leaf_count);
        }
    }

    assert!(
        relocations.is_empty(),
        "{} leaf counts accept a relocated leaf: {:?}; first (leaf count, stored at, verified \
         at): {:?}",
        leaf_counts_with_a_relocation.len(),
        leaf_counts_with_a_relocation,
        &relocations[..relocations.len().min(8)]
    );
}
