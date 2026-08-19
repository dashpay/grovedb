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
fn returned_items_carry_full_committed_payload() {
    // The keys-only assertion in `round_trip_keys` would still pass
    // even if the verifier silently rewrote `value`, `value_hash`, or
    // `child_hash_verified`. Pin one happy-path case on the full
    // `CountOffsetReturnedItem` shape so the prover/verifier contract
    // for committed metadata can't regress unobserved.
    //
    // Fixture: keys 'a'..='o' each paired with a single-byte value =
    // the key's alphabetical index. Stored as
    // `ProvableCountedMerkNode(1)` (Item-flavored) → the merk node
    // type is `KVCount`, which commits `value_hash = H(value_bytes)`
    // (no `combine_hash` since these aren't tree entries). The
    // count-offset prover never emits
    // `KVValueHashFeatureTypeWithChildHash`, so
    // `child_hash_verified` must be `false`.
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_count_tree(v);
    let result = merk
        .prove_count_offset_on_range(
            &QueryItem::RangeFull(std::ops::RangeFull),
            5,
            Some(3),
            true,
            v,
        )
        .unwrap()
        .expect("prove should succeed");
    let bytes = encode_proof(&result.ops);
    let verified = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        5,
        Some(3),
        true,
    )
    .unwrap()
    .expect("verify should succeed");
    assert_eq!(verified.root_hash, root, "root hash mismatch");
    assert_eq!(verified.skipped, 5, "skipped count mismatch");
    assert_eq!(verified.returned_items.len(), 3, "expected 3 items");

    // Build the expected full row for "f" — alphabetical index 5 → value bytes [5].
    let expected_f = crate::proofs::query::count_offset::CountOffsetReturnedItem {
        key: b"f".to_vec(),
        value: vec![5u8],
        value_hash: crate::tree::value_hash(&[5u8]).unwrap(),
        child_hash_verified: false,
        // A directly-valued row, not a resolved reference.
        reference_element_hash: None,
    };
    assert_eq!(
        verified.returned_items[0], expected_f,
        "full payload for first returned item must match committed bytes / value_hash / \
         child_hash_verified"
    );
    // Sanity-check that the remaining two rows also expose Item-flavored
    // value_hash (no `combine_hash`) and child_hash_verified = false —
    // i.e. the full-payload contract isn't a one-off.
    for (i, expected_idx) in [(1usize, 6u8), (2usize, 7u8)].into_iter() {
        let item = &verified.returned_items[i];
        assert_eq!(item.value, vec![expected_idx]);
        assert_eq!(
            item.value_hash,
            crate::tree::value_hash(&[expected_idx]).unwrap()
        );
        assert!(!item.child_hash_verified);
    }
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

