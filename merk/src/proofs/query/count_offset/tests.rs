//! Unit and integration tests for the offset-paginated count-tree
//! prover/verifier. Mirrors the test layout of
//! [`super::super::aggregate_count::tests`] — same fixture trees, same
//! round-trip helper shape, but the assertion target is "skipped count
//! + returned items" rather than "in-range count".

use std::collections::LinkedList;

use grovedb_version::version::GroveVersion;

use super::verify_count_offset_on_range_proof;
use crate::{
    proofs::{encode_into, query::QueryItem, Op as ProofOp},
    test_utils::TempMerk,
    tree::{Op, TreeFeatureType::ProvableCountedMerkNode},
    Merk, TreeType,
};

/// Build the same 15-key fixture the aggregate-count tests use: keys
/// 'a'..='o' each paired with a single-byte value carrying the key's
/// alphabetical index, all stored as `ProvableCountedMerkNode(1)`
/// entries in a `ProvableCountTree`.
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

fn encode_proof(ops: &LinkedList<ProofOp>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(128);
    encode_into(ops.iter(), &mut bytes);
    bytes
}

/// Round-trip helper: prove an offset-paginated range, encode the
/// proof, verify it, assert the recovered root matches the expected
/// root and the returned/skipped counts match expectations. Returns
/// the verifier's keys for caller-side ordering assertions.
fn round_trip_keys(
    merk: &Merk<impl grovedb_storage::StorageContext<'static>>,
    expected_root: [u8; 32],
    inner_range: QueryItem,
    offset: u64,
    limit: Option<u64>,
    left_to_right: bool,
    expected_skipped: u64,
    expected_keys: &[&[u8]],
    grove_version: &GroveVersion,
) -> Vec<Vec<u8>> {
    let result = merk
        .prove_count_offset_on_range(&inner_range, offset, limit, left_to_right, grove_version)
        .unwrap()
        .expect("prove should succeed");
    let bytes = encode_proof(&result.ops);
    let verified =
        verify_count_offset_on_range_proof(&bytes, &inner_range, offset, limit, left_to_right)
            .unwrap()
            .expect("verify should succeed");
    assert_eq!(
        verified.root_hash, expected_root,
        "reconstructed root mismatch for range={:?} off={} lim={:?} ltr={}",
        inner_range, offset, limit, left_to_right
    );
    assert_eq!(
        verified.skipped, expected_skipped,
        "skipped count mismatch for range={:?} off={} lim={:?} ltr={}",
        inner_range, offset, limit, left_to_right
    );
    let keys: Vec<Vec<u8>> = verified
        .returned_items
        .iter()
        .map(|i| i.key.clone())
        .collect();
    let expected: Vec<Vec<u8>> = expected_keys.iter().map(|k| k.to_vec()).collect();
    assert_eq!(
        keys, expected,
        "returned keys mismatch for range={:?} off={} lim={:?} ltr={}",
        inner_range, offset, limit, left_to_right
    );
    keys
}

#[test]
fn round_trip_offset_0_limit_none_full_range_ascending() {
    // Sanity: with no offset and no limit, an offset proof should
    // return every in-range key. This exercises the per-element
    // descent path for an entirely Contained subtree.
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    let all_key_bufs: Vec<[u8; 1]> = (b'a'..=b'o').map(|c| [c]).collect();
    let all_keys: Vec<&[u8]> = all_key_bufs.iter().map(|k| k.as_slice()).collect();
    // RangeFull → entire tree contained, fall through to per-element
    // descent.
    round_trip_keys(
        &merk,
        root,
        QueryItem::RangeFull(std::ops::RangeFull),
        0,
        None,
        true,
        0,
        all_keys.as_slice(),
        v,
    );
}

#[test]
fn round_trip_offset_5_limit_3_full_range_ascending() {
    // 15 keys, ascending: offset 5 → skip a..e, limit 3 → return f, g, h.
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    round_trip_keys(
        &merk,
        root,
        QueryItem::RangeFull(std::ops::RangeFull),
        5,
        Some(3),
        true,
        5,
        &[b"f", b"g", b"h"],
        v,
    );
}

