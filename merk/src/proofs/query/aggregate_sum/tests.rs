//! Unit + integration tests for the aggregate-sum prover/verifier.
//!
//! Split out of the legacy single-file `aggregate_sum.rs` along with the
//! prover/walker/verifier when the module became a directory. The body
//! is byte-identical to the previous in-file `mod tests { ... }` block;
//! only the `use super::*;` line at the top expanded into explicit
//! imports from the new sub-modules so the test bodies can reach the
//! private helpers (`walk_sum_only`, `classify_subtree`, etc.) they
//! were already exercising.

use std::collections::LinkedList;

use grovedb_version::version::GroveVersion;

use super::{
    is_provable_sum_bearing, provable_sum_from_aggregate, verify_aggregate_sum_on_range_proof,
};
use crate::{
    proofs::{
        encode_into,
        query::{
            aggregate_common::{
                classify_subtree, key_strictly_inside, SubtreeClassification, NULL_HASH,
            },
            QueryItem,
        },
        Node, Op as ProofOp,
    },
    test_utils::TempMerk,
    tree::{AggregateData, Op, TreeFeatureType::ProvableSummedMerkNode},
    Error, Merk, TreeType,
};

fn range_inclusive(lo: &[u8], hi: &[u8]) -> QueryItem {
    QueryItem::RangeInclusive(lo.to_vec()..=hi.to_vec())
}

fn range_full() -> QueryItem {
    QueryItem::RangeFull(std::ops::RangeFull)
}

#[test]
fn classify_disjoint_below_sum() {
    let r = range_inclusive(b"d", b"f");
    assert_eq!(
        classify_subtree(None, Some(b"c"), &r),
        SubtreeClassification::Disjoint,
    );
}

#[test]
fn classify_contained_full_range_full_subtree_sum() {
    let r = range_full();
    assert_eq!(
        classify_subtree(None, None, &r),
        SubtreeClassification::Contained,
    );
}

#[test]
fn classify_boundary_overlapping_lower_sum() {
    let r = range_inclusive(b"d", b"f");
    assert_eq!(
        classify_subtree(Some(b"c"), Some(b"e"), &r),
        SubtreeClassification::Boundary,
    );
}

// ---------- end-to-end integration tests on a real merk ----------

/// Build a fresh `ProvableSumTree` populated with single-byte keys
/// "a".."o" (15 keys), each carrying sum 1, 2, ..., 15 respectively.
/// Returns the merk and its current root hash.
fn make_15_key_provable_sum_tree(grove_version: &GroveVersion) -> (TempMerk, [u8; 32]) {
    let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::ProvableSumTree);
    let keys: Vec<Vec<u8>> = (b'a'..=b'o').map(|c| vec![c]).collect();
    let entries: Vec<(Vec<u8>, Op)> = keys
        .iter()
        .enumerate()
        .map(|(i, k)| {
            let s = (i as i64) + 1;
            (k.clone(), Op::Put(vec![i as u8], ProvableSummedMerkNode(s)))
        })
        .collect();
    merk.apply::<_, Vec<_>>(&entries, &[], None, grove_version)
        .unwrap()
        .expect("apply should succeed");
    merk.commit(grove_version);
    let root_hash = merk.root_hash().unwrap();
    (merk, root_hash)
}

/// Encode a `LinkedList<Op>` into the wire format.
fn encode_proof(ops: &LinkedList<ProofOp>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(128);
    encode_into(ops.iter(), &mut bytes);
    bytes
}

/// Round-trip: prove → encode → verify, assert root + sum match.
fn round_trip(
    merk: &Merk<impl grovedb_storage::StorageContext<'static>>,
    expected_root: [u8; 32],
    inner_range: QueryItem,
    expected_sum: i64,
    grove_version: &GroveVersion,
) {
    let (ops, prover_sum) = merk
        .prove_aggregate_sum_on_range(&inner_range, grove_version)
        .unwrap()
        .expect("prove should succeed");
    assert_eq!(
        prover_sum, expected_sum,
        "prover sum mismatch for range {:?}",
        inner_range
    );
    let bytes = encode_proof(&ops);
    let (root, verifier_sum) = verify_aggregate_sum_on_range_proof(&bytes, &inner_range)
        .unwrap()
        .expect("verify should succeed");
    assert_eq!(
        root, expected_root,
        "verifier reconstructed wrong root for range {:?}",
        inner_range
    );
    assert_eq!(
        verifier_sum, expected_sum,
        "verifier sum mismatch for range {:?}",
        inner_range
    );
}

#[test]
fn integration_full_range_sum_of_1_to_15() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_sum_tree(v);
    // Full range with RangeFrom("a"..) — sum = 1+2+...+15 = 120.
    round_trip(&merk, root, QueryItem::RangeFrom(b"a".to_vec()..), 120, v);
}

#[test]
fn integration_closed_range_inclusive_sum() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_sum_tree(v);
    // Keys "c"..="l" → values 3..=12 → sum = 75.
    round_trip(
        &merk,
        root,
        QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        75,
        v,
    );
}

#[test]
fn integration_range_below_all_keys_sum() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_sum_tree(v);
    round_trip(
        &merk,
        root,
        QueryItem::RangeInclusive(vec![0x00]..=vec![0x10]),
        0,
        v,
    );
}

