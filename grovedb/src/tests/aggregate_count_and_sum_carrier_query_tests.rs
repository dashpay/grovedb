//! End-to-end GroveDB tests for the **carrier** shape of
//! `AggregateCountAndSumOnRange` queries — outer `Key`/`Range*` items
//! routing to a combined-aggregate subquery, verified via
//! [`GroveDb::verify_aggregate_count_and_sum_query_per_key`].
//!
//! Mirrors `aggregate_sum_carrier_query_tests.rs` for the dual-axis
//! `ProvableCountProvableSumTree` flavor. Round-trip leaf coverage of
//! `AggregateCountAndSumOnRange` lives in
//! `provable_count_provable_sum_tree_tests.rs`.
//!
//! **Dual-axis invariant under test:** Only
//! `ProvableCountProvableSumTree` hosts can ground a combined-aggregate
//! proof. The terminal-type gate in the verifier rejects every other
//! tree type (single-axis ProvableCountTree, single-axis
//! ProvableSumTree, plain ProvableCountSumTree, etc.).

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::query::QueryItem;
    use grovedb_query::Query;
    use grovedb_version::version::GroveVersion;

    use crate::{
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb, PathQuery, SizedQuery,
    };

    /// Set up a two-level GroveDB tree shaped like an "index lookup"
    /// reverse index whose leaf merks are
    /// `ProvableCountProvableSumTree` (PCPS) hosts:
    ///
    /// ```text
    /// TEST_LEAF / byBrand /
    ///     <brand_key>/value/        (ProvableCountProvableSumTree)
    ///         <key_NNNNN>           (SumItem(value_i64))
    /// ```
    ///
    /// Each brand subtree has a `value` child that is a PCPS host
    /// populated with `values_per_brand` keys `value_<i:05>` whose sum
    /// items are simply `i + 1`. The total count over the full range
    /// for a brand is `values_per_brand`, and the total sum is
    /// `values_per_brand * (values_per_brand + 1) / 2`.
    fn setup_brand_value_pcps_carrier_tree(
        grove_version: &GroveVersion,
        brands: &[&[u8]],
        values_per_brand: u32,
    ) -> (crate::tests::TempGroveDb, [u8; 32]) {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"byBrand",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert byBrand");
        for brand in brands {
            db.insert(
                [TEST_LEAF, b"byBrand"].as_ref(),
                brand,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert brand");
            db.insert(
                [TEST_LEAF, b"byBrand", brand].as_ref(),
                b"value",
                Element::empty_provable_count_provable_sum_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCPS value subtree");
            for i in 0..values_per_brand {
                let key = format!("value_{:05}", i);
                db.insert(
                    [TEST_LEAF, b"byBrand", brand, b"value"].as_ref(),
                    key.as_bytes(),
                    Element::new_sum_item((i + 1) as i64),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert sum item");
            }
        }
        let root = db
            .grove_db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root_hash");
        (db, root)
    }

    /// Build a carrier combined-aggregate `PathQuery` rooted at
    /// `[TEST_LEAF, "byBrand"]`, fanning out across `outer_keys` and
    /// aggregating (count + sum) in each brand's `value` PCPS subtree
    /// against the inner range.
    fn carrier_combined_path_query(outer_keys: &[&[u8]], inner_range: QueryItem) -> PathQuery {
        let mut carrier = Query::new();
        for k in outer_keys {
            carrier.insert_key(k.to_vec());
        }
        carrier.set_subquery_path(vec![b"value".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_and_sum_on_range(inner_range));

        PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, None, None),
        )
    }

    fn triangular(n: u32) -> i64 {
        (n as i64) * ((n as i64) + 1) / 2
    }

    #[test]
    fn carrier_combined_two_outer_keys_succeeds() {
        let v = GroveVersion::latest();
        let (db, expected_root) =
            setup_brand_value_pcps_carrier_tree(v, &[b"brand_000", b"brand_001"], 10);
        // Take values strictly after `value_00004` → value_00005 ..
        // value_00009 (5 items: count=5, sum=6+7+8+9+10=40).
        let path_query = carrier_combined_path_query(
            &[b"brand_000", b"brand_001"],
            QueryItem::RangeAfter(b"value_00004".to_vec()..),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query (carrier combined-aggregate) should succeed");
        let (got_root, results) =
            GroveDb::verify_aggregate_count_and_sum_query_per_key(&proof, &path_query, v)
                .expect("verify carrier combined-aggregate should succeed");
        assert_eq!(got_root, expected_root, "root must match GroveDB root");
        assert_eq!(results.len(), 2, "expected one result per outer key");
        assert_eq!(results[0].0, b"brand_000".to_vec());
        assert_eq!(results[1].0, b"brand_001".to_vec());
        // (count, sum) = (5, 40) for both brands.
        assert_eq!(results[0].1, 5);
        assert_eq!(results[0].2, 40);
        assert_eq!(results[1].1, 5);
        assert_eq!(results[1].2, 40);
    }

    #[test]
    fn carrier_combined_with_unknown_outer_key_returns_present_keys_only() {
        let v = GroveVersion::latest();
        let (db, expected_root) = setup_brand_value_pcps_carrier_tree(v, &[b"brand_000"], 10);
        let path_query = carrier_combined_path_query(
            &[b"brand_000", b"brand_999_missing"],
            QueryItem::RangeAfter(b"value_00004".to_vec()..),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let (got_root, results) =
            GroveDb::verify_aggregate_count_and_sum_query_per_key(&proof, &path_query, v)
                .expect("verify should succeed");
        assert_eq!(got_root, expected_root);
        assert_eq!(
            results.len(),
            1,
            "absent outer keys must not contribute an entry"
        );
        assert_eq!(results[0].0, b"brand_000".to_vec());
        assert_eq!(results[0].1, 5);
        assert_eq!(results[0].2, 40);
    }

    #[test]
    fn carrier_combined_keys_outer_with_limit_caps_results() {
        let v = GroveVersion::latest();
        let (db, expected_root) = setup_brand_value_pcps_carrier_tree(
            v,
            &[b"brand_000", b"brand_001", b"brand_002", b"brand_003"],
            10,
        );

        let mut carrier = Query::new();
        for k in [b"brand_000", b"brand_001", b"brand_002", b"brand_003"] {
            carrier.insert_key(k.to_vec());
        }
        carrier.set_subquery_path(vec![b"value".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_and_sum_on_range(
            QueryItem::RangeFrom(b"value_00000".to_vec()..),
        ));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, Some(2), None),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query (carrier with Keys outer + limit) should succeed");
        let (got_root, results) =
            GroveDb::verify_aggregate_count_and_sum_query_per_key(&proof, &path_query, v)
                .expect("verify carrier with Keys outer + limit should succeed");
        assert_eq!(got_root, expected_root, "root must match GroveDB root");
        assert_eq!(results.len(), 2, "expected exactly `limit` outer matches");
        assert_eq!(results[0].0, b"brand_000".to_vec());
        assert_eq!(results[1].0, b"brand_001".to_vec());
        let expected_sum = triangular(10);
        for (_, count, sum) in &results {
            assert_eq!(*count, 10);
            assert_eq!(*sum, expected_sum);
        }
    }

    #[test]
    fn carrier_combined_rejects_offset() {
        let v = GroveVersion::latest();
        let mut carrier = Query::new();
        carrier.insert_key(b"brand_000".to_vec());
        carrier.set_subquery_path(vec![b"value".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_and_sum_on_range(
            QueryItem::Range(b"value_00000".to_vec()..b"value_00010".to_vec()),
        ));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, None, Some(2)),
        );
        let dummy_proof = vec![0u8; 8];
        let err =
            GroveDb::verify_aggregate_count_and_sum_query_per_key(&dummy_proof, &path_query, v)
                .expect_err("carrier combined-aggregate with offset must be rejected at entry");
        match err {
            crate::Error::InvalidQuery(msg) => {
                assert!(msg.contains("offset"), "unexpected message: {msg}");
                assert!(msg.contains("carrier"), "unexpected message: {msg}");
            }
            other => panic!("expected InvalidQuery, got {:?}", other),
        }
    }

    #[test]
    fn carrier_combined_right_to_left_returns_descending_order() {
        let v = GroveVersion::latest();
        let (db, expected_root) =
            setup_brand_value_pcps_carrier_tree(v, &[b"brand_000", b"brand_001", b"brand_002"], 5);
        let mut carrier = Query::new_with_direction(false);
        carrier.insert_key(b"brand_000".to_vec());
        carrier.insert_key(b"brand_001".to_vec());
        carrier.insert_key(b"brand_002".to_vec());
        carrier.set_subquery_path(vec![b"value".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_and_sum_on_range(
            QueryItem::RangeFrom(b"value_00000".to_vec()..),
        ));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, None, None),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query (carrier combined, right-to-left) should succeed");
        let (got_root, results) =
            GroveDb::verify_aggregate_count_and_sum_query_per_key(&proof, &path_query, v)
                .expect("verify carrier combined (right-to-left) should succeed");
        assert_eq!(got_root, expected_root);
        assert_eq!(results.len(), 3, "expected 3 outer-key matches");
        // Descending lex: brand_002, brand_001, brand_000.
        assert_eq!(results[0].0, b"brand_002".to_vec());
        assert_eq!(results[1].0, b"brand_001".to_vec());
        assert_eq!(results[2].0, b"brand_000".to_vec());
        let expected_sum = triangular(5);
        for (_, count, sum) in results {
            assert_eq!(count, 5);
            assert_eq!(sum, expected_sum);
        }
    }

    #[test]
    fn leaf_combined_round_trip_via_per_key_returns_one_entry() {
        // The leaf shape — a single-`AggregateCountAndSumOnRange` query
        // — produces exactly the same proof bytes it did before this
        // feature. Verifying it via the new per-key entry point returns
        // a one-entry Vec with an empty key and the same `(count, sum)`
        // `verify_aggregate_count_and_sum_query` returns.
        let v = GroveVersion::latest();
        let (db, expected_root) = setup_brand_value_pcps_carrier_tree(v, &[b"brand_000"], 10);
        let path_query = PathQuery::new_aggregate_count_and_sum_on_range(
            vec![
                TEST_LEAF.to_vec(),
                b"byBrand".to_vec(),
                b"brand_000".to_vec(),
                b"value".to_vec(),
            ],
            QueryItem::RangeFrom(b"value_00000".to_vec()..),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        // Existing single-(u64, i64) entry point still works.
        let (root_one, count_one, sum_one) =
            GroveDb::verify_aggregate_count_and_sum_query(&proof, &path_query, v)
                .expect("legacy leaf verifier must still accept legacy leaf proof");
        // New per-key entry point also accepts leaf and returns a
        // one-entry Vec with an empty key.
        let (root_many, results) =
            GroveDb::verify_aggregate_count_and_sum_query_per_key(&proof, &path_query, v)
                .expect("per-key verifier must accept leaf proofs");
        assert_eq!(root_one, expected_root);
        assert_eq!(root_one, root_many);
        assert_eq!(count_one, 10);
        assert_eq!(sum_one, triangular(10));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, Vec::<u8>::new());
        assert_eq!(results[0].1, count_one);
        assert_eq!(results[0].2, sum_one);
    }

    #[test]
    fn legacy_verify_aggregate_count_and_sum_query_rejects_carrier_query() {
        let v = GroveVersion::latest();
        let (db, _root) = setup_brand_value_pcps_carrier_tree(v, &[b"brand_000"], 5);
        let path_query = carrier_combined_path_query(
            &[b"brand_000"],
            QueryItem::Range(b"value_00000".to_vec()..b"value_00010".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let err = GroveDb::verify_aggregate_count_and_sum_query(&proof, &path_query, v)
            .expect_err("legacy leaf verifier must reject carrier shape");
        match err {
            crate::Error::InvalidQuery(_) => {}
            other => panic!("expected InvalidQuery, got {:?}", other),
        }
    }

    #[test]
    fn carrier_combined_with_range_outer_succeeds() {
        let v = GroveVersion::latest();
        let (db, expected_root) =
            setup_brand_value_pcps_carrier_tree(v, &[b"brand_000", b"brand_001", b"brand_002"], 10);

        let mut carrier = Query::new();
        carrier
            .items
            .push(QueryItem::RangeAfter(b"brand_000".to_vec()..));
        carrier.set_subquery_path(vec![b"value".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_count_and_sum_on_range(
            QueryItem::RangeFrom(b"value_00000".to_vec()..),
        ));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, None, None),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query (carrier with Range outer) should succeed");
        let (got_root, results) =
            GroveDb::verify_aggregate_count_and_sum_query_per_key(&proof, &path_query, v)
                .expect("verify carrier with Range outer should succeed");
        assert_eq!(got_root, expected_root);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, b"brand_001".to_vec());
        assert_eq!(results[1].0, b"brand_002".to_vec());
        let expected_sum = triangular(10);
        for (_, count, sum) in results {
            assert_eq!(count, 10);
            assert_eq!(sum, expected_sum);
        }
    }

    #[test]
    fn per_key_combined_rejects_non_combined_path_query() {
        let v = GroveVersion::latest();
        let bad_query = PathQuery::new_single_query_item(
            vec![TEST_LEAF.to_vec()],
            QueryItem::Key(b"k".to_vec()),
        );
        let dummy_proof = vec![0u8; 16];
        let err =
            GroveDb::verify_aggregate_count_and_sum_query_per_key(&dummy_proof, &bad_query, v)
                .expect_err("non-ACASOR path_query must be rejected up front");
        match err {
            crate::Error::InvalidQuery(_) => {}
            other => panic!("expected InvalidQuery, got {:?}", other),
        }
    }

    #[test]
    fn carrier_combined_rejects_non_pcps_leaf_tree() {
        // **Dual-axis invariant test:** the combined-aggregate verifier
        // rejects every carrier whose terminal merk isn't a
        // ProvableCountProvableSumTree. Here the leaf merks are
        // **plain** ProvableSumTrees — the sum side accepts them, the
        // combined side must not.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"byBrand",
            Element::empty_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert byBrand");
        for brand in [b"brand_000", b"brand_001"] {
            db.insert(
                [TEST_LEAF, b"byBrand"].as_ref(),
                brand,
                Element::empty_tree(),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert brand");
            // Plain ProvableSumTree (NOT PCPS) as the leaf merk — this is
            // the dual-axis-invariant violation.
            db.insert(
                [TEST_LEAF, b"byBrand", brand].as_ref(),
                b"value",
                Element::empty_provable_sum_tree(),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert plain ProvableSumTree");
            for i in 0..5u32 {
                let key = format!("value_{:05}", i);
                db.insert(
                    [TEST_LEAF, b"byBrand", brand, b"value"].as_ref(),
                    key.as_bytes(),
                    Element::new_sum_item((i + 1) as i64),
                    None,
                    None,
                    v,
                )
                .unwrap()
                .expect("insert sum item");
            }
        }

        let path_query = carrier_combined_path_query(
            &[b"brand_000", b"brand_001"],
            QueryItem::RangeFrom(b"value_00000".to_vec()..),
        );
        // The prover hits its own merk-level tree-type gate first when
        // it tries to emit a combined-aggregate proof over a non-PCPS
        // host — so we don't actually expect to reach the verifier
        // here. Either an error during prove (more common) or an
        // InvalidProof during verify is acceptable; both prove the
        // dual-axis invariant is enforced before any (count, sum)
        // result ever reaches the caller.
        let prove_result = db.grove_db.prove_query(&path_query, None, v);
        match prove_result.value() {
            // Prover refused — perfect.
            Err(_) => {}
            Ok(proof) => {
                // If the prover somehow produced a proof, the verifier
                // must refuse.
                let err = GroveDb::verify_aggregate_count_and_sum_query_per_key(
                    proof.as_slice(),
                    &path_query,
                    v,
                )
                .expect_err(
                    "combined-aggregate verifier must reject a carrier whose terminal merk \
                     isn't a ProvableCountProvableSumTree",
                );
                match err {
                    crate::Error::InvalidProof(_, msg) => {
                        assert!(
                            msg.contains("ProvableCountProvableSumTree"),
                            "unexpected message: {msg}"
                        );
                    }
                    other => panic!("expected InvalidProof, got {:?}", other),
                }
            }
        }
    }
}
