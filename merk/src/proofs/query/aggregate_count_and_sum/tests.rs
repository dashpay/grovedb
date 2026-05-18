//! Round-trip and adversarial tests for
//! `AggregateCountAndSumOnRange` against `ProvableCountProvableSumTree`
//! (PCPS) hosts. Mirrors the test surface of
//! [`super::super::aggregate_count::tests`] and
//! [`super::super::aggregate_sum::tests`] but exercises BOTH axes
//! from a single proof.

use std::collections::LinkedList;

use grovedb_version::version::GroveVersion;

use super::verify_aggregate_count_and_sum_on_range_proof;
use crate::{
    proofs::{
        encode_into,
        query::{aggregate_common::NULL_HASH, QueryItem},
        Node, Op as ProofOp,
    },
    test_utils::TempMerk,
    tree::{Op, TreeFeatureType::ProvableCountedAndProvableSummedMerkNode},
    Error, TreeType,
};

/// Encode a `LinkedList<ProofOp>` into the on-the-wire byte stream.
fn encode_proof(ops: &LinkedList<ProofOp>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(256);
    encode_into(ops.iter(), &mut bytes);
    bytes
}

/// Build a fresh `ProvableCountProvableSumTree` populated with 15
/// single-byte keys "a".."o", each carrying count=1 and a value that
/// mixes positive, negative, and zero so the running sum exercises
/// signed arithmetic.
fn make_15_key_pcps(grove_version: &GroveVersion) -> (TempMerk, [u8; 32], i64) {
    let mut merk =
        TempMerk::new_with_tree_type(grove_version, TreeType::ProvableCountProvableSumTree);
    let mut full_sum: i64 = 0;
    let entries: Vec<(Vec<u8>, Op)> = (0u8..15)
        .map(|i| {
            // Mix signs to make the full-range sum non-trivial:
            // i % 4 == 0 → negative, == 2 → zero, others positive.
            let value: i64 = match i % 4 {
                0 => -(i as i64) * 3,
                2 => 0,
                _ => (i as i64 + 1) * 2,
            };
            full_sum += value;
            (
                vec![b'a' + i],
                Op::Put(vec![i], ProvableCountedAndProvableSummedMerkNode(1, value)),
            )
        })
        .collect();
    merk.apply::<_, Vec<_>>(&entries, &[], None, grove_version)
        .unwrap()
        .expect("apply PCPS entries");
    merk.commit(grove_version);
    let root_hash = merk.root_hash().unwrap();
    (merk, root_hash, full_sum)
}

/// Headline: a single combined-aggregate proof against a PCPS host
/// produces a verifiable `(count, sum)` pair that matches both the
/// merk's stored aggregate and the expected slice over the inner
/// range.
#[test]
fn pcps_round_trip_count_and_sum_aggregates_both_axes() {
    let v = GroveVersion::latest();
    let (merk, expected_root, _full_sum) = make_15_key_pcps(v);

    // Slice on the inner range "c".."m" (inclusive) — 11 keys
    // (c=2..m=12). Compute the expected sum by replaying the value
    // formula for each key.
    let expected_count: u64 = (b'c'..=b'm').count() as u64;
    let mut expected_sum: i64 = 0;
    for i in (b'c' - b'a')..=(b'm' - b'a') {
        let value: i64 = match i % 4 {
            0 => -(i as i64) * 3,
            2 => 0,
            _ => (i as i64 + 1) * 2,
        };
        expected_sum += value;
    }

    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"m".to_vec());
    let (ops, prover_count, prover_sum) = merk
        .prove_aggregate_count_and_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove combined aggregate on PCPS");
    assert_eq!(prover_count, expected_count);
    assert_eq!(prover_sum, expected_sum);

    let bytes = encode_proof(&ops);
    let (verifier_root, verifier_count, verifier_sum) =
        verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range)
            .unwrap()
            .expect("verify combined aggregate");
    assert_eq!(verifier_root, expected_root);
    assert_eq!(verifier_count, expected_count);
    assert_eq!(verifier_sum, expected_sum);
}

/// PCPS-only enforcement at the merk prover entry: every non-PCPS
/// tree type returns `InvalidProofError`.
#[test]
fn prover_rejects_non_pcps_hosts() {
    let v = GroveVersion::latest();
    let inner_range = QueryItem::Range(b"a".to_vec()..b"z".to_vec());
    for tt in [
        TreeType::NormalTree,
        TreeType::SumTree,
        TreeType::CountTree,
        TreeType::CountSumTree,
        TreeType::BigSumTree,
        TreeType::ProvableSumTree,
        TreeType::ProvableCountTree,
        TreeType::ProvableCountSumTree,
    ] {
        let merk = TempMerk::new_with_tree_type(v, tt);
        let err = merk
            .prove_aggregate_count_and_sum_on_range(&inner_range, v)
            .unwrap()
            .expect_err("must reject non-PCPS host");
        match err {
            Error::InvalidProofError(msg) => {
                assert!(
                    msg.contains("ProvableCountProvableSumTree"),
                    "expected PCPS-only message, got: {}",
                    msg
                );
            }
            other => panic!("expected InvalidProofError for {:?}, got {:?}", tt, other),
        }
    }
}

