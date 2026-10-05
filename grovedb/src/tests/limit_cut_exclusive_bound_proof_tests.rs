//! A limit that cuts a range walk short must not make an honest proof fail
//! to verify at that same limit.
//!
//! Once its limit runs out the prover hides the rest of the walk, but it can
//! still reveal a boundary key it passed (an exclusive range end), behind
//! the nodes it hid. Whether that happens depends on the tree's shape: it is
//! the norm for subtrees written in one batch, the way Platform writes, and
//! rare for subtrees built one insert at a time. The merk verifier checked
//! that key as a range bound and rejected the proof with "Cannot verify lower
//! bound of queried range". From GROVE_V4
//! (`merk_versions.proof.execute_proof_limit_reached_tail: 1`) it accepts the
//! proof; GROVE_V3 keeps rejecting it.

use grovedb_merk::proofs::{query::QueryItem, Query};
use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

use crate::{
    batch::QualifiedGroveDbOp,
    tests::{make_test_grovedb, TEST_LEAF},
    Element, GroveDb, PathQuery, SizedQuery,
};

/// Every limit that cuts the walk short round-trips through `prove_query`
/// and `verify_query`, returning the first `limit` matches in walk order,
/// for exclusive bounds walked ascending and descending over subtrees
/// written in one batch.
#[test]
fn limit_cut_walk_over_exclusive_bound_verifies() {
    let v = GroveVersion::latest();
    for n in [20u8, 100] {
        let db = make_test_grovedb(v);
        let ops = (0..n)
            .map(|i| {
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    vec![i],
                    Element::new_item(vec![i]),
                )
            })
            .collect();
        db.apply_batch(ops, None, None, v)
            .unwrap()
            .expect("apply batch");
        let root = db.grove_db.root_hash(None, v).unwrap().expect("root hash");

        let mid = n / 2;
        let cases = [
            ("Range asc", QueryItem::Range(vec![2]..vec![mid]), true),
            ("RangeTo asc", QueryItem::RangeTo(..vec![mid]), true),
            (
                "RangeAfterTo asc",
                QueryItem::RangeAfterTo(vec![1]..vec![mid]),
                true,
            ),
            ("RangeAfter desc", QueryItem::RangeAfter(vec![mid]..), false),
        ];
        for (name, item, left_to_right) in cases {
            let mut matches: Vec<u8> = (0..n).filter(|k| item.contains(&[*k])).collect();
            if !left_to_right {
                matches.reverse();
            }
            for limit in 1..matches.len() as u16 {
                let mut query = Query::new_with_direction(left_to_right);
                query.insert_item(item.clone());
                let path_query = PathQuery::new(
                    vec![TEST_LEAF.to_vec()],
                    SizedQuery::new(query, Some(limit), None),
                );
                let proof = db
                    .grove_db
                    .prove_query(&path_query, None, v)
                    .unwrap()
                    .expect("prove_query");
                let (hash, results) = GroveDb::verify_query(&proof, &path_query, v)
                    .unwrap_or_else(|e| panic!("{name}, {n} keys, limit {limit}: {e}"));
                assert_eq!(hash, root);
                let keys: Vec<u8> = results.iter().map(|(_, key, _)| key[0]).collect();
                assert_eq!(
                    keys,
                    matches[..limit as usize],
                    "{name}, {n} keys, limit {limit}"
                );
            }
        }
    }
}

/// The same through a subquery: the limit counts the leaf rows, so it cuts
/// the outer layer's walk, whose range has the exclusive bound. One row per
/// outer key, written in one batch.
#[test]
fn limit_cut_outer_layer_of_subquery_verifies() {
    let v = GroveVersion::latest();
    let n = 40u8;
    let db = make_test_grovedb(v);
    let mut ops = Vec::new();
    for i in 0..n {
        ops.push(QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            vec![i],
            Element::empty_tree(),
        ));
        ops.push(QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), vec![i]],
            b"row".to_vec(),
            Element::new_item(vec![i]),
        ));
    }
    db.apply_batch(ops, None, None, v)
        .unwrap()
        .expect("apply batch");
    let root = db.grove_db.root_hash(None, v).unwrap().expect("root hash");

    let mid = n / 2;
    let cases = [
        ("RangeTo asc", QueryItem::RangeTo(..vec![mid]), true),
        ("RangeAfter desc", QueryItem::RangeAfter(vec![mid]..), false),
    ];
    for (name, item, left_to_right) in cases {
        let mut matches: Vec<u8> = (0..n).filter(|k| item.contains(&[*k])).collect();
        if !left_to_right {
            matches.reverse();
        }
        for limit in 1..matches.len() as u16 {
            let mut query = Query::new_with_direction(left_to_right);
            query.insert_item(item.clone());
            query.set_subquery(Query::new_range_full());
            let path_query = PathQuery::new(
                vec![TEST_LEAF.to_vec()],
                SizedQuery::new(query, Some(limit), None),
            );
            let proof = db
                .grove_db
                .prove_query(&path_query, None, v)
                .unwrap()
                .expect("prove_query");
            let (hash, results) = GroveDb::verify_query(&proof, &path_query, v)
                .unwrap_or_else(|e| panic!("{name}, limit {limit}: {e}"));
            assert_eq!(hash, root);
            let outer: Vec<u8> = results
                .iter()
                .map(|(path, key, _)| {
                    assert_eq!(key.as_slice(), b"row");
                    path.last().expect("row sits under an outer key")[0]
                })
                .collect();
            assert_eq!(outer, matches[..limit as usize], "{name}, limit {limit}");
        }
    }
}