#[test]
fn round_trip_offset_5_limit_3_full_range_descending() {
    // 15 keys, descending: offset 5 → skip o,n,m,l,k, limit 3 → return j, i, h.
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    round_trip_keys(
        &merk,
        root,
        QueryItem::RangeFull(std::ops::RangeFull),
        5,
        Some(3),
        false,
        5,
        &[b"j", b"i", b"h"],
        v,
    );
}

#[test]
fn round_trip_offset_past_end_returns_empty_and_truncated_skip() {
    // Offset larger than the population: expect 0 items returned,
    // skipped == population (not the requested offset).
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    round_trip_keys(
        &merk,
        root,
        QueryItem::RangeFull(std::ops::RangeFull),
        1000,
        Some(3),
        true,
        15, // entire population skipped, requested offset unsatisfied
        &[],
        v,
    );
}

#[test]
fn round_trip_offset_in_middle_of_partial_range() {
    // RangeInclusive c..=l → 10 in-range keys. Offset 4 → skip c,d,e,f.
    // Limit 3 → return g,h,i.
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    round_trip_keys(
        &merk,
        root,
        QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        4,
        Some(3),
        true,
        4,
        &[b"g", b"h", b"i"],
        v,
    );
}

#[test]
fn round_trip_offset_equals_population_returns_empty() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    round_trip_keys(
        &merk,
        root,
        QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        10, // exactly equal to in-range population
        Some(3),
        true,
        10,
        &[],
        v,
    );
}

#[test]
fn round_trip_limit_none_returns_all_after_offset() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    round_trip_keys(
        &merk,
        root,
        QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        3,
        None,
        true,
        3,
        &[b"f", b"g", b"h", b"i", b"j", b"k", b"l"],
        v,
    );
}

#[test]
fn round_trip_empty_tree() {
    let v = GroveVersion::latest();
    let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableCountTree);
    let root_hash = merk.root_hash().unwrap();
    // An empty merk produces an empty op stream; the verifier returns
    // NULL_HASH for it, which matches the merk's root_hash because an
    // empty count tree's root hash is also NULL_HASH.
    let result = merk
        .prove_count_offset_on_range(
            &QueryItem::RangeFull(std::ops::RangeFull),
            0,
            Some(5),
            true,
            v,
        )
        .unwrap()
        .expect("prove on empty merk should succeed");
    assert!(result.ops.is_empty());
    let bytes = encode_proof(&result.ops);
    let verified = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        0,
        Some(5),
        true,
    )
    .unwrap()
    .expect("verify on empty proof should succeed");
    assert_eq!(verified.root_hash, root_hash);
    assert_eq!(verified.skipped, 0);
    assert!(verified.returned_items.is_empty());
}

// ───────────────── Adversarial / mismatch tests ─────────────────
//
// The verifier's job is not just to compute a result on honest input
// — it has to reject every tampering an attacker could conceivably
// apply. These tests cover the rejection branches in
// `verify_count_offset_shape` / `apply_self_state` / `classify_self`
// that the happy-path round-trips don't exercise:
//
//   - parameter mismatch between prover and verifier (range / offset /
//     limit / direction)
//   - structural tampering (count fields on `HashWithCount`,
//     boundary keys outside their inherited bounds)
//   - shape tampering (truncating the proof, prepending garbage bytes)
//
// Each test generates a legitimate proof first, then either invokes
// the verifier with the wrong parameters or mutates the proof bytes
// in a targeted way. All such tests must observe the verifier
// returning `Err`; a panic or unwrap means the verifier accepted
// something it shouldn't have.

/// Verifier called with a different range than the prover used —
/// should fail. Mismatched ranges shift every classification, so
/// some node that the prover emitted as `HashWithCount(Disjoint)`
/// looks like a `Contained` collapse to the verifier (or vice versa),
/// and the shape check rejects it.
#[test]
fn rejects_wrong_inner_range() {
    let v = GroveVersion::latest();
    let (merk, _) = make_15_key_provable_count_tree(v);
    let proven_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let result = merk
        .prove_count_offset_on_range(&proven_range, 0, Some(5), true, v)
        .unwrap()
        .expect("prove");
    let bytes = encode_proof(&result.ops);
    // Verify with a different range:
    let mismatched_range = QueryItem::RangeInclusive(b"a".to_vec()..=b"d".to_vec());
    let res =
        verify_count_offset_on_range_proof(&bytes, &mismatched_range, 0, Some(5), true).unwrap();
    assert!(
        res.is_err(),
        "verifier with mismatched range must reject; got {:?}",
        res
    );
}

