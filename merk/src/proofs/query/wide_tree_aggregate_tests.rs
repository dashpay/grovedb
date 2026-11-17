//! Range aggregates and count offsets against a
//! `ProvableCountProvableBigSumTree`: the count, the 128-bit sum and both
//! together on a range, proved and verified against the tree's root hash,
//! read without a proof, and a count-offset page. The narrow and the wide
//! verifiers each refuse the other's proofs.

use std::collections::LinkedList;

use grovedb_version::version::GroveVersion;

use crate::{
    proofs::{
        encode_into,
        query::{
            aggregate_common::NULL_HASH,
            aggregate_count::verify_aggregate_count_on_range_proof,
            aggregate_count_and_sum::{
                verify_aggregate_count_and_big_sum_on_range_proof,
                verify_aggregate_count_and_sum_on_range_proof,
            },
            aggregate_sum::{
                verify_aggregate_big_sum_on_range_proof, verify_aggregate_sum_on_range_proof,
            },
            count_offset::verify_count_offset_on_range_proof,
            QueryItem,
        },
        Node, Op as ProofOp,
    },
    test_utils::TempMerk,
    tree::{
        Op,
        TreeFeatureType::{
            ProvableCountedAndProvableBigSummedMerkNode, ProvableCountedAndProvableSummedMerkNode,
        },
    },
    TreeType,
};

fn encode_proof(ops: &LinkedList<ProofOp>) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(256);
    encode_into(ops.iter(), &mut bytes);
    bytes
}

/// The own sum of the entry at `index`: far outside the `i64` range, with
/// alternating signs so a range sum is not a multiple of one value.
fn own_sum(index: u8) -> i128 {
    let magnitude = i128::from(i64::MAX) * 4 + i128::from(index);
    if index.is_multiple_of(3) {
        -magnitude
    } else {
        magnitude
    }
}

/// A wide tree of 25 single-byte keys `0..25`, each counting one.
fn wide_merk(grove_version: &GroveVersion) -> (TempMerk, [u8; 32]) {
    let mut merk =
        TempMerk::new_with_tree_type(grove_version, TreeType::ProvableCountProvableBigSumTree);
    let entries: Vec<(Vec<u8>, Op)> = (0u8..25)
        .map(|index| {
            (
                vec![index],
                Op::Put(
                    vec![index; 4],
                    ProvableCountedAndProvableBigSummedMerkNode(1, own_sum(index)),
                ),
            )
        })
        .collect();
    merk.apply::<_, Vec<_>>(&entries, &[], None, grove_version)
        .unwrap()
        .expect("apply the entries");
    merk.commit(grove_version);
    let root_hash = merk.root_hash().unwrap();
    (merk, root_hash)
}

/// The inner ranges the tests run: the whole tree, slices with and without
/// their ends, half-open ones, a single key, and ranges holding nothing.
fn ranges() -> Vec<(QueryItem, std::ops::Range<u8>)> {
    vec![
        (QueryItem::RangeFull(..), 0..25),
        (QueryItem::Range(vec![5]..vec![15]), 5..15),
        (QueryItem::RangeInclusive(vec![5]..=vec![15]), 5..16),
        (QueryItem::RangeFrom(vec![20]..), 20..25),
        (QueryItem::RangeTo(..vec![3]), 0..3),
        (QueryItem::RangeInclusive(vec![7]..=vec![7]), 7..8),
        (QueryItem::Range(vec![30]..vec![40]), 0..0),
        (QueryItem::Range(vec![9]..vec![9]), 0..0),
    ]
}

fn expected(keys: &std::ops::Range<u8>) -> (u64, i128) {
    (keys.len() as u64, keys.clone().map(own_sum).sum())
}

#[test]
fn wide_tree_proves_the_count_on_a_range() {
    let v = GroveVersion::latest();
    let (merk, root_hash) = wide_merk(v);
    for (range, keys) in ranges() {
        let (count, _) = expected(&keys);
        let (ops, proved) = merk
            .prove_aggregate_count_on_range(&range, v)
            .unwrap()
            .expect("prove the count");
        assert_eq!(proved, count, "{range:?}");
        let (root, verified) = verify_aggregate_count_on_range_proof(&encode_proof(&ops), &range)
            .unwrap()
            .expect("verify the count");
        assert_eq!(root, root_hash, "{range:?}");
        assert_eq!(verified, count, "{range:?}");
        assert_eq!(
            merk.count_aggregate_on_range(&range, v).unwrap().unwrap(),
            count,
            "{range:?}"
        );
    }
}