/// The fix is gated on the grove version: a limit-cut proof made under
/// GROVE_V3 is still rejected when verified under GROVE_V3 and accepted under
/// GROVE_V4.
#[test]
fn limit_cut_acceptance_is_gated_on_grove_version() {
    let v = GroveVersion::latest();
    let db = make_test_grovedb(v);
    let ops = (0..20u8)
        .map(|i| {
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                vec![i],
                Element::new_item(vec![i]),
            )
        })
        .collect();
    db.apply_batch(ops, None, None, v)
        .unwrap()
        .expect("apply batch");

    let mut query = Query::new();
    query.insert_item(QueryItem::Range(vec![2]..vec![10]));
    let path_query = PathQuery::new(
        vec![TEST_LEAF.to_vec()],
        SizedQuery::new(query, Some(3), None),
    );
    let proof = db
        .grove_db
        .prove_query(&path_query, None, &GROVE_V3)
        .unwrap()
        .expect("prove_query under GROVE_V3");

    let err = GroveDb::verify_query(&proof, &path_query, &GROVE_V3)
        .expect_err("GROVE_V3 keeps rejecting the limit-cut proof");
    assert!(
        err.to_string().contains("Cannot verify lower bound"),
        "{err}"
    );
    let (_, results) = GroveDb::verify_query(&proof, &path_query, v).expect("GROVE_V4 accepts it");
    let keys: Vec<u8> = results.iter().map(|(_, key, _)| key[0]).collect();
    assert_eq!(keys, vec![2, 3, 4]);
}

/// A provable-sum indexed tree holding sums 0..19, written in one batch, with
/// the empty primary key at sum 10 and distinct keys elsewhere, and the
/// ascending bounded read over `[2, 9]` at limit 3. The bound lowers to an
/// exclusive upper bound on the secondary key of sum 10, which sits at the
/// secondary tree's root, so the limit-cut secondary proof reveals it behind
/// hidden nodes.
fn limit_cut_bounded_axis_fixture(v: &GroveVersion) -> (crate::tests::TempGroveDb, PathQuery) {
    use grovedb_merk::proofs::query::IndexAxis;

    let db = make_test_grovedb(v);
    db.insert(
        [TEST_LEAF].as_ref(),
        b"psit",
        Element::empty_provable_sum_indexed_tree(),
        None,
        None,
        v,
    )
    .unwrap()
    .expect("create PSIT");
    let ops = (0..20i64)
        .map(|sum| {
            let key = if sum == 10 {
                Vec::new()
            } else {
                format!("k{sum:02}").into_bytes()
            };
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"psit".to_vec()],
                key,
                Element::new_sum_item(sum),
            )
        })
        .collect();
    db.apply_batch(ops, None, None, v)
        .unwrap()
        .expect("apply batch");
    let path_query = PathQuery::new_axis_bounded(
        vec![TEST_LEAF.to_vec(), b"psit".to_vec()],
        IndexAxis::Sum,
        2,
        9,
        3,
        false,
    );
    (db, path_query)
}

/// The prover replays the limit-cut secondary proof to build its row
/// chains. Under GROVE_V4 the replay accepts it, as the verifier does, and
/// the bounded read proves and verifies.
#[test]
fn limit_cut_bounded_axis_read_proves_and_verifies() {
    use crate::operations::proof::{indexed_axis::AxisEntries, VerifiedPathQuery};

    let v = GroveVersion::latest();
    let (db, path_query) = limit_cut_bounded_axis_fixture(v);
    let root = db.grove_db.root_hash(None, v).unwrap().expect("root hash");
    let proof = db
        .grove_db
        .prove_query(&path_query, None, v)
        .unwrap()
        .expect("prove_query of the limit-cut bounded read");
    match GroveDb::verify_path_query(&proof, &path_query, v).expect("bounded read verifies") {
        VerifiedPathQuery::AxisEntries {
            root_hash, entries, ..
        } => {
            assert_eq!(root_hash, root);
            let AxisEntries::Sum(entries) = entries else {
                panic!("expected sum entries, got {entries:?}");
            };
            let sums: Vec<i64> = entries.iter().map(|e| e.ordering_value).collect();
            assert_eq!(sums, vec![2, 3, 4]);
        }
        other => panic!("expected AxisEntries, got {other:?}"),
    }
}

/// The prover's replay follows the same version slot as the verifier: with
/// the slot at 0 it keeps the released replay, which rejects this proof and
/// aborts `prove_query`, and an unknown slot version is refused.
#[test]
fn limit_cut_axis_replay_follows_the_version_slot() {
    let mut released = GroveVersion::latest().clone();
    released
        .merk_versions
        .proof
        .execute_proof_limit_reached_tail = 0;
    let (db, path_query) = limit_cut_bounded_axis_fixture(&released);
    let err = db
        .grove_db
        .prove_query(&path_query, None, &released)
        .unwrap()
        .expect_err("the released replay rejects the limit-cut proof");
    assert!(
        err.to_string().contains("replaying the secondary proof"),
        "{err}"
    );

    let mut unknown = GroveVersion::latest().clone();
    unknown.merk_versions.proof.execute_proof_limit_reached_tail = 2;
    let err = db
        .grove_db
        .prove_query(&path_query, None, &unknown)
        .unwrap()
        .expect_err("an unknown slot version is refused");
    assert!(err.to_string().contains("build_row_target_chains"), "{err}");
}