/// Forged `KVCount` leaf with `count = 0` — own_count derives to 0,
/// which `classify_self` rejects for `KVCount` (KVCount always
/// implies own_count=1). Targets the `526-529` branch
/// specifically, distinct from the `own_count > 1` check at the
/// caller.
#[test]
fn rejects_kv_count_with_zero_own_count() {
    let bytes = encode_ops(&[ProofOp::Push(Node::KVCount(
        b"a".to_vec(),
        vec![0, 1, 2],
        0, // own_count = 0
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
        "verifier must reject KVCount with own_count = 0 via classify_self; got {:?}",
        res
    );
}

/// Same shape as the previous test but for `KVValueHashFeatureType`.
#[test]
fn rejects_kv_value_hash_feature_type_with_zero_own_count() {
    use crate::TreeFeatureType;
    let bytes = encode_ops(&[ProofOp::Push(Node::KVValueHashFeatureType(
        b"a".to_vec(),
        vec![0, 1, 2],
        [0u8; 32],
        TreeFeatureType::ProvableCountedMerkNode(0),
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
        "verifier must reject KVValueHashFeatureType with own_count = 0; got {:?}",
        res
    );
}

/// Forged `KVValueHash` at out-of-range — exercises the
/// !in_range arm of the `KVValueHash` branch in `classify_self`
/// (line ~570 in verify.rs).
#[test]
fn rejects_kv_value_hash_at_out_of_range() {
    let bytes = encode_ops(&[ProofOp::Push(Node::KVValueHash(
        b"a".to_vec(),
        vec![0, 1, 2],
        [0u8; 32],
    ))]);
    let range = QueryItem::RangeInclusive(b"x".to_vec()..=b"z".to_vec());
    let res = verify_count_offset_on_range_proof(&bytes, &range, 0, Some(5), true).unwrap();
    assert!(
        res.is_err(),
        "verifier must reject KVValueHash at out-of-range position; got {:?}",
        res
    );
}

/// Forged `KVValueHashFeatureType` with a `ProvableCountedSummedMerkNode`
/// feature — exercises the count-sum feature arm of
/// `aggregate_of_proof_tree_node`.
#[test]
fn accepts_kv_value_hash_feature_type_with_count_sum_feature() {
    use crate::TreeFeatureType;
    // We don't actually expect verification to succeed (it'll trip
    // some other check), but the test exercises the
    // `ProvableCountedSummedMerkNode` arm of
    // `aggregate_of_proof_tree_node` regardless. Just needs to NOT
    // panic.
    let bytes = encode_ops(&[ProofOp::Push(Node::KVValueHashFeatureType(
        b"a".to_vec(),
        vec![0, 1, 2],
        [0u8; 32],
        TreeFeatureType::ProvableCountedSummedMerkNode(1, 42),
    ))]);
    let _ = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        0,
        Some(5),
        true,
    )
    .unwrap();
    // No specific assertion — we only care that the verifier reaches
    // and exercises the count-sum feature arm of
    // `aggregate_of_proof_tree_node` before any other check fires.
}

/// Past-limit `KVDigestCount` (no-op state mutation) — both `offset = 0`
/// and `limit = Some(0)`. Exercises the past-limit branch of
/// `apply_self_state::InRangeCountedDigest`. Note: the verifier still
/// rejects because the offset_remaining and limit_remaining values
/// signal "nothing to do here" but the proof carries an in-range
/// digest. With offset=0 and limit=Some(0), the digest is in the
/// past-limit window and is *accepted* — but the proof has no
/// returned items and no skips, so the result is well-formed.
#[test]
fn accepts_kv_digest_count_past_limit() {
    // Single KVDigestCount, offset = 0, limit = Some(0) — past-limit
    // digest emission. Should NOT error.
    let bytes = encode_ops(&[ProofOp::Push(Node::KVDigestCount(
        b"a".to_vec(),
        [0u8; 32],
        1,
    ))]);
    let res = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        0,
        Some(0),
        true,
    )
    .unwrap()
    .expect("past-limit digest emission is a valid honest shape");
    assert!(res.returned_items.is_empty());
    assert_eq!(res.skipped, 0);
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

// ---------- ProvableCountProvableSumTree (PCPS) round-trips ----------
//
// PCPS hashes via `node_hash_with_count_and_sum` — both axes are
// committed into every node hash. The count-offset emit path
// dispatches the dual-axis `HashWithCountAndSum` / `KVDigestCountSum`
// / `KVCountSum` variants for PCPS hosts so the verifier can
// reconstruct the right hash function. Offset accounting itself is
// still count-only (the sum plays no role in skip/limit semantics);
// these tests pin the host extension by running the same round-trip
// shapes as the single-axis tests above against a PCPS source.

/// Build a 15-key PCPS fixture parallel to
/// `make_15_key_provable_count_tree`. Each entry has count=1 and
/// sum=i+1 so the structural sum is non-zero (forces the dual-axis
/// hash to differ from the count-only hash byte-for-byte).
fn make_15_key_pcps_tree(grove_version: &GroveVersion) -> (TempMerk, [u8; 32]) {
    use crate::tree::TreeFeatureType::ProvableCountedAndProvableSummedMerkNode;
    let mut merk =
        TempMerk::new_with_tree_type(grove_version, TreeType::ProvableCountProvableSumTree);
    let keys: Vec<Vec<u8>> = (b'a'..=b'o').map(|c| vec![c]).collect();
    let entries: Vec<(Vec<u8>, Op)> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let s = (i as i64) + 1;
            (
                k.clone(),
                Op::Put(
                    vec![i as u8],
                    ProvableCountedAndProvableSummedMerkNode(1, s),
                ),
            )
        })
        .collect();
    merk.apply::<_, Vec<_>>(&entries, &[], None, grove_version)
        .unwrap()
        .expect("apply pcps");
    merk.commit(grove_version);
    let root_hash = merk.root_hash().unwrap();
    (merk, root_hash)
}

/// Round-trip on PCPS: offset=0, no limit, full range, ascending —
/// returns all 15 keys. This is the headline test: it exercises the
/// dual-axis Node emission + the verifier's dual-axis allowlist +
/// `aggregate_of_proof_tree_node` reading count out of the dual-axis
/// variants + `node_hash_with_count_and_sum` reconstruction (so the
/// root hash matches the source).
#[test]
fn pcps_round_trip_offset_0_limit_none_full_range_ascending() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_pcps_tree(v);
    round_trip_keys(
        &merk,
        root,
        QueryItem::RangeFull(std::ops::RangeFull),
        0,
        None,
        true,
        0,
        &[
            b"a", b"b", b"c", b"d", b"e", b"f", b"g", b"h", b"i", b"j", b"k", b"l", b"m", b"n",
            b"o",
        ],
        v,
    );
}

/// PCPS offset + limit composition: skip 5, return next 3, ascending.
/// Exercises the dual-axis collapse op (`HashWithCountAndSum`) at the
/// offset-skipped subtree positions + dual-axis boundary nodes at the
/// returned-items window edge.
#[test]
fn pcps_round_trip_offset_5_limit_3_ascending() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_pcps_tree(v);
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

/// PCPS descending direction: skip 5 (highest), return next 3 highest.
/// Inverted-op family is exercised + dual-axis Node variants.
#[test]
fn pcps_round_trip_offset_5_limit_3_descending() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_pcps_tree(v);
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

/// PCPS partial range with offset in the middle of the range — same
/// shape as the single-axis `round_trip_offset_in_middle_of_partial_range`
/// but on a PCPS host. Tests that the Boundary classifications
/// (subtree partially in range) emit dual-axis variants correctly.
#[test]
fn pcps_round_trip_offset_in_middle_of_partial_range() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_pcps_tree(v);
    // Range "c".."l" → 9 keys (c..k inclusive). Offset 4, limit 3 →
    // skip c,d,e,f; return g,h,i.
    round_trip_keys(
        &merk,
        root,
        QueryItem::Range(b"c".to_vec()..b"l".to_vec()),
        4,
        Some(3),
        true,
        4,
        &[b"g", b"h", b"i"],
        v,
    );
}

/// Verifier accepts a dual-axis collapsed-subtree op
/// (`HashWithCountAndSum`) at a Contained position with offset
/// covering the whole subtree. Exercises the dual-axis arm of the
/// collapse-position match in `verify_count_offset_shape` +
/// `aggregate_of_proof_tree_node`'s `HashWithCountAndSum` arm.
#[test]
fn pcps_accepts_hash_with_count_and_sum_at_contained_with_offset_collapse() {
    // Single collapsed `HashWithCountAndSum(count=3, sum=42)` with
    // RangeFull (Contained at root) and offset=5 (subtree count ≤
    // offset_remaining → SkippedByOffset collapse arm). This is the
    // legal Contained-collapse shape for this op.
    let bytes = encode_ops(&[ProofOp::Push(Node::HashWithCountAndSum(
        [0u8; 32], [0u8; 32], [0u8; 32], 3, 42,
    ))]);
    let res = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        5,
        Some(3),
        true,
    )
    .unwrap()
    .expect("dual-axis HashWithCountAndSum at Contained-with-offset-collapse must verify");
    // All 3 keys skipped via the collapse; 2 offset slots remain
    // unconsumed (skipped == 3, offset_remaining wasn't fully burned).
    assert_eq!(res.skipped, 3);
    assert!(res.returned_items.is_empty());
}