#[test]
fn wide_tree_proves_the_big_sum_on_a_range() {
    let v = GroveVersion::latest();
    let (merk, root_hash) = wide_merk(v);
    for (range, keys) in ranges() {
        let (_, sum) = expected(&keys);
        let (ops, proved) = merk
            .prove_aggregate_big_sum_on_range(&range, v)
            .unwrap()
            .expect("prove the sum");
        assert_eq!(proved, sum, "{range:?}");
        let proof = encode_proof(&ops);
        let (root, verified) = verify_aggregate_big_sum_on_range_proof(&proof, &range)
            .unwrap()
            .expect("verify the sum");
        assert_eq!(root, root_hash, "{range:?}");
        assert_eq!(verified, sum, "{range:?}");
        assert_eq!(
            merk.big_sum_aggregate_on_range(&range, v).unwrap().unwrap(),
            sum,
            "{range:?}"
        );
        // The narrow verifier takes none of the wide nodes.
        assert!(verify_aggregate_sum_on_range_proof(&proof, &range)
            .unwrap()
            .is_err());
    }
}

#[test]
fn wide_tree_proves_the_count_and_big_sum_on_a_range() {
    let v = GroveVersion::latest();
    let (merk, root_hash) = wide_merk(v);
    for (range, keys) in ranges() {
        let (count, sum) = expected(&keys);
        let (ops, proved_count, proved_sum) = merk
            .prove_aggregate_count_and_big_sum_on_range(&range, v)
            .unwrap()
            .expect("prove the count and sum");
        assert_eq!((proved_count, proved_sum), (count, sum), "{range:?}");
        let proof = encode_proof(&ops);
        let (root, verified_count, verified_sum) =
            verify_aggregate_count_and_big_sum_on_range_proof(&proof, &range)
                .unwrap()
                .expect("verify the count and sum");
        assert_eq!(root, root_hash, "{range:?}");
        assert_eq!((verified_count, verified_sum), (count, sum), "{range:?}");
        assert_eq!(
            merk.count_and_big_sum_aggregate_on_range(&range, v)
                .unwrap()
                .unwrap(),
            (count, sum),
            "{range:?}"
        );
        assert!(
            verify_aggregate_count_and_sum_on_range_proof(&proof, &range)
                .unwrap()
                .is_err()
        );
    }
}

/// The narrow entry points keep refusing the wide tree, and the wide ones
/// refuse a narrow tree and its proofs: the width is named by the function.
#[test]
fn narrow_and_wide_entry_points_refuse_each_other() {
    let v = GroveVersion::latest();
    let (wide, _) = wide_merk(v);
    let range = QueryItem::Range(vec![5]..vec![15]);

    assert!(wide
        .prove_aggregate_sum_on_range(&range, v)
        .unwrap()
        .is_err());
    assert!(wide
        .prove_aggregate_count_and_sum_on_range(&range, v)
        .unwrap()
        .is_err());
    assert!(wide.sum_aggregate_on_range(&range, v).unwrap().is_err());
    assert!(wide
        .count_and_sum_aggregate_on_range(&range, v)
        .unwrap()
        .is_err());

    let mut narrow = TempMerk::new_with_tree_type(v, TreeType::ProvableCountProvableSumTree);
    let entries: Vec<(Vec<u8>, Op)> = (0u8..25)
        .map(|index| {
            (
                vec![index],
                Op::Put(
                    vec![index; 4],
                    ProvableCountedAndProvableSummedMerkNode(1, i64::from(index)),
                ),
            )
        })
        .collect();
    narrow
        .apply::<_, Vec<_>>(&entries, &[], None, v)
        .unwrap()
        .expect("apply the entries");
    narrow.commit(v);

    assert!(narrow
        .prove_aggregate_big_sum_on_range(&range, v)
        .unwrap()
        .is_err());
    assert!(narrow
        .prove_aggregate_count_and_big_sum_on_range(&range, v)
        .unwrap()
        .is_err());
    assert!(narrow
        .big_sum_aggregate_on_range(&range, v)
        .unwrap()
        .is_err());
    assert!(narrow
        .count_and_big_sum_aggregate_on_range(&range, v)
        .unwrap()
        .is_err());

    let (ops, _) = narrow
        .prove_aggregate_sum_on_range(&range, v)
        .unwrap()
        .expect("prove the narrow sum");
    assert!(
        verify_aggregate_big_sum_on_range_proof(&encode_proof(&ops), &range)
            .unwrap()
            .is_err()
    );
    let (ops, ..) = narrow
        .prove_aggregate_count_and_sum_on_range(&range, v)
        .unwrap()
        .expect("prove the narrow count and sum");
    assert!(
        verify_aggregate_count_and_big_sum_on_range_proof(&encode_proof(&ops), &range)
            .unwrap()
            .is_err()
    );
}

