//! Unit + integration tests for the aggregate-count prover/verifier.
//!
//! Split out of the legacy single-file `aggregate_count.rs` along with
//! the prover/walker/verifier when the module became a directory. Body
//! is byte-identical to the previous in-file `mod tests { ... }` block;
//! only the `use super::*;` line at the top expanded into explicit
//! imports from the new sub-modules.

use std::collections::LinkedList;

use grovedb_costs::CostsExt;
use grovedb_version::version::GroveVersion;

use super::verify_aggregate_count_on_range_proof;
use crate::{
    proofs::{
        encode_into,
        query::{
            aggregate_common::{classify_subtree, SubtreeClassification, NULL_HASH},
            QueryItem,
        },
        Node, Op as ProofOp,
    },
    test_utils::TempMerk,
    tree::{Op, TreeFeatureType::ProvableCountedMerkNode},
    Error, Merk, TreeType,
};

/// Asserts the hardcoded fixture in the `verify_only_tests` module
/// still matches the bytes a fresh prove run produces. If the proof
/// encoding ever changes, this test fails and prints the new
/// constants — copy them into `verify_only_tests`.
#[test]
fn verify_only_fixture_matches_fresh_prover_output() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (ops, count) = merk
        .prove_aggregate_count_on_range(&inner_range, v)
        .unwrap()
        .expect("prove");
    let proof_hex = hex::encode(encode_proof(&ops));
    let root_hex = hex::encode(root);

    let drift_msg = format!(
        "aggregate_count proof encoding has drifted — update verify_only_tests:\n\
         const FIXTURE_15_KEY_C_TO_L_PROOF_HEX: &str = \"{}\";\n\
         const FIXTURE_15_KEY_C_TO_L_ROOT_HEX: &str = \"{}\";\n\
         const FIXTURE_15_KEY_C_TO_L_COUNT: u64 = {};",
        proof_hex, root_hex, count
    );
    assert_eq!(
        proof_hex,
        super::verify_only_tests::FIXTURE_15_KEY_C_TO_L_PROOF_HEX,
        "{}",
        drift_msg
    );
    assert_eq!(
        root_hex,
        super::verify_only_tests::FIXTURE_15_KEY_C_TO_L_ROOT_HEX,
        "{}",
        drift_msg
    );
    assert_eq!(
        count,
        super::verify_only_tests::FIXTURE_15_KEY_C_TO_L_COUNT,
        "{}",
        drift_msg
    );
}

fn range_inclusive(lo: &[u8], hi: &[u8]) -> QueryItem {
    QueryItem::RangeInclusive(lo.to_vec()..=hi.to_vec())
}

fn range_full() -> QueryItem {
    QueryItem::RangeFull(std::ops::RangeFull)
}

fn range_from(lo: &[u8]) -> QueryItem {
    QueryItem::RangeFrom(lo.to_vec()..)
}

fn range_after(lo: &[u8]) -> QueryItem {
    QueryItem::RangeAfter(lo.to_vec()..)
}

#[test]
fn classify_disjoint_below() {
    let r = range_inclusive(b"d", b"f");
    // subtree (None, b"c") — keys < "c", entirely below ["d", "f"].
    assert_eq!(
        classify_subtree(None, Some(b"c"), &r),
        SubtreeClassification::Disjoint,
    );
}

#[test]
fn classify_disjoint_above() {
    let r = range_inclusive(b"d", b"f");
    // subtree (b"g", None) — keys > "g", entirely above ["d", "f"].
    assert_eq!(
        classify_subtree(Some(b"g"), None, &r),
        SubtreeClassification::Disjoint,
    );
}

#[test]
fn classify_disjoint_at_lower_boundary_inclusive() {
    let r = range_inclusive(b"d", b"f");
    // subtree (None, b"d") — keys < "d", just below the inclusive bound.
    assert_eq!(
        classify_subtree(None, Some(b"d"), &r),
        SubtreeClassification::Disjoint,
    );
}

#[test]
fn classify_disjoint_at_upper_boundary_inclusive() {
    let r = range_inclusive(b"d", b"f");
    // subtree (b"f", None) — keys > "f", just above the inclusive bound.
    assert_eq!(
        classify_subtree(Some(b"f"), None, &r),
        SubtreeClassification::Disjoint,
    );
}

#[test]
fn classify_contained_simple() {
    let r = range_inclusive(b"a", b"z");
    // subtree (b"d", b"f") — keys in ("d", "f"), all in ["a", "z"].
    assert_eq!(
        classify_subtree(Some(b"d"), Some(b"f"), &r),
        SubtreeClassification::Contained,
    );
}

#[test]
fn classify_contained_full_range_full_subtree() {
    let r = range_full();
    // The full range matches everything — even an unbounded subtree is
    // contained.
    assert_eq!(
        classify_subtree(None, None, &r),
        SubtreeClassification::Contained,
    );
}

#[test]
fn classify_boundary_overlapping_lower() {
    let r = range_inclusive(b"d", b"f");
    // subtree (b"c", b"e") — keys in ("c", "e"), straddles the lower bound.
    assert_eq!(
        classify_subtree(Some(b"c"), Some(b"e"), &r),
        SubtreeClassification::Boundary,
    );
}

#[test]
fn classify_boundary_overlapping_upper() {
    let r = range_inclusive(b"d", b"f");
    // subtree (b"e", b"g") — keys in ("e", "g"), straddles the upper bound.
    assert_eq!(
        classify_subtree(Some(b"e"), Some(b"g"), &r),
        SubtreeClassification::Boundary,
    );
}

#[test]
fn classify_boundary_unbounded_below_with_bounded_range() {
    let r = range_from(b"d");
    // subtree (None, b"e") — could include keys < "d", so boundary.
    assert_eq!(
        classify_subtree(None, Some(b"e"), &r),
        SubtreeClassification::Boundary,
    );
}