/// Empty PCPS merk: prover returns an empty op stream and the
/// verifier returns `(NULL_HASH, 0, 0)`.
#[test]
fn empty_pcps_merk_returns_null_hash_zero_zero() {
    let v = GroveVersion::latest();
    let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableCountProvableSumTree);

    let inner_range = QueryItem::Range(b"a".to_vec()..b"z".to_vec());
    let (ops, count, sum) = merk
        .prove_aggregate_count_and_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove against empty PCPS merk");
    assert!(ops.is_empty(), "empty merk yields empty op stream");
    assert_eq!(count, 0);
    assert_eq!(sum, 0);

    let (root_hash, v_count, v_sum) =
        verify_aggregate_count_and_sum_on_range_proof(&[], &inner_range)
            .unwrap()
            .expect("verify empty");
    assert_eq!(root_hash, NULL_HASH);
    assert_eq!(v_count, 0);
    assert_eq!(v_sum, 0);
}

/// Forged-count detection: bumping a `HashWithCountAndSum`'s count
/// field changes the reconstructed merk root. The verifier's
/// arithmetic checks may also fire first (e.g.
/// `child_struct_count > parent_count` triggers the
/// `checked_sub` rejection). Either path is a successful forgery
/// rejection from the caller's perspective.
#[test]
fn forged_count_changes_reconstructed_root_hash_or_fails() {
    let v = GroveVersion::latest();
    let (merk, honest_root, _full_sum) = make_15_key_pcps(v);

    let inner_range = QueryItem::Range(b"c".to_vec()..b"g".to_vec());
    let (mut ops, _, _) = merk
        .prove_aggregate_count_and_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove");

    // Find a HashWithCountAndSum op and bump its count by 1.
    let mut tampered = false;
    for op in ops.iter_mut() {
        if let ProofOp::Push(Node::HashWithCountAndSum(_, _, _, c, _))
        | ProofOp::PushInverted(Node::HashWithCountAndSum(_, _, _, c, _)) = op
        {
            *c = c.wrapping_add(1);
            tampered = true;
            break;
        }
    }
    assert!(
        tampered,
        "test fixture must produce at least one HashWithCountAndSum op for this range — \
         pick a different fixture range if this fails"
    );
    let bytes = encode_proof(&ops);
    match verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap() {
        Ok((forged_root, _c, _s)) => {
            assert_ne!(
                forged_root, honest_root,
                "tampered HashWithCountAndSum count must change reconstructed root hash"
            );
        }
        // Internal arithmetic mismatch is also a valid rejection.
        Err(_) => {}
    }
}

/// Forged-sum detection: bumping a `HashWithCountAndSum`'s sum field
/// changes the reconstructed merk root.
#[test]
fn forged_sum_changes_reconstructed_root_hash_or_fails() {
    let v = GroveVersion::latest();
    let (merk, honest_root, _full_sum) = make_15_key_pcps(v);

    let inner_range = QueryItem::Range(b"c".to_vec()..b"g".to_vec());
    let (mut ops, _, _) = merk
        .prove_aggregate_count_and_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove");

    let mut tampered = false;
    for op in ops.iter_mut() {
        if let ProofOp::Push(Node::HashWithCountAndSum(_, _, _, _, s))
        | ProofOp::PushInverted(Node::HashWithCountAndSum(_, _, _, _, s)) = op
        {
            *s = s.wrapping_add(1);
            tampered = true;
            break;
        }
    }
    assert!(
        tampered,
        "test fixture must produce at least one HashWithCountAndSum op for this range"
    );
    let bytes = encode_proof(&ops);
    match verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap() {
        Ok((forged_root, _c, _s)) => {
            assert_ne!(
                forged_root, honest_root,
                "tampered HashWithCountAndSum sum must change reconstructed root hash"
            );
        }
        Err(_) => {}
    }
}

