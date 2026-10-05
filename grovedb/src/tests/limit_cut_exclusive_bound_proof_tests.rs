//! A limit that cuts a range walk short must not make an honest proof fail
//! to verify at that same limit.
//!
//! Once its limit runs out the prover hides the rest of the walk, but it can
//! still reveal a boundary key it passed (an exclusive range end), behind
//! the nodes it hid. Whether that happens depends on the tree's shape: it is
//! the norm for subtrees written in one batch, the way Platform writes, and
//! rare for subtrees built one insert at a time. The merk verifier used to
//! check that key as a range bound and reject the proof with "Cannot verify
//! lower bound of queried range".

use grovedb_merk::proofs::{query::QueryItem, Query};
use grovedb_version::version::GroveVersion;

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