#[test]
fn classify_contained_range_after_exclusive() {
    let r = range_after(b"b");
    // RangeAfter(b"b") = (b, +inf). subtree (b"b", b"e") — keys > "b" and
    // < "e", all in (b, +inf). Contained.
    assert_eq!(
        classify_subtree(Some(b"b"), Some(b"e"), &r),
        SubtreeClassification::Contained,
    );
}

// ---------- end-to-end integration tests on a real merk ----------
//
// These tests build a small ProvableCountTree, generate count proofs
// through the merk-level API, then verify them with the count verifier.
// They cover the four documented categories: open-range (lower-only and
// upper-only) and closed-range (inclusive and after-to-inclusive). Empty
// tree and single-bound edge cases are also exercised.

/// Build a fresh `ProvableCountTree` populated with single-byte keys
/// "a".."o" (15 keys) — same shape as the running example in the book
/// chapter's "Closed ranges" section. Returns the merk and its current
/// root hash.
fn make_15_key_provable_count_tree(grove_version: &GroveVersion) -> (TempMerk, [u8; 32]) {
    let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::ProvableCountTree);
    let keys: Vec<Vec<u8>> = (b'a'..=b'o').map(|c| vec![c]).collect();
    let entries: Vec<(Vec<u8>, Op)> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| {
            (
                k.clone(),
                Op::Put(vec![i as u8], ProvableCountedMerkNode(1)),
            )
        })
        .collect();
    merk.apply::<_, Vec<_>>(&entries, &[], None, grove_version)
        .unwrap()
        .expect("apply should succeed");
    merk.commit(grove_version);
    let root_hash = merk.root_hash().unwrap();
    (merk, root_hash)
}

/// Encode a `LinkedList<Op>` into the wire format that the verifier
/// consumes.
fn encode_proof(ops: &LinkedList<ProofOp>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(128);
    encode_into(ops.iter(), &mut bytes);
    bytes
}

/// Round-trip helper: prove the inner range, encode the proof, verify it,
/// assert the recovered root hash matches and the recovered count matches
/// `expected_count`.
fn round_trip(
    merk: &Merk<impl grovedb_storage::StorageContext<'static>>,
    expected_root: [u8; 32],
    inner_range: QueryItem,
    expected_count: u64,
    grove_version: &GroveVersion,
) {
    let (ops, prover_count) = merk
        .prove_aggregate_count_on_range(&inner_range, grove_version)
        .unwrap()
        .expect("prove should succeed");
    assert_eq!(
        prover_count, expected_count,
        "prover count mismatch for range {:?}",
        inner_range
    );
    let bytes = encode_proof(&ops);
    let (root, verifier_count) = verify_aggregate_count_on_range_proof(&bytes, &inner_range)
        .unwrap()
        .expect("verify should succeed");
    assert_eq!(
        root, expected_root,
        "verifier reconstructed wrong root for range {:?}",
        inner_range
    );
    assert_eq!(
        verifier_count, expected_count,
        "verifier count mismatch for range {:?}",
        inner_range
    );
}

#[test]
fn integration_open_range_from() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    // RangeFrom("c"..) → keys c..o (13 keys).
    round_trip(&merk, root, QueryItem::RangeFrom(b"c".to_vec()..), 13, v);
}

#[test]
fn integration_open_range_after() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    // RangeAfter(("b", ..)) → keys c..o (13 keys), same set as RangeFrom("c"..)
    // but proof shape differs — the boundary lands on "b" exclusive.
    round_trip(&merk, root, QueryItem::RangeAfter(b"b".to_vec()..), 13, v);
}

#[test]
fn integration_open_range_to() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    // RangeTo(..b"e") → keys a..d (4 keys, exclusive upper).
    round_trip(&merk, root, QueryItem::RangeTo(..b"e".to_vec()), 4, v);
}

#[test]
fn integration_open_range_to_inclusive() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    // RangeToInclusive(..=b"e") → keys a..e (5 keys, inclusive upper).
    round_trip(
        &merk,
        root,
        QueryItem::RangeToInclusive(..=b"e".to_vec()),
        5,
        v,
    );
}

#[test]
fn integration_closed_range_inclusive() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    // RangeInclusive("c"..="l") → 10 keys.
    round_trip(
        &merk,
        root,
        QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        10,
        v,
    );
}

#[test]
fn integration_closed_range_exclusive() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    // Range("c".."l") → c..k (9 keys, exclusive upper).
    round_trip(
        &merk,
        root,
        QueryItem::Range(b"c".to_vec()..b"l".to_vec()),
        9,
        v,
    );
}

#[test]
fn integration_closed_range_after_to_inclusive() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    // RangeAfterToInclusive(("c", "l")) → keys d..l (9 keys: d..=l excluding c).
    round_trip(
        &merk,
        root,
        QueryItem::RangeAfterToInclusive(b"c".to_vec()..=b"l".to_vec()),
        9,
        v,
    );
}

#[test]
fn integration_closed_range_after_to_exclusive() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    // RangeAfterTo(("c", "l")) → keys d..l (8 keys, both exclusive).
    round_trip(
        &merk,
        root,
        QueryItem::RangeAfterTo(b"c".to_vec()..b"l".to_vec()),
        8,
        v,
    );
}

#[test]
fn integration_range_below_all_keys() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    // Entire range below the smallest key — should produce count = 0
    // and a Disjoint proof at the root level.
    round_trip(
        &merk,
        root,
        QueryItem::RangeInclusive(vec![0x00]..=vec![0x10]),
        0,
        v,
    );
}

#[test]
fn integration_range_above_all_keys() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    // Entire range above the largest key.
    round_trip(
        &merk,
        root,
        QueryItem::RangeInclusive(b"z".to_vec()..=vec![0xff]),
        0,
        v,
    );
}

#[test]
fn integration_empty_merk() {
    let v = GroveVersion::latest();
    let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableCountTree);
    let (ops, prover_count) = merk
        .prove_aggregate_count_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
        .unwrap()
        .expect("prove on empty merk should succeed");
    assert_eq!(prover_count, 0);
    // Empty proof means the verifier returns NULL_HASH and count = 0.
    let bytes = encode_proof(&ops);
    let (root, verifier_count) = verify_aggregate_count_on_range_proof(
        &bytes,
        &QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
    )
    .unwrap()
    .expect("verify on empty merk should succeed");
    assert_eq!(root, NULL_HASH);
    assert_eq!(verifier_count, 0);
}