/// Verifier called with the wrong direction — should fail. The
/// prover walked left-first (ascending) so item ops are emitted in
/// ascending key order; a descending verifier would interpret the
/// same ops in reverse order, producing inconsistent state mutations
/// and either an `apply_self_state` rejection (digest where a value
/// was expected) or a bound-check failure.
#[test]
fn rejects_wrong_direction() {
    let v = GroveVersion::latest();
    let (merk, _) = make_15_key_provable_count_tree(v);
    let range = QueryItem::RangeFull(std::ops::RangeFull);
    let result = merk
        .prove_count_offset_on_range(&range, 5, Some(3), true, v)
        .unwrap()
        .expect("prove");
    let bytes = encode_proof(&result.ops);
    // Verify with descending:
    let res = verify_count_offset_on_range_proof(&bytes, &range, 5, Some(3), false).unwrap();
    assert!(
        res.is_err(),
        "verifier with wrong direction must reject; got {:?}",
        res
    );
}

/// Verifier called with a different offset — the `skipped` running
/// total ends up different from `offset`, and either an apply step
/// (digest at offset=0 with limit slots free, or value with offset
/// remaining) or the final consistency check rejects.
#[test]
fn rejects_wrong_offset_smaller() {
    let v = GroveVersion::latest();
    let (merk, _) = make_15_key_provable_count_tree(v);
    let range = QueryItem::RangeFull(std::ops::RangeFull);
    let result = merk
        .prove_count_offset_on_range(&range, 5, Some(3), true, v)
        .unwrap()
        .expect("prove");
    let bytes = encode_proof(&result.ops);
    // Verify with smaller offset → verifier expects 3 skipped slots
    // but the proof's first KVDigestCount appears at the prover's
    // offset position 4 (not 3), tripping the offset=0/limit-free
    // digest check.
    let res = verify_count_offset_on_range_proof(&bytes, &range, 3, Some(3), true).unwrap();
    assert!(
        res.is_err(),
        "verifier with smaller offset must reject; got {:?}",
        res
    );
}

/// Verifier called with a larger offset than the prover used —
/// proof has fewer digest skips than the verifier expects, so a
/// value-bearing node appears with offset_remaining > 0 and
/// `apply_self_state` rejects.
#[test]
fn rejects_wrong_offset_larger() {
    let v = GroveVersion::latest();
    let (merk, _) = make_15_key_provable_count_tree(v);
    let range = QueryItem::RangeFull(std::ops::RangeFull);
    let result = merk
        .prove_count_offset_on_range(&range, 2, Some(3), true, v)
        .unwrap()
        .expect("prove");
    let bytes = encode_proof(&result.ops);
    let res = verify_count_offset_on_range_proof(&bytes, &range, 10, Some(3), true).unwrap();
    assert!(
        res.is_err(),
        "verifier with larger offset must reject; got {:?}",
        res
    );
}

/// Verifier called with a smaller limit — value nodes appear past
/// the verifier's limit window, tripping the "value emitted past
/// the limit" rejection in `apply_self_state`.
#[test]
fn rejects_wrong_limit_smaller() {
    let v = GroveVersion::latest();
    let (merk, _) = make_15_key_provable_count_tree(v);
    let range = QueryItem::RangeFull(std::ops::RangeFull);
    let result = merk
        .prove_count_offset_on_range(&range, 0, Some(5), true, v)
        .unwrap()
        .expect("prove");
    let bytes = encode_proof(&result.ops);
    let res = verify_count_offset_on_range_proof(&bytes, &range, 0, Some(2), true).unwrap();
    assert!(
        res.is_err(),
        "verifier with smaller limit must reject; got {:?}",
        res
    );
}

