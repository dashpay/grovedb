//! End-to-end GroveDB tests for the **carrier** shape of
//! `AggregateSumOnRange` queries — outer `Key`/`Range*` items routing
//! to an aggregate-sum subquery, verified via
//! [`GroveDb::verify_aggregate_sum_query_per_key`].
//!
//! Mirrors the carrier portion of `aggregate_count_query_tests.rs` for
//! the signed-sum flavor. Round-trip leaf coverage of
//! `AggregateSumOnRange` lives in `aggregate_sum_query_tests.rs`.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::query::QueryItem;
    use grovedb_query::Query;
    use grovedb_version::version::GroveVersion;

    use crate::{
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb, PathQuery, SizedQuery,
    };

    /// Set up a two-level GroveDB tree shaped like an "index lookup" /
    /// "by-brand" reverse index:
    ///
    /// ```text
    /// TEST_LEAF / byBrand /
    ///     <brand_key>/value/        (ProvableSumTree)
    ///         <key_NNNNN>           (SumItem(value_i64))
    /// ```
    ///
    /// Each brand subtree has a `value` child that is a `ProvableSumTree`
    /// populated with `values_per_brand` keys `value_<i:05>` whose sum
    /// items are simply `i + 1`. The total sum over the full range for a
    /// brand is therefore `values_per_brand * (values_per_brand + 1) / 2`.
    fn setup_brand_value_carrier_tree(
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
                Element::empty_provable_sum_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert value subtree");
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

    /// Build a carrier aggregate-sum `PathQuery` rooted at
    /// `[TEST_LEAF, "byBrand"]`, fanning out across `outer_keys` and
    /// summing elements in each brand's `value` subtree matching the
    /// inner range.
    fn carrier_sum_path_query(outer_keys: &[&[u8]], inner_range: QueryItem) -> PathQuery {
        let mut carrier = Query::new();
        for k in outer_keys {
            // Use `insert_key` (not `items.push`) so items end up in
            // sorted-ascending order — the merk multi-key walker
            // expects that invariant.
            carrier.insert_key(k.to_vec());
        }
        carrier.set_subquery_path(vec![b"value".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_sum_on_range(inner_range));

        PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, None, None),
        )
    }

    // Sum of i for i in 1..=n.
    fn triangular(n: u32) -> i64 {
        (n as i64) * ((n as i64) + 1) / 2
    }

    #[test]
    fn carrier_sum_two_outer_keys_succeeds() {
        // Carrier with two outer brand keys, RangeFull-equivalent inner
        // range. Expected: two (key, sum) pairs in query-direction order
        // with the correct per-brand aggregate. The carrier defaults to
        // `left_to_right=true`, so output is ascending lex.
        let v = GroveVersion::latest();
        let (db, expected_root) =
            setup_brand_value_carrier_tree(v, &[b"brand_000", b"brand_001"], 10);
        // Take everything strictly after `value_00004` → values
        // value_00005 .. value_00009 (5 items: 6+7+8+9+10 = 40).
        let path_query = carrier_sum_path_query(
            &[b"brand_000", b"brand_001"],
            QueryItem::RangeAfter(b"value_00004".to_vec()..),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query (carrier aggregate-sum) should succeed");
        let (got_root, results) =
            GroveDb::verify_aggregate_sum_query_per_key(&proof, &path_query, v)
                .expect("verify carrier aggregate-sum should succeed");
        assert_eq!(got_root, expected_root, "root must match GroveDB root");
        assert_eq!(results.len(), 2, "expected one result per outer key");
        assert_eq!(results[0].0, b"brand_000".to_vec());
        assert_eq!(results[1].0, b"brand_001".to_vec());
        // 6 + 7 + 8 + 9 + 10 = 40
        assert_eq!(results[0].1, 40);
        assert_eq!(results[1].1, 40);
    }

    #[test]
    fn carrier_sum_with_unknown_outer_key_returns_present_keys_only() {
        // An outer-key match that doesn't exist contributes no entry to
        // the result vector (it's an absence, not an error).
        let v = GroveVersion::latest();
        let (db, expected_root) = setup_brand_value_carrier_tree(v, &[b"brand_000"], 10);
        let path_query = carrier_sum_path_query(
            &[b"brand_000", b"brand_999_missing"],
            QueryItem::RangeAfter(b"value_00004".to_vec()..),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let (got_root, results) =
            GroveDb::verify_aggregate_sum_query_per_key(&proof, &path_query, v)
                .expect("verify should succeed");
        assert_eq!(got_root, expected_root);
        assert_eq!(
            results.len(),
            1,
            "absent outer keys must not contribute an entry"
        );
        assert_eq!(results[0].0, b"brand_000".to_vec());
        assert_eq!(results[0].1, 40);
    }

    #[test]
    fn carrier_sum_keys_outer_with_limit_caps_results() {
        // Carrier ASOR with `Keys` outer items and `SizedQuery::limit`
        // set. The walk must stop after `limit` outer-key matches have
        // produced their leaf-ASOR i64 — each match is a complete sum,
        // the inner range is not capped.
        let v = GroveVersion::latest();
        let (db, expected_root) = setup_brand_value_carrier_tree(
            v,
            &[b"brand_000", b"brand_001", b"brand_002", b"brand_003"],
            10,
        );

        let mut carrier = Query::new();
        for k in [b"brand_000", b"brand_001", b"brand_002", b"brand_003"] {
            carrier.insert_key(k.to_vec());
        }
        carrier.set_subquery_path(vec![b"value".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_sum_on_range(QueryItem::RangeFrom(
            b"value_00000".to_vec()..,
        )));
        // Cap the outer walk at 2 matches.
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
            GroveDb::verify_aggregate_sum_query_per_key(&proof, &path_query, v)
                .expect("verify carrier with Keys outer + limit should succeed");
        assert_eq!(got_root, expected_root, "root must match GroveDB root");
        assert_eq!(results.len(), 2, "expected exactly `limit` outer matches");
        // left_to_right defaults to true: first two brand keys ascending.
        assert_eq!(results[0].0, b"brand_000".to_vec());
        assert_eq!(results[1].0, b"brand_001".to_vec());
        // 10 values per brand, sum = triangular(10) = 55.
        let expected_sum = triangular(10);
        for (_, sum) in &results {
            assert_eq!(*sum, expected_sum);
        }
    }

    #[test]
    fn carrier_sum_rejects_offset() {
        // Carriers reject SizedQuery::offset: skipping the first M outer
        // matches changes which (outer_key, i64) pairs end up in the
        // proof, and the use case for that hasn't been designed yet.
        let v = GroveVersion::latest();
        let mut carrier = Query::new();
        carrier.insert_key(b"brand_000".to_vec());
        carrier.set_subquery_path(vec![b"value".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_sum_on_range(QueryItem::Range(
            b"value_00000".to_vec()..b"value_00010".to_vec(),
        )));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, None, Some(2)),
        );
        let dummy_proof = vec![0u8; 8];
        let err = GroveDb::verify_aggregate_sum_query_per_key(&dummy_proof, &path_query, v)
            .expect_err("carrier aggregate-sum with offset must be rejected at entry");
        match err {
            crate::Error::InvalidQuery(msg) => {
                assert!(msg.contains("offset"), "unexpected message: {msg}");
                assert!(msg.contains("carrier"), "unexpected message: {msg}");
            }
            other => panic!("expected InvalidQuery, got {:?}", other),
        }
    }

    #[test]
    fn carrier_sum_right_to_left_returns_descending_order() {
        // Flip the carrier's `left_to_right` flag — output must come back
        // in descending lex order, mirroring the merk walker's reversed
        // emission.
        let v = GroveVersion::latest();
        let (db, expected_root) =
            setup_brand_value_carrier_tree(v, &[b"brand_000", b"brand_001", b"brand_002"], 5);
        let mut carrier = Query::new_with_direction(false);
        carrier.insert_key(b"brand_000".to_vec());
        carrier.insert_key(b"brand_001".to_vec());
        carrier.insert_key(b"brand_002".to_vec());
        carrier.set_subquery_path(vec![b"value".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_sum_on_range(QueryItem::RangeFrom(
            b"value_00000".to_vec()..,
        )));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, None, None),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query (carrier aggregate-sum, right-to-left) should succeed");
        let (got_root, results) =
            GroveDb::verify_aggregate_sum_query_per_key(&proof, &path_query, v)
                .expect("verify carrier aggregate-sum (right-to-left) should succeed");
        assert_eq!(got_root, expected_root);
        assert_eq!(results.len(), 3, "expected 3 outer-key matches");
        // Descending lex: brand_002, brand_001, brand_000.
        assert_eq!(results[0].0, b"brand_002".to_vec());
        assert_eq!(results[1].0, b"brand_001".to_vec());
        assert_eq!(results[2].0, b"brand_000".to_vec());
        let expected_sum = triangular(5); // 1+2+3+4+5 = 15
        for (_, sum) in results {
            assert_eq!(sum, expected_sum);
        }
    }

    #[test]
    fn leaf_aggregate_sum_round_trip_via_per_key_returns_one_entry() {
        // The leaf shape — a single-`AggregateSumOnRange` query — produces
        // the same proof bytes whether the caller verifies via
        // `verify_aggregate_sum_query` or the per-key entry point.
        // Verifying it via the per-key entry point returns a one-entry
        // Vec with an empty key and the same sum
        // `verify_aggregate_sum_query` returns.
        let v = GroveVersion::latest();
        let (db, expected_root) = setup_brand_value_carrier_tree(v, &[b"brand_000"], 10);
        let path_query = PathQuery::new_aggregate_sum_on_range(
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
        // Existing single-i64 entry point still works.
        let (root_one, sum_one) = GroveDb::verify_aggregate_sum_query(&proof, &path_query, v)
            .expect("legacy leaf verifier must still accept legacy leaf proof");
        // New per-key entry point also accepts leaf and returns a
        // one-entry Vec with an empty key.
        let (root_many, results) =
            GroveDb::verify_aggregate_sum_query_per_key(&proof, &path_query, v)
                .expect("per-key verifier must accept leaf proofs");
        assert_eq!(root_one, expected_root);
        assert_eq!(root_one, root_many);
        assert_eq!(sum_one, triangular(10));
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, Vec::<u8>::new());
        assert_eq!(results[0].1, sum_one);
    }

    #[test]
    fn legacy_verify_aggregate_sum_query_rejects_carrier_query() {
        // The legacy single-`i64` `verify_aggregate_sum_query` strictly
        // validates the leaf shape and rejects carrier queries — even
        // though the proof bytes themselves are well-formed. Callers
        // must use `verify_aggregate_sum_query_per_key` for carriers.
        let v = GroveVersion::latest();
        let (db, _root) = setup_brand_value_carrier_tree(v, &[b"brand_000"], 5);
        let path_query = carrier_sum_path_query(
            &[b"brand_000"],
            QueryItem::Range(b"value_00000".to_vec()..b"value_00010".to_vec()),
        );
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let err = GroveDb::verify_aggregate_sum_query(&proof, &path_query, v)
            .expect_err("legacy leaf verifier must reject carrier shape");
        match err {
            crate::Error::InvalidQuery(_) => {}
            other => panic!("expected InvalidQuery, got {:?}", other),
        }
    }

    #[test]
    fn carrier_sum_with_range_outer_succeeds() {
        // The carrier supports a Range outer item. With an outer
        // `RangeAfter`, the matched outer keys come back in lex-asc
        // order and each contributes its own sum.
        let v = GroveVersion::latest();
        let (db, expected_root) =
            setup_brand_value_carrier_tree(v, &[b"brand_000", b"brand_001", b"brand_002"], 10);

        let mut carrier = Query::new();
        // Take everything strictly after brand_000 → brand_001, brand_002.
        carrier
            .items
            .push(QueryItem::RangeAfter(b"brand_000".to_vec()..));
        carrier.set_subquery_path(vec![b"value".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_sum_on_range(QueryItem::RangeFrom(
            b"value_00000".to_vec()..,
        )));
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
            GroveDb::verify_aggregate_sum_query_per_key(&proof, &path_query, v)
                .expect("verify carrier with Range outer should succeed");
        assert_eq!(got_root, expected_root);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, b"brand_001".to_vec());
        assert_eq!(results[1].0, b"brand_002".to_vec());
        let expected_sum = triangular(10);
        for (_, sum) in results {
            assert_eq!(sum, expected_sum);
        }
    }

    /// Root-carrier regression: a carrier `AggregateSumOnRange` query
    /// with an empty `PathQuery::path` must validate and round-trip
    /// correctly. The auto-dispatcher's empty-path rejection is
    /// shape-aware — root-carrier queries (where each root-level outer
    /// match descends via `subquery_path` to a leaf sum merk) are
    /// permitted, while leaf-shape queries at empty path are still
    /// rejected (the GroveDB root is always a `NormalTree`, never a
    /// `ProvableSumTree`).
    #[test]
    fn root_carrier_sum_with_empty_path_succeeds() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        // Build `ProvableSumTree`s under each of the two root leaves
        // (TEST_LEAF, ANOTHER_TEST_LEAF), each holding 1..=5 sum items.
        for leaf in [TEST_LEAF, b"test_leaf2"] {
            db.insert(
                [leaf].as_ref(),
                b"st",
                Element::empty_provable_sum_tree(),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert st");
            for (i, c) in (b'a'..=b'e').enumerate() {
                db.insert(
                    [leaf, b"st"].as_ref(),
                    &[c],
                    Element::new_sum_item((i as i64) + 1),
                    None,
                    None,
                    v,
                )
                .unwrap()
                .expect("insert sum item");
            }
        }
        let expected_root = db.grove_db.root_hash(None, v).unwrap().expect("root_hash");

        // Carrier rooted at the GroveDB root (empty path). Outer matches
        // are TEST_LEAF and ANOTHER_TEST_LEAF; subquery_path descends
        // through `st` to the leaf sum merk.
        let mut carrier = Query::new();
        carrier.insert_key(TEST_LEAF.to_vec());
        carrier.insert_key(b"test_leaf2".to_vec());
        carrier.set_subquery_path(vec![b"st".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_sum_on_range(QueryItem::RangeFrom(
            b"a".to_vec()..,
        )));
        let path_query = PathQuery::new(
            Vec::new(), // empty path → root-carrier
            SizedQuery::new(carrier, None, None),
        );

        // Sanity: shape-aware empty-path check accepts carrier shapes.
        path_query
            .validate_aggregate_sum_on_range()
            .expect("root-carrier ASOR must validate");

        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove root-carrier ASOR");
        let (got_root, results) =
            GroveDb::verify_aggregate_sum_query_per_key(&proof, &path_query, v)
                .expect("verify root-carrier ASOR");
        assert_eq!(got_root, expected_root, "root must match GroveDB root");
        assert_eq!(
            results.len(),
            2,
            "expected one entry per matched root-level outer key"
        );
        // Both subtrees hold 1+2+3+4+5 = 15. Order: ascending lex —
        // `test_leaf` < `test_leaf2`.
        assert_eq!(results[0].0, TEST_LEAF.to_vec());
        assert_eq!(results[1].0, b"test_leaf2".to_vec());
        assert_eq!(results[0].1, 15);
        assert_eq!(results[1].1, 15);
    }

    /// Mirror of `root_carrier_sum_with_empty_path_succeeds`: a leaf
    /// `AggregateSumOnRange` query against an empty path is STILL
    /// rejected — only carriers get the relaxation.
    #[test]
    fn root_leaf_sum_with_empty_path_still_rejected() {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        let pq = PathQuery::new_aggregate_sum_on_range(
            Vec::new(),
            QueryItem::RangeFrom(b"a".to_vec()..),
        );
        let err = pq
            .validate_aggregate_sum_on_range()
            .expect_err("leaf at empty path must still be rejected");
        let msg = format!("{err}");
        assert!(
            msg.contains("leaf") && msg.contains("ProvableSumTree"),
            "expected leaf-only rejection message, got: {msg}"
        );
        // Also exercise the verifier surface.
        let dummy = vec![0u8; 4];
        assert!(GroveDb::verify_aggregate_sum_query(&dummy, &pq, v).is_err());
        assert!(db
            .grove_db
            .query_aggregate_sum(&pq, None, v)
            .unwrap()
            .is_err());
    }

    #[test]
    fn per_key_sum_rejects_non_aggregate_sum_path_query() {
        // The per-key entry point rejects path queries that aren't
        // aggregate-sum queries at all — neither leaf nor carrier —
        // before decoding proof bytes.
        let v = GroveVersion::latest();
        let bad_query = PathQuery::new_single_query_item(
            vec![TEST_LEAF.to_vec()],
            QueryItem::Key(b"k".to_vec()),
        );
        let dummy_proof = vec![0u8; 16];
        let err = GroveDb::verify_aggregate_sum_query_per_key(&dummy_proof, &bad_query, v)
            .expect_err("non-aggregate-sum path_query must be rejected up front");
        match err {
            crate::Error::InvalidQuery(_) => {}
            other => panic!("expected InvalidQuery, got {:?}", other),
        }
    }

    // ---------- No-proof per-key entry point ----------
    //
    // `query_aggregate_sum_per_key` is the trusted-read counterpart of
    // `verify_aggregate_sum_query_per_key`: same surface shape
    // (`Vec<(Vec<u8>, i64)>`), accepts both leaf and carrier path
    // queries, but skips proof generation and verification entirely.
    // The strongest assertion available is differential equality with
    // the proved path, which is already consensus-tested above.

    #[test]
    fn no_proof_per_key_sum_leaf_matches_single_sum() {
        // Leaf-shape path query → one-entry vec with an empty stand-in
        // key and the same sum `query_aggregate_sum` returns. This is
        // the leaf-symmetry contract the per-key verifier also honors.
        let v = GroveVersion::latest();
        let (db, _) = setup_brand_value_carrier_tree(v, &[b"brand_000"], 10);
        let path_query = PathQuery::new_aggregate_sum_on_range(
            vec![
                TEST_LEAF.to_vec(),
                b"byBrand".to_vec(),
                b"brand_000".to_vec(),
                b"value".to_vec(),
            ],
            QueryItem::RangeAfter(b"value_00004".to_vec()..),
        );

        let single = db
            .grove_db
            .query_aggregate_sum(&path_query, None, v)
            .unwrap()
            .expect("single-i64 entry should succeed");
        let per_key = db
            .grove_db
            .query_aggregate_sum_per_key(&path_query, None, v)
            .unwrap()
            .expect("per-key entry should succeed");

        // 6 + 7 + 8 + 9 + 10 = 40
        assert_eq!(single, 40);
        assert_eq!(per_key.len(), 1);
        assert_eq!(per_key[0].0, Vec::<u8>::new());
        assert_eq!(per_key[0].1, single);
    }

    #[test]
    fn no_proof_per_key_sum_leaf_matches_proof_path() {
        // Differential: the leaf shape must agree with the proved leaf
        // shape, including the empty stand-in key.
        let v = GroveVersion::latest();
        let (db, _) = setup_brand_value_carrier_tree(v, &[b"brand_000"], 10);
        let path_query = PathQuery::new_aggregate_sum_on_range(
            vec![
                TEST_LEAF.to_vec(),
                b"byBrand".to_vec(),
                b"brand_000".to_vec(),
                b"value".to_vec(),
            ],
            QueryItem::RangeAfter(b"value_00004".to_vec()..),
        );
        let no_proof = db
            .grove_db
            .query_aggregate_sum_per_key(&path_query, None, v)
            .unwrap()
            .expect("no-proof leaf per-key should succeed");
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let (_root, proved) = GroveDb::verify_aggregate_sum_query_per_key(&proof, &path_query, v)
            .expect("verify should succeed");
        assert_eq!(no_proof, proved);
    }

    #[test]
    fn no_proof_per_key_sum_carrier_returns_per_outer_sum() {
        // Carrier shape → one (brand, sum) entry per matched outer key
        // in query-direction order.
        let v = GroveVersion::latest();
        let (db, _root) = setup_brand_value_carrier_tree(v, &[b"brand_000", b"brand_001"], 10);
        let path_query = carrier_sum_path_query(
            &[b"brand_000", b"brand_001"],
            QueryItem::RangeAfter(b"value_00004".to_vec()..),
        );
        let results = db
            .grove_db
            .query_aggregate_sum_per_key(&path_query, None, v)
            .unwrap()
            .expect("no-proof carrier query should succeed");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, b"brand_000".to_vec());
        assert_eq!(results[1].0, b"brand_001".to_vec());
        assert_eq!(results[0].1, 40);
        assert_eq!(results[1].1, 40);
    }

    #[test]
    fn no_proof_per_key_sum_matches_proof_path_per_key() {
        // Differential over a non-trivial carrier: the trusted read must
        // agree element-for-element with the proved per-key result.
        let v = GroveVersion::latest();
        let (db, _root) =
            setup_brand_value_carrier_tree(v, &[b"brand_000", b"brand_001", b"brand_002"], 10);
        let path_query = carrier_sum_path_query(
            &[b"brand_000", b"brand_001", b"brand_002"],
            QueryItem::RangeAfter(b"value_00004".to_vec()..),
        );
        let no_proof = db
            .grove_db
            .query_aggregate_sum_per_key(&path_query, None, v)
            .unwrap()
            .expect("no-proof should succeed");
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let (_root, proved) = GroveDb::verify_aggregate_sum_query_per_key(&proof, &path_query, v)
            .expect("verify should succeed");
        assert_eq!(no_proof, proved);
    }

    #[test]
    fn no_proof_per_key_sum_right_to_left_matches_proof_path() {
        // Direction propagation: a descending carrier must produce the
        // same ordering the proved path produces.
        let v = GroveVersion::latest();
        let (db, _root) =
            setup_brand_value_carrier_tree(v, &[b"brand_000", b"brand_001", b"brand_002"], 10);
        let mut carrier = Query::new_with_direction(false);
        for k in [b"brand_000", b"brand_001", b"brand_002"] {
            carrier.insert_key(k.to_vec());
        }
        carrier.set_subquery_path(vec![b"value".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_sum_on_range(QueryItem::RangeFrom(
            b"value_00000".to_vec()..,
        )));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, None, None),
        );
        let no_proof = db
            .grove_db
            .query_aggregate_sum_per_key(&path_query, None, v)
            .unwrap()
            .expect("no-proof should succeed");
        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let (_root, proved) = GroveDb::verify_aggregate_sum_query_per_key(&proof, &path_query, v)
            .expect("verify should succeed");
        assert_eq!(no_proof, proved);
        assert_eq!(no_proof[0].0, b"brand_002".to_vec(), "descending order");
    }

    #[test]
    fn no_proof_per_key_sum_skips_absent_outer_keys() {
        // Absent outer keys contribute no entry — same as the proved
        // path's behavior.
        let v = GroveVersion::latest();
        let (db, _root) = setup_brand_value_carrier_tree(v, &[b"brand_000"], 10);
        let path_query = carrier_sum_path_query(
            &[b"brand_000", b"brand_999_missing"],
            QueryItem::RangeAfter(b"value_00004".to_vec()..),
        );
        let no_proof = db
            .grove_db
            .query_aggregate_sum_per_key(&path_query, None, v)
            .unwrap()
            .expect("no-proof carrier query should succeed");
        assert_eq!(no_proof.len(), 1, "absent key contributes no entry");
        assert_eq!(no_proof[0].0, b"brand_000".to_vec());
        assert_eq!(no_proof[0].1, 40);

        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let (_root, proved) = GroveDb::verify_aggregate_sum_query_per_key(&proof, &path_query, v)
            .expect("verify should succeed");
        assert_eq!(no_proof, proved);
    }

    #[test]
    fn no_proof_per_key_sum_empty_carrier_result_set() {
        // No outer key matches at all → empty result vector, not an
        // error, and the proved path agrees.
        let v = GroveVersion::latest();
        let (db, _root) = setup_brand_value_carrier_tree(v, &[b"brand_000"], 10);
        let mut carrier = Query::new();
        // Everything strictly after the only present brand → no matches.
        carrier.insert_range_after(b"brand_zzz".to_vec()..);
        carrier.set_subquery_path(vec![b"value".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_sum_on_range(QueryItem::RangeFrom(
            b"value_00000".to_vec()..,
        )));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, None, None),
        );
        let no_proof = db
            .grove_db
            .query_aggregate_sum_per_key(&path_query, None, v)
            .unwrap()
            .expect("empty carrier result set must not be an error");
        assert!(no_proof.is_empty());

        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let (_root, proved) = GroveDb::verify_aggregate_sum_query_per_key(&proof, &path_query, v)
            .expect("verify should succeed");
        assert_eq!(no_proof, proved, "empty result sets must agree too");
    }

    #[test]
    fn no_proof_per_key_sum_empty_leaf_returns_zero() {
        // Outer key exists and `subquery_path` resolves cleanly, but the
        // leaf sum tree is empty. Match the proved path: emit `(key, 0)`
        // rather than skipping or erroring.
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
        db.insert(
            [TEST_LEAF, b"byBrand"].as_ref(),
            b"brand_000",
            Element::empty_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert brand");
        db.insert(
            [TEST_LEAF, b"byBrand", b"brand_000"].as_ref(),
            b"value",
            Element::empty_provable_sum_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert empty value subtree");

        let path_query = carrier_sum_path_query(
            &[b"brand_000"],
            QueryItem::Range(b"value_00000".to_vec()..b"value_99999".to_vec()),
        );
        let results = db
            .grove_db
            .query_aggregate_sum_per_key(&path_query, None, v)
            .unwrap()
            .expect("no-proof carrier with empty leaf should succeed");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, b"brand_000".to_vec());
        assert_eq!(results[0].1, 0);
    }

    #[test]
    fn no_proof_per_key_sum_limit_caps_outer_matches() {
        // `SizedQuery::limit` caps the number of outer-key matches. Each
        // surviving match still carries a complete leaf-ASOR i64 — the
        // inner range is not capped.
        let v = GroveVersion::latest();
        let (db, _root) = setup_brand_value_carrier_tree(
            v,
            &[b"brand_000", b"brand_001", b"brand_002", b"brand_003"],
            10,
        );
        let mut carrier = Query::new();
        for k in [b"brand_000", b"brand_001", b"brand_002", b"brand_003"] {
            carrier.insert_key(k.to_vec());
        }
        carrier.set_subquery_path(vec![b"value".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_sum_on_range(QueryItem::RangeFrom(
            b"value_00000".to_vec()..,
        )));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, Some(2), None),
        );

        let no_proof = db
            .grove_db
            .query_aggregate_sum_per_key(&path_query, None, v)
            .unwrap()
            .expect("carrier with limit should succeed");
        assert_eq!(no_proof.len(), 2, "expected exactly `limit` outer matches");
        assert_eq!(no_proof[0].0, b"brand_000".to_vec());
        assert_eq!(no_proof[1].0, b"brand_001".to_vec());
        let expected_sum = triangular(10);
        for (_, sum) in &no_proof {
            assert_eq!(*sum, expected_sum, "inner range must not be capped");
        }

        let proof = db
            .grove_db
            .prove_query(&path_query, None, v)
            .unwrap()
            .expect("prove_query should succeed");
        let (_root, proved) = GroveDb::verify_aggregate_sum_query_per_key(&proof, &path_query, v)
            .expect("verify should succeed");
        assert_eq!(no_proof, proved);
    }

    #[test]
    fn no_proof_per_key_sum_rejects_non_tree_outer_match() {
        // An outer-key match that resolves to a non-tree element can't
        // be descended into, so the carrier walk rejects it rather than
        // silently dropping the key.
        let v = GroveVersion::latest();
        let (db, _root) = setup_brand_value_carrier_tree(v, &[b"brand_000"], 10);
        db.insert(
            [TEST_LEAF, b"byBrand"].as_ref(),
            b"brand_001",
            Element::new_item(b"not a tree".to_vec()),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert item at outer layer");

        let path_query = carrier_sum_path_query(
            &[b"brand_000", b"brand_001"],
            QueryItem::RangeFrom(b"value_00000".to_vec()..),
        );
        let err = db
            .grove_db
            .query_aggregate_sum_per_key(&path_query, None, v)
            .unwrap()
            .expect_err("non-tree outer match must be rejected");
        match err {
            crate::Error::InvalidQuery(msg) => {
                assert!(
                    msg.contains("non-tree element"),
                    "unexpected message: {msg}"
                );
            }
            other => panic!("expected InvalidQuery, got {other:?}"),
        }
    }

    #[test]
    fn no_proof_per_key_sum_rejects_non_aggregate_sum_query() {
        // Same validation gate as the proved per-key entry: non-ASOR
        // path queries are rejected up front with `InvalidQuery`.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        let path_query = PathQuery::new_single_query_item(
            vec![TEST_LEAF.to_vec()],
            QueryItem::Key(b"k".to_vec()),
        );
        let err = db
            .grove_db
            .query_aggregate_sum_per_key(&path_query, None, v)
            .unwrap()
            .expect_err("non-aggregate-sum path query must be rejected");
        assert!(matches!(err, crate::Error::InvalidQuery(_)));
    }

    #[test]
    fn no_proof_per_key_sum_error_surface_matches_validator() {
        // For malformed queries the trusted read must surface exactly
        // the validator's error — it does no shape reasoning of its own.
        // Covers a carrier with `offset` (carrier-illegal), a leaf with
        // `limit` (leaf-illegal), and an outright non-aggregate query.
        let v = GroveVersion::latest();
        let (db, _root) = setup_brand_value_carrier_tree(v, &[b"brand_000"], 10);

        let mut carrier = Query::new();
        carrier.insert_key(b"brand_000".to_vec());
        carrier.set_subquery_path(vec![b"value".to_vec()]);
        carrier.set_subquery(Query::new_aggregate_sum_on_range(QueryItem::RangeFrom(
            b"value_00000".to_vec()..,
        )));
        let carrier_with_offset = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"byBrand".to_vec()],
            SizedQuery::new(carrier, None, Some(1)),
        );

        let mut leaf_with_limit = PathQuery::new_aggregate_sum_on_range(
            vec![
                TEST_LEAF.to_vec(),
                b"byBrand".to_vec(),
                b"brand_000".to_vec(),
                b"value".to_vec(),
            ],
            QueryItem::RangeFrom(b"value_00000".to_vec()..),
        );
        leaf_with_limit.query.limit = Some(1);

        let not_aggregate = PathQuery::new_single_query_item(
            vec![TEST_LEAF.to_vec()],
            QueryItem::Key(b"k".to_vec()),
        );

        for bad in [&carrier_with_offset, &leaf_with_limit, &not_aggregate] {
            let validator_err = bad
                .validate_aggregate_sum_on_range()
                .expect_err("validator must reject");
            let read_err = db
                .grove_db
                .query_aggregate_sum_per_key(bad, None, v)
                .unwrap()
                .expect_err("trusted read must reject");
            assert_eq!(
                read_err.to_string(),
                validator_err.to_string(),
                "trusted read must surface the validator's error verbatim"
            );
        }
    }
}
