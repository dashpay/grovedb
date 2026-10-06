//! A proof whose limit runs out before its query does. The walk hides what it
//! has not reached, but still passes back up through the ancestors of its
//! last result, and one of them can be a range bound. The version 0 shape
//! reveals that key behind the nodes it hid, where the verifier cannot check
//! it as a bound, so the proof fails to verify even at the prover's own
//! limit. The version 1 shape hides it, and the released verifier accepts
//! the proof. These pin both shapes and that they differ only there.

use grovedb_version::version::GroveVersion;

use super::{verify::PROOF_VERSION_LATEST, Query, QueryItem, QueryProofVerify};
use crate::{
    proofs::{encode_into, tree::execute, Decoder, Node, Op as ProofOp},
    test_utils::TempMerk,
    tree::Op,
    CryptoHash,
    TreeFeatureType::BasicMerkNode,
};

/// A merk holding the single-byte keys `0..n`, written in one batch. The
/// values are not serialized elements, so every key proves as a plain `KV`.
fn batch_merk(n: u8, grove_version: &GroveVersion) -> TempMerk {
    let mut merk = TempMerk::new(grove_version);
    let entries: Vec<(Vec<u8>, Op)> = (0..n)
        .map(|i| (vec![i], Op::Put(vec![0xee, i], BasicMerkNode)))
        .collect();
    merk.apply::<_, Vec<_>>(&entries, &[], None, grove_version)
        .unwrap()
        .expect("apply should succeed");
    merk.commit(grove_version);
    merk
}

fn query_of(items: Vec<QueryItem>, left_to_right: bool) -> Query {
    let mut query = Query::new_with_direction(left_to_right);
    for item in items {
        query.insert_item(item);
    }
    query
}

/// The encoded proof of `query` at `limit` in the shape of `proof_version`.
fn prove(
    merk: &TempMerk,
    query: &Query,
    limit: Option<u16>,
    proof_version: u16,
    grove_version: &GroveVersion,
) -> Vec<u8> {
    let (ops, _) = merk
        .prove_unchecked_query_items_for_proof_version(
            &query.items,
            limit,
            query.left_to_right,
            proof_version,
            grove_version,
        )
        .unwrap()
        .expect("prove should succeed");
    let mut bytes = vec![];
    encode_into(ops.iter(), &mut bytes);
    bytes
}

/// The keys `query` returns from a proof verified at `limit`, or the error.
fn verify(
    query: &Query,
    proof: &[u8],
    limit: Option<u16>,
) -> Result<(CryptoHash, Vec<Vec<u8>>), crate::Error> {
    query
        .execute_proof(proof, limit, query.left_to_right, PROOF_VERSION_LATEST)
        .unwrap()
        .map(|(hash, result)| {
            (
                hash,
                result.result_set.into_iter().map(|row| row.key).collect(),
            )
        })
}

/// The root a proof rebuilds, whether or not it verifies against a query.
fn rebuilt_root(proof: &[u8]) -> CryptoHash {
    execute(Decoder::new(proof), true, |_| Ok(()))
        .unwrap()
        .expect("proof should rebuild a tree")
        .hash()
        .unwrap()
}