#[test]
fn integration_rejected_on_normal_tree() {
    let v = GroveVersion::latest();
    let merk = TempMerk::new(v); // NormalTree
    let err = merk
        .prove_aggregate_count_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
        .unwrap();
    assert!(
        err.is_err(),
        "expected an InvalidProofError on NormalTree, got Ok({:?})",
        err.ok().map(|(_, c)| c)
    );
}

#[test]
fn integration_count_forgery_is_rejected() {
    // Demonstrates the cryptographic binding: tamper with the count in a
    // HashWithCount op and the verifier's root-hash recomputation must
    // diverge from the expected root.
    let v = GroveVersion::latest();
    let (merk, expected_root) = make_15_key_provable_count_tree(v);
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (mut ops, _prover_count) = merk
        .prove_aggregate_count_on_range(&inner_range, v)
        .unwrap()
        .expect("prove should succeed");

    // Forge: bump the count on the first HashWithCount op we see.
    let mut tampered = false;
    for op in ops.iter_mut() {
        if let ProofOp::Push(Node::HashWithCount(_, _, _, count))
        | ProofOp::PushInverted(Node::HashWithCount(_, _, _, count)) = op
        {
            *count = count.saturating_add(1);
            tampered = true;
            break;
        }
    }
    assert!(
        tampered,
        "test setup: expected at least one HashWithCount op"
    );

    let bytes = encode_proof(&ops);
    let (root, _count) = verify_aggregate_count_on_range_proof(&bytes, &inner_range)
        .unwrap()
        .expect("verify should still complete (root mismatch is the caller's job)");
    assert_ne!(
        root, expected_root,
        "tampered count must produce a different reconstructed root hash"
    );
}

// ---------- no-proof variant: count_aggregate_on_range ----------
//
// The no-proof entry point must return exactly the same count as the
// proof path for every range shape, without producing any proof ops.
// These tests cross-check the two paths on the same merk.

/// Cross-check: assert that `count_aggregate_on_range` and the count
/// returned by `prove_aggregate_count_on_range` agree for the given
/// range, and that both equal `expected_count`.
fn no_proof_matches_prover(
    merk: &Merk<impl grovedb_storage::StorageContext<'static>>,
    inner_range: QueryItem,
    expected_count: u64,
    grove_version: &GroveVersion,
) {
    let no_proof = merk
        .count_aggregate_on_range(&inner_range, grove_version)
        .unwrap()
        .expect("count_aggregate_on_range should succeed");
    assert_eq!(
        no_proof, expected_count,
        "no-proof variant returned wrong count for range {:?}",
        inner_range
    );
    let (_ops, prover_count) = merk
        .prove_aggregate_count_on_range(&inner_range, grove_version)
        .unwrap()
        .expect("prove should succeed");
    assert_eq!(
        no_proof, prover_count,
        "no-proof variant disagrees with prover count for range {:?}",
        inner_range
    );
}

#[test]
fn no_proof_matches_prover_closed_range_inclusive() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_count_tree(v);
    no_proof_matches_prover(
        &merk,
        QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        10,
        v,
    );
}

#[test]
fn no_proof_matches_prover_closed_range_exclusive() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_count_tree(v);
    no_proof_matches_prover(&merk, QueryItem::Range(b"c".to_vec()..b"l".to_vec()), 9, v);
}

#[test]
fn no_proof_matches_prover_open_range_from() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_count_tree(v);
    no_proof_matches_prover(&merk, QueryItem::RangeFrom(b"c".to_vec()..), 13, v);
}

#[test]
fn no_proof_matches_prover_range_below_all_keys() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_count_tree(v);
    no_proof_matches_prover(
        &merk,
        QueryItem::RangeInclusive(vec![0x00]..=vec![0x10]),
        0,
        v,
    );
}

#[test]
fn no_proof_empty_merk_returns_zero() {
    let v = GroveVersion::latest();
    let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableCountTree);
    let count = merk
        .count_aggregate_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
        .unwrap()
        .expect("count_aggregate_on_range on empty merk should succeed");
    assert_eq!(count, 0);
}

#[test]
fn no_proof_rejected_on_normal_tree() {
    let v = GroveVersion::latest();
    let merk = TempMerk::new(v); // NormalTree
    let result = merk
        .count_aggregate_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
        .unwrap();
    assert!(
        result.is_err(),
        "expected InvalidProofError on NormalTree, got Ok({:?})",
        result.ok()
    );
}

#[test]
fn no_proof_matches_prover_range_after() {
    // RangeAfter at the root pushes the left boundary exclusive to "b",
    // which causes the walk to descend into the right subtree from the
    // root — exercising the right-child arm of walk_count_only.
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_count_tree(v);
    no_proof_matches_prover(&merk, QueryItem::RangeAfter(b"b".to_vec()..), 13, v);
}

#[test]
fn no_proof_matches_prover_range_to() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_count_tree(v);
    // RangeTo(..b"e") — exclusive upper, keys a..d (4 keys).
    no_proof_matches_prover(&merk, QueryItem::RangeTo(..b"e".to_vec()), 4, v);
}

#[test]
fn no_proof_matches_prover_range_to_inclusive() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_count_tree(v);
    // RangeToInclusive(..=b"e") — keys a..=e (5 keys).
    no_proof_matches_prover(&merk, QueryItem::RangeToInclusive(..=b"e".to_vec()), 5, v);
}

#[test]
fn no_proof_matches_prover_range_after_to_inclusive() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_count_tree(v);
    // RangeAfterToInclusive(("c", "l")) — keys d..=l (9 keys).
    no_proof_matches_prover(
        &merk,
        QueryItem::RangeAfterToInclusive(b"c".to_vec()..=b"l".to_vec()),
        9,
        v,
    );
}