#[test]
fn integration_range_above_all_keys_sum() {
    let v = GroveVersion::latest();
    let (merk, root) = make_15_key_provable_sum_tree(v);
    round_trip(
        &merk,
        root,
        QueryItem::RangeInclusive(b"z".to_vec()..=vec![0xff]),
        0,
        v,
    );
}

#[test]
fn integration_empty_merk_sum() {
    let v = GroveVersion::latest();
    let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableSumTree);
    let (ops, prover_sum) = merk
        .prove_aggregate_sum_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
        .unwrap()
        .expect("prove on empty merk should succeed");
    assert_eq!(prover_sum, 0);
    let bytes = encode_proof(&ops);
    let (root, verifier_sum) = verify_aggregate_sum_on_range_proof(
        &bytes,
        &QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
    )
    .unwrap()
    .expect("verify on empty merk should succeed");
    assert_eq!(root, NULL_HASH);
    assert_eq!(verifier_sum, 0);
}

#[test]
fn integration_rejected_on_normal_tree() {
    let v = GroveVersion::latest();
    let merk = TempMerk::new(v);
    let err = merk
        .prove_aggregate_sum_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
        .unwrap();
    assert!(
        err.is_err(),
        "expected InvalidProofError on NormalTree, got Ok({:?})",
        err.ok().map(|(_, s)| s)
    );
}

#[test]
fn integration_rejected_on_provable_count_tree() {
    // ProvableSumTree-only — count trees use a different hash dispatch
    // and are not valid input here.
    let v = GroveVersion::latest();
    let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableCountTree);
    let err = merk
        .prove_aggregate_sum_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
        .unwrap();
    assert!(
        err.is_err(),
        "expected InvalidProofError on ProvableCountTree, got Ok"
    );
}

#[test]
fn integration_sum_forgery_is_rejected() {
    // Tamper with a HashWithSum's sum field — the verifier's root-hash
    // recomputation must diverge from the expected root.
    let v = GroveVersion::latest();
    let (merk, expected_root) = make_15_key_provable_sum_tree(v);
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (mut ops, _prover_sum) = merk
        .prove_aggregate_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove should succeed");

    let mut tampered = false;
    for op in ops.iter_mut() {
        if let ProofOp::Push(Node::HashWithSum(_, _, _, sum))
        | ProofOp::PushInverted(Node::HashWithSum(_, _, _, sum)) = op
        {
            *sum = sum.saturating_add(1);
            tampered = true;
            break;
        }
    }
    assert!(tampered, "test setup: expected at least one HashWithSum op");

    let bytes = encode_proof(&ops);
    let (root, _sum) = verify_aggregate_sum_on_range_proof(&bytes, &inner_range)
        .unwrap()
        .expect("verify should still complete (root mismatch is the caller's job)");
    assert_ne!(
        root, expected_root,
        "tampered sum must produce a different reconstructed root hash"
    );
}

#[test]
fn shape_walk_rejects_single_hash_undercount_sum() {
    let v = GroveVersion::latest();
    let (merk, expected_root) = make_15_key_provable_sum_tree(v);
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());

    // Forged proof: a single Hash op carrying the genuine root hash.
    let mut forged: LinkedList<ProofOp> = LinkedList::new();
    forged.push_back(ProofOp::Push(Node::Hash(expected_root)));
    let bytes = encode_proof(&forged);

    let result = verify_aggregate_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("single-Hash forgery must be rejected");
    let _ = merk;
    match err {
        Error::InvalidProofError(msg) => {
            assert!(
                msg.contains("unexpected node type")
                    || msg.contains("expected KVDigestSum")
                    || msg.contains("Boundary"),
                "unexpected message: {msg}"
            );
        }
        other => panic!("expected InvalidProofError, got {other:?}"),
    }
}

