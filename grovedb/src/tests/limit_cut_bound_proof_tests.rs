//! A limit that cuts a range walk short must not make an honest proof fail
//! to verify at that same limit.
//!
//! Once its limit runs out the merk prover hides the rest of the walk, but it
//! still passes back up through the ancestors of its last result. When one
//! of them is a range bound (an exclusive range end, or a later item's
//! bound), the V0 proof shape reveals it behind the nodes it hid, where the
//! verifier cannot check it as a bound, and verification fails with "Cannot
//! verify lower bound of queried range". Whether that happens depends on the
//! tree's shape; it is common for subtrees written in one batch, the way
//! Platform writes. V1 proofs hide the key instead. The verifier is
//! unchanged, so the V1 proof verifies under every grove version that reads
//! V1 proofs, including the released GROVE_V3. V0 proofs (grove v1 and v2)
//! keep their shape.

use grovedb_merk::proofs::{query::QueryItem, Query};
use grovedb_version::version::{v2::GROVE_V2, v3::GROVE_V3, GroveVersion};

use crate::{
    batch::QualifiedGroveDbOp,
    tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
    Element, GroveDb, PathQuery, SizedQuery,
};

/// The grove versions that prove and verify V1 proofs.
fn v1_proof_versions() -> [&'static GroveVersion; 2] {
    [&GROVE_V3, GroveVersion::latest()]
}

/// A test grove with the items `0..n` under `TEST_LEAF`, written in one
/// batch.
fn batch_written_items(n: u8, v: &GroveVersion) -> TempGroveDb {
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
    db
}

/// The range walks a limit can cut over an exclusive bound.
fn exclusive_bound_cases(mid: u8) -> [(&'static str, QueryItem, bool); 4] {
    [
        ("Range asc", QueryItem::Range(vec![2]..vec![mid]), true),
        ("RangeTo asc", QueryItem::RangeTo(..vec![mid]), true),
        (
            "RangeAfterTo asc",
            QueryItem::RangeAfterTo(vec![1]..vec![mid]),
            true,
        ),
        ("RangeAfter desc", QueryItem::RangeAfter(vec![mid]..), false),
    ]
}

/// Every limit that cuts the walk short round-trips through `prove_query`
/// and `verify_query`, returning the first `limit` matches in walk order,
/// for exclusive bounds walked ascending and descending over subtrees
/// written in one batch.
#[test]
fn limit_cut_walk_over_exclusive_bound_verifies() {
    for v in v1_proof_versions() {
        for n in [20u8, 100] {
            let db = batch_written_items(n, v);
            let root = db.grove_db.root_hash(None, v).unwrap().expect("root hash");
            for (name, item, left_to_right) in exclusive_bound_cases(n / 2) {
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
                        .unwrap_or_else(|e| panic!("{name}, {n} keys, limit {limit}, {v:?}: {e}"));
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
}

/// The same through a subquery: the limit counts the leaf rows, so it cuts
/// the outer layer's walk, whose range has the exclusive bound. One row per
/// outer key, written in one batch.
#[test]
fn limit_cut_outer_layer_of_subquery_verifies() {
    for v in v1_proof_versions() {
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
                    .unwrap_or_else(|e| panic!("{name}, limit {limit}, {v:?}: {e}"));
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
}

/// V0 proofs keep the shape they shipped with: under GROVE_V2 the same
/// limit-cut query still reveals the bound and fails to verify at its own
/// limit, exactly as released.
#[test]
fn v0_proofs_keep_their_released_shape() {
    let v = &GROVE_V2;
    let db = batch_written_items(20, v);
    let mut query = Query::new_with_direction(true);
    query.insert_item(QueryItem::Range(vec![2]..vec![10]));
    let path_query = PathQuery::new(
        vec![TEST_LEAF.to_vec()],
        SizedQuery::new(query, Some(1), None),
    );
    let proof = db
        .grove_db
        .prove_query(&path_query, None, v)
        .unwrap()
        .expect("prove_query");
    let err = GroveDb::verify_query(&proof, &path_query, v)
        .expect_err("the released V0 shape is unchanged");
    assert!(
        err.to_string()
            .contains("Cannot verify lower bound of queried range"),
        "{err}"
    );
}

/// A provable-sum indexed tree whose secondary index is written in one
/// batch, with the empty primary key at sum 10 and distinct keys elsewhere,
/// and the ascending bounded read over `[2, 9]` at limit 3. The bound lowers
/// to an exclusive upper bound on the secondary key of sum 10, which sits at
/// the secondary tree's root, so a limit-cut secondary proof in the V0 shape
/// would reveal it behind hidden nodes.
fn limit_cut_bounded_axis_fixture(v: &GroveVersion) -> (TempGroveDb, PathQuery) {
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

/// The indexed-axis prover replays its own secondary proof to build row
/// chains. With the bound revealed, that replay rejected the proof and
/// aborted `prove_query`; with the V1 shape the bounded read proves and
/// verifies.
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