/// An empty wide tree answers zero with an empty proof.
#[test]
fn empty_wide_tree_answers_zero() {
    let v = GroveVersion::latest();
    let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableCountProvableBigSumTree);
    let range = QueryItem::RangeFull(..);

    let (ops, sum) = merk
        .prove_aggregate_big_sum_on_range(&range, v)
        .unwrap()
        .expect("prove the sum");
    assert!(ops.is_empty());
    assert_eq!(sum, 0);
    assert_eq!(
        verify_aggregate_big_sum_on_range_proof(&[], &range)
            .unwrap()
            .unwrap(),
        (NULL_HASH, 0)
    );
    let (ops, count, sum) = merk
        .prove_aggregate_count_and_big_sum_on_range(&range, v)
        .unwrap()
        .expect("prove the count and sum");
    assert!(ops.is_empty());
    assert_eq!((count, sum), (0, 0));
    assert_eq!(
        verify_aggregate_count_and_big_sum_on_range_proof(&[], &range)
            .unwrap()
            .unwrap(),
        (NULL_HASH, 0, 0)
    );
    assert_eq!(
        merk.big_sum_aggregate_on_range(&range, v).unwrap().unwrap(),
        0
    );
}

/// A count or a sum altered in a proof changes the root the proof rebuilds,
/// or fails the shape checks: neither aggregate can be forged.
#[test]
fn altered_wide_aggregates_do_not_verify_against_the_root() {
    let v = GroveVersion::latest();
    let (merk, root_hash) = wide_merk(v);
    let range = QueryItem::Range(vec![5]..vec![15]);
    let (ops, _, _) = merk
        .prove_aggregate_count_and_big_sum_on_range(&range, v)
        .unwrap()
        .expect("prove the count and sum");

    let altered = |alter: &dyn Fn(u64, i128) -> (u64, i128)| -> Vec<Vec<u8>> {
        // One proof per wide node, with that node altered.
        let nodes = ops
            .iter()
            .filter(|op| matches!(op, ProofOp::Push(_)))
            .count();
        (0..nodes)
            .map(|target| {
                let mut seen = 0;
                let rewritten: LinkedList<ProofOp> = ops
                    .iter()
                    .cloned()
                    .map(|op| match op {
                        ProofOp::Push(node) => {
                            let hit = seen == target;
                            seen += 1;
                            ProofOp::Push(match node {
                                Node::HashWithCountAndBigSum(kv, l, r, c, s) if hit => {
                                    let (c, s) = alter(c, s);
                                    Node::HashWithCountAndBigSum(kv, l, r, c, s)
                                }
                                Node::KVDigestCountBigSum(k, vh, c, s) if hit => {
                                    let (c, s) = alter(c, s);
                                    Node::KVDigestCountBigSum(k, vh, c, s)
                                }
                                other => other,
                            })
                        }
                        other => other,
                    })
                    .collect();
                encode_proof(&rewritten)
            })
            .collect()
    };

    for proof in altered(&|c, s| (c, s + 1))
        .into_iter()
        .chain(altered(&|c, s| (c + 1, s)))
        .chain(altered(&|c, s| (c, s + (1i128 << 64))))
    {
        if let Ok((root, ..)) =
            verify_aggregate_count_and_big_sum_on_range_proof(&proof, &range).unwrap()
        {
            assert_ne!(root, root_hash, "a forged aggregate kept the root");
        }
    }
}