#[test]
fn no_proof_provable_count_sum_tree() {
    // Exercise the ProvableCountSumTree branch of the tree-type gate —
    // it should accept the walk and return the same count as a
    // ProvableCountTree with the same key set.
    let v = GroveVersion::latest();
    let mut merk = TempMerk::new_with_tree_type(v, TreeType::ProvableCountSumTree);
    // ProvableCountedAndSummedMerkNode(count=1, sum=0): treats each
    // entry as count-1 with sum-contribution 0.
    let entries: Vec<(Vec<u8>, Op)> = (b'a'..=b'o')
        .enumerate()
        .map(|(i, c)| {
            (
                vec![c],
                Op::Put(
                    vec![i as u8],
                    crate::tree::TreeFeatureType::ProvableCountedSummedMerkNode(1, 0),
                ),
            )
        })
        .collect();
    merk.apply::<_, Vec<_>>(&entries, &[], None, v)
        .unwrap()
        .expect("apply ProvableCountSumTree entries");
    merk.commit(v);

    let count = merk
        .count_aggregate_on_range(&QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()), v)
        .unwrap()
        .expect("count_aggregate_on_range on ProvableCountSumTree should succeed");
    assert_eq!(count, 10, "c..=l should be 10 keys");
}

/// Build a fresh `ProvableCountProvableSumTree` populated with single-byte
/// keys "a".."o" (15 keys), each carrying count=1 and sum=(i+1). Sums
/// 1+..+15 = 120.
fn make_15_key_provable_count_provable_sum_tree(
    grove_version: &GroveVersion,
) -> (TempMerk, [u8; 32]) {
    let mut merk =
        TempMerk::new_with_tree_type(grove_version, TreeType::ProvableCountProvableSumTree);
    let keys: Vec<Vec<u8>> = (b'a'..=b'o').map(|c| vec![c]).collect();
    let entries: Vec<(Vec<u8>, Op)> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let sum = (i as i64) + 1;
            (
                k.clone(),
                Op::Put(
                    vec![i as u8],
                    crate::tree::TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(1, sum),
                ),
            )
        })
        .collect();
    merk.apply::<_, Vec<_>>(&entries, &[], None, grove_version)
        .unwrap()
        .expect("apply ProvableCountProvableSumTree entries");
    merk.commit(grove_version);
    let root_hash = merk.root_hash().unwrap();
    (merk, root_hash)
}

/// Aggregate-count proof against `ProvableCountProvableSumTree`
/// round-trips. Same shape as `integration_open_range_from`, but the
/// emitter dispatches dual-axis variants (`HashWithCountAndSum`,
/// `KVDigestCountSum`) and the verifier reconstructs
/// `node_hash_with_count_and_sum`.
#[test]
fn integration_count_proof_against_pcps_round_trips() {
    let v = GroveVersion::latest();
    let (merk, expected_root) = make_15_key_provable_count_provable_sum_tree(v);
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (ops, prover_count) = merk
        .prove_aggregate_count_on_range(&inner_range, v)
        .unwrap()
        .expect("prove count on PCPS should succeed");
    assert_eq!(prover_count, 10, "c..=l is 10 keys");
    let bytes = encode_proof(&ops);
    let (root, verifier_count) = verify_aggregate_count_on_range_proof(&bytes, &inner_range)
        .unwrap()
        .expect("verify count proof on PCPS should succeed");
    assert_eq!(root, expected_root);
    assert_eq!(verifier_count, 10);
}

/// Disjoint-leaf rejection on the dual-axis side: forging
/// `HashWithCountAndSum` children under a leaf-classification node must
/// be rejected by the shape walk. Mirrors
/// `shape_walk_rejects_disjoint_hashwithcount_with_children` for the
/// count-only side.
#[test]
fn shape_walk_rejects_disjoint_hashwithcountandsum_with_children_pcps() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_count_provable_sum_tree(v);
    // Range above all keys → Disjoint at root.
    let inner_range = QueryItem::RangeAfter(b"o".to_vec()..);
    let (mut ops, _) = merk
        .prove_aggregate_count_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

    // Splice in a child under the first HashWithCountAndSum to force the
    // "leaf at Disjoint position must be a leaf" rejection.
    let mut spliced = LinkedList::<ProofOp>::new();
    let mut done = false;
    for op in ops.iter() {
        spliced.push_back(op.clone());
        if !done && matches!(op, ProofOp::Push(Node::HashWithCountAndSum(..))) {
            spliced.push_back(ProofOp::Push(Node::HashWithCountAndSum(
                [0u8; 32], [0u8; 32], [0u8; 32], 1, 0,
            )));
            spliced.push_back(ProofOp::Parent);
            done = true;
        }
    }
    assert!(done, "test setup: need at least one HashWithCountAndSum op");
    ops = spliced;

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_count_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err(
        "spliced child under Disjoint HashWithCountAndSum must be rejected by shape walk",
    );
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("Disjoint position must be a leaf")
                || msg.contains("at a Disjoint position"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// Forged-count rejection: replacing the dual-axis HashWithCountAndSum
/// count with a wrong value forces the verifier's hash reconstruction
/// to diverge — and the caller's root-hash check would catch it.
/// We verify the proof returns a successful in-range count (the shape
/// walk itself doesn't check the count value, only structure), then
/// assert the returned root hash is NOT the honest root.
#[test]
fn integration_pcps_count_forgery_changes_root_hash() {
    let v = GroveVersion::latest();
    let (merk, honest_root) = make_15_key_provable_count_provable_sum_tree(v);
    let inner_range = QueryItem::RangeFrom(b"o".to_vec()..);
    let (mut ops, _) = merk
        .prove_aggregate_count_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

    // Tamper the first HashWithCountAndSum count field.
    let mut tampered = LinkedList::<ProofOp>::new();
    let mut done = false;
    for op in ops.iter() {
        if !done && let ProofOp::Push(Node::HashWithCountAndSum(kv, l, r, count, sum)) = op {
            // Forge: bump count by 1 to claim an extra key.
            tampered.push_back(ProofOp::Push(Node::HashWithCountAndSum(
                *kv,
                *l,
                *r,
                count + 1,
                *sum,
            )));
            done = true;
        } else {
            tampered.push_back(op.clone());
        }
    }
    assert!(done, "test setup: need at least one HashWithCountAndSum op");
    ops = tampered;

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_count_on_range_proof(&bytes, &inner_range).unwrap();
    if let Ok((forged_root, _)) = result {
        // If shape walk accepted the tampered proof (it might, since the
        // count field isn't shape-validated), the reconstructed root MUST
        // diverge from the honest one — that's the cryptographic binding.
        assert_ne!(
            forged_root, honest_root,
            "forging the count on a HashWithCountAndSum must change the reconstructed root hash"
        );
    }
    // If the shape walk rejected outright, that's also fine — the proof
    // is invalid either way.
}

// ---------- attack tests for the shape-walk verifier ----------
//
// These three tests exercise attacks the old allowlist-only verifier let
// through. With the shape walk in `verify_count_shape`, each one is
// rejected before the caller's root-hash check.

/// A malicious prover sends a single `Push(Hash(expected_root))` for a
/// non-empty tree. Without the shape check this would return
/// `(expected_root, 0)` for any range. The shape check classifies the
/// root with `(None, None)` against a bounded inner range as `Boundary`,
/// expects `KVDigestCount`, and rejects.
#[test]
fn shape_walk_rejects_single_hash_undercount() {
    let v = GroveVersion::latest();
    let (merk, expected_root) = make_15_key_provable_count_tree(v);
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());

    // Forged proof: a single Hash op carrying the genuine root hash.
    let mut forged: LinkedList<ProofOp> = LinkedList::new();
    forged.push_back(ProofOp::Push(Node::Hash(expected_root)));
    let bytes = encode_proof(&forged);

    let result = verify_aggregate_count_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("single-Hash forgery must be rejected");
    // keep merk alive for clarity in the test scope
    let _ = merk;
    // Plain `Hash` is no longer in the count-proof allowlist (it would
    // carry an unbound structural count), so the rejection now lands
    // in Phase 1's coarse allowlist rather than Phase 2's shape walk.
    // Either error message is fine — the attack is rejected.
    match err {
        Error::InvalidProofError(msg) => {
            assert!(
                msg.contains("unexpected node type")
                    || msg.contains("expected KVDigestCount")
                    || msg.contains("Boundary"),
                "unexpected message: {msg}"
            );
        }
        other => panic!("expected InvalidProofError, got {other:?}"),
    }
}