/// Mutating the proof bytes corrupts the hash chain. Any change
/// to the encoded count fields produces a different reconstructed
/// root hash *and* potentially trips earlier shape checks. We just
/// confirm verification fails — the precise error path varies with
/// where the mutation lands.
#[test]
fn rejects_byte_mutated_proof() {
    let v = GroveVersion::latest();
    let (merk, _) = make_15_key_provable_count_tree(v);
    let range = QueryItem::RangeFull(std::ops::RangeFull);
    let result = merk
        .prove_count_offset_on_range(&range, 5, Some(3), true, v)
        .unwrap()
        .expect("prove");
    let mut bytes = encode_proof(&result.ops);
    // Flip a byte in the middle of the proof. The exact effect
    // depends on which field landed there (could be a key, a hash,
    // or a length tag), but any one-byte mutation should make
    // verification fail — either with a shape/decoder error or a
    // root-hash mismatch.
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    // The verifier returns `Ok(_)` *with a different root hash* if the
    // mutation only corrupted hash bytes (the shape replay still
    // succeeds, but the reconstructed root hash diverges from the
    // expected one). In other cases it returns `Err`. Both outcomes
    // are acceptable rejections — the caller catches the hash
    // mismatch by comparing against their trusted root.
    let verified = verify_count_offset_on_range_proof(&bytes, &range, 5, Some(3), true).unwrap();
    let original_root = merk.root_hash().unwrap();
    match verified {
        Ok(res) => assert_ne!(
            res.root_hash, original_root,
            "byte mutation must either error or produce a non-matching root hash"
        ),
        Err(_) => {} // explicit rejection — also fine
    }
}

/// Truncating the proof bytes corrupts the op stream. The decoder
/// either bails on a truncated op or `execute_with_options` ends
/// with a stack size != 1. Either way verification must fail.
#[test]
fn rejects_truncated_proof() {
    let v = GroveVersion::latest();
    let (merk, _) = make_15_key_provable_count_tree(v);
    let range = QueryItem::RangeFull(std::ops::RangeFull);
    let result = merk
        .prove_count_offset_on_range(&range, 5, Some(3), true, v)
        .unwrap()
        .expect("prove");
    let bytes = encode_proof(&result.ops);
    // Drop the last 10 bytes:
    let truncated = &bytes[..bytes.len().saturating_sub(10)];
    let res = verify_count_offset_on_range_proof(truncated, &range, 5, Some(3), true).unwrap();
    assert!(
        res.is_err(),
        "truncated proof must be rejected; got {:?}",
        res
    );
}

/// Trailing garbage bytes — the decoder should reject (it consumes
/// ops until exhausted, and a partial trailing op fails to decode).
#[test]
fn rejects_trailing_garbage() {
    let v = GroveVersion::latest();
    let (merk, _) = make_15_key_provable_count_tree(v);
    let range = QueryItem::RangeFull(std::ops::RangeFull);
    let result = merk
        .prove_count_offset_on_range(&range, 5, Some(3), true, v)
        .unwrap()
        .expect("prove");
    let mut bytes = encode_proof(&result.ops);
    bytes.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
    let res = verify_count_offset_on_range_proof(&bytes, &range, 5, Some(3), true).unwrap();
    // Note: the decoder may or may not reject trailing bytes
    // depending on whether the trailing bytes happen to parse as a
    // standalone op. The honest case: the decoder consumes the
    // legitimate ops, then sees `0xAA` (which is not a valid op
    // opcode), and returns Err. If the trailing bytes happen to
    // parse, the stack check at the end of execute_with_options
    // catches it. Either way, verification fails.
    if let Ok(verified) = res {
        // Acceptable only if the trailing bytes still parse and the
        // reconstructed hash diverges; assert that.
        let original_root = merk.root_hash().unwrap();
        assert_ne!(
            verified.root_hash, original_root,
            "trailing garbage either errors or shifts the reconstructed root"
        );
    }
}

// ─────────── Forged-proof tests targeting verifier error branches ───────────
//
// These build proof byte streams from hand-crafted `Op` sequences and
// feed them straight to the verifier (bypassing the prover). Each one
// targets a specific rejection branch in
// `verify_count_offset_on_range_proof` / `verify_count_offset_shape` /
// `classify_self` that the happy-path round-trips don't exercise.

use crate::proofs::Node;

/// Encode a hand-crafted op sequence into proof bytes for direct
/// verification.
fn encode_ops(ops: &[ProofOp]) -> Vec<u8> {
    let list: LinkedList<ProofOp> = ops.iter().cloned().collect();
    encode_proof(&list)
}