#[test]
fn shape_walk_rejects_disjoint_hashwithsum_with_children() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_sum_tree(v);
    let inner_range = QueryItem::RangeAfter(b"o".to_vec()..);
    let (mut ops, _) = merk
        .prove_aggregate_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

    let mut spliced = LinkedList::<ProofOp>::new();
    let mut done = false;
    for op in ops.iter() {
        spliced.push_back(op.clone());
        if !done && matches!(op, ProofOp::Push(Node::HashWithSum(_, _, _, _))) {
            spliced.push_back(ProofOp::Push(Node::HashWithSum(
                [0u8; 32], [0u8; 32], [0u8; 32], 1,
            )));
            spliced.push_back(ProofOp::Parent);
            done = true;
        }
    }
    assert!(done, "test setup: expected at least one HashWithSum op");
    ops = spliced;

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("Disjoint HashWithSum with children must be rejected");
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("Disjoint position must be a leaf"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// Regular `Merk::prove` on a `ProvableSumTree` must emit the sum-bearing
/// proof node variants. Queried items yield `KVSum` (via `to_kv_sum_node`),
/// non-queried path nodes yield `KVHashSum` (via `to_kvhash_sum_node`).
/// This exercises the sum-node helper functions whose only callers are
/// inside `create_proof_internal`.
#[test]
fn regular_prove_on_provable_sum_tree_emits_kv_sum_and_kvhash_sum() {
    use crate::proofs::{query::Query, Decoder, Node, Op as ProofOp};

    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_sum_tree(v);

    // Query a few keys, leaving most unqueried so we get both queried
    // (KVSum) and path (KVHashSum) nodes.
    let mut q = Query::new();
    q.insert_key(b"a".to_vec());
    q.insert_key(b"h".to_vec()); // middle
    q.insert_key(b"o".to_vec());

    let proof_result = merk.prove(q, None, v).unwrap().expect("regular prove");
    let proof_bytes = proof_result.proof;

    let ops: Vec<ProofOp> = Decoder::new(&proof_bytes)
        .collect::<Result<Vec<_>, _>>()
        .expect("decode");

    let mut saw_kvsum = false;
    let mut saw_kvhashsum = false;
    for op in &ops {
        match op {
            ProofOp::Push(node) | ProofOp::PushInverted(node) => match node {
                Node::KVSum(..) => saw_kvsum = true,
                Node::KVHashSum(..) => saw_kvhashsum = true,
                _ => {}
            },
            _ => {}
        }
    }
    assert!(
        saw_kvsum,
        "expected at least one KVSum node from queried Items on a ProvableSumTree"
    );
    assert!(
        saw_kvhashsum,
        "expected at least one KVHashSum node on the proof path"
    );
}

/// Querying an out-of-range absent key on a `ProvableSumTree` must emit a
/// boundary `KVDigestSum` node — i.e. the result of `to_kvdigest_sum_node`.
/// We do this on a single-key tree so that one of the absence-flank keys
/// IS on the tree's boundary, forcing the `on_boundary_not_found` branch.
#[test]
fn regular_prove_on_provable_sum_tree_emits_kvdigest_sum() {
    use crate::proofs::{query::Query, Decoder, Node, Op as ProofOp};

    let v = GroveVersion::latest();
    let mut merk = TempMerk::new_with_tree_type(v, TreeType::ProvableSumTree);
    // Single-key tree: querying any absent key forces a boundary emission.
    merk.apply::<_, Vec<_>>(
        &[(b"m".to_vec(), Op::Put(vec![0], ProvableSummedMerkNode(7)))],
        &[],
        None,
        v,
    )
    .unwrap()
    .expect("apply");
    merk.commit(v);

    let mut q = Query::new();
    q.insert_key(b"zz".to_vec()); // absent, above the single key
    let proof_result = merk.prove(q, None, v).unwrap().expect("regular prove");
    let ops: Vec<ProofOp> = Decoder::new(&proof_result.proof)
        .collect::<Result<Vec<_>, _>>()
        .expect("decode");

    let saw_kvdigestsum = ops.iter().any(|op| {
        matches!(
            op,
            ProofOp::Push(Node::KVDigestSum(..)) | ProofOp::PushInverted(Node::KVDigestSum(..))
        )
    });
    assert!(
        saw_kvdigestsum,
        "expected KVDigestSum boundary node for absent-key proof, got ops: {:?}",
        ops
    );
}

/// Two i64::MAX children sum to 2*i64::MAX, which exceeds i64. The
/// verifier's final i64-narrowing check must surface this as a
/// proof-error. This exercises the i128 accumulator + overflow gate.
#[test]
fn integration_overflow_at_i64_max_is_rejected() {
    let v = GroveVersion::latest();
    let mut merk = TempMerk::new_with_tree_type(v, TreeType::ProvableSumTree);
    // Two children, each i64::MAX. Sum exceeds i64::MAX.
    let entries: Vec<(Vec<u8>, Op)> = vec![
        (
            b"a".to_vec(),
            Op::Put(vec![0], ProvableSummedMerkNode(i64::MAX)),
        ),
        (
            b"b".to_vec(),
            Op::Put(vec![0], ProvableSummedMerkNode(i64::MAX)),
        ),
    ];
    // Insertion itself may or may not succeed depending on the apply
    // path's intermediate-overflow handling. Skip if not; this scenario
    // is additionally exercised at the verify layer via fabricated
    // proofs.
    if merk
        .apply::<_, Vec<_>>(&entries, &[], None, v)
        .unwrap()
        .is_err()
    {
        return;
    }
    merk.commit(v);
    let inner_range = QueryItem::RangeFrom(b"a".to_vec()..);
    let result = merk.prove_aggregate_sum_on_range(&inner_range, v).unwrap();
    // Either the prover detects the overflow during its narrowing pass,
    // or it produces a proof whose verifier-side narrowing catches it.
    // Both are acceptable end states for this safety net.
    match result {
        Err(_) => { /* prover-side overflow detection — done */ }
        Ok((ops, _)) => {
            let bytes = encode_proof(&ops);
            let v_result = verify_aggregate_sum_on_range_proof(&bytes, &inner_range).unwrap();
            assert!(
                v_result.is_err(),
                "verifier must reject an i128-sized sum that doesn't fit in i64"
            );
        }
    }
}

// ---------- no-proof variant: sum_aggregate_on_range ----------
//
// The no-proof entry point must return exactly the same sum as the
// proof path for every range shape, without producing any proof ops.
// These tests cross-check the two paths on the same merk and also
// cover the failure modes unique to the no-proof variant (wrong tree
// type, empty merk, overflow narrowing).

/// Cross-check: assert `sum_aggregate_on_range` and the sum returned
/// by `prove_aggregate_sum_on_range` agree for the given range, and
/// that both equal `expected_sum`.
fn no_proof_sum_matches_prover(
    merk: &Merk<impl grovedb_storage::StorageContext<'static>>,
    inner_range: QueryItem,
    expected_sum: i64,
    grove_version: &GroveVersion,
) {
    let no_proof = merk
        .sum_aggregate_on_range(&inner_range, grove_version)
        .unwrap()
        .expect("sum_aggregate_on_range should succeed");
    assert_eq!(
        no_proof, expected_sum,
        "no-proof variant returned wrong sum for range {:?}",
        inner_range
    );
    let (_ops, prover_sum) = merk
        .prove_aggregate_sum_on_range(&inner_range, grove_version)
        .unwrap()
        .expect("prove should succeed");
    assert_eq!(
        no_proof, prover_sum,
        "no-proof variant disagrees with prover sum for range {:?}",
        inner_range
    );
}

#[test]
fn no_proof_sum_matches_prover_closed_range_inclusive() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_sum_tree(v);
    // sums for keys c..=l are 3..=12 → 75
    no_proof_sum_matches_prover(
        &merk,
        QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
        75,
        v,
    );
}

