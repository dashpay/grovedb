//! `verify_query_get_parent_tree_info` and its subset twin report the
//! aggregate of the tree the query's path ends in. A result row that is
//! itself a tree, and whose lower layer is in the proof, must not replace it
//! with its own aggregate.
//!
//! A V1 proof carries such a lower layer in two cases:
//!
//! * the proof was made for a wider query that descended into the row, and is
//!   verified as a subset against a query that stops at the row;
//! * the row is a populated indexed tree, which always carries a terminal
//!   attestation layer, even in a proof made for the very same query.

#[cfg(test)]
mod tests {
    use grovedb_merk::{
        element::tree_type::ElementTreeTypeExtensions, proofs::Query, TreeFeatureType,
    };
    use grovedb_version::version::{v4::GROVE_V4, GroveVersion};

    use crate::{
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, GroveDb, PathQuery,
    };

    const PARENT: &[u8] = b"parent";
    const CHILD: &[u8] = b"child";
    const INDEXED: &[u8] = b"indexed";

    /// Both grove versions that produce V1 proofs and are in use.
    fn versions() -> [&'static GroveVersion; 2] {
        [&GROVE_V4, GroveVersion::latest()]
    }

    fn insert(db: &TempGroveDb, path: &[&[u8]], key: &[u8], element: Element, v: &GroveVersion) {
        db.insert(path, key, element, None, None, v)
            .unwrap()
            .expect("insert");
    }

    /// The aggregate the trusted read reports for the tree at `path`/`key`.
    fn trusted_tree_type(
        db: &TempGroveDb,
        path: &[&[u8]],
        key: &[u8],
        v: &GroveVersion,
    ) -> TreeFeatureType {
        db.get(path, key, None, v)
            .unwrap()
            .expect("get tree")
            .tree_feature_type()
            .expect("element is a tree")
    }

    fn key_query(path: &[&[u8]], key: &[u8]) -> PathQuery {
        let mut query = Query::new();
        query.insert_key(key.to_vec());
        PathQuery::new_unsized(path.iter().map(|segment| segment.to_vec()).collect(), query)
    }

    /// `key` and everything below it.
    fn key_with_everything_below_query(path: &[&[u8]], key: &[u8]) -> PathQuery {
        let mut query = Query::new();
        query.insert_key(key.to_vec());
        query.set_subquery(Query::new_range_full());
        PathQuery::new_unsized(path.iter().map(|segment| segment.to_vec()).collect(), query)
    }

    /// `[TEST_LEAF, parent]` is a sum tree holding `a = 5` and the sum tree
    /// `child`, which holds `x = 1000`: the parent's sum is 1005, the child's
    /// is 1000.
    fn sum_tree_holding_a_sum_tree(v: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(v);
        insert(&db, &[TEST_LEAF], PARENT, Element::empty_sum_tree(), v);
        insert(&db, &[TEST_LEAF, PARENT], b"a", Element::new_sum_item(5), v);
        insert(
            &db,
            &[TEST_LEAF, PARENT],
            CHILD,
            Element::empty_sum_tree(),
            v,
        );
        insert(
            &db,
            &[TEST_LEAF, PARENT, CHILD],
            b"x",
            Element::new_sum_item(1000),
            v,
        );
        db
    }

    /// `[TEST_LEAF, parent]` is a count tree holding the item `a` and the
    /// count tree `child`, which holds the items `x` and `y`.
    fn count_tree_holding_a_count_tree(v: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(v);
        insert(&db, &[TEST_LEAF], PARENT, Element::empty_count_tree(), v);
        insert(
            &db,
            &[TEST_LEAF, PARENT],
            b"a",
            Element::new_item(vec![1]),
            v,
        );
        insert(
            &db,
            &[TEST_LEAF, PARENT],
            CHILD,
            Element::empty_count_tree(),
            v,
        );
        for key in [b"x", b"y"] {
            insert(
                &db,
                &[TEST_LEAF, PARENT, CHILD],
                key,
                Element::new_item(vec![2]),
                v,
            );
        }
        db
    }

