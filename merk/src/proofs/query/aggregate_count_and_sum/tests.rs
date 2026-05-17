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
    if tampered {
        let bytes = encode_proof(&ops);
        match verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap() {
            Ok((forged_root, _c, _s)) => {
                assert_ne!(forged_root, honest_root);
            }
            Err(_) => {}
        }
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
    if tampered {
        let bytes = encode_proof(&ops);
        match verify_aggregate_count_and_sum_on_range_proof(&bytes, &inner_range).unwrap() {
            Ok((forged_root, _c, _s)) => {
                assert_ne!(forged_root, honest_root);
            }
            Err(_) => {}
        }
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