/// Forged proof using a `Hash(_)` node (not on the verifier's
/// allowlist). The visit-node callback in `execute_with_options`
/// rejects it before tree reconstruction completes.
#[test]
fn rejects_unknown_node_kind_in_proof() {
    let bytes = encode_ops(&[ProofOp::Push(Node::Hash([0u8; 32]))]);
    let res = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        0,
        Some(5),
        true,
    )
    .unwrap();
    assert!(
        res.is_err(),
        "verifier must reject Hash(_) (not on count-offset allowlist); got {:?}",
        res
    );
}

/// Forged proof with a single `KVValueHash` returned-item — the
/// verifier rejects in `classify_self` because the count-offset flow
/// requires count-bearing variants.
#[test]
fn rejects_kv_value_hash_inside_count_tree() {
    let bytes = encode_ops(&[ProofOp::Push(Node::KVValueHash(
        b"a".to_vec(),
        vec![0, 1, 2],
        [0u8; 32],
    ))]);
    let res = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        0,
        Some(5),
        true,
    )
    .unwrap();
    assert!(
        res.is_err(),
        "verifier must reject KVValueHash in a count-offset proof; got {:?}",
        res
    );
}

/// Forged proof emitting a `KVValueHashFeatureType` with a non-count
/// feature type. `aggregate_of_proof_tree_node` rejects.
#[test]
fn rejects_kv_value_hash_feature_type_with_basic_feature() {
    use crate::TreeFeatureType;
    let bytes = encode_ops(&[ProofOp::Push(Node::KVValueHashFeatureType(
        b"a".to_vec(),
        vec![0, 1, 2],
        [0u8; 32],
        TreeFeatureType::BasicMerkNode, // not a count feature
    ))]);
    let res = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        0,
        Some(5),
        true,
    )
    .unwrap();
    assert!(
        res.is_err(),
        "verifier must reject KVValueHashFeatureType with non-count feature type; got {:?}",
        res
    );
}

/// Forged proof with a single `KVDigestCount` carrying a key
/// **outside** the inherited subtree bounds (which at the root call
/// are `(None, None)` — so this fires only at descended levels). We
/// build a parent `KVDigestCount` with key "m" and attach a left
/// child whose own key is "z" (impossible at left-subtree position,
/// which must have keys < "m"). The verifier's
/// `key_strictly_inside` check rejects.
#[test]
fn rejects_boundary_key_outside_inherited_bounds() {
    let bytes = encode_ops(&[
        // left child: KVDigestCount("z", ...)  — key > parent's "m" but
        // appears under parent's left child, violating the bound.
        ProofOp::Push(Node::KVDigestCount(b"z".to_vec(), [0u8; 32], 1)),
        // parent: KVDigestCount("m", ...)
        ProofOp::Push(Node::KVDigestCount(b"m".to_vec(), [0u8; 32], 2)),
        // attach left
        ProofOp::Parent,
    ]);
    let res = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        2,
        Some(5),
        true,
    )
    .unwrap();
    assert!(
        res.is_err(),
        "verifier must reject a boundary key outside its inherited subtree window; \
         got {:?}",
        res
    );
}

/// Forged proof with an internal `HashWithCount` carrying a child —
/// `HashWithCount` must be a leaf at any classification. Construct a
/// `Push HashWithCount`, then `Push KVDigestCount`, then `Parent` to
/// attach the digest as the hash node's left child. The verifier
/// rejects with the "must be a leaf" check.
#[test]
fn rejects_hash_with_count_with_attached_child() {
    let bytes = encode_ops(&[
        // child slot
        ProofOp::Push(Node::KVDigestCount(b"a".to_vec(), [0u8; 32], 1)),
        // hash node (would-be parent)
        ProofOp::Push(Node::HashWithCount([0u8; 32], [0u8; 32], [0u8; 32], 2)),
        // attach child as the hash node's left
        ProofOp::Parent,
    ]);
    let res = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        2,
        Some(5),
        true,
    )
    .unwrap();
    assert!(
        res.is_err(),
        "verifier must reject HashWithCount with an attached child; got {:?}",
        res
    );
}