/// A malicious prover replaces an in-range `HashWithCount` subtree with
/// a `Hash` carrying that subtree's node_hash, undercounting by the
/// subtree's count. The hash chain still matches (same node_hash), so
/// the old allowlist verifier would have happily returned a wrong
/// count. The shape walk classifies that position as `Contained` and
/// requires `HashWithCount`, rejecting the swap.
#[test]
fn shape_walk_rejects_hash_swap_for_contained_subtree() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_count_tree(v);
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (mut ops, _) = merk
        .prove_aggregate_count_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

    // Swap the first HashWithCount op for a Hash op carrying the
    // computed node_hash for that subtree (so the chain check still
    // matches and only the shape walk can detect the attack).
    let mut swapped = false;
    for op in ops.iter_mut() {
        if let ProofOp::Push(Node::HashWithCount(kv_hash, l, r, c)) = op {
            let node_hash = crate::tree::node_hash_with_count(kv_hash, l, r, *c).unwrap();
            *op = ProofOp::Push(Node::Hash(node_hash));
            swapped = true;
            break;
        }
    }
    assert!(
        swapped,
        "test setup: expected at least one HashWithCount op"
    );

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_count_on_range_proof(&bytes, &inner_range).unwrap();
    assert!(
        result.is_err(),
        "HashWithCount→Hash swap on a Contained subtree must be rejected by the shape walk"
    );
}

/// A malicious prover attaches a `KVDigestCount` child under a leaf
/// `HashWithCount`. Because `Tree::hash()` for `HashWithCount` is
/// computed from the four embedded fields and ignores any reconstructed
/// children, the root hash check passes — but a naive verifier that
/// counts every visited node would credit the bogus child as +1. The
/// shape walk requires `Contained` positions to be **leaves**, so it
/// rejects the smuggled-in child.
#[test]
fn shape_walk_rejects_keyless_node_with_attached_children() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_count_tree(v);
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (mut ops, _honest_count) = merk
        .prove_aggregate_count_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

    // Smuggle a fake +1 child under the first HashWithCount op. After
    // any HashWithCount(...), insert: Push(Hash(zero)) Parent — that
    // attaches an extra hashed node as the LEFT child of the
    // HashWithCount during reconstruction. Then add a fake
    // Push(KVDigestCount) Child that would be picked up by an
    // allowlist verifier counting visited keys.
    //
    // Concretely we splice 4 ops right after the HashWithCount:
    //   Push(KVDigestCount(in_range_key, value_hash, 1))
    //   Parent             (attach KVDigestCount as the LEFT child of HashWithCount)
    //   Push(Hash([0; 32]))
    //   Child              (attach Hash as the RIGHT child of HashWithCount)
    //
    // The HashWithCount's hash() ignores these children, so the root
    // hash recomputation is unaffected. The shape walk catches the
    // Contained-position-with-children violation.
    let mut new_ops: LinkedList<ProofOp> = LinkedList::new();
    let mut spliced = false;
    for op in ops.iter() {
        new_ops.push_back(op.clone());
        if !spliced && matches!(op, ProofOp::Push(Node::HashWithCount(_, _, _, _))) {
            let in_range_key = b"d".to_vec();
            new_ops.push_back(ProofOp::Push(Node::KVDigestCount(
                in_range_key,
                [0u8; 32],
                1,
            )));
            new_ops.push_back(ProofOp::Parent);
            new_ops.push_back(ProofOp::Push(Node::Hash([0u8; 32])));
            new_ops.push_back(ProofOp::Child);
            spliced = true;
        }
    }
    assert!(
        spliced,
        "test setup: expected to splice into a HashWithCount"
    );
    ops = new_ops;

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_count_on_range_proof(&bytes, &inner_range).unwrap();
    assert!(
        result.is_err(),
        "attaching children under HashWithCount must be rejected (root hash alone wouldn't catch it)"
    );
}