#[test]
fn no_proof_sum_matches_prover_closed_range_exclusive() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_sum_tree(v);
    // sums for keys c..l are 3..=11 → 63
    no_proof_sum_matches_prover(&merk, QueryItem::Range(b"c".to_vec()..b"l".to_vec()), 63, v);
}

#[test]
fn no_proof_sum_matches_prover_open_range_from() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_sum_tree(v);
    // c..o → 3+4+...+15 = 117
    no_proof_sum_matches_prover(&merk, QueryItem::RangeFrom(b"c".to_vec()..), 117, v);
}

#[test]
fn no_proof_sum_matches_prover_range_after() {
    // RangeAfter at the root pushes the left boundary exclusive to
    // "b", exercising the right-child arm of walk_sum_only.
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_sum_tree(v);
    no_proof_sum_matches_prover(&merk, QueryItem::RangeAfter(b"b".to_vec()..), 117, v);
}

#[test]
fn no_proof_sum_matches_prover_range_to_inclusive() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_sum_tree(v);
    // ..=e → 1+2+3+4+5 = 15
    no_proof_sum_matches_prover(&merk, QueryItem::RangeToInclusive(..=b"e".to_vec()), 15, v);
}

#[test]
fn no_proof_sum_matches_prover_range_below_all_keys() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_sum_tree(v);
    no_proof_sum_matches_prover(
        &merk,
        QueryItem::RangeInclusive(vec![0x00]..=vec![0x10]),
        0,
        v,
    );
}

#[test]
fn no_proof_sum_empty_merk_returns_zero() {
    let v = GroveVersion::latest();
    let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableSumTree);
    let sum = merk
        .sum_aggregate_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
        .unwrap()
        .expect("sum_aggregate_on_range on empty merk should succeed");
    assert_eq!(sum, 0);
}

#[test]
fn no_proof_sum_rejected_on_normal_tree() {
    let v = GroveVersion::latest();
    let merk = TempMerk::new(v); // NormalTree
    let result = merk
        .sum_aggregate_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
        .unwrap();
    assert!(
        result.is_err(),
        "expected InvalidProofError on NormalTree, got Ok({:?})",
        result.ok()
    );
}

#[test]
fn no_proof_sum_rejected_on_provable_count_tree() {
    // Sum variant must reject ProvableCountTree too (precise tree-type
    // match), parallel to the verify-side terminal-type gate.
    let v = GroveVersion::latest();
    let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableCountTree);
    let result = merk
        .sum_aggregate_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
        .unwrap();
    assert!(
        result.is_err(),
        "expected InvalidProofError on ProvableCountTree for a sum query, got Ok({:?})",
        result.ok()
    );
}

// ---------- Unit tests for helper-function error paths --------------
//
// These exercise small internal helpers that the integration tests
// can only reach indirectly. Each one pins a specific Err-classification
// arm so that future refactors can't silently drop the diagnostic.

#[test]
fn provable_sum_from_aggregate_rejects_non_provable_sum_variants() {
    // Cover every non-`ProvableSum` arm of `provable_sum_from_aggregate`.
    // The fallback "other" arm should fire for each.
    let cases = [
        AggregateData::NoAggregateData,
        AggregateData::Sum(5),
        AggregateData::BigSum(5),
        AggregateData::Count(5),
        AggregateData::CountAndSum(2, 3),
        AggregateData::ProvableCount(5),
        AggregateData::ProvableCountAndSum(2, 3),
    ];
    for case in cases {
        let result = provable_sum_from_aggregate(case);
        match result {
            Err(Error::CorruptedData(msg)) => {
                assert!(
                    msg.contains("expected ProvableSum"),
                    "wrong message for {:?}: {msg}",
                    case
                );
            }
            other => panic!("expected CorruptedData for {:?}, got {:?}", case, other),
        }
    }
}

