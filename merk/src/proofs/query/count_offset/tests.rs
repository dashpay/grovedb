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