    /// `[TEST_LEAF, parent]` is a sum tree holding `a = 5` and a
    /// `ProvableCountIndexedTree` with three rows.
    fn sum_tree_holding_a_populated_indexed_tree(v: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(v);
        insert(&db, &[TEST_LEAF], PARENT, Element::empty_sum_tree(), v);
        insert(&db, &[TEST_LEAF, PARENT], b"a", Element::new_sum_item(5), v);
        insert(
            &db,
            &[TEST_LEAF, PARENT],
            INDEXED,
            Element::empty_provable_count_indexed_tree(),
            v,
        );
        for row in [b"r1", b"r2", b"r3"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, PARENT, INDEXED].as_ref(),
                row,
                Element::new_item(b"v".to_vec()),
                None,
                v,
            )
            .unwrap()
            .expect("insert indexed row");
        }
        db
    }

    /// Verifies `proof` against `query` as a subset and returns the reported
    /// parent aggregate, after checking the root hash and that exactly `key`
    /// came back.
    fn subset_parent_tree_type(
        db: &TempGroveDb,
        proof: &[u8],
        query: &PathQuery,
        key: &[u8],
        v: &GroveVersion,
    ) -> TreeFeatureType {
        let (root_hash, parent, rows) =
            GroveDb::verify_subset_query_get_parent_tree_info(proof, query, v)
                .expect("subset verification");
        assert_eq!(root_hash, db.root_hash(None, v).unwrap().expect("root"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, key);
        parent
    }

    #[test]
    fn subset_verify_reports_the_parent_when_a_sum_tree_row_carries_its_lower_layer() {
        for v in versions() {
            let db = sum_tree_holding_a_sum_tree(v);
            let wide = key_with_everything_below_query(&[TEST_LEAF, PARENT], CHILD);
            let proof = db.prove_query(&wide, None, v).unwrap().expect("prove");

            let narrow = key_query(&[TEST_LEAF, PARENT], CHILD);
            let parent = subset_parent_tree_type(&db, &proof, &narrow, CHILD, v);

            assert_eq!(parent, TreeFeatureType::SummedMerkNode(1005));
            assert_eq!(parent, trusted_tree_type(&db, &[TEST_LEAF], PARENT, v));
        }
    }

    #[test]
    fn subset_verify_reports_the_parent_when_a_count_tree_row_carries_its_lower_layer() {
        for v in versions() {
            let db = count_tree_holding_a_count_tree(v);
            let wide = key_with_everything_below_query(&[TEST_LEAF, PARENT], CHILD);
            let proof = db.prove_query(&wide, None, v).unwrap().expect("prove");

            let narrow = key_query(&[TEST_LEAF, PARENT], CHILD);
            let parent = subset_parent_tree_type(&db, &proof, &narrow, CHILD, v);

            let child = trusted_tree_type(&db, &[TEST_LEAF, PARENT], CHILD, v);
            assert_eq!(child, TreeFeatureType::CountedMerkNode(2));
            assert_ne!(parent, child);
            assert_eq!(parent, trusted_tree_type(&db, &[TEST_LEAF], PARENT, v));
        }
    }

    /// The same wide proof, verified against a query one level down: the walk
    /// steps into `child`, so `child` is the parent of the row it reports.
    #[test]
    fn subset_verify_reports_the_tree_the_walk_steps_into() {
        for v in versions() {
            let db = sum_tree_holding_a_sum_tree(v);
            let wide = key_with_everything_below_query(&[TEST_LEAF, PARENT], CHILD);
            let proof = db.prove_query(&wide, None, v).unwrap().expect("prove");

            let below = key_query(&[TEST_LEAF, PARENT, CHILD], b"x");
            let parent = subset_parent_tree_type(&db, &proof, &below, b"x", v);

            assert_eq!(parent, TreeFeatureType::SummedMerkNode(1000));
        }
    }

    /// No subset verification is needed here: a populated indexed tree
    /// reported as a row always carries a terminal attestation layer, so the
    /// proof below is the one the prover makes for this exact query.
    #[test]
    fn verify_reports_the_parent_when_the_row_is_a_populated_indexed_tree() {
        for v in versions() {
            let db = sum_tree_holding_a_populated_indexed_tree(v);
            let query = key_query(&[TEST_LEAF, PARENT], INDEXED);
            let proof = db.prove_query(&query, None, v).unwrap().expect("prove");

            let (root_hash, parent, rows) =
                GroveDb::verify_query_get_parent_tree_info(&proof, &query, v)
                    .expect("verification");
            assert_eq!(root_hash, db.root_hash(None, v).unwrap().expect("root"));
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].1, INDEXED);
            assert_eq!(parent, TreeFeatureType::SummedMerkNode(5));
            assert_eq!(parent, trusted_tree_type(&db, &[TEST_LEAF], PARENT, v));

            assert_eq!(
                subset_parent_tree_type(&db, &proof, &query, INDEXED, v),
                parent
            );
        }
    }

    /// A query whose path ends in the indexed tree still reports the indexed
    /// tree as the parent of its rows.
    #[test]
    fn verify_reports_the_indexed_tree_when_the_path_ends_in_it() {
        for v in versions() {
            let db = sum_tree_holding_a_populated_indexed_tree(v);
            let query = key_query(&[TEST_LEAF, PARENT, INDEXED], b"r1");
            let proof = db.prove_query(&query, None, v).unwrap().expect("prove");

            let (root_hash, parent, rows) =
                GroveDb::verify_query_get_parent_tree_info(&proof, &query, v)
                    .expect("verification");
            assert_eq!(root_hash, db.root_hash(None, v).unwrap().expect("root"));
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].1, b"r1");
            assert_eq!(parent, TreeFeatureType::ProvableCountedMerkNode(3));
            assert_eq!(
                parent,
                trusted_tree_type(&db, &[TEST_LEAF, PARENT], INDEXED, v)
            );
        }
    }
}