/// `HashWithCount` is only safe inside the dedicated aggregate-count
/// verifier (which shape-checks the collapsed subtree). The plain
/// `Query::execute_proof` verifier must reject it on sight — otherwise
/// a malicious prover could include `HashWithCount` in a regular
/// query proof, attach fake KV children to it (whose pushes the
/// verifier would credit as query results via `execute_node`), and
/// have the parent's hash chain still verify because
/// `Tree::hash()` for `HashWithCount` ignores attached children.
#[test]
fn regular_query_verifier_rejects_hash_with_count_node() {
    use crate::proofs::query::QueryProofVerify;
    let v = GroveVersion::latest();

    // Build a regular merk and a regular range query against it.
    let mut merk = TempMerk::new(v);
    for i in 0u8..5 {
        merk.apply::<_, Vec<_>>(
            &[(
                vec![i],
                Op::Put(vec![i], crate::TreeFeatureType::BasicMerkNode),
            )],
            &[],
            None,
            v,
        )
        .unwrap()
        .expect("apply");
    }
    merk.commit(v);
    let q =
        crate::proofs::query::Query::new_single_query_item(QueryItem::Range(vec![0u8]..vec![5u8]));

    // Generate an honest proof, then splice a `HashWithCount` push into
    // it. The exact op sequence doesn't matter for what we're testing —
    // we just need the regular verifier to refuse to process the proof
    // because it contains a `HashWithCount`.
    let (mut ops, _) = merk
        .prove_unchecked_query_items(&[QueryItem::Range(vec![0u8]..vec![5u8])], None, true, v)
        .unwrap()
        .expect("prove");
    ops.push_front(ProofOp::Push(Node::HashWithCount(
        [0u8; 32], [0u8; 32], [0u8; 32], 0,
    )));
    let bytes = encode_proof(&ops);

    let result = q.execute_proof(&bytes, None, true, 0).unwrap();
    let err = result.expect_err("regular query verifier must reject HashWithCount on sight");
    let msg = format!("{}", err);
    assert!(
        msg.contains("HashWithCount") || msg.contains("aggregate-count"),
        "expected HashWithCount-rejection message, got: {msg}"
    );
}

/// Parallel guard for the dual-axis variant: the regular query verifier
/// must reject `HashWithCountAndSum` on sight, since it's only valid in
/// aggregate proofs against `ProvableCountProvableSumTree`.
#[test]
fn regular_query_verifier_rejects_hash_with_count_and_sum_node() {
    use crate::proofs::query::QueryProofVerify;
    let v = GroveVersion::latest();

    let mut merk = TempMerk::new(v);
    for i in 0u8..5 {
        merk.apply::<_, Vec<_>>(
            &[(
                vec![i],
                Op::Put(vec![i], crate::TreeFeatureType::BasicMerkNode),
            )],
            &[],
            None,
            v,
        )
        .unwrap()
        .expect("apply");
    }
    merk.commit(v);
    let q =
        crate::proofs::query::Query::new_single_query_item(QueryItem::Range(vec![0u8]..vec![5u8]));

    let (mut ops, _) = merk
        .prove_unchecked_query_items(&[QueryItem::Range(vec![0u8]..vec![5u8])], None, true, v)
        .unwrap()
        .expect("prove");
    // Splice in HashWithCountAndSum — only valid in aggregate proofs
    // against PCPS; the regular verifier must refuse it.
    ops.push_front(ProofOp::Push(Node::HashWithCountAndSum(
        [0u8; 32], [0u8; 32], [0u8; 32], 0, 0,
    )));
    let bytes = encode_proof(&ops);

    let result = q.execute_proof(&bytes, None, true, 0).unwrap();
    let err = result.expect_err(
        "regular query verifier must reject HashWithCountAndSum on sight (aggregate proofs only)",
    );
    let msg = format!("{}", err);
    assert!(
        msg.contains("HashWithCountAndSum")
            || msg.contains("aggregate-count")
            || msg.contains("aggregate-sum"),
        "expected HashWithCountAndSum-rejection message, got: {msg}"
    );
}

/// `KVHashCountSum` (non-queried-path dual-axis kv-hash) must be
/// rejected by the regular query verifier — it carries an aggregate that
/// is meaningful only inside an aggregate proof.
#[test]
fn regular_query_verifier_rejects_kv_hash_count_sum_node() {
    use crate::proofs::query::QueryProofVerify;
    let v = GroveVersion::latest();

    let mut merk = TempMerk::new(v);
    for i in 0u8..5 {
        merk.apply::<_, Vec<_>>(
            &[(
                vec![i],
                Op::Put(vec![i], crate::TreeFeatureType::BasicMerkNode),
            )],
            &[],
            None,
            v,
        )
        .unwrap()
        .expect("apply");
    }
    merk.commit(v);
    let q =
        crate::proofs::query::Query::new_single_query_item(QueryItem::Range(vec![0u8]..vec![5u8]));

    let (mut ops, _) = merk
        .prove_unchecked_query_items(&[QueryItem::Range(vec![0u8]..vec![5u8])], None, true, v)
        .unwrap()
        .expect("prove");
    ops.push_front(ProofOp::Push(Node::KVHashCountSum([0u8; 32], 0, 0)));
    let bytes = encode_proof(&ops);

    let result = q.execute_proof(&bytes, None, true, 0).unwrap();
    // KVHashCountSum is a path-hash node (no key, no value), so it
    // doesn't trigger an "unexpected node type" path — instead, splicing
    // it into a valid proof leaves the proof tree malformed (the extra
    // op produces more than one stack item at the end), which the
    // verifier also rejects. Either rejection path counts: the goal is
    // that the regular query verifier doesn't accept the dual-axis
    // path-hash variant as a substitute for a normal kv-hash node.
    let err = result.expect_err("regular query verifier must reject KVHashCountSum-bearing proofs");
    let msg = format!("{}", err);
    assert!(
        msg.contains("unexpected")
            || msg.contains("KVHash")
            || msg.contains("missing data")
            || msg.contains("stack")
            || msg.contains("proof"),
        "expected proof-level rejection, got: {msg}"
    );
}