/// Forged-KVDigestCountSum-count detection: tampering a boundary
/// node's count likewise changes the reconstructed root.
#[test]
fn forged_kvdigest_count_changes_root_or_fails() {
    let v = GroveVersion::latest();
    let (merk, honest_root, _full_sum) = make_15_key_pcps(v);

    let inner_range = QueryItem::Range(b"c".to_vec()..b"g".to_vec());
    let (mut ops, _, _) = merk
        .prove_aggregate_count_and_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove");

    let mut tampered = false;
    for op in ops.iter_mut() {
        if let ProofOp::Push(Node::KVDigestCountSum(_, _, c, _))
        | ProofOp::PushInverted(Node::KVDigestCountSum(_, _, c, _)) = op
        {
            *c = c.wrapping_add(1);
            tampered = true;
            break;
        }
    }
    assert!(
        tampered,
        "test fixture must produce at least one KVDigestCountSum op for this range"
    );
    let bytes = encode_proof(&ops);
    match verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap() {
        Ok((forged_root, _c, _s)) => {
            assert_ne!(
                forged_root, honest_root,
                "tampered KVDigestCountSum count must change reconstructed root hash"
            );
        }
        Err(_) => {}
    }
}

/// Forged-KVDigestCountSum-sum detection.
#[test]
fn forged_kvdigest_sum_changes_root_or_fails() {
    let v = GroveVersion::latest();
    let (merk, honest_root, _full_sum) = make_15_key_pcps(v);

    let inner_range = QueryItem::Range(b"c".to_vec()..b"g".to_vec());
    let (mut ops, _, _) = merk
        .prove_aggregate_count_and_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove");

    let mut tampered = false;
    for op in ops.iter_mut() {
        if let ProofOp::Push(Node::KVDigestCountSum(_, _, _, s))
        | ProofOp::PushInverted(Node::KVDigestCountSum(_, _, _, s)) = op
        {
            *s = s.wrapping_add(1);
            tampered = true;
            break;
        }
    }
    assert!(
        tampered,
        "test fixture must produce at least one KVDigestCountSum op for this range"
    );
    let bytes = encode_proof(&ops);
    match verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap() {
        Ok((forged_root, _c, _s)) => {
            assert_ne!(
                forged_root, honest_root,
                "tampered KVDigestCountSum sum must change reconstructed root hash"
            );
        }
        Err(_) => {}
    }
}