/// Forged proof with `HashWithCount` at a position the verifier
/// classifies as `Boundary`. We use a non-trivial range so that the
/// root subtree-bounds (None, None) classify as Boundary, then place
/// `HashWithCount` there — the verifier rejects with the "cannot
/// appear at a Boundary position" check.
#[test]
fn rejects_hash_with_count_at_boundary_position() {
    let bytes = encode_ops(&[ProofOp::Push(Node::HashWithCount(
        [0u8; 32], [0u8; 32], [0u8; 32], 3,
    ))]);
    let range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let res = verify_count_offset_on_range_proof(&bytes, &range, 0, Some(5), true).unwrap();
    assert!(
        res.is_err(),
        "verifier must reject HashWithCount at Boundary classification; got {:?}",
        res
    );
}

/// Forged proof with children claiming more aggregate count than the
/// parent. Two `KVDigestCount` children (count=5 each) attached
/// under a parent with count=2 — the verifier's `checked_sub` for
/// own_count derivation fails with "child structural counts exceed
/// parent's aggregate".
#[test]
fn rejects_child_counts_exceeding_parent_aggregate() {
    let bytes = encode_ops(&[
        ProofOp::Push(Node::KVDigestCount(b"a".to_vec(), [0u8; 32], 5)),
        ProofOp::Push(Node::KVDigestCount(b"m".to_vec(), [0u8; 32], 2)),
        ProofOp::Parent,
        ProofOp::Push(Node::KVDigestCount(b"z".to_vec(), [0u8; 32], 5)),
        ProofOp::Child,
    ]);
    let res = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        10,
        Some(5),
        true,
    )
    .unwrap();
    assert!(
        res.is_err(),
        "verifier must reject when child counts exceed parent's aggregate; got {:?}",
        res
    );
}

/// Forged proof where a child's recursive structural count disagrees
/// with the count it claims via its immediate count field. We build
/// a parent with one leaf child whose recursive sum says aggregate=1
/// but the parent's `left_aggregate` snapshot says... hmm actually
/// that one's hard to forge in isolation because the immediate-child
/// read and the recursive return are computed from the same node.
/// Skip — the other checks cover the same code path.

/// Forged `KVCount` returned-item at an out-of-range key. The
/// verifier's `classify_self` rejects in the !in_range arm of the
/// `KVCount` branch.
#[test]
fn rejects_kv_count_at_out_of_range_position() {
    let bytes = encode_ops(&[ProofOp::Push(Node::KVCount(
        b"a".to_vec(),
        vec![0, 1, 2],
        1,
    ))]);
    // Range "x"..="z" doesn't contain "a".
    let range = QueryItem::RangeInclusive(b"x".to_vec()..=b"z".to_vec());
    let res = verify_count_offset_on_range_proof(&bytes, &range, 0, Some(5), true).unwrap();
    assert!(
        res.is_err(),
        "verifier must reject KVCount at out-of-range position; got {:?}",
        res
    );
}

/// Forged `KVCount` leaf with count=2 (so derived own_count=2). The
/// `classify_self` KVCount-branch rejects on `own_count != 1`.
#[test]
fn rejects_kv_count_with_wrong_own_count() {
    let bytes = encode_ops(&[ProofOp::Push(Node::KVCount(
        b"a".to_vec(),
        vec![0, 1, 2],
        2, // own_count derived = 2 (leaf, no children), expected 1
    ))]);
    let res = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        0,
        Some(5),
        true,
    )
    .unwrap();
    assert!(
        res.is_err(),
        "verifier must reject KVCount with own_count != 1; got {:?}",
        res
    );
}

/// Forged `KVValueHashFeatureType` returned-item at out-of-range
/// position.
#[test]
fn rejects_kv_value_hash_feature_type_at_out_of_range() {
    use crate::TreeFeatureType;
    let bytes = encode_ops(&[ProofOp::Push(Node::KVValueHashFeatureType(
        b"a".to_vec(),
        vec![0, 1, 2],
        [0u8; 32],
        TreeFeatureType::ProvableCountedMerkNode(1),
    ))]);
    let range = QueryItem::RangeInclusive(b"x".to_vec()..=b"z".to_vec());
    let res = verify_count_offset_on_range_proof(&bytes, &range, 0, Some(5), true).unwrap();
    assert!(
        res.is_err(),
        "verifier must reject KVValueHashFeatureType at out-of-range position; got {:?}",
        res
    );
}

