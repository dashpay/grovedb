//! Differential tests for the non-axis arms of
//! [`GroveDb::verify_path_query`], the unified verification dispatch:
//! for every shape it routes, the unified answer must equal the answer
//! of the dedicated `verify_*` entry point it delegates to, over the
//! same proof. The axis arms live in `axis_descent_proof_tests`.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::{query::query_item::QueryItem, Query};
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::proof::{SumBudgetStop, VerifiedPathQuery},
        tests::{make_test_grovedb, TEST_LEAF},
        Element, Error, GroveDb, PathQuery, SizedQuery,
    };

    // -----------------------------------------------------------------
    // Fixtures
    // -----------------------------------------------------------------

    /// A provable count tree at `[TEST_LEAF, name]` holding `keys`
    /// plain items.
    fn build_pct(db: &GroveDb, name: &[u8], keys: &[&[u8]], grove_version: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            name,
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create provable count tree");
        for key in keys {
            db.insert(
                [TEST_LEAF, name].as_ref(),
                key,
                Element::new_item(b"v".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert counted item");
        }
    }

    /// A provable sum tree at `[TEST_LEAF, name]` holding `entries`.
    fn build_pst(
        db: &GroveDb,
        name: &[u8],
        entries: &[(&[u8], i64)],
        grove_version: &GroveVersion,
    ) {
        db.insert(
            [TEST_LEAF].as_ref(),
            name,
            Element::empty_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create provable sum tree");
        for (key, sum) in entries {
            db.insert(
                [TEST_LEAF, name].as_ref(),
                key,
                Element::new_sum_item(*sum),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum item");
        }
    }

    /// A provable count + provable sum tree at `[TEST_LEAF, name]`.
    fn build_pcps(
        db: &GroveDb,
        name: &[u8],
        entries: &[(&[u8], i64)],
        grove_version: &GroveVersion,
    ) {
        db.insert(
            [TEST_LEAF].as_ref(),
            name,
            Element::empty_provable_count_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPS tree");
        for (key, sum) in entries {
            db.insert(
                [TEST_LEAF, name].as_ref(),
                key,
                Element::new_sum_item(*sum),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCPS sum item");
        }
    }

    fn full_range() -> QueryItem {
        QueryItem::Range(b"a".to_vec()..b"z".to_vec())
    }

    fn prove(db: &GroveDb, path_query: &PathQuery, grove_version: &GroveVersion) -> Vec<u8> {
        db.prove_query(path_query, None, grove_version)
            .unwrap()
            .expect("prove path query")
    }

    // -----------------------------------------------------------------
    // Aggregate leaves: one aggregate over one range in one tree
    // -----------------------------------------------------------------

    #[test]
    fn aggregate_leaf_count_verifies_through_the_unified_entry() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pct(&db, b"pct", &[b"a", b"b", b"c"], grove_version);

        let pq = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"pct".to_vec()],
            full_range(),
        );
        let proof = prove(&db, &pq, grove_version);

        let (dedicated_root, dedicated_count) =
            GroveDb::verify_aggregate_count_query(&proof, &pq, grove_version)
                .expect("dedicated count verify");
        match GroveDb::verify_path_query(&proof, &pq, grove_version).expect("unified count verify")
        {
            VerifiedPathQuery::AggregateCount { root_hash, count } => {
                assert_eq!(root_hash, dedicated_root);
                assert_eq!(count, dedicated_count);
                assert_eq!(count, 3);
            }
            other => panic!("expected AggregateCount, got {other:?}"),
        }
    }

    #[test]
    fn aggregate_leaf_sum_verifies_through_the_unified_entry() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pst(
            &db,
            b"pst",
            &[(b"a", 5), (b"b", -2), (b"c", 11)],
            grove_version,
        );

        let pq = PathQuery::new_aggregate_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"pst".to_vec()],
            full_range(),
        );
        let proof = prove(&db, &pq, grove_version);

        let (dedicated_root, dedicated_sum) =
            GroveDb::verify_aggregate_sum_query(&proof, &pq, grove_version)
                .expect("dedicated sum verify");
        match GroveDb::verify_path_query(&proof, &pq, grove_version).expect("unified sum verify") {
            VerifiedPathQuery::AggregateSum { root_hash, sum } => {
                assert_eq!(root_hash, dedicated_root);
                assert_eq!(sum, dedicated_sum);
                assert_eq!(sum, 14);
            }
            other => panic!("expected AggregateSum, got {other:?}"),
        }
    }

    #[test]
    fn aggregate_leaf_count_and_sum_verifies_through_the_unified_entry() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcps(&db, b"pcps", &[(b"a", 7), (b"b", 3)], grove_version);

        let pq = PathQuery::new_aggregate_count_and_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"pcps".to_vec()],
            full_range(),
        );
        let proof = prove(&db, &pq, grove_version);

        let (dedicated_root, dedicated_count, dedicated_sum) =
            GroveDb::verify_aggregate_count_and_sum_query(&proof, &pq, grove_version)
                .expect("dedicated combined verify");
        match GroveDb::verify_path_query(&proof, &pq, grove_version)
            .expect("unified combined verify")
        {
            VerifiedPathQuery::AggregateCountAndSum {
                root_hash,
                count,
                sum,
            } => {
                assert_eq!(root_hash, dedicated_root);
                assert_eq!((count, sum), (dedicated_count, dedicated_sum));
                assert_eq!((count, sum), (2, 10));
            }
            other => panic!("expected AggregateCountAndSum, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Aggregate carriers: one aggregate per matched outer key
    // -----------------------------------------------------------------

    /// Outer keys at `TEST_LEAF`, each an aggregate leaf underneath.
    fn carrier_path_query(outer_keys: &[&[u8]], subquery: Query) -> PathQuery {
        let mut carrier = Query::new();
        for key in outer_keys {
            carrier.insert_key(key.to_vec());
        }
        carrier.set_subquery(subquery);
        PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], carrier)
    }

    #[test]
    fn aggregate_carrier_count_verifies_through_the_unified_entry() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pct(&db, b"one", &[b"a", b"b"], grove_version);
        build_pct(&db, b"two", &[b"a", b"b", b"c"], grove_version);

        let pq = carrier_path_query(
            &[b"one", b"two"],
            Query::new_aggregate_count_on_range(full_range()),
        );
        let proof = prove(&db, &pq, grove_version);

        let (dedicated_root, dedicated) =
            GroveDb::verify_aggregate_count_query_per_key(&proof, &pq, grove_version)
                .expect("dedicated per-key count verify");
        match GroveDb::verify_path_query(&proof, &pq, grove_version)
            .expect("unified per-key count verify")
        {
            VerifiedPathQuery::AggregatePerKey { root_hash, per_key } => {
                assert_eq!(root_hash, dedicated_root);
                // The carrier arm widens `(key, count)` into
                // `(key, Some(count), None)` — sums are not proved here.
                assert_eq!(
                    per_key,
                    dedicated
                        .into_iter()
                        .map(|(key, count)| (key, Some(count), None))
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    per_key,
                    vec![
                        (b"one".to_vec(), Some(2), None),
                        (b"two".to_vec(), Some(3), None),
                    ]
                );
            }
            other => panic!("expected AggregatePerKey, got {other:?}"),
        }
    }

    #[test]
    fn aggregate_carrier_sum_verifies_through_the_unified_entry() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pst(&db, b"one", &[(b"a", 5), (b"b", -2)], grove_version);
        build_pst(&db, b"two", &[(b"a", 11)], grove_version);

        let pq = carrier_path_query(
            &[b"one", b"two"],
            Query::new_aggregate_sum_on_range(full_range()),
        );
        let proof = prove(&db, &pq, grove_version);

        let (dedicated_root, dedicated) =
            GroveDb::verify_aggregate_sum_query_per_key(&proof, &pq, grove_version)
                .expect("dedicated per-key sum verify");
        match GroveDb::verify_path_query(&proof, &pq, grove_version)
            .expect("unified per-key sum verify")
        {
            VerifiedPathQuery::AggregatePerKey { root_hash, per_key } => {
                assert_eq!(root_hash, dedicated_root);
                // Sum carriers report `count = None`.
                assert_eq!(
                    per_key,
                    dedicated
                        .into_iter()
                        .map(|(key, sum)| (key, None, Some(sum)))
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    per_key,
                    vec![
                        (b"one".to_vec(), None, Some(3)),
                        (b"two".to_vec(), None, Some(11)),
                    ]
                );
            }
            other => panic!("expected AggregatePerKey, got {other:?}"),
        }
    }

    #[test]
    fn aggregate_carrier_count_and_sum_verifies_through_the_unified_entry() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcps(&db, b"one", &[(b"a", 7), (b"b", 3)], grove_version);
        build_pcps(&db, b"two", &[(b"a", -4)], grove_version);

        let pq = carrier_path_query(
            &[b"one", b"two"],
            Query::new_aggregate_count_and_sum_on_range(full_range()),
        );
        let proof = prove(&db, &pq, grove_version);

        let (dedicated_root, dedicated) =
            GroveDb::verify_aggregate_count_and_sum_query_per_key(&proof, &pq, grove_version)
                .expect("dedicated per-key combined verify");
        match GroveDb::verify_path_query(&proof, &pq, grove_version)
            .expect("unified per-key combined verify")
        {
            VerifiedPathQuery::AggregatePerKey { root_hash, per_key } => {
                assert_eq!(root_hash, dedicated_root);
                assert_eq!(
                    per_key,
                    dedicated
                        .into_iter()
                        .map(|(key, count, sum)| (key, Some(count), Some(sum)))
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    per_key,
                    vec![
                        (b"one".to_vec(), Some(2), Some(10)),
                        (b"two".to_vec(), Some(1), Some(-4)),
                    ]
                );
            }
            other => panic!("expected AggregatePerKey, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Key selection, count-offset pagination, and the unproved shape
    // -----------------------------------------------------------------

    #[test]
    fn count_offset_paginated_verifies_through_the_unified_entry() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pct(&db, b"pct", &[b"a", b"b", b"c", b"d"], grove_version);

        // A non-zero offset over a single range item is the count-offset
        // paginated shape — a distinct classification from plain key
        // selection, sharing the unified `Elements` arm.
        let pq = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"pct".to_vec()],
            SizedQuery::new(
                Query::new_single_query_item(QueryItem::RangeFull(..)),
                Some(2),
                Some(1),
            ),
        );
        let proof = prove(&db, &pq, grove_version);

        let (dedicated_root, dedicated) =
            GroveDb::verify_query(&proof, &pq, grove_version).expect("dedicated verify");
        match GroveDb::verify_path_query(&proof, &pq, grove_version)
            .expect("unified paginated verify")
        {
            VerifiedPathQuery::Elements {
                root_hash,
                elements,
            } => {
                assert_eq!(root_hash, dedicated_root);
                assert_eq!(elements, dedicated);
                assert_eq!(elements.len(), 2);
            }
            other => panic!("expected Elements, got {other:?}"),
        }
    }

    #[test]
    fn sum_budget_verifies_through_the_unified_entry() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pst(&db, b"pst", &[(b"a", 5), (b"b", 7)], grove_version);

        let pq = PathQuery::new_sum_budget(
            vec![TEST_LEAF.to_vec(), b"pst".to_vec()],
            vec![QueryItem::RangeFull(..)],
            true,
            10,
            None,
        );
        let proof = prove(&db, &pq, grove_version);

        // The proved window must agree with the trusted read over the
        // same state.
        let direct = db
            .run_path_query(
                &pq,
                true,
                true,
                true,
                crate::query_result_type::QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("trusted sum-budget read");
        let crate::operations::get::PathQueryRun::SumBudget(direct) = direct else {
            panic!("expected PathQueryRun::SumBudget, got {direct:?}");
        };

        match GroveDb::verify_path_query(&proof, &pq, grove_version)
            .expect("unified sum-budget verify")
        {
            VerifiedPathQuery::SumBudget {
                root_hash,
                matches,
                total,
                stop,
            } => {
                assert_eq!(
                    root_hash,
                    db.root_hash(None, grove_version).unwrap().expect("root")
                );
                // Budget 10 over 5 then 7: the walk stops on the entry
                // that crosses the limit, so both are matched.
                assert_eq!(matches, vec![(b"a".to_vec(), 5i64), (b"b".to_vec(), 7i64)]);
                assert_eq!(total, 12);
                assert_eq!(matches, direct.results);
                // 5 + 7 crosses the budget of 10 on the second entry.
                assert_eq!(stop, SumBudgetStop::BudgetReached);
            }
            other => panic!("expected SumBudget, got {other:?}"),
        }
    }

    #[test]
    fn root_hash_is_exposed_for_every_verified_shape() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pct(&db, b"pct", &[b"a", b"b"], grove_version);
        build_pst(&db, b"pst", &[(b"a", 5)], grove_version);
        build_pcps(&db, b"pcps", &[(b"a", 7)], grove_version);

        let expected = db.root_hash(None, grove_version).unwrap().expect("root");

        let mut verified = Vec::new();
        // Elements.
        let pq = PathQuery::new_single_key(vec![TEST_LEAF.to_vec()], b"pct".to_vec());
        verified.push(
            GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
                .expect("key selection"),
        );
        // Three aggregate leaves.
        let pq = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"pct".to_vec()],
            full_range(),
        );
        verified.push(
            GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
                .expect("count leaf"),
        );
        let pq = PathQuery::new_aggregate_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"pst".to_vec()],
            full_range(),
        );
        verified.push(
            GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
                .expect("sum leaf"),
        );
        let pq = PathQuery::new_aggregate_count_and_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"pcps".to_vec()],
            full_range(),
        );
        verified.push(
            GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
                .expect("combined leaf"),
        );
        // A carrier.
        let pq = carrier_path_query(&[b"pct"], Query::new_aggregate_count_on_range(full_range()));
        verified.push(
            GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
                .expect("count carrier"),
        );

        for shape in &verified {
            assert_eq!(
                shape.root_hash(),
                &expected,
                "every shape must expose the same reconstructed root: {shape:?}"
            );
        }
    }
}