#[test]
fn provable_sum_from_aggregate_accepts_provable_sum() {
    // Sanity: the happy-path arm preserves the inner value (including
    // negative values).
    assert_eq!(
        provable_sum_from_aggregate(AggregateData::ProvableSum(0)).unwrap(),
        0
    );
    assert_eq!(
        provable_sum_from_aggregate(AggregateData::ProvableSum(-42)).unwrap(),
        -42
    );
    assert_eq!(
        provable_sum_from_aggregate(AggregateData::ProvableSum(i64::MAX)).unwrap(),
        i64::MAX
    );
    assert_eq!(
        provable_sum_from_aggregate(AggregateData::ProvableSum(i64::MIN)).unwrap(),
        i64::MIN
    );
}

/// `provable_sum_from_aggregate` also accepts the dual-axis variant —
/// it extracts the sum from a `ProvableCountAndProvableSum(_, sum)`.
#[test]
fn provable_sum_from_aggregate_accepts_dual_axis_variant() {
    assert_eq!(
        provable_sum_from_aggregate(AggregateData::ProvableCountAndProvableSum(7, -42)).unwrap(),
        -42
    );
    assert_eq!(
        provable_sum_from_aggregate(AggregateData::ProvableCountAndProvableSum(
            u64::MAX,
            i64::MAX
        ))
        .unwrap(),
        i64::MAX
    );
    assert_eq!(
        provable_sum_from_aggregate(AggregateData::ProvableCountAndProvableSum(0, i64::MIN))
            .unwrap(),
        i64::MIN
    );
}

#[test]
fn is_provable_sum_bearing_for_provable_sum_tree_and_pcps() {
    // Both ProvableSumTree (sum-only) and ProvableCountProvableSumTree
    // (dual-axis) bind the sum into their node hash and therefore
    // accept AggregateSumOnRange proofs.
    assert!(is_provable_sum_bearing(TreeType::ProvableSumTree));
    assert!(is_provable_sum_bearing(
        TreeType::ProvableCountProvableSumTree
    ));
    // Every other variant is rejected.
    for t in [
        TreeType::NormalTree,
        TreeType::SumTree,
        TreeType::BigSumTree,
        TreeType::CountTree,
        TreeType::CountSumTree,
        TreeType::ProvableCountTree,
        TreeType::ProvableCountSumTree,
        TreeType::CommitmentTree(0),
        TreeType::MmrTree,
        TreeType::BulkAppendTree(0),
        TreeType::DenseAppendOnlyFixedSizeTree(0),
    ] {
        assert!(!is_provable_sum_bearing(t), "false expected for {:?}", t);
    }
}

// ---------- ProvableCountProvableSumTree (dual-axis) tests ----------

/// Build a fresh `ProvableCountProvableSumTree` populated with single-byte
/// keys "a".."o" (15 keys), each carrying count=1 and sum=(i+1). Returns
/// the merk and root hash.
fn make_15_key_provable_count_provable_sum_tree(
    grove_version: &GroveVersion,
) -> (TempMerk, [u8; 32]) {
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
        .expect("apply ProvableCountProvableSumTree entries");
    merk.commit(grove_version);
    let root_hash = merk.root_hash().unwrap();
    (merk, root_hash)
}

/// Aggregate-sum proof against `ProvableCountProvableSumTree` round-trips.
/// Same shape as `single_key_provable_sum_tree_round_trip` for the
/// sum-only host, but the emitter dispatches dual-axis variants
/// (`HashWithCountAndSum`, `KVDigestCountSum`) and the verifier
/// reconstructs `node_hash_with_count_and_sum`.
#[test]
fn integration_sum_proof_against_pcps_round_trips() {
    let v = GroveVersion::latest();
    let (merk, expected_root) = make_15_key_provable_count_provable_sum_tree(v);
    // c..=l → sums 3+4+5+6+7+8+9+10+11+12 = 75
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (ops, prover_sum) = merk
        .prove_aggregate_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove sum on PCPS should succeed");
    assert_eq!(prover_sum, 75, "c..=l sum should be 75");
    let bytes = encode_proof(&ops);
    let (root, verifier_sum) = verify_aggregate_sum_on_range_proof(&bytes, &inner_range)
        .unwrap()
        .expect("verify sum proof on PCPS should succeed");
    assert_eq!(root, expected_root);
    assert_eq!(verifier_sum, 75);
}