/// Forged `KVValueHashFeatureType` leaf with count=2. `own_count`
/// derived = 2, classify_self rejects on `own_count != 1`.
#[test]
fn rejects_kv_value_hash_feature_type_with_wrong_own_count() {
    use crate::TreeFeatureType;
    let bytes = encode_ops(&[ProofOp::Push(Node::KVValueHashFeatureType(
        b"a".to_vec(),
        vec![0, 1, 2],
        [0u8; 32],
        TreeFeatureType::ProvableCountedMerkNode(2),
    ))]);
    let res = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        0,
        Some(5),
        true,
    )
    .unwrap();
    assert!(
        res.is_err(),
        "verifier must reject KVValueHashFeatureType with own_count != 1; got {:?}",
        res
    );
}

/// Forged `KVDigestCount` at an in-range counted position with
/// `offset_remaining = 0` but `limit_remaining` not yet exhausted —
/// an honest prover would have emitted a value-bearing node here.
/// The verifier's `apply_self_state` rejects in the
/// `InRangeCountedDigest` branch.
#[test]
fn rejects_kv_digest_count_with_limit_remaining() {
    // Leaf KVDigestCount with count=1 (own_count=1). Pass offset=0,
    // limit=5 — verifier sees a digest where a value should be.
    let bytes = encode_ops(&[ProofOp::Push(Node::KVDigestCount(
        b"a".to_vec(),
        [0u8; 32],
        1,
    ))]);
    let res = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        0,
        Some(5),
        true,
    )
    .unwrap();
    assert!(
        res.is_err(),
        "verifier must reject KVDigestCount at offset=0 with limit slots free; got {:?}",
        res
    );
}

/// Forged `HashWithCount` at a Contained position with offset=0 and
/// limit > 0 — an honest prover would have descended to emit the
/// values. The verifier rejects in the "collapse only valid in
/// offset window or past limit" branch.
#[test]
fn rejects_hash_with_count_at_contained_with_limit_remaining() {
    // RangeFull → root subtree (None, None) is Contained for any
    // range that's unbounded both sides... actually RangeFull is
    // Contained-trivial. Set offset=0, limit=5.
    let bytes = encode_ops(&[ProofOp::Push(Node::HashWithCount(
        [0u8; 32], [0u8; 32], [0u8; 32], 3,
    ))]);
    let res = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        0,
        Some(5),
        true,
    )
    .unwrap();
    assert!(
        res.is_err(),
        "verifier must reject HashWithCount-collapse at Contained position when neither \
         offset window nor past-limit; got {:?}",
        res
    );
}

/// Forged `HashWithCount` at a Contained position with count
/// exceeding `offset_remaining` — the prover's collapse rule is
/// `count ≤ offset_remaining`, so the verifier rejects.
#[test]
fn rejects_hash_with_count_exceeding_offset_remaining() {
    let bytes = encode_ops(&[ProofOp::Push(Node::HashWithCount(
        [0u8; 32], [0u8; 32], [0u8; 32], 10,
    ))]);
    // offset=3 < count=10
    let res = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        3,
        Some(5),
        true,
    )
    .unwrap();
    assert!(
        res.is_err(),
        "verifier must reject HashWithCount-collapse with count > offset_remaining; got {:?}",
        res
    );
}

#[test]
fn rejects_non_provable_count_tree() {
    // Regular Normal merk: prover entry must reject.
    let v = GroveVersion::latest();
    let mut merk = TempMerk::new_with_tree_type(v, TreeType::NormalTree);
    merk.apply::<_, Vec<_>>(
        &[(
            b"a".to_vec(),
            Op::Put(b"v".to_vec(), crate::TreeFeatureType::BasicMerkNode),
        )],
        &[],
        None,
        v,
    )
    .unwrap()
    .expect("apply");
    merk.commit(v);
    let res = merk
        .prove_count_offset_on_range(
            &QueryItem::RangeFull(std::ops::RangeFull),
            0,
            Some(5),
            true,
            v,
        )
        .unwrap();
    assert!(res.is_err(), "non-provable-count tree must reject");
}