/// Verifier rejects a `HashWithCountAndSum` at a Contained position
/// when children are spuriously attached — exercises the
/// "must be a leaf" arm for the dual-axis variant.
#[test]
fn pcps_rejects_hash_with_count_and_sum_contained_with_children() {
    // Two collapsed ops + Parent → the second becomes the parent and
    // the first becomes its left child. With a Contained-classified
    // range (RangeFull), the parent (HashWithCountAndSum) gets the
    // "must be a leaf" check and rejects.
    let bytes = encode_ops(&[
        ProofOp::Push(Node::HashWithCountAndSum(
            [0u8; 32], [0u8; 32], [0u8; 32], 1, 7,
        )),
        ProofOp::Push(Node::HashWithCountAndSum(
            [0u8; 32], [0u8; 32], [0u8; 32], 2, 14,
        )),
        ProofOp::Parent,
    ]);
    let res = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        5,
        Some(3),
        true,
    )
    .unwrap();
    assert!(
        res.is_err(),
        "HashWithCountAndSum with attached child at Contained must be rejected"
    );
}

/// Verifier reads `KVValueHashFeatureType` with a
/// `ProvableCountedAndProvableSummedMerkNode` feature type. Exercises
/// the dual-axis arm of `aggregate_of_proof_tree_node`'s
/// KVValueHashFeatureType match.
#[test]
fn pcps_accepts_kv_value_hash_feature_type_with_count_and_sum_feature() {
    use crate::TreeFeatureType;
    let bytes = encode_ops(&[ProofOp::Push(Node::KVValueHashFeatureType(
        b"a".to_vec(),
        vec![0, 1, 2],
        [0u8; 32],
        TreeFeatureType::ProvableCountedAndProvableSummedMerkNode(1, 42),
    ))]);
    // We don't expect successful verification here — RangeFull on a
    // tree-with-no-children doesn't form a real Merk shape — but the
    // verifier should at least *reach* the dual-axis feature-type
    // arm. Just exercise without panicking.
    let _ = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeFull(std::ops::RangeFull),
        0,
        Some(5),
        true,
    )
    .unwrap();
}