/// Disjoint-leaf rejection on the dual-axis sum side. Mirrors
/// `shape_walk_rejects_disjoint_hashwithsum_with_children` for the
/// sum-only host.
#[test]
fn shape_walk_rejects_disjoint_hashwithcountandsum_with_children_pcps() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_count_provable_sum_tree(v);
    let inner_range = QueryItem::RangeAfter(b"o".to_vec()..);
    let (mut ops, _) = merk
        .prove_aggregate_sum_on_range(&inner_range, v)
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
    let result = verify_aggregate_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result
        .expect_err("spliced child under Disjoint HashWithCountAndSum (sum side) must be rejected");
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("Disjoint position must be a leaf")
                || msg.contains("at a Disjoint position"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

#[test]
fn classify_subtree_disjoint_above_sum() {
    // Subtree entirely above the range → Disjoint. Mirror of
    // classify_disjoint_below_sum.
    let r = range_inclusive(b"d", b"f");
    assert_eq!(
        classify_subtree(Some(b"g"), None, &r),
        SubtreeClassification::Disjoint,
    );
}

#[test]
fn classify_subtree_boundary_overlapping_upper_sum() {
    let r = range_inclusive(b"d", b"f");
    assert_eq!(
        classify_subtree(Some(b"e"), Some(b"h"), &r),
        SubtreeClassification::Boundary,
    );
}

#[test]
fn classify_subtree_contained_within_inclusive_sum() {
    // Subtree (b, c] with range [a..=z] → Contained.
    let r = range_inclusive(b"a", b"z");
    assert_eq!(
        classify_subtree(Some(b"b"), Some(b"c"), &r),
        SubtreeClassification::Contained,
    );
}

#[test]
fn key_strictly_inside_handles_unbounded_endpoints() {
    // -inf lower bound: any key > None is true.
    assert!(key_strictly_inside(b"a", None, Some(b"z")));
    // +inf upper bound: any key < None is true.
    assert!(key_strictly_inside(b"z", Some(b"a"), None));
    // Both unbounded: trivially true.
    assert!(key_strictly_inside(b"m", None, None));
    // Strictly outside lo.
    assert!(!key_strictly_inside(b"a", Some(b"a"), None));
    assert!(!key_strictly_inside(b"a", Some(b"z"), None));
    // Strictly outside hi.
    assert!(!key_strictly_inside(b"z", None, Some(b"z")));
    assert!(!key_strictly_inside(b"z", None, Some(b"a")));
}

#[test]
fn empty_provable_sum_tree_proof_round_trip() {
    // Hits the "empty merk" branch of `prove_aggregate_sum_on_range`
    // (the no-proof side has its own test; this is the prover side).
    let v = GroveVersion::latest();
    let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableSumTree);
    let (ops, sum) = merk
        .prove_aggregate_sum_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
        .unwrap()
        .expect("prove on empty merk should succeed");
    assert_eq!(sum, 0);
    // The empty-merk proof should verify to (NULL_HASH, 0).
    let bytes = encode_proof(&ops);
    let (_root, verified) = verify_aggregate_sum_on_range_proof(
        &bytes,
        &QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
    )
    .unwrap()
    .expect("verify on empty proof should succeed");
    assert_eq!(verified, 0);
}

#[test]
fn no_proof_sum_with_negative_values_matches_prover() {
    // A tree with mixed positive and negative sum items must yield the
    // same net sum from both the no-proof and proof paths.
    let v = GroveVersion::latest();
    let mut merk = TempMerk::new_with_tree_type(v, TreeType::ProvableSumTree);
    let entries: [(&[u8], i64); 4] = [(b"a", 50), (b"b", -100), (b"c", 30), (b"d", -50)];
    let ops: Vec<(Vec<u8>, Op)> = entries
        .iter()
        .map(|(k, val)| (k.to_vec(), Op::Put(vec![], ProvableSummedMerkNode(*val))))
        .collect();
    merk.apply::<_, Vec<_>>(&ops, &[], None, v)
        .unwrap()
        .expect("apply mixed-sign items");
    merk.commit(v);
    // Full range → 50 − 100 + 30 − 50 = −70
    no_proof_sum_matches_prover(&merk, QueryItem::RangeFrom(b"a".to_vec()..), -70, v);
    // Subrange b..=c → −100 + 30 = −70
    no_proof_sum_matches_prover(
        &merk,
        QueryItem::RangeInclusive(b"b".to_vec()..=b"c".to_vec()),
        -70,
        v,
    );
}

// ---------- Additional negative-path coverage for verify_sum_shape ----------
//
// These tests target the rejection arms inside `verify_sum_shape`
// (single-axis HashWithSum/KVDigestSum + dual-axis HashWithCountAndSum/
// KVDigestCountSum) that aren't otherwise exercised by happy-path
// round-trips. Each test handcrafts a minimal proof to land cleanly
// on the targeted Phase-2 arm without tripping Phase 1's
// reconstruction checks (key ordering, balance, etc.).