/// A proof for the range `[2, ..)` over a root at key 5 whose left subtree
/// (rooted at key 2) and right subtree each hold `i128::MAX` in the range,
/// with the root's aggregate chosen so that its own sum is `own`. Of the
/// five entries, four are in the range.
fn proof_with_sums_at_the_end_of_the_range(own: i128) -> Vec<u8> {
    let leaf = |byte: u8, sum: i128| {
        ProofOp::Push(Node::HashWithCountAndBigSum(
            [byte; 32], NULL_HASH, NULL_HASH, 1, sum,
        ))
    };
    let mut ops = LinkedList::new();
    // The left subtree: the entries below key 2 are outside the range,
    // key 2 holds 0 and the entries between 2 and 5 hold `i128::MAX`.
    ops.push_back(leaf(1, 0));
    ops.push_back(ProofOp::Push(Node::KVDigestCountBigSum(
        vec![2],
        [8; 32],
        3,
        i128::MAX,
    )));
    ops.push_back(ProofOp::Parent);
    ops.push_back(leaf(3, i128::MAX));
    ops.push_back(ProofOp::Child);
    ops.push_back(ProofOp::Push(Node::KVDigestCountBigSum(
        vec![5],
        [9; 32],
        5,
        own.wrapping_add(i128::MAX).wrapping_add(i128::MAX),
    )));
    ops.push_back(ProofOp::Parent);
    ops.push_back(leaf(6, i128::MAX));
    ops.push_back(ProofOp::Child);
    encode_proof(&ops)
}

/// A proof whose in-range sum leaves the `i128` range is refused with an
/// error instead of wrapping or panicking.
#[test]
fn wide_sum_verifier_refuses_a_sum_that_overflows_i128() {
    let range = QueryItem::RangeFrom(vec![2]..);
    let proof = proof_with_sums_at_the_end_of_the_range(1);

    let overflowed = |result: Result<(), crate::Error>| {
        matches!(
            result,
            Err(crate::Error::InvalidProofError(message)) if message.contains("overflowed i128")
        )
    };
    assert!(overflowed(
        verify_aggregate_big_sum_on_range_proof(&proof, &range)
            .unwrap()
            .map(|_| ())
    ));
    assert!(overflowed(
        verify_aggregate_count_and_big_sum_on_range_proof(&proof, &range)
            .unwrap()
            .map(|_| ())
    ));
}

/// The verifiers add a node's sums in the prover's order: left, own, right.
/// Sums that cancel (`MAX - MAX + MAX`) stay in range in that order, where
/// adding the two children first would overflow.
#[test]
fn wide_sum_verifier_adds_in_the_order_of_the_prover() {
    let range = QueryItem::RangeFrom(vec![2]..);
    let proof = proof_with_sums_at_the_end_of_the_range(-i128::MAX);

    let (_, sum) = verify_aggregate_big_sum_on_range_proof(&proof, &range)
        .unwrap()
        .expect("the sum stays in range");
    assert_eq!(sum, i128::MAX);
    let (_, count, sum) = verify_aggregate_count_and_big_sum_on_range_proof(&proof, &range)
        .unwrap()
        .expect("the sum stays in range");
    assert_eq!((count, sum), (4, i128::MAX));
}

/// A count-offset page of a wide tree returns the rows after the skipped
/// ones and verifies against the root, in both directions.
#[test]
fn wide_tree_proves_a_count_offset_page() {
    let v = GroveVersion::latest();
    let (merk, root_hash) = wide_merk(v);
    let range = QueryItem::RangeFull(..);

    for (left_to_right, expected_keys) in [
        (true, vec![7u8, 8, 9, 10, 11]),
        (false, vec![17u8, 16, 15, 14, 13]),
    ] {
        let proved = merk
            .prove_count_offset_on_range(&range, 7, Some(5), left_to_right, v)
            .unwrap()
            .expect("prove the page");
        assert_eq!(proved.returned, 5);
        assert_eq!(proved.offset_remaining, 0);
        let verified = verify_count_offset_on_range_proof(
            &encode_proof(&proved.ops),
            &range,
            7,
            Some(5),
            left_to_right,
        )
        .unwrap()
        .expect("verify the page");
        assert_eq!(verified.root_hash, root_hash);
        assert_eq!(verified.skipped, 7);
        assert_eq!(
            verified
                .returned_items
                .iter()
                .map(|item| item.key[0])
                .collect::<Vec<_>>(),
            expected_keys
        );
    }
}