// ---------- byte-mutation fuzzer ----------
//
// Stronger forgery-resistance check than the three hand-crafted attack
// tests above: enumerate every byte of an honest proof, flip it to
// each of three different values, and assert the verifier never
// produces a "silent forgery" — i.e. an `Ok((root, count))` where
// the root **matches** the honest one but the count **differs**.
//
// Three safe outcomes per mutation:
//  - **Rejection** — Phase 1 decode error, or Phase 2 shape mismatch.
//  - **Divergence** — `Ok((root', _))` where `root' != honest_root`,
//    so any caller comparing against their trusted root catches it.
//  - **Same outcome** — `Ok((honest_root, honest_count))`. This can
//    happen for non-canonical re-encodings (e.g. swapping
//    `Push` ↔ `PushInverted` doesn't change the reconstructed tree's
//    root or the shape walk's count). Harmless: the verifier is
//    deterministic on (root, count), and that pair is what the
//    caller acts on.
//
// The **unsafe** outcome is `Ok((honest_root, count'))` where
// `count' != honest_count`. The hash chain binds count via
// `node_hash_with_count`, so this should be impossible — the test
// panics if it ever happens.
//
// We also assert each safe branch fires at least once as a sanity
// check that the test is actually exercising the surface.
#[test]
fn fuzz_byte_mutation_no_silent_forgery() {
    let v = GroveVersion::latest();
    let (merk, honest_root) = make_15_key_provable_count_tree(v);
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (ops, honest_count) = merk
        .prove_aggregate_count_on_range(&inner_range, v)
        .unwrap()
        .expect("prove");
    let honest_bytes = encode_proof(&ops);
    assert!(!honest_bytes.is_empty());

    let mut rejected = 0usize;
    let mut diverged = 0usize;
    let mut same_outcome = 0usize;
    let mut total = 0usize;

    // Three different mutations per byte: +1, +0x55, XOR 0xff.
    let deltas: [u8; 3] = [1, 0x55, 0xff];
    for byte_idx in 0..honest_bytes.len() {
        for &delta in &deltas {
            let mut bytes = honest_bytes.clone();
            let original = bytes[byte_idx];
            let mutated = if delta == 0xff {
                original ^ 0xff
            } else {
                original.wrapping_add(delta)
            };
            if mutated == original {
                continue; // no-op, don't count
            }
            bytes[byte_idx] = mutated;
            total += 1;

            let result = verify_aggregate_count_on_range_proof(&bytes, &inner_range).unwrap();
            match result {
                Err(_) => rejected += 1,
                Ok((root, count)) => {
                    if root == honest_root {
                        // Same root — the verifier MUST also produce
                        // the same count, otherwise we have a silent
                        // count-forgery: the caller would accept the
                        // forged count thinking it's the honest one.
                        assert_eq!(
                            count, honest_count,
                            "SILENT FORGERY at byte index {} (delta=0x{:02x}): \
                             verifier returned the honest root but a wrong count \
                             ({} != {}). The hash chain should bind count.",
                            byte_idx, delta, count, honest_count
                        );
                        same_outcome += 1;
                    } else {
                        // Different root — caller's root check catches it.
                        diverged += 1;
                    }
                }
            }
        }
    }

    // Sanity: each safe branch should fire at least once on a real proof.
    assert!(
        rejected > 0,
        "expected at least one mutation to be rejected outright"
    );
    assert!(
        diverged > 0,
        "expected at least one mutation to diverge the root hash"
    );
    // `same_outcome` may legitimately be zero on some encoders, so we
    // don't require it. We just require no silent forgery occurred,
    // which the inner assert_eq! guarantees.
    let _ = same_outcome;
    assert_eq!(rejected + diverged + same_outcome, total);
}

// ---------- randomized round-trip property test ----------
//
// Build merks with varying sizes and key shapes from a deterministic
// RNG, run a bunch of randomly-chosen ranges through the prove → encode
// → verify pipeline, and assert the verifier's count agrees with a
// ground-truth count computed by directly intersecting the inserted
// keys with the range. Catches silent miscounts that the fixed
// examples above would miss (off-by-one, edge-of-tree, exact-bound
// matches against multi-byte keys, etc.).
#[test]
fn fuzz_random_trees_and_ranges_round_trip() {
    // Tiny custom xorshift RNG so we don't have to add a dev-dep.
    struct XorShift(u64);
    impl XorShift {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn gen_range(&mut self, lo: usize, hi: usize) -> usize {
            lo + (self.next_u64() as usize) % (hi - lo)
        }
        fn gen_key(&mut self, max_len: usize) -> Vec<u8> {
            let len = 1 + self.gen_range(0, max_len);
            (0..len).map(|_| (self.next_u64() & 0xff) as u8).collect()
        }
    }

    let v = GroveVersion::latest();
    let mut rng = XorShift(0xDEAD_BEEF_C0FFEE);
    let trials = 16;
    for trial in 0..trials {
        let key_count = rng.gen_range(1, 64);
        let mut keys: Vec<Vec<u8>> = (0..key_count).map(|_| rng.gen_key(8)).collect();
        keys.sort();
        keys.dedup();

        let mut merk = TempMerk::new_with_tree_type(v, TreeType::ProvableCountTree);
        let entries: Vec<(Vec<u8>, Op)> = keys
            .iter()
            .map(|k| (k.clone(), Op::Put(vec![0xAB], ProvableCountedMerkNode(1))))
            .collect();
        merk.apply::<_, Vec<_>>(&entries, &[], None, v)
            .unwrap()
            .expect("apply");
        merk.commit(v);
        let root = merk.root_hash().unwrap();

        // Try several random ranges per tree, picking shapes that
        // exercise both bounded and half-bounded variants.
        for sub_trial in 0..6 {
            let lo = rng.gen_key(8);
            let hi = rng.gen_key(8);
            let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };

            let inner_range = match sub_trial % 6 {
                0 => QueryItem::Range(lo.clone()..hi.clone()),
                1 => QueryItem::RangeInclusive(lo.clone()..=hi.clone()),
                2 => QueryItem::RangeFrom(lo.clone()..),
                3 => QueryItem::RangeAfter(lo.clone()..),
                4 => QueryItem::RangeTo(..hi.clone()),
                _ => QueryItem::RangeToInclusive(..=hi.clone()),
            };

            let expected = keys
                .iter()
                .filter(|k| inner_range.contains(k.as_slice()))
                .count() as u64;

            let (ops, prover_count) = merk
                .prove_aggregate_count_on_range(&inner_range, v)
                .unwrap()
                .expect("prove");
            assert_eq!(
                prover_count, expected,
                "trial {} sub {}: prover count mismatch for range {:?}",
                trial, sub_trial, inner_range
            );
            let bytes = encode_proof(&ops);
            let (vroot, vcount) = verify_aggregate_count_on_range_proof(&bytes, &inner_range)
                .unwrap()
                .expect("verify");
            assert_eq!(
                vroot, root,
                "trial {} sub {}: verifier root mismatch",
                trial, sub_trial
            );
            assert_eq!(
                vcount, expected,
                "trial {} sub {}: verifier count mismatch for range {:?}",
                trial, sub_trial, inner_range
            );
        }
    }
}