/// At a Contained position the sum-side shape walk requires
/// `HashWithSum` or `HashWithCountAndSum`. A `KVDigestSum` (boundary
/// node type) there must hit the "expected HashWithSum or
/// HashWithCountAndSum at Contained position" rejection arm.
#[test]
fn shape_walk_rejects_non_hashwithsum_at_contained() {
    let inner_range = QueryItem::RangeFull(std::ops::RangeFull);
    let mut ops = LinkedList::<ProofOp>::new();
    ops.push_back(ProofOp::Push(Node::KVDigestSum(
        b"d".to_vec(),
        [0u8; 32],
        7,
    )));
    let bytes = encode_proof(&ops);

    let result = verify_aggregate_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("non-HashWithSum at Contained must be rejected");
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("expected HashWithSum") && msg.contains("Contained"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// At a Disjoint position the sum-side shape walk requires
/// `HashWithSum` or `HashWithCountAndSum`. Build a two-level proof so
/// the boundary root's left child is classified Disjoint, then place
/// a `KVDigestSum` (boundary type) at that Disjoint leaf to trigger
/// the "expected HashWithSum ... at Disjoint position" rejection.
#[test]
fn shape_walk_rejects_non_hashwithsum_at_disjoint() {
    // Range [n, +∞) means: parent boundary at key "m" classifies its
    // left subtree (bounds (-∞, m)) as Disjoint (everything below
    // "m" is below "n").
    let inner_range = QueryItem::RangeFrom(b"n".to_vec()..);
    let mut ops = LinkedList::<ProofOp>::new();
    // Op::Parent semantics: the LAST-pushed op becomes parent and the
    // PREVIOUSLY-pushed op becomes its left child. So to build
    //         m            (root, bounds (None, None) — Boundary)
    //        /
    //       a              (left child, bounds (-∞, m) — Disjoint
    //                       against range [n, +∞))
    // we push the LEFT child first, then the root, then `Parent`.
    //
    // The left child position is bounds (-∞, m) and range [n, +∞);
    // (-∞, m) is entirely below n, so Disjoint. Putting a KVDigestSum
    // there (wrong type for Disjoint) trips the Disjoint arm's
    // "expected HashWithSum ..." rejection.
    ops.push_back(ProofOp::Push(Node::KVDigestSum(
        b"a".to_vec(),
        [0u8; 32],
        0,
    )));
    ops.push_back(ProofOp::Push(Node::KVDigestSum(
        b"m".to_vec(),
        [0u8; 32],
        0,
    )));
    ops.push_back(ProofOp::Parent);
    let bytes = encode_proof(&ops);

    let result = verify_aggregate_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("non-HashWithSum at Disjoint must be rejected");
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("expected HashWithSum") && msg.contains("Disjoint"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// Counterpart for the dual-axis (PCPS) Contained arm. Crafting a
/// single-op proof with a `KVDigestCountSum` at the Contained-root
/// position lands directly on the dual-axis "expected HashWithSum or
/// HashWithCountAndSum at Contained position" arm.
#[test]
fn shape_walk_rejects_non_hashwithcountandsum_at_contained_pcps() {
    let inner_range = QueryItem::RangeFull(std::ops::RangeFull);
    let mut ops = LinkedList::<ProofOp>::new();
    ops.push_back(ProofOp::Push(Node::KVDigestCountSum(
        b"d".to_vec(),
        [0u8; 32],
        1,
        7,
    )));
    let bytes = encode_proof(&ops);

    let result = verify_aggregate_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("KVDigestCountSum at Contained must be rejected");
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("expected HashWithSum") && msg.contains("Contained"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// `verify_sum_shape` requires Contained-classified `HashWithSum` nodes
/// to be leaves. Splicing a dummy child under the Contained
/// `HashWithSum` exercises the Contained-side leaf check.
#[test]
fn shape_walk_rejects_contained_hashwithsum_with_children() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_sum_tree(v);
    // Full range → root Contained.
    let inner_range = QueryItem::RangeFrom(b"a".to_vec()..);
    let (mut ops, _) = merk
        .prove_aggregate_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

    let mut spliced = LinkedList::<ProofOp>::new();
    let mut done = false;
    for op in ops.iter() {
        spliced.push_back(op.clone());
        if !done && matches!(op, ProofOp::Push(Node::HashWithSum(_, _, _, _))) {
            spliced.push_back(ProofOp::Push(Node::HashWithSum(
                [0u8; 32], [0u8; 32], [0u8; 32], 0,
            )));
            spliced.push_back(ProofOp::Parent);
            done = true;
        }
    }
    assert!(done, "test setup: expected at least one HashWithSum op");
    ops = spliced;

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("Contained HashWithSum with children must be rejected");
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("Contained position must be a leaf"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// Dual-axis counterpart — splicing children under a Contained-position
/// `HashWithCountAndSum` exercises the dual-axis Contained leaf check
/// from the sum-side verifier.
#[test]
fn shape_walk_rejects_contained_hashwithcountandsum_with_children_pcps_sum() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_count_provable_sum_tree(v);
    let inner_range = QueryItem::RangeFrom(b"a".to_vec()..);
    let (mut ops, _) = merk
        .prove_aggregate_sum_on_range(&inner_range, v)
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
    let result = verify_aggregate_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err(
        "Contained HashWithCountAndSum with children must be rejected (sum side, dual-axis)",
    );
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("Contained position must be a leaf"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// At a Boundary position the sum-side shape walk requires
/// `KVDigestSum` or `KVDigestCountSum`. A `HashWithSum` there must
/// trip the "expected KVDigestSum or KVDigestCountSum at Boundary
/// position" rejection arm.
#[test]
fn shape_walk_rejects_non_kvdigestsum_at_boundary() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_sum_tree(v);
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (mut ops, _) = merk
        .prove_aggregate_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

    // Swap the first KVDigestSum (a boundary node) with a HashWithSum
    // (the Contained/Disjoint leaf type). Phase 1's allowlist accepts
    // both; Phase 2's shape walk must reject the mismatch.
    let mut swapped = false;
    for op in ops.iter_mut() {
        if let ProofOp::Push(Node::KVDigestSum(_, _, sum)) = op {
            *op = ProofOp::Push(Node::HashWithSum([0u8; 32], [0u8; 32], [0u8; 32], *sum));
            swapped = true;
            break;
        }
    }
    assert!(swapped, "test setup: expected a KVDigestSum op to swap");

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_sum_on_range_proof(&bytes, &inner_range).unwrap();
    let err = result.expect_err("non-KVDigestSum at Boundary must be rejected");
    match err {
        Error::InvalidProofError(msg) => assert!(
            msg.contains("expected KVDigestSum") && msg.contains("Boundary"),
            "unexpected message: {msg}"
        ),
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// Sum-side counterpart of
/// `shape_walk_rejects_kvdigestcount_outside_inherited_bounds`. A
/// `KVDigestSum` whose key is outside its inherited (lo, hi) bounds
/// triggers the "boundary key ... falls outside its inherited subtree
/// bounds" arm.
#[test]
fn shape_walk_rejects_kvdigestsum_outside_inherited_bounds() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_sum_tree(v);
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (mut ops, _) = merk
        .prove_aggregate_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove succeeds");

    // Rewrite the first KVDigestSum's key to one beyond any in-tree
    // value. Phase 1's reconstruction passes (single key, no ordering
    // conflict), but Phase 2's `key_strictly_inside` check fires.
    let mut rewrote = false;
    for op in ops.iter_mut() {
        if let ProofOp::Push(Node::KVDigestSum(key, _, _)) = op {
            *key = vec![0xff, 0xff];
            rewrote = true;
            break;
        }
    }
    assert!(rewrote, "test setup: expected a KVDigestSum to rewrite");

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_sum_on_range_proof(&bytes, &inner_range).unwrap();
    // The rewrite can either trip Phase 1's key-ordering check or
    // Phase 2's inherited-bounds check, depending on where the
    // KVDigestSum sat in the proof. Either rejection path counts —
    // the goal is that an out-of-bounds boundary key never produces
    // a successful verification.
    let err = result.expect_err("KVDigestSum outside inherited bounds must be rejected");
    match err {
        Error::InvalidProofError(_) => {}
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// Dual-axis Boundary key violating its inherited bounds. Counterpart
/// of the count-side `shape_walk_rejects_kvdigestcountsum_outside_inherited_bounds_pcps`
/// — exercises the same arm from the sum-side verifier.
#[test]
fn shape_walk_rejects_kvdigestcountsum_outside_inherited_bounds_pcps_sum() {
    let v = GroveVersion::latest();
    let (merk, _root) = make_15_key_provable_count_provable_sum_tree(v);
    let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
    let (mut ops, _) = merk
        .prove_aggregate_sum_on_range(&inner_range, v)
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
        "test setup: expected a KVDigestCountSum op to rewrite"
    );

    let bytes = encode_proof(&ops);
    let result = verify_aggregate_sum_on_range_proof(&bytes, &inner_range).unwrap();
    // Either Phase 1's key-ordering check fires (now that
    // execute_with_options also enforces BST-order on dual-axis
    // nodes) or Phase 2's inherited-bounds check does. Both are
    // acceptable rejections.
    let err =
        result.expect_err("KVDigestCountSum outside inherited bounds must be rejected (sum side)");
    match err {
        Error::InvalidProofError(_) => {}
        other => panic!("expected InvalidProofError, got {:?}", other),
    }
}

/// The sum verifier narrows its i128 accumulator to i64 at the very
/// end. A `verify_aggregate_sum_on_range_proof` call against an
/// `integration_overflow_at_i64_max_is_rejected`-style adversarial
/// tree already exercises the narrow on the rejection side; this
/// test instead exercises the i64 narrow on the SUCCESS side, by
/// proving against a tree whose total in-range sum is exactly
/// i64::MAX (no overflow) and confirming the verifier returns
/// i64::MAX without error.
#[test]
fn verify_sum_narrows_i128_to_i64_at_max_boundary() {
    let v = GroveVersion::latest();
    let mut merk = TempMerk::new_with_tree_type(v, TreeType::ProvableSumTree);
    // Two entries whose net sum is exactly i64::MAX. This forces
    // the narrow to succeed at the boundary.
    let entries: [(&[u8], i64); 2] = [(b"a", i64::MAX - 1), (b"b", 1)];
    let apply_ops: Vec<(Vec<u8>, Op)> = entries
        .iter()
        .map(|(k, val)| (k.to_vec(), Op::Put(vec![], ProvableSummedMerkNode(*val))))
        .collect();
    merk.apply::<_, Vec<_>>(&apply_ops, &[], None, v)
        .unwrap()
        .expect("apply");
    merk.commit(v);
    let root = merk.root_hash().unwrap();

    let inner_range = QueryItem::RangeFrom(b"a".to_vec()..);
    let (ops, prover_sum) = merk
        .prove_aggregate_sum_on_range(&inner_range, v)
        .unwrap()
        .expect("prove");
    assert_eq!(prover_sum, i64::MAX);
    let bytes = encode_proof(&ops);
    let (verifier_root, verifier_sum) = verify_aggregate_sum_on_range_proof(&bytes, &inner_range)
        .unwrap()
        .expect("verify");
    assert_eq!(verifier_root, root);
    assert_eq!(verifier_sum, i64::MAX);
}
