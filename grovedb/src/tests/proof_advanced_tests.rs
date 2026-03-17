//! Advanced proof operation tests.
//!
//! Tests for `prove_query_many`, verification with options (absence proofs,
//! raw verification, limits), `verify_query_get_parent_tree_info`, and nested
//! subquery proofs. These complement the existing v1_proof_tests and the
//! proof-related tests in query_tests.

#[cfg(test)]
mod tests {
    use grovedb_merk::{
        proofs::{
            query::{QueryItem, SubqueryBranch},
            Query,
        },
        TreeFeatureType::SummedMerkNode,
    };
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::proof::GroveDBProof,
        tests::{make_deep_tree, make_test_grovedb, ANOTHER_TEST_LEAF, TEST_LEAF},
        Element, GroveDb, PathQuery, SizedQuery,
    };

    // =========================================================================
    // prove_query_many tests
    // =========================================================================

    #[test]
    fn prove_query_many_single_query_delegates() {
        // prove_query_many with a single PathQuery should produce the same
        // verifiable proof as prove_query called directly.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert some items under TEST_LEAF.
        for i in 0u8..5 {
            let key = format!("key_{}", i).into_bytes();
            let val = format!("val_{}", i).into_bytes();
            db.insert(
                [TEST_LEAF].as_ref(),
                &key,
                Element::new_item(val),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item");
        }

        // Build a PathQuery that selects all items under TEST_LEAF.
        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        // Prove via prove_query_many with a single query.
        let proof_many = db
            .prove_query_many(vec![&path_query], None, grove_version)
            .unwrap()
            .expect("prove_query_many should succeed with single query");

        // Prove via prove_query directly.
        let proof_single = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove_query should succeed");

        // Both proofs must verify successfully and produce the same root hash
        // and result set.
        let (hash_many, results_many) =
            GroveDb::verify_query(&proof_many, &path_query, grove_version)
                .expect("should verify proof from prove_query_many");
        let (hash_single, results_single) =
            GroveDb::verify_query(&proof_single, &path_query, grove_version)
                .expect("should verify proof from prove_query");

        assert_eq!(
            hash_many, hash_single,
            "root hashes should match between prove_query_many and prove_query"
        );
        assert_eq!(
            results_many.len(),
            results_single.len(),
            "result set lengths should match"
        );
        for (i, (r_many, r_single)) in results_many.iter().zip(results_single.iter()).enumerate() {
            assert_eq!(
                r_many, r_single,
                "result at index {} should match between prove_query_many and prove_query",
                i
            );
        }
    }

    #[test]
    fn prove_query_many_two_queries() {
        // prove_query_many merges two PathQueries targeting different subtrees
        // and produces a valid combined proof.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert items under TEST_LEAF.
        for i in 0u8..3 {
            let key = format!("a_{}", i).into_bytes();
            let val = format!("va_{}", i).into_bytes();
            db.insert(
                [TEST_LEAF].as_ref(),
                &key,
                Element::new_item(val),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item into TEST_LEAF");
        }

        // Insert items under ANOTHER_TEST_LEAF.
        for i in 0u8..3 {
            let key = format!("b_{}", i).into_bytes();
            let val = format!("vb_{}", i).into_bytes();
            db.insert(
                [ANOTHER_TEST_LEAF].as_ref(),
                &key,
                Element::new_item(val),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item into ANOTHER_TEST_LEAF");
        }

        // Build two PathQueries, one for each leaf.
        let mut query_a = Query::new();
        query_a.insert_all();
        let pq_a = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query_a);

        let mut query_b = Query::new();
        query_b.insert_all();
        let pq_b = PathQuery::new_unsized(vec![ANOTHER_TEST_LEAF.to_vec()], query_b);

        // Prove with both queries merged.
        let proof = db
            .prove_query_many(vec![&pq_a, &pq_b], None, grove_version)
            .unwrap()
            .expect("prove_query_many with two queries should succeed");

        // The merged query is the union of both; build the same merged query
        // for verification.
        let merged =
            PathQuery::merge(vec![&pq_a, &pq_b], grove_version).expect("merge should succeed");

        let (root_hash, results) = GroveDb::verify_query(&proof, &merged, grove_version)
            .expect("should verify merged proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root, "root hash should match");

        // We inserted 3 items in each subtree, so the result should have 6 items.
        assert_eq!(results.len(), 6, "should have 6 results from merged query");
    }

    #[test]
    fn prove_query_many_empty_queries_returns_error() {
        // prove_query_many with an empty query list should return an error.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let err = db
            .prove_query_many(vec![], None, grove_version)
            .unwrap()
            .expect_err("prove_query_many with empty vector should return an error");
        assert!(
            matches!(
                err,
                crate::Error::InvalidInput("prove_query_many called with empty query vector")
            ),
            "expected InvalidInput with exact message, got: {:?}",
            err
        );
    }

    // =========================================================================
    // Verification with options tests
    // =========================================================================

    #[test]
    fn verify_query_with_absence_proof() {
        // When querying for keys that do not exist, absence proofs return
        // None for the missing elements while still returning existing ones.
        let grove_version = GroveVersion::latest();
        let db = make_deep_tree(grove_version);

        // The deep tree has key1, key2, key3 under [TEST_LEAF, "innertree"].
        // Query for key2 (exists), key4 (does not exist), key5 (does not exist).
        let mut query = Query::new();
        query.insert_key(b"key2".to_vec());
        query.insert_key(b"key4".to_vec());
        query.insert_key(b"key5".to_vec());

        // Absence proofs require a limit.
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
            SizedQuery::new(query, Some(3), None),
        );

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        let (root_hash, result_set) =
            GroveDb::verify_query_with_absence_proof(&proof, &path_query, grove_version)
                .expect("should verify absence proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root, "root hash should match");

        // We should have 3 entries: key2 (Some), key4 (None), key5 (None).
        assert_eq!(result_set.len(), 3, "should have 3 results");

        // key2 should be present.
        assert_eq!(result_set[0].1, b"key2".to_vec());
        assert_eq!(
            result_set[0].2,
            Some(Element::new_item(b"value2".to_vec())),
            "key2 should exist"
        );

        // key4 should be absent.
        assert_eq!(result_set[1].1, b"key4".to_vec());
        assert_eq!(result_set[1].2, None, "key4 should be absent");

        // key5 should be absent.
        assert_eq!(result_set[2].1, b"key5".to_vec());
        assert_eq!(result_set[2].2, None, "key5 should be absent");
    }

    #[test]
    fn verify_query_raw_round_trip() {
        // verify_query_raw returns raw serialized bytes for each element,
        // while verify_query returns deserialized Elements. Both should agree
        // on the root hash and return equivalent data.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert items under TEST_LEAF.
        let items: Vec<(Vec<u8>, Vec<u8>)> = (0u8..4)
            .map(|i| {
                (
                    format!("rk_{}", i).into_bytes(),
                    format!("rv_{}", i).into_bytes(),
                )
            })
            .collect();

        for (key, val) in &items {
            db.insert(
                [TEST_LEAF].as_ref(),
                key,
                Element::new_item(val.clone()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item");
        }

        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        // Verify with verify_query (returns Elements).
        let (hash_elem, result_elem) = GroveDb::verify_query(&proof, &path_query, grove_version)
            .expect("verify_query should succeed");

        // Verify with verify_query_raw (returns raw bytes).
        let (hash_raw, result_raw) = GroveDb::verify_query_raw(&proof, &path_query, grove_version)
            .expect("verify_query_raw should succeed");

        // Root hashes must match.
        assert_eq!(
            hash_elem, hash_raw,
            "root hashes from verify_query and verify_query_raw should match"
        );

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(hash_elem, expected_root, "root hash should match db");

        // The number of results should match.
        assert_eq!(
            result_elem.len(),
            result_raw.len(),
            "result lengths should match"
        );

        // Each raw result, when deserialized, should match the Element result.
        for (i, raw_entry) in result_raw.iter().enumerate() {
            let deserialized = Element::deserialize(&raw_entry.value, grove_version)
                .expect("should deserialize raw value");
            let elem_entry = &result_elem[i];
            assert_eq!(
                elem_entry.2,
                Some(deserialized),
                "deserialized raw value at index {} should match Element result",
                i
            );
        }
    }

    #[test]
    fn verify_query_with_options_limit() {
        // Prove a query with a limit and verify the results are correctly
        // bounded by the limit.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert 10 items under TEST_LEAF.
        for i in 0u8..10 {
            let key = format!("item_{:02}", i).into_bytes();
            let val = format!("data_{:02}", i).into_bytes();
            db.insert(
                [TEST_LEAF].as_ref(),
                &key,
                Element::new_item(val),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item");
        }

        // Query all items, but with a limit of 3.
        let mut query = Query::new();
        query.insert_all();
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(query, Some(3), None),
        );

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof with limit");

        let (root_hash, result_set) = GroveDb::verify_query(&proof, &path_query, grove_version)
            .expect("should verify proof with limit");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root, "root hash should match");

        // Limit of 3 should return exactly 3 results.
        assert_eq!(result_set.len(), 3, "should have exactly 3 results");

        // Results should be the first 3 in sorted order.
        assert_eq!(result_set[0].1, b"item_00".to_vec());
        assert_eq!(result_set[1].1, b"item_01".to_vec());
        assert_eq!(result_set[2].1, b"item_02".to_vec());
    }

    // =========================================================================
    // verify_query_get_parent_tree_info tests
    // =========================================================================

    #[test]
    fn verify_returns_sum_tree_info() {
        // When querying inside a SumTree, verify_query_get_parent_tree_info
        // should return the SummedMerkNode feature type with the correct sum.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a SumTree under TEST_LEAF.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"my_sum_tree",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum tree");

        // Insert SumItems into the SumTree.
        db.insert(
            [TEST_LEAF, b"my_sum_tree"].as_ref(),
            b"s1",
            Element::new_sum_item(10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item s1");

        db.insert(
            [TEST_LEAF, b"my_sum_tree"].as_ref(),
            b"s2",
            Element::new_sum_item(25),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item s2");

        db.insert(
            [TEST_LEAF, b"my_sum_tree"].as_ref(),
            b"s3",
            Element::new_sum_item(7),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sum item s3");

        // Query inside the SumTree (no subquery, so parent tree info is available).
        let mut query = Query::new();
        query.insert_all();
        let path_query =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"my_sum_tree".to_vec()], query);

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate proof");

        let (root_hash, parent_feature_type, result_set) =
            GroveDb::verify_query_get_parent_tree_info(&proof, &path_query, grove_version)
                .expect("should verify proof with parent tree info");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root, "root hash should match");

        // The parent tree is a SumTree with sum = 10 + 25 + 7 = 42.
        assert_eq!(
            parent_feature_type,
            SummedMerkNode(42),
            "parent should be SummedMerkNode with sum 42"
        );

        // We should get all 3 sum items.
        assert_eq!(result_set.len(), 3, "should have 3 sum items");
    }

    // =========================================================================
    // Nested subquery proof tests
    // =========================================================================

    #[test]
    fn prove_and_verify_nested_query() {
        // Use the deep tree to test a PathQuery with a default subquery that
        // descends into nested subtrees.
        //
        // Tree Structure (relevant part):
        //   root
        //     test_leaf
        //       innertree
        //         key1 -> value1
        //         key2 -> value2
        //         key3 -> value3
        //       innertree4
        //         key4 -> value4
        //         key5 -> value5
        //
        // We query: path=[test_leaf], query=all, subquery=all.
        // This should return all items from both innertree and innertree4.
        let grove_version = GroveVersion::latest();
        let db = make_deep_tree(grove_version);

        let mut query = Query::new();
        query.insert_all();
        let mut subquery = Query::new();
        subquery.insert_all();
        query.set_subquery(subquery);

        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        // Generate proof.
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate nested query proof");

        // Verify proof.
        let (root_hash, result_set) = GroveDb::verify_query(&proof, &path_query, grove_version)
            .expect("should verify nested query proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root, "root hash should match");

        // We expect 5 results: key1, key2, key3 from innertree + key4, key5
        // from innertree4.
        assert_eq!(
            result_set.len(),
            5,
            "should have 5 results from nested query"
        );

        // Verify the paths are correct.
        let innertree_path = vec![TEST_LEAF.to_vec(), b"innertree".to_vec()];
        let innertree4_path = vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()];

        assert_eq!(result_set[0].0, innertree_path);
        assert_eq!(result_set[0].1, b"key1".to_vec());
        assert_eq!(result_set[0].2, Some(Element::new_item(b"value1".to_vec())));

        assert_eq!(result_set[1].0, innertree_path);
        assert_eq!(result_set[1].1, b"key2".to_vec());
        assert_eq!(result_set[1].2, Some(Element::new_item(b"value2".to_vec())));

        assert_eq!(result_set[2].0, innertree_path);
        assert_eq!(result_set[2].1, b"key3".to_vec());
        assert_eq!(result_set[2].2, Some(Element::new_item(b"value3".to_vec())));

        assert_eq!(result_set[3].0, innertree4_path);
        assert_eq!(result_set[3].1, b"key4".to_vec());
        assert_eq!(result_set[3].2, Some(Element::new_item(b"value4".to_vec())));

        assert_eq!(result_set[4].0, innertree4_path);
        assert_eq!(result_set[4].1, b"key5".to_vec());
        assert_eq!(result_set[4].2, Some(Element::new_item(b"value5".to_vec())));
    }

    #[test]
    fn prove_and_verify_nested_query_with_limit() {
        // Same structure as above but with a limit on the subquery results.
        let grove_version = GroveVersion::latest();
        let db = make_deep_tree(grove_version);

        let mut query = Query::new();
        query.insert_all();
        let mut subquery = Query::new();
        subquery.insert_all();
        query.set_subquery(subquery);

        // Limit to 3 results total.
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(query, Some(3), None),
        );

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate limited nested query proof");

        let (root_hash, result_set) = GroveDb::verify_query(&proof, &path_query, grove_version)
            .expect("should verify limited nested query proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root, "root hash should match");

        // We asked for 3, should get exactly 3: key1, key2, key3 from innertree.
        assert_eq!(result_set.len(), 3, "should have 3 results with limit");
        assert_eq!(result_set[0].1, b"key1".to_vec());
        assert_eq!(result_set[1].1, b"key2".to_vec());
        assert_eq!(result_set[2].1, b"key3".to_vec());
    }

    #[test]
    fn prove_and_verify_conditional_subquery() {
        // Test conditional subquery branches: different subqueries applied
        // depending on the matched key at the first level.
        //
        // Deep tree has under test_leaf: innertree (key1,key2,key3) and
        // innertree4 (key4,key5). We set up a conditional subquery so that
        // innertree gets subquery for key1 only, while innertree4 gets all.
        let grove_version = GroveVersion::latest();
        let db = make_deep_tree(grove_version);

        let mut query = Query::new();
        query.insert_all();

        // Default subquery: select nothing (will be overridden by conditionals).
        // We use a key query for a key that does not exist to effectively
        // return nothing from the default path.
        let mut default_subquery = Query::new();
        default_subquery.insert_key(b"nonexistent".to_vec());
        query.default_subquery_branch = SubqueryBranch {
            subquery_path: None,
            subquery: Some(Box::new(default_subquery)),
        };

        // Conditional: for "innertree" -> select only key1.
        let mut innertree_subquery = Query::new();
        innertree_subquery.insert_key(b"key1".to_vec());
        query.add_conditional_subquery(
            QueryItem::Key(b"innertree".to_vec()),
            None,
            Some(innertree_subquery),
        );

        // Conditional: for "innertree4" -> select all.
        let mut innertree4_subquery = Query::new();
        innertree4_subquery.insert_all();
        query.add_conditional_subquery(
            QueryItem::Key(b"innertree4".to_vec()),
            None,
            Some(innertree4_subquery),
        );

        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("should generate conditional subquery proof");

        // Use verify_subset_query since the proof contains more data than a
        // strict succinctness check expects (conditional queries may include
        // extra context).
        let (root_hash, result_set) = GroveDb::verify_query(&proof, &path_query, grove_version)
            .expect("should verify conditional subquery proof");

        let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(root_hash, expected_root, "root hash should match");

        // We should get: key1 from innertree + key4,key5 from innertree4 = 3.
        assert_eq!(
            result_set.len(),
            3,
            "should have 3 results from conditional subqueries"
        );

        assert_eq!(result_set[0].1, b"key1".to_vec());
        assert_eq!(result_set[0].2, Some(Element::new_item(b"value1".to_vec())));

        assert_eq!(result_set[1].1, b"key4".to_vec());
        assert_eq!(result_set[1].2, Some(Element::new_item(b"value4".to_vec())));

        assert_eq!(result_set[2].1, b"key5".to_vec());
        assert_eq!(result_set[2].2, Some(Element::new_item(b"value5".to_vec())));
    }

    // =========================================================================
    // key_exists_as_boundary_in_proof tests
    // =========================================================================

    #[test]
    fn grovedb_range_after_boundary_proof() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert items with keys 1..=5 under TEST_LEAF
        for i in 1u8..=5 {
            db.insert(
                [TEST_LEAF].as_ref(),
                &[i],
                Element::new_item(vec![i; 4]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert");
        }

        // Query: RangeAfter(3..) — results should be keys 4, 5.
        // Key 3 should be a boundary.
        let mut query = Query::new();
        query.insert_range_after(vec![3]..);
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove should succeed");

        // Decode proof once, then verify and check boundaries on the same object
        let config = bincode::config::standard()
            .with_big_endian()
            .with_limit::<{ 256 * 1024 * 1024 }>();
        let (grovedb_proof, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof_bytes, config).expect("should decode proof");

        // Verify proof is valid
        let (_, results) = grovedb_proof
            .verify(&path_query, grove_version)
            .expect("should verify");

        // Keys 4 and 5 should be in results
        assert!(results.iter().any(|r| r.1 == vec![4]));
        assert!(results.iter().any(|r| r.1 == vec![5]));
        // Key 3 should NOT be in results
        assert!(!results.iter().any(|r| r.1 == vec![3]));

        // Key 3 should exist as boundary in the proof at path [TEST_LEAF]
        assert!(
            grovedb_proof
                .key_exists_as_boundary(&[TEST_LEAF], &[3])
                .expect("should not error"),
            "Key 3 should be boundary in RangeAfter(3) proof"
        );

        // Key 4 should NOT be a boundary
        assert!(
            !grovedb_proof
                .key_exists_as_boundary(&[TEST_LEAF], &[4])
                .expect("should not error"),
            "Key 4 is a result element, not a boundary"
        );
    }
}