/// Unrelated node type substitution: replacing the dual-axis ops with
/// a single-axis `HashWithCount` (count-only) op is rejected by the
/// Phase 1 allowlist.
#[test]
fn verifier_rejects_single_axis_count_only_node_types() {
    let v = GroveVersion::latest();
    let (merk, _root, _full_sum) = make_15_key_pcps(v);
    let inner_range = QueryItem::Range(b"c".to_vec()..b"g".to_vec());
    let (mut ops, _, _) = merk
        .prove_aggregate_count_and_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove");

    // Replace the first HashWithCountAndSum with a single-axis
    // HashWithCount.
    for op in ops.iter_mut() {
        if let ProofOp::Push(Node::HashWithCountAndSum(kv, l, r, c, _s)) = op {
            *op = ProofOp::Push(Node::HashWithCount(*kv, *l, *r, *c));
            break;
        }
    }
    let bytes = encode_proof(&ops);
    let err = verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range)
        .unwrap()
        .expect_err("single-axis node type must be rejected");
    match err {
        Error::InvalidProofError(msg) => {
            assert!(
                msg.contains("unexpected node type"),
                "expected allowlist rejection, got: {}",
                msg
            );
        }
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

// ---------- shape-walk rejection of malformed proof shapes ----------
//
// These tests synthesize op streams that are well-formed bytes (Phase 1
// decode succeeds) but violate the structural invariants the combined
// verifier's Phase 2 shape walk requires. Mirror of the count-side
// `shape_walk_rejects_*` and `aggregate_sum/tests.rs` rejection arms,
// driven through `verify_aggregate_count_and_sum_on_range_proof` so the
// combined verifier's branches are exercised directly.

/// Disjoint position: replacing the HashWithCountAndSum with a plain
/// `Hash` op fails the Phase 1 allowlist. The combined verifier only
/// accepts `HashWithCountAndSum` and `KVDigestCountSum` — every other
/// node type is rejected up front.
#[test]
fn combined_verifier_rejects_non_dual_axis_at_disjoint() {
    let v = GroveVersion::latest();
    let (merk, _root, _full_sum) = make_15_key_pcps(v);
    // RangeAfter("o") puts the entire tree at a Disjoint position →
    // single Push(HashWithCountAndSum(...)) honest proof.
    let inner_range = QueryItem::RangeAfter(b"o".to_vec()..);
    let (mut ops, _, _) = merk
        .prove_aggregate_count_and_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

    // Swap the Disjoint HashWithCountAndSum for a plain Hash —
    // Phase 1 allowlist rejects it before Phase 2 even runs.
    let mut swapped = false;
    for op in ops.iter_mut() {
        if matches!(op, ProofOp::Push(Node::HashWithCountAndSum(..))) {
            // We don't care what hash content; the allowlist trips on
            // type alone.
            *op = ProofOp::Push(Node::Hash([0u8; 32]));
            swapped = true;
            break;
        }
    }
    assert!(
        swapped,
        "test setup: expected a HashWithCountAndSum to swap"
    );

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("plain Hash at Disjoint must be rejected");
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("unexpected node type"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// Contained position: handcraft a single-op proof with a
/// `KVDigestCountSum` at the root for a `RangeFull` inner range. That
/// classifies the root as `Contained`, but the shape walk requires a
/// `HashWithCountAndSum` there. Triggers the Phase 2
/// "expected HashWithCountAndSum at Contained position" arm.
#[test]
fn combined_verifier_rejects_non_hashwithcountandsum_at_contained() {
    let inner_range = QueryItem::RangeFull(std::ops::RangeFull);
    let mut ops = LinkedList::<ProofOp>::new();
    ops.push_back(ProofOp::Push(Node::KVDigestCountSum(
        b"d".to_vec(),
        [0u8; 32],
        1,
        0,
    )));
    let bytes = encode_proof(&ops);
    let result = verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("non-HashWithCountAndSum at Contained must be rejected");
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("expected HashWithCountAndSum") && msg.contains("Contained"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// Disjoint position with leaf-having-children: splice a child op
/// under the single Disjoint `HashWithCountAndSum`. The Phase 1
/// allowlist accepts the child op type, but the Phase 2 shape walk
/// rejects "Disjoint position must be a leaf".
#[test]
fn combined_verifier_rejects_disjoint_leaf_with_children() {
    let v = GroveVersion::latest();
    let (merk, _root, _full_sum) = make_15_key_pcps(v);
    let inner_range = QueryItem::RangeAfter(b"o".to_vec()..);
    let (mut ops, _, _) = merk
        .prove_aggregate_count_and_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

    // Splice in a child under the first HashWithCountAndSum.
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
    let result = verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("Disjoint HashWithCountAndSum with children must be rejected");
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("Disjoint position must be a leaf"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// Contained position with leaf-having-children: same splice as the
/// Disjoint test but with an inner range that Contains the entire
/// tree, so the root is classified `Contained`.
#[test]
fn combined_verifier_rejects_contained_leaf_with_children() {
    let v = GroveVersion::latest();
    let (merk, _root, _full_sum) = make_15_key_pcps(v);
    let inner_range = QueryItem::RangeFrom(b"a".to_vec()..);
    let (mut ops, _, _) = merk
        .prove_aggregate_count_and_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

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
    let result = verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("Contained HashWithCountAndSum with children must be rejected");
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("Contained position must be a leaf"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// Boundary position: replace the boundary `KVDigestCountSum` with a
/// `HashWithCountAndSum` (both Phase-1 allowlisted) so Phase 2 must
/// reject "expected KVDigestCountSum at Boundary position".
#[test]
fn combined_verifier_rejects_non_kvdigestcountsum_at_boundary() {
    let v = GroveVersion::latest();
    let (merk, _root, _full_sum) = make_15_key_pcps(v);
    // Bounded inner range so the root is classified Boundary.
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (mut ops, _, _) = merk
        .prove_aggregate_count_and_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

    let mut swapped = false;
    for op in ops.iter_mut() {
        if let ProofOp::Push(Node::KVDigestCountSum(_, _, c, s)) = op {
            *op = ProofOp::Push(Node::HashWithCountAndSum(
                [0u8; 32], [0u8; 32], [0u8; 32], *c, *s,
            ));
            swapped = true;
            break;
        }
    }
    assert!(swapped, "test setup: expected a KVDigestCountSum to swap");

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("non-KVDigestCountSum at Boundary must be rejected");
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("expected KVDigestCountSum") && msg.contains("Boundary"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// Boundary key outside inherited subtree bounds: rewrite the boundary
/// node's key to something past every tree key. Either Phase 1's
/// key-ordering check or Phase 2's "falls outside its inherited subtree
/// bounds" check trips — both are acceptable rejection paths.
#[test]
fn combined_verifier_rejects_boundary_key_outside_bounds() {
    let v = GroveVersion::latest();
    let (merk, _root, _full_sum) = make_15_key_pcps(v);
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (mut ops, _, _) = merk
        .prove_aggregate_count_and_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

    let mut rewrote = false;
    for op in ops.iter_mut() {
        if let ProofOp::Push(Node::KVDigestCountSum(key, _, _, _)) = op {
            *key = vec![0xff, 0xff];
            rewrote = true;
            break;
        }
    }
    assert!(
        rewrote,
        "test setup: expected a KVDigestCountSum to rewrite"
    );

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("KVDigestCountSum outside inherited bounds must be rejected");
    match err {
        Error::InvalidProofError(_) => {}
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// own_count underflow: rewrite the parent KVDigestCountSum's count
/// to zero so the verifier's `checked_sub` on
/// `aggregate - left_struct - right_struct` underflows. Mirrors the
/// single-axis `shape_walk_rejects_own_count_underflow` for the
/// combined dual-axis verifier.
#[test]
fn combined_verifier_rejects_own_count_underflow() {
    let v = GroveVersion::latest();
    let (merk, _root, _full_sum) = make_15_key_pcps(v);
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (mut ops, _, _) = merk
        .prove_aggregate_count_and_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

    // Mutate ONLY the last KVDigestCountSum op (matching the count-side
    // pattern): the parent boundary node whose children are already on
    // the proof stack. Zeroing it specifically triggers the
    // `checked_sub` underflow when the verifier computes
    // `own_count = aggregate - left_struct - right_struct`. Mutating
    // every op risks tripping an earlier shape error.
    let mut rewrote = false;
    for op in ops.iter_mut().rev() {
        if let ProofOp::Push(Node::KVDigestCountSum(_, _, c, _)) = op {
            *c = 0;
            rewrote = true;
            break;
        }
    }
    assert!(
        rewrote,
        "test setup: expected at least one KVDigestCountSum op"
    );

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result
        .expect_err("child structural counts exceeding parent's aggregate count must be rejected");
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("exceed parent's aggregate count")
                || msg.contains("expected HashWithCountAndSum")
                || msg.contains("Disjoint position must be a leaf"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// i64 narrow rejection: if the in-range sum, accumulated in i128
/// during the shape walk, doesn't fit in i64 the verifier returns
/// `InvalidProofError`. We craft a synthetic two-child boundary fixture
/// where both children carry HashWithCountAndSum subtrees with i64::MAX
/// sums — Phase 1 accepts them, Phase 2 accumulates 2 * i64::MAX in
/// i128, and the i64 narrow at the top entry rejects.
#[test]
fn combined_verifier_rejects_i64_sum_narrow_overflow() {
    let v = GroveVersion::latest();
    // Build a real merk with two extreme values; the prover may
    // detect the overflow itself, otherwise the verifier's narrow gate
    // must catch it. Either path is an acceptable safety net.
    let mut merk = TempMerk::new_with_tree_type(v, TreeType::ProvableCountProvableSumTree);
    let entries: Vec<(Vec<u8>, Op)> = vec![
        (
            b"a".to_vec(),
            Op::Put(
                vec![0],
                ProvableCountedAndProvableSummedMerkNode(1, i64::MAX),
            ),
        ),
        (
            b"b".to_vec(),
            Op::Put(
                vec![0],
                ProvableCountedAndProvableSummedMerkNode(1, i64::MAX),
            ),
        ),
    ];
    if merk
        .apply::<_, Vec<_>>(&entries, &[], None, v)
        .unwrap()
        .is_err()
    {
        // The apply path detected the i128 / aggregate overflow.
        // The narrow-gate scenario is still exercised by the
        // adversarial verifier-only path below.
    } else {
        merk.commit(v);
    }
    let inner_range = QueryItem::RangeFrom(b"a".to_vec()..);
    let result = merk
        .prove_aggregate_count_and_sum_on_range(&inner_range, v)
        .unwrap();
    match result {
        // Prover detected the overflow — also acceptable.
        Err(Error::InvalidProofError(_)) | Err(Error::CorruptedData(_)) => {}
        Err(other) => panic!("unexpected prover error: {:?}", other),
        Ok((ops, _c, _s)) => {
            let bytes = encode_proof(&ops);
            let v_result =
                verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap();
            assert!(
                v_result.is_err(),
                "verifier must reject an i128-sized sum that doesn't fit in i64"
            );
        }
    }
}

/// Byte-mutation fuzzer: flip arbitrary bytes of an honest proof and
/// confirm there's no silent forgery — every mutation either gets
/// rejected, returns a divergent root hash, or (rarely) returns the
/// honest answers unchanged. Mirrors `fuzz_byte_mutation_no_silent_forgery`
/// in the count-side tests for the dual-axis surface.
#[test]
fn combined_fuzz_byte_mutation_no_silent_forgery() {
    let v = GroveVersion::latest();
    let (merk, honest_root, _full_sum) = make_15_key_pcps(v);
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (ops, honest_count, honest_sum) = merk
        .prove_aggregate_count_and_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove");
    let honest_bytes = encode_proof(&ops);
    assert!(!honest_bytes.is_empty());

    let mut rejected = 0usize;
    let mut diverged = 0usize;
    let mut same_outcome = 0usize;
    let mut total = 0usize;

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
                continue;
            }
            bytes[byte_idx] = mutated;
            total += 1;

            let result =
                verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap();
            match result {
                Err(_) => rejected += 1,
                Ok((root, count, sum)) => {
                    if root == honest_root {
                        // Same root — verifier MUST also produce
                        // the same count + sum, otherwise we have a
                        // silent forgery: the hash chain should bind
                        // both axes.
                        assert_eq!(
                            count, honest_count,
                            "SILENT COUNT FORGERY at byte index {} (delta=0x{:02x}): \
                             verifier returned the honest root but a wrong count \
                             ({} != {})",
                            byte_idx, delta, count, honest_count
                        );
                        assert_eq!(
                            sum, honest_sum,
                            "SILENT SUM FORGERY at byte index {} (delta=0x{:02x}): \
                             verifier returned the honest root but a wrong sum \
                             ({} != {})",
                            byte_idx, delta, sum, honest_sum
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

    // Sanity: each safe branch should fire at least once.
    assert!(
        rejected > 0,
        "expected at least one mutation to be rejected outright"
    );
    assert!(
        diverged > 0,
        "expected at least one mutation to diverge the root hash"
    );
    let _ = same_outcome;
    assert_eq!(rejected + diverged + same_outcome, total);
}

// ---------- direct unit tests for the helper predicates ----------
//
// Mirror the single-axis tests like
// `provable_count_from_aggregate_accepts_all_count_bearing_variants`.

#[test]
fn provable_count_and_sum_from_aggregate_accepts_dual_axis() {
    use super::provable_count_and_sum_from_aggregate;
    use crate::tree::AggregateData;

    let (c, s) =
        provable_count_and_sum_from_aggregate(AggregateData::ProvableCountAndProvableSum(13, -42))
            .expect("dual-axis aggregate must be accepted");
    assert_eq!(c, 13);
    assert_eq!(s, -42);

    // Extremes pass through unchanged.
    let (c, s) = provable_count_and_sum_from_aggregate(AggregateData::ProvableCountAndProvableSum(
        u64::MAX,
        i64::MIN,
    ))
    .expect("extremes accepted");
    assert_eq!(c, u64::MAX);
    assert_eq!(s, i64::MIN);
}

#[test]
fn provable_count_and_sum_from_aggregate_rejects_non_dual_axis() {
    use super::provable_count_and_sum_from_aggregate;
    use crate::tree::AggregateData;

    for case in [
        AggregateData::NoAggregateData,
        AggregateData::Sum(7),
        AggregateData::BigSum(7),
        AggregateData::ProvableSum(-3),
        AggregateData::ProvableCount(5),
        AggregateData::ProvableCountAndSum(11, 99),
    ] {
        let result = provable_count_and_sum_from_aggregate(case);
        match result {
            Err(Error::CorruptedData(msg)) => assert!(
                msg.contains("ProvableCountAndProvableSum"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected CorruptedData, got {:?}", other),
        }
    }
}

#[test]
fn is_provable_count_and_sum_bearing_only_for_pcps() {
    use super::is_provable_count_and_sum_bearing;

    assert!(is_provable_count_and_sum_bearing(
        TreeType::ProvableCountProvableSumTree
    ));
    for tt in [
        TreeType::NormalTree,
        TreeType::SumTree,
        TreeType::CountTree,
        TreeType::CountSumTree,
        TreeType::BigSumTree,
        TreeType::ProvableSumTree,
        TreeType::ProvableCountTree,
        TreeType::ProvableCountSumTree,
    ] {
        assert!(
            !is_provable_count_and_sum_bearing(tt),
            "{:?} must not qualify as PCPS-bearing",
            tt
        );
    }
}

// ---------- direct phase-2 shape-walk rejection tests ----------
//
// The Phase-1 allowlist for the combined-aggregate proof permits
// exactly `HashWithCountAndSum` and `KVDigestCountSum`. The Phase-2
// shape walk then binds each leaf's node TYPE to the classification
// derived from inherited bounds: `HashWithCountAndSum` at
// Disjoint/Contained, `KVDigestCountSum` at Boundary. Mixing the two
// allowed types into the wrong slot must be rejected by Phase 2 — not
// Phase 1.
//
// Tests below craft single-op proofs to hit each of the four
// type-shape mismatches directly:
// - `KVDigestCountSum` at a Disjoint position
// - `HashWithCountAndSum` at a Boundary position
// and the i64 narrow-overflow gate (the synthetic two-`i64::MAX`
// fixture in `combined_verifier_rejects_i64_sum_narrow_overflow`
// is non-deterministic: the merk's apply may reject the overflow
// before the verifier ever gets to the narrow gate, so the gate
// arm may not actually be exercised under coverage).

#[test]
fn combined_verifier_rejects_kvdigest_at_disjoint_position() {
    // Build a synthetic 3-op proof where a Boundary parent has a
    // child whose inherited bounds make it Disjoint relative to the
    // range, but the child node is the wrong allowed type
    // (`KVDigestCountSum` instead of `HashWithCountAndSum`).
    //
    // Inner range: `RangeInclusive("g".."=g")` — the parent boundary
    // key "h" sits OUTSIDE the range, and the left child inherits
    // bounds `(None, "h")` against the range `["g", "g"]`. The left
    // sub-range upper bound "h" > "g" but the subtree includes keys
    // < "h" which spans "g" — Boundary again. So we need a tighter
    // setup: pick parent "h" with inner range "z" so that left
    // (None, "h") doesn't span "z" (Disjoint) and right ("h", None)
    // spans "z" (Boundary).
    let inner_range = QueryItem::RangeInclusive(b"z".to_vec()..=b"z".to_vec());
    let mut ops = LinkedList::<ProofOp>::new();
    // Left disjoint child: should be HashWithCountAndSum but is
    // KVDigestCountSum.
    ops.push_back(ProofOp::Push(Node::KVDigestCountSum(
        b"a".to_vec(),
        [0u8; 32],
        0,
        0,
    )));
    // Parent boundary at "h".
    ops.push_back(ProofOp::Push(Node::KVDigestCountSum(
        b"h".to_vec(),
        [0u8; 32],
        1,
        0,
    )));
    ops.push_back(ProofOp::Parent);
    // Right boundary child: HashWithCountAndSum standin for the
    // remaining subtree (None, no key needed since the parent's
    // walker descends).
    ops.push_back(ProofOp::Push(Node::HashWithCountAndSum(
        [0u8; 32], [0u8; 32], [0u8; 32], 0, 0,
    )));
    ops.push_back(ProofOp::Child);

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("KVDigestCountSum at Disjoint position must be rejected");
    match err {
        Error::InvalidProofError(msg) => assert!(
            // Either Phase-2 directly rejects the wrong type at
            // Disjoint, or an earlier shape rule (boundary-key
            // outside inherited bounds for "a" at (None, "h")) trips
            // first. Both are valid rejections; the key contract
            // here is "no silent accept of a wrong-typed Disjoint".
            msg.contains("Disjoint")
                || msg.contains("Boundary")
                || msg.contains("expected HashWithCountAndSum")
                || msg.contains("inherited subtree bounds"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

#[test]
fn combined_verifier_rejects_hashwithcountandsum_at_boundary_position() {
    // Build a synthetic three-op proof where a parent
    // KVDigestCountSum sits Boundary but one of its leaves is the
    // wrong Phase-1-allowed type (HashWithCountAndSum). Phase 2
    // routes the leaf classification to Boundary (the parent key
    // splits the inherited window at the leaf), expecting
    // KVDigestCountSum but finding HashWithCountAndSum.
    //
    // Setup: parent boundary key "h" with the inner range
    // `RangeInclusive("c"..="l")`. Replace the LEFT KVDigestCountSum
    // child (originally a Boundary node for `(None, h)`) with a
    // HashWithCountAndSum — Phase 1 accepts, Phase 2 expects
    // KVDigestCountSum at Boundary.
    let v = GroveVersion::latest();
    let (merk, _root, _full_sum) = make_15_key_pcps(v);
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (mut ops, _, _) = merk
        .prove_aggregate_count_and_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

    // Replace the FIRST KVDigestCountSum (the left-most boundary
    // node) with a HashWithCountAndSum carrying the same counts +
    // sums so the structural-aggregate doesn't trip first.
    let mut swapped = false;
    for op in ops.iter_mut() {
        if let ProofOp::Push(Node::KVDigestCountSum(_, _, c, s)) = op {
            *op = ProofOp::Push(Node::HashWithCountAndSum(
                [0u8; 32], [0u8; 32], [0u8; 32], *c, *s,
            ));
            swapped = true;
            break;
        }
    }
    assert!(swapped, "test setup: expected a KVDigestCountSum op");
    let bytes = encode_proof(&ops);
    let result = verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err =
        result.expect_err("HashWithCountAndSum at a Boundary position must be rejected by Phase 2");
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("expected KVDigestCountSum") && msg.contains("Boundary"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

#[test]
fn combined_verifier_rejects_phase2_boundary_key_outside_inherited_bounds() {
    // Synthesize a proof where a boundary KVDigestCountSum's key
    // sits OUTSIDE the (lo, hi) window its position in the
    // reconstructed tree implies. Phase 1 accepts (allowlisted node
    // type, no immediate ordering issue), then Phase 2's
    // `key_strictly_inside` check on the boundary node fires.
    //
    // Construct: top-level Boundary parent at key "m" inside range
    // ["a"..="z"]. Right child is itself Boundary because the
    // remaining bound window ("m", None) overlaps with ["a"..="z"]
    // at "n".."z". Put the right-child boundary key at "a" (which
    // is outside ("m", None)).
    let inner_range = QueryItem::RangeInclusive(b"a".to_vec()..=b"z".to_vec());

    let mut ops = LinkedList::<ProofOp>::new();
    // Left disjoint child (None, "m"): structural agg = 0, 0.
    ops.push_back(ProofOp::Push(Node::HashWithCountAndSum(
        [0u8; 32], [0u8; 32], [0u8; 32], 0, 0,
    )));
    // Parent boundary at "m".
    ops.push_back(ProofOp::Push(Node::KVDigestCountSum(
        b"m".to_vec(),
        [0u8; 32],
        2,
        0,
    )));
    ops.push_back(ProofOp::Parent);
    // Right boundary child at "a" (outside the inherited ("m", None) window).
    ops.push_back(ProofOp::Push(Node::KVDigestCountSum(
        b"a".to_vec(),
        [0u8; 32],
        1,
        0,
    )));
    ops.push_back(ProofOp::Child);

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("boundary key outside inherited subtree bounds must be rejected");
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("falls outside its inherited subtree bounds")
                || msg.contains("ordering")
                || msg.contains("aggregate-count-and-sum proof"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

#[test]
fn combined_verifier_narrow_gate_rejects_i128_overflow_via_crafted_proof() {
    // Synthetic crafted proof — two HashWithCountAndSum leaves with
    // i64::MAX sums under a KVDigestCountSum parent. Phase 1 accepts
    // both node types, Phase 2 walks both axes in i128 and the
    // narrow-to-i64 gate at the top rejects.
    //
    // Structure (Op stream): push L, push P (parent), Parent, push R,
    // Child — yielding a Boundary parent at "h" with Disjoint
    // children for the range "{}..{}".
    // Handcraft a Boundary parent with key "h" inside an inner range
    // of `RangeInclusive("h"..="h")` so left/right children are
    // Disjoint. The own_sum derivation: agg - left_struct -
    // right_struct in i128. Set parent_sum = 0; both children
    // declare structural sum = i64::MIN each. Then own_sum (i128) =
    // 0 - i64::MIN - i64::MIN = 2 * |i64::MIN| which doesn't fit in
    // i64 → narrow gate fires.
    //
    // own_sum is added to in_range_sum only if the parent key
    // matches the range — set inner_range = "h" to "h".
    let inner_range = QueryItem::RangeInclusive(b"h".to_vec()..=b"h".to_vec());
    let mut ops = LinkedList::<ProofOp>::new();
    // Left disjoint child at (None, "h"): structural count = 0
    // sum = i64::MIN.
    ops.push_back(ProofOp::Push(Node::HashWithCountAndSum(
        [0u8; 32],
        [0u8; 32],
        [0u8; 32],
        0,
        i64::MIN,
    )));
    // Parent boundary at "h" with aggregate count=1 sum=0.
    ops.push_back(ProofOp::Push(Node::KVDigestCountSum(
        b"h".to_vec(),
        [0u8; 32],
        1,
        0,
    )));
    ops.push_back(ProofOp::Parent);
    // Right disjoint child at ("h", None): structural count = 0, sum
    // = i64::MIN.
    ops.push_back(ProofOp::Push(Node::HashWithCountAndSum(
        [0u8; 32],
        [0u8; 32],
        [0u8; 32],
        0,
        i64::MIN,
    )));
    ops.push_back(ProofOp::Child);

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap();
    // own_sum = parent_sum(0) - left(i64::MIN) - right(i64::MIN) =
    // 2 * |i64::MIN| = 2^64 which overflows i64. Either the shape
    // walk catches it earlier (own_count subtraction is fine,
    // own_sum is signed and doesn't have a "child exceeds parent"
    // check) or the i64 narrow gate fires.
    match result {
        Err(Error::InvalidProofError(msg)) => {
            // Acceptable: any rejection covers either the narrow
            // gate or an earlier shape error.
            assert!(
                msg.contains("overflowed i64")
                    || msg.contains("position must be a leaf")
                    || msg.contains("aggregate-count-and-sum proof:"),
                "unexpected rejection message: {msg}"
            );
        }
        Ok(_) => panic!("synthetic i128-overflow proof must not verify"),
        Err(other) => panic!("unexpected error type: {:?}", other),
    }
}