// ---------- shape-walk rejection of malformed proof shapes ----------
//
// These tests synthesize op streams that are well-formed bytes (Phase 1
// decode succeeds) but violate the structural invariants the shape walk
// requires (Phase 2 rejection). They exist to lock down the defensive
// error branches in `verify_count_shape` so future refactors that
// accidentally relax them are caught by the test suite.

/// `HashWithCount` is only valid as a leaf in the proof tree. If the
/// prover attaches children to a Disjoint-position `HashWithCount`,
/// the shape walk must reject — even though the parent's hash chain
/// (which uses `Tree::hash()` for `HashWithCount`, computed from the
/// four embedded fields and ignoring children) would still verify.
#[test]
fn shape_walk_rejects_disjoint_hashwithcount_with_children() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_count_tree(v);
    // RangeAfter("o") → all 15 keys are below; the entire tree is
    // Disjoint relative to the inner range, so the honest proof is a
    // single Push(HashWithCount(...)).
    let inner_range = QueryItem::RangeAfter(b"o".to_vec()..);
    let (mut ops, _) = merk
        .prove_aggregate_count_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

    // Splice in another HashWithCount as the child (no key, so no
    // ordering constraint at Phase 1) so we exercise Phase 2's
    // leaf-only assertion at the Disjoint position.
    let mut spliced = LinkedList::<ProofOp>::new();
    let mut done = false;
    for op in ops.iter() {
        spliced.push_back(op.clone());
        if !done && matches!(op, ProofOp::Push(Node::HashWithCount(_, _, _, _))) {
            spliced.push_back(ProofOp::Push(Node::HashWithCount(
                [0u8; 32], [0u8; 32], [0u8; 32], 1,
            )));
            spliced.push_back(ProofOp::Parent);
            done = true;
        }
    }
    assert!(done, "test setup: expected at least one HashWithCount op");
    ops = spliced;

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_count_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("Disjoint HashWithCount with children must be rejected");
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("Disjoint position must be a leaf"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// At a Disjoint position the shape walk requires `HashWithCount` (only
/// node type with a hash-bound count). A `Hash` op there would carry an
/// untrusted structural count for the parent's `own_count` derivation,
/// so it must be rejected.
#[test]
fn shape_walk_rejects_non_hashwithcount_at_disjoint() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_count_tree(v);
    let inner_range = QueryItem::RangeAfter(b"o".to_vec()..);
    let (mut ops, _) = merk
        .prove_aggregate_count_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

    // Replace the single Disjoint HashWithCount with a plain Hash.
    let mut swapped = false;
    for op in ops.iter_mut() {
        if let ProofOp::Push(Node::HashWithCount(kv, l, r, c)) = op {
            let node_hash = crate::tree::node_hash_with_count(kv, l, r, *c).unwrap();
            *op = ProofOp::Push(Node::Hash(node_hash));
            swapped = true;
            break;
        }
    }
    assert!(swapped, "test setup: expected a HashWithCount op to swap");

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_count_on_range_proof(&bytes, &inner_range).unwrap();
    // Phase 1 rejects plain Hash via the allowlist; Phase 2 would also
    // reject "expected HashWithCount at Disjoint position". Either is fine.
    let err = result.expect_err("plain Hash at Disjoint must be rejected");
    match err {
        Error::InvalidProofError(_) => {}
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// At a Boundary position the shape walk requires the node's key to
/// fall strictly inside the inherited subtree bounds. A prover that
/// emits a `KVDigestCount` whose key is outside those bounds is trying
/// to confuse the recursion's bound tracking — it must be rejected.
#[test]
fn shape_walk_rejects_kvdigestcount_outside_inherited_bounds() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_count_tree(v);
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (mut ops, _) = merk
        .prove_aggregate_count_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

    // Find a Boundary KVDigestCount and rewrite its key to something
    // outside the tree (way past 'z'). This will violate the inherited
    // (lo, hi) bounds at the verifier's recursion frame.
    let mut rewrote = false;
    for op in ops.iter_mut() {
        if let ProofOp::Push(Node::KVDigestCount(key, _, _)) = op {
            *key = vec![0xff, 0xff];
            rewrote = true;
            break;
        }
    }
    assert!(rewrote, "test setup: expected a KVDigestCount to rewrite");

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_count_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("KVDigestCount outside bounds must be rejected");
    match err {
        Error::InvalidProofError(_) => {}
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}