/// The keys of the key-bearing nodes a proof pushes, in push order.
fn pushed_keys(proof: &[u8]) -> Vec<(Vec<u8>, bool)> {
    Decoder::new(proof)
        .filter_map(|op| match op.expect("proof should decode") {
            ProofOp::Push(node) | ProofOp::PushInverted(node) => match node {
                Node::KV(key, _) => Some((key, true)),
                Node::KVDigest(key, _) => Some((key, false)),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// `[2, 10)` ascending at limit 3 on 20 batch-written keys: key 10 is an
/// ancestor of the last result, 4. Version 0 reveals it after the hidden
/// keys 5..=9 and fails to verify at its own limit; version 1 hides it and
/// verifies to the first three keys. Both rebuild the merk's root.
#[test]
fn v1_hides_a_range_bound_passed_after_the_limit() {
    let v = GroveVersion::latest();
    let merk = batch_merk(20, v);
    let root = merk.root_hash().unwrap();
    let query = query_of(vec![QueryItem::Range(vec![2]..vec![10])], true);

    let v0 = prove(&merk, &query, Some(3), 0, v);
    assert_eq!(
        pushed_keys(&v0).last(),
        Some(&(vec![10], false)),
        "version 0 reveals the exclusive end as a boundary node"
    );
    let err = verify(&query, &v0, Some(3)).expect_err("version 0 shape must fail at its limit");
    assert!(
        err.to_string()
            .contains("Cannot verify lower bound of queried range"),
        "{err}"
    );

    let v1 = prove(&merk, &query, Some(3), 1, v);
    assert_eq!(
        pushed_keys(&v1),
        vec![(vec![2], true), (vec![3], true), (vec![4], true)],
        "version 1 reveals nothing after the last result"
    );
    let (hash, keys) = verify(&query, &v1, Some(3)).expect("version 1 shape verifies");
    assert_eq!(hash, root);
    assert_eq!(keys, vec![vec![2], vec![3], vec![4]]);

    assert_eq!(rebuilt_root(&v0), root);
    assert_eq!(rebuilt_root(&v1), root);

    // The existing entry points keep the version 0 shape.
    let (ops, _) = merk
        .prove_unchecked_query_items(&query.items, Some(3), true, v)
        .unwrap()
        .expect("prove should succeed");
    let mut released = vec![];
    encode_into(ops.iter(), &mut released);
    assert_eq!(released, v0);
    assert_eq!(
        merk.prove(query.clone(), Some(3), v)
            .unwrap()
            .expect("prove should succeed")
            .proof,
        v0
    );
}

/// The query shapes a limit can cut over a bound: exclusive ends in both
/// directions, an inclusive range, and a later item's bound after a cut.
fn cut_shapes(a: u8, b: u8, n: u8) -> Vec<(&'static str, Vec<QueryItem>)> {
    let mut shapes = vec![
        ("Range", vec![QueryItem::Range(vec![a]..vec![b])]),
        ("RangeTo", vec![QueryItem::RangeTo(..vec![b])]),
        ("RangeAfter", vec![QueryItem::RangeAfter(vec![a]..)]),
        (
            "RangeAfterTo",
            vec![QueryItem::RangeAfterTo(vec![a]..vec![b])],
        ),
        (
            "RangeInclusive",
            vec![QueryItem::RangeInclusive(vec![a]..=vec![b - 1])],
        ),
    ];
    if b + 1 < n {
        shapes.push((
            "Key, Range, Key, RangeAfter",
            vec![
                QueryItem::Key(vec![a]),
                QueryItem::Range(vec![a + 1]..vec![b]),
                QueryItem::Key(vec![b]),
                QueryItem::RangeAfter(vec![b + 1]..),
            ],
        ));
    }
    shapes
}

/// Every limit that cuts a walk short, over a spread of bound pairs on 20
/// and 40 keys, in both directions: the version 1 proof verifies
/// at its own limit with the released verifier and returns the first
/// `limit` matches. Some version 0 proofs of the same cuts fail, so the
/// sweep reaches the shape the fix is for.
#[test]
fn v1_limit_cut_proofs_verify_at_their_own_limit() {
    let v = GroveVersion::latest();
    let mut v0_failures = 0;
    for (n, step) in [(20u8, 2usize), (40, 7)] {
        let merk = batch_merk(n, v);
        let root = merk.root_hash().unwrap();
        for a in (0..n - 2).step_by(step) {
            for b in (a + 2..n).step_by(step) {
                for (name, items) in cut_shapes(a, b, n) {
                    for left_to_right in [true, false] {
                        let query = query_of(items.clone(), left_to_right);
                        let full = prove(&merk, &query, None, 1, v);
                        let (_, all) = verify(&query, &full, None).expect("full proof verifies");
                        for limit in 1..all.len() as u16 {
                            let proof = prove(&merk, &query, Some(limit), 1, v);
                            let (hash, keys) =
                                verify(&query, &proof, Some(limit)).unwrap_or_else(|e| {
                                    panic!("{name} {a}..{b} {left_to_right} limit {limit}: {e}")
                                });
                            assert_eq!(hash, root);
                            assert_eq!(keys, all[..limit as usize], "{name} {a}..{b} {limit}");
                            let released = prove(&merk, &query, Some(limit), 0, v);
                            if verify(&query, &released, Some(limit)).is_err() {
                                v0_failures += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(v0_failures > 0, "the sweep never reached a failing cut");
}

/// A proof its limit does not cut has the same bytes in both shapes: the
/// versions differ only past a used-up limit.
#[test]
fn proof_versions_agree_when_the_limit_does_not_cut() {
    let v = GroveVersion::latest();
    let n = 20u8;
    let merk = batch_merk(n, v);
    for a in (0..n - 2).step_by(2) {
        for b in (a + 2..n).step_by(2) {
            for (name, items) in cut_shapes(a, b, n) {
                for left_to_right in [true, false] {
                    let query = query_of(items.clone(), left_to_right);
                    let full = prove(&merk, &query, None, 1, v);
                    let (_, all) = verify(&query, &full, None).expect("full proof verifies");
                    for limit in [None, Some(all.len() as u16 + 1)] {
                        assert_eq!(
                            prove(&merk, &query, limit, 0, v),
                            prove(&merk, &query, limit, 1, v),
                            "{name} {a}..{b} {left_to_right} {limit:?}"
                        );
                    }
                }
            }
        }
    }
}