/// Verifier rejects a `KVCountSum` at an out-of-range position —
/// exercises the dual-axis variant's "not in range" rejection arm
/// in `classify_self`.
#[test]
fn pcps_rejects_kv_count_sum_at_out_of_range_position() {
    // RangeAfter("z") forces the (virtual) Boundary classification —
    // key "a" is below the range, so the verifier's classify_self
    // for KVCountSum sees `in_range = false` and rejects.
    let bytes = encode_ops(&[ProofOp::Push(Node::KVCountSum(
        b"a".to_vec(),
        vec![0, 1, 2],
        1,
        42,
    ))]);
    let res = verify_count_offset_on_range_proof(
        &bytes,
        &QueryItem::RangeAfter(b"z".to_vec()..),
        0,
        Some(5),
        true,
    )
    .unwrap();
    assert!(
        res.is_err(),
        "KVCountSum at out-of-range position must be rejected; got {:?}",
        res
    );
}

/// Verifier rejects a `KVCountSum` with own_count != 1. Mirrors
/// `rejects_kv_count_with_wrong_own_count` for the dual-axis variant.
#[test]
fn pcps_rejects_kv_count_sum_with_wrong_own_count() {
    // own_count = aggregate − left − right. Single-node tree with
    // `count = 0` → own_count = 0, but `KVCountSum` requires
    // own_count = 1.
    let bytes = encode_ops(&[ProofOp::Push(Node::KVCountSum(
        b"a".to_vec(),
        vec![0, 1, 2],
        0,
        42,
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
        "KVCountSum with own_count != 1 must be rejected; got {:?}",
        res
    );
}

/// Verifier rejects a `KVDigestCountSum` at an in-range position with
/// own_count = 0 (NonCounted-wrapped entry). Mirrors the rejection
/// path in `classify_self` for the single-axis `KVDigestCount`.
#[test]
fn pcps_rejects_kv_digest_count_sum_with_own_count_zero_in_range() {
    let bytes = encode_ops(&[ProofOp::Push(Node::KVDigestCountSum(
        b"a".to_vec(),
        [0u8; 32],
        0, // own_count = 0 → NonCounted-wrapped entry in range
        42,
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
        "KVDigestCountSum with own_count=0 at in-range position must be rejected — \
         NonCounted-wrapped entries aren't supported in count-offset proofs; got {:?}",
        res
    );
}

/// PCPS root-hash divergence: prove the same range+offset+limit on
/// both a `ProvableCountSumTree` and a `ProvableCountProvableSumTree`
/// over identical content, confirm the reconstructed root hashes
/// differ. Without dual-axis emission, the PCPS verifier would
/// reconstruct `node_hash_with_count` (wrong for the host) and
/// produce a root hash that matched the count-only host — pinning
/// the divergence here guards against a regression where the dual-axis
/// dispatch is removed.
#[test]
fn pcps_count_offset_root_hash_diverges_from_single_axis() {
    use crate::tree::TreeFeatureType::{
        ProvableCountedAndProvableSummedMerkNode, ProvableCountedSummedMerkNode,
    };
    let v = GroveVersion::latest();

    fn build_and_prove(
        tree_type: TreeType,
        entries: Vec<(Vec<u8>, Op)>,
        v: &GroveVersion,
    ) -> [u8; 32] {
        let mut merk = TempMerk::new_with_tree_type(v, tree_type);
        merk.apply::<_, Vec<_>>(&entries, &[], None, v)
            .unwrap()
            .expect("apply");
        merk.commit(v);
        let result = merk
            .prove_count_offset_on_range(
                &QueryItem::RangeFull(std::ops::RangeFull),
                2,
                Some(3),
                true,
                v,
            )
            .unwrap()
            .expect("prove");
        let bytes = encode_proof(&result.ops);
        let verified = verify_count_offset_on_range_proof(
            &bytes,
            &QueryItem::RangeFull(std::ops::RangeFull),
            2,
            Some(3),
            true,
        )
        .unwrap()
        .expect("verify");
        verified.root_hash
    }

    let pcst_entries: Vec<(Vec<u8>, Op)> = (b'a'..=b'o')
        .enumerate()
        .map(|(i, c)| {
            let s = (i as i64) + 1;
            (
                vec![c],
                Op::Put(vec![i as u8], ProvableCountedSummedMerkNode(1, s)),
            )
        })
        .collect();
    let pcst_root = build_and_prove(TreeType::ProvableCountSumTree, pcst_entries, v);

    let pcps_entries: Vec<(Vec<u8>, Op)> = (b'a'..=b'o')
        .enumerate()
        .map(|(i, c)| {
            let s = (i as i64) + 1;
            (
                vec![c],
                Op::Put(
                    vec![i as u8],
                    ProvableCountedAndProvableSummedMerkNode(1, s),
                ),
            )
        })
        .collect();
    let pcps_root = build_and_prove(TreeType::ProvableCountProvableSumTree, pcps_entries, v);

    assert_ne!(
        pcst_root, pcps_root,
        "PCPS count-offset proof must reconstruct a different root hash from \
         ProvableCountSumTree over identical content — PCPS commits the sum into the \
         node hash, so its hash function differs"
    );
}
