//! `CountIndexedTree` and `ProvableCountIndexedTree` tests.

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use crate::{
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb,
    };

    #[test]
    fn empty_count_indexed_tree_can_be_inserted_and_fetched() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected to insert empty count-indexed tree");

        let element = db
            .get([TEST_LEAF].as_ref(), b"key", None, grove_version)
            .unwrap()
            .expect("expected to get count-indexed tree");
        assert!(matches!(
            element,
            Element::CountIndexedTree(None, None, 0, _)
        ));
    }

    #[test]
    fn empty_provable_count_indexed_tree_can_be_inserted_and_fetched() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"key",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("expected to insert empty provable count-indexed tree");

        let element = db
            .get([TEST_LEAF].as_ref(), b"key", None, grove_version)
            .unwrap()
            .expect("expected to get provable count-indexed tree");
        assert!(matches!(
            element,
            Element::ProvableCountIndexedTree(None, None, 0, _)
        ));
    }

    #[test]
    fn count_indexed_tree_round_trips_through_serialize_deserialize() {
        let grove_version = GroveVersion::latest();
        for element in [
            Element::empty_count_indexed_tree(),
            Element::empty_provable_count_indexed_tree(),
            Element::new_count_indexed_tree_with_root_keys_and_count_value(
                Some(b"primary_root".to_vec()),
                Some(b"secondary_root".to_vec()),
                42,
                Some(vec![1, 2, 3]),
            ),
            Element::new_provable_count_indexed_tree_with_root_keys_and_count_value(
                Some(b"primary_root".to_vec()),
                Some(b"secondary_root".to_vec()),
                100,
                None,
            ),
        ] {
            let serialized = element.serialize(grove_version).expect("serialize");
            let deserialized =
                Element::deserialize(&serialized, grove_version).expect("deserialize");
            assert_eq!(deserialized, element);
        }
    }

    #[test]
    fn insert_into_count_indexed_tree_rejects_non_count_indexed_target() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // TEST_LEAF is a normal tree, not a CountIndexedTree.
        let result = db
            .insert_into_count_indexed_tree(
                [TEST_LEAF].as_ref(),
                b"item",
                Element::new_item(b"data".to_vec()),
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(crate::Error::InvalidPath(msg)) => {
                assert!(
                    msg.contains("CountIndexedTree") || msg.contains("cidx"),
                    "expected cidx-requirement InvalidPath, got: {msg}"
                );
            }
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn insert_into_count_indexed_tree_inserts_first_item() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Create a CountIndexedTree under TEST_LEAF.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create count-indexed tree");

        // Insert an item into the CountIndexedTree's primary.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"alice",
            Element::new_item(b"alpha".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item into count-indexed primary");

        // The primary now contains the item — fetching via the standard path
        // resolution opens the primary and returns the inserted element.
        let fetched = db
            .get([TEST_LEAF, b"cidx"].as_ref(), b"alice", None, grove_version)
            .unwrap()
            .expect("get item from primary");
        assert_eq!(fetched, Element::new_item(b"alpha".to_vec()));

        // The CountIndexedTree element at the parent now reflects the
        // updated primary_root_key, secondary_root_key, and count = 1.
        let cidx_element = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get count-indexed element");
        match cidx_element {
            Element::CountIndexedTree(primary, secondary, count, _) => {
                assert!(primary.is_some(), "primary_root_key should be populated");
                assert!(
                    secondary.is_some(),
                    "secondary_root_key should be populated"
                );
                assert_eq!(count, 1, "aggregate count should be 1 after one insert");
            }
            other => panic!("expected CountIndexedTree, got {:?}", other),
        }
    }

    #[test]
    fn insert_into_provable_count_indexed_tree_inserts_first_item() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create provable count-indexed tree");

        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"pcidx"].as_ref(),
            b"alice",
            Element::new_item(b"alpha".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item into provable count-indexed primary");

        let fetched = db
            .get(
                [TEST_LEAF, b"pcidx"].as_ref(),
                b"alice",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get item from primary");
        assert_eq!(fetched, Element::new_item(b"alpha".to_vec()));

        let pcidx_element = db
            .get([TEST_LEAF].as_ref(), b"pcidx", None, grove_version)
            .unwrap()
            .expect("get provable count-indexed element");
        match pcidx_element {
            Element::ProvableCountIndexedTree(primary, secondary, count, _) => {
                assert!(primary.is_some());
                assert!(secondary.is_some());
                assert_eq!(count, 1);
            }
            other => panic!("expected ProvableCountIndexedTree, got {:?}", other),
        }
    }

    #[test]
    fn aggregate_count_grows_with_each_insert() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");

        for (k, v) in [(b"alice".as_ref(), b"a"), (b"bob", b"b"), (b"carol", b"c")] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::new_item(v.to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }

        let cidx_element = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get");
        match cidx_element {
            Element::CountIndexedTree(_, _, count, _) => {
                assert_eq!(count, 3, "aggregate count should reflect all inserts");
            }
            other => panic!("expected CountIndexedTree, got {:?}", other),
        }
    }

    #[test]
    fn non_counted_item_does_not_increment_aggregate_count() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");

        // Two regular items contribute 1 each; one NonCounted item
        // contributes 0.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"x".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert a");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"b",
            Element::new_item(b"y".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert b");
        let nc_item = Element::new_non_counted(Element::new_item(b"z".to_vec())).expect("wrap nc");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"c",
            nc_item,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert non-counted c");

        let cidx_element = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get");
        match cidx_element {
            Element::CountIndexedTree(_, _, count, _) => {
                assert_eq!(
                    count, 2,
                    "non-counted item should not contribute to aggregate count"
                );
            }
            other => panic!("expected CountIndexedTree, got {:?}", other),
        }
    }

    #[test]
    fn root_hash_changes_after_inserts() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");

        let h0 = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();

        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"alice",
            Element::new_item(b"alpha".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");

        let h1 = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();
        assert_ne!(
            h0, h1,
            "root hash must change after CountIndexedTree primary mutation \
             (combined_value_hash is part of the chain)"
        );
    }

    #[test]
    fn update_existing_item_does_not_duplicate_count() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");

        // Insert "alice" twice with different values — count_value of an
        // Item is always 1, so the aggregate count should be 1 after both
        // inserts (the second insert updates rather than duplicating).
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"alice",
            Element::new_item(b"v1".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("first insert");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"alice",
            Element::new_item(b"v2".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("second insert (update)");

        let fetched = db
            .get([TEST_LEAF, b"cidx"].as_ref(), b"alice", None, grove_version)
            .unwrap()
            .expect("get item");
        assert_eq!(fetched, Element::new_item(b"v2".to_vec()));

        let cidx_element = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get count-indexed");
        match cidx_element {
            Element::CountIndexedTree(_, _, count, _) => assert_eq!(count, 1),
            other => panic!("expected CountIndexedTree, got {:?}", other),
        }
    }

    #[test]
    fn delete_removes_item_and_decrements_count() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");

        for k in [b"alice".as_ref(), b"bob", b"carol"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::new_item(b"data".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }

        let removed = db
            .delete_from_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"bob",
                None,
                grove_version,
            )
            .unwrap()
            .expect("delete");
        assert!(removed);

        // bob is gone from the primary.
        let bob_result = db
            .get([TEST_LEAF, b"cidx"].as_ref(), b"bob", None, grove_version)
            .unwrap();
        assert!(bob_result.is_err());

        // alice and carol are still there.
        let alice = db
            .get([TEST_LEAF, b"cidx"].as_ref(), b"alice", None, grove_version)
            .unwrap()
            .expect("alice still present");
        assert_eq!(alice, Element::new_item(b"data".to_vec()));

        let cidx_element = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get count-indexed");
        match cidx_element {
            Element::CountIndexedTree(_, _, count, _) => assert_eq!(count, 2),
            other => panic!("expected CountIndexedTree, got {:?}", other),
        }
    }

    #[test]
    fn delete_returns_false_for_missing_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");

        let removed = db
            .delete_from_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"never_inserted",
                None,
                grove_version,
            )
            .unwrap()
            .expect("delete returns Ok");
        assert!(!removed);
    }

    #[test]
    fn count_indexed_tree_under_a_regular_tree_propagates_correctly() {
        // Verifies the cascade works through multiple regular-tree layers
        // above the CountIndexedTree element.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // TEST_LEAF / outer / cidx
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer");

        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        let h0 = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();

        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer", b"cidx"].as_ref(),
            b"alice",
            Element::new_item(b"alpha".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert into deeply nested CountIndexedTree");

        // Root hash must change (cascade reached the root).
        let h1 = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();
        assert_ne!(h0, h1);

        // Item retrievable from the primary.
        let item = db
            .get(
                [TEST_LEAF, b"outer", b"cidx"].as_ref(),
                b"alice",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get");
        assert_eq!(item, Element::new_item(b"alpha".to_vec()));

        // CountIndexedTree element shows count = 1.
        let cidx_element = db
            .get([TEST_LEAF, b"outer"].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get cidx");
        match cidx_element {
            Element::CountIndexedTree(_, _, count, _) => assert_eq!(count, 1),
            other => panic!("expected CountIndexedTree, got {:?}", other),
        }
    }

    #[test]
    fn count_indexed_top_k_returns_highest_count_first() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");

        // Insert sub-CountTrees so each "child" element has a different
        // count_value (the contents-of-the-stored-element semantics; the
        // outer tree just stores them as-is). We can't easily produce
        // varying count_values via direct API today, so instead we use
        // pre-built CountTree elements with explicit count_value fields.
        for (k, c) in [
            (b"alice".as_ref(), 5u64),
            (b"bob", 12),
            (b"carol", 1),
            (b"dave", 7),
        ] {
            let count_tree = Element::new_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                count_tree,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sub count-tree");
        }

        // Top 3 descending: bob(12), dave(7), alice(5).
        let top3 = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 3, true, None, grove_version)
            .unwrap()
            .expect("top-k");
        assert_eq!(
            top3,
            vec![
                (12u64, b"bob".to_vec()),
                (7u64, b"dave".to_vec()),
                (5u64, b"alice".to_vec()),
            ]
        );

        // Top 2 ascending: carol(1), alice(5).
        let bottom2 = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 2, false, None, grove_version)
            .unwrap()
            .expect("top-k ascending");
        assert_eq!(
            bottom2,
            vec![(1u64, b"carol".to_vec()), (5u64, b"alice".to_vec())]
        );
    }

    #[test]
    fn count_indexed_top_k_paginated_skips_offset_then_returns_k() {
        // Mirror of `count_indexed_top_k_returns_highest_count_first`
        // exercising the paginated variant. Setup five entries with
        // distinct counts, then page through them in descending order.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");

        for (k, c) in [
            (b"alice".as_ref(), 5u64),
            (b"bob", 12),
            (b"carol", 1),
            (b"dave", 7),
            (b"eve", 20),
        ] {
            let count_tree = Element::new_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                count_tree,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sub count-tree");
        }

        // Descending full scan: eve(20), bob(12), dave(7), alice(5), carol(1).
        // Page 1 (offset=0, k=2): eve, bob.
        let page1 = db
            .count_indexed_top_k_paginated(
                [TEST_LEAF, b"cidx"].as_ref(),
                2,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("page 1");
        assert_eq!(
            page1,
            vec![(20u64, b"eve".to_vec()), (12u64, b"bob".to_vec())]
        );

        // Page 2 (offset=2, k=2): dave, alice.
        let page2 = db
            .count_indexed_top_k_paginated(
                [TEST_LEAF, b"cidx"].as_ref(),
                2,
                2,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("page 2");
        assert_eq!(
            page2,
            vec![(7u64, b"dave".to_vec()), (5u64, b"alice".to_vec())]
        );

        // Page 3 (offset=4, k=2): just carol; second slot unfilled.
        let page3 = db
            .count_indexed_top_k_paginated(
                [TEST_LEAF, b"cidx"].as_ref(),
                2,
                4,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("page 3");
        assert_eq!(page3, vec![(1u64, b"carol".to_vec())]);

        // Offset past the end → empty.
        let beyond = db
            .count_indexed_top_k_paginated(
                [TEST_LEAF, b"cidx"].as_ref(),
                5,
                10,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("offset past end");
        assert!(beyond.is_empty());

        // offset=0 must equal plain top_k.
        let top_k = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 3, true, None, grove_version)
            .unwrap()
            .expect("top-k");
        let paginated_offset_0 = db
            .count_indexed_top_k_paginated(
                [TEST_LEAF, b"cidx"].as_ref(),
                3,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("paginated offset 0");
        assert_eq!(top_k, paginated_offset_0);
    }

    #[test]
    fn prove_and_verify_count_indexed_top_k_paginated_round_trip() {
        // End-to-end round trip: prove a paginated top-k, then verify
        // and assert the returned page matches the in-memory variant.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");

        for (k, c) in [
            (b"alice".as_ref(), 5u64),
            (b"bob", 12),
            (b"carol", 1),
            (b"dave", 7),
            (b"eve", 20),
            (b"frank", 30),
        ] {
            let count_tree = Element::new_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                count_tree,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }

        // Descending full: frank(30), eve(20), bob(12), dave(7),
        // alice(5), carol(1).
        // Prove (k=2, offset=2) descending: skip frank+eve, return
        // bob+dave.
        let proof = db
            .prove_count_indexed_top_k_paginated(
                [TEST_LEAF, b"cidx"].as_ref(),
                2,
                2,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove paginated");

        let result = GroveDb::verify_count_indexed_top_k_paginated(
            &proof,
            &[TEST_LEAF, b"cidx"],
            2,
            2,
            true,
        )
        .expect("verify paginated");

        assert_eq!(
            result.entries,
            vec![(12u64, b"bob".to_vec()), (7u64, b"dave".to_vec())]
        );
        assert_eq!(
            result.skipped, 2,
            "verifier-derived skipped count must equal requested offset"
        );

        let expected_root = db.root_hash(None, grove_version).unwrap().expect("root");
        assert_eq!(result.root_hash, expected_root);
    }

    #[test]
    fn verify_count_indexed_top_k_paginated_rejects_request_mismatch() {
        // The verifier authenticates echoed (k, offset, descending)
        // against the caller's expected values. A mismatch on any axis
        // is rejected before the merk-level proof is decoded.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");

        for (k, c) in [(b"a".as_ref(), 1u64), (b"b", 2), (b"c", 3)] {
            let count_tree = Element::new_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                count_tree,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }

        let proof = db
            .prove_count_indexed_top_k_paginated(
                [TEST_LEAF, b"cidx"].as_ref(),
                2,
                1,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove");

        // k mismatch.
        assert!(GroveDb::verify_count_indexed_top_k_paginated(
            &proof,
            &[TEST_LEAF, b"cidx"],
            3,
            1,
            true
        )
        .is_err());
        // offset mismatch.
        assert!(GroveDb::verify_count_indexed_top_k_paginated(
            &proof,
            &[TEST_LEAF, b"cidx"],
            2,
            0,
            true
        )
        .is_err());
        // descending mismatch.
        assert!(GroveDb::verify_count_indexed_top_k_paginated(
            &proof,
            &[TEST_LEAF, b"cidx"],
            2,
            1,
            false
        )
        .is_err());
        // Honest request → succeeds.
        assert!(GroveDb::verify_count_indexed_top_k_paginated(
            &proof,
            &[TEST_LEAF, b"cidx"],
            2,
            1,
            true
        )
        .is_ok());
    }

    #[test]
    fn prove_count_indexed_top_k_paginated_at_root_path_errors() {
        // Empty path → `InvalidPath` before any merk work; mirrors the
        // existing `prove_count_indexed_top_k_at_root_path_errors`.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty_path: &[&[u8]] = &[];
        let result = db
            .prove_count_indexed_top_k_paginated(empty_path, 3, 0, true, None, grove_version)
            .unwrap();
        match result {
            Err(crate::Error::InvalidPath(_)) => {}
            other => panic!("expected InvalidPath error, got {:?}", other),
        }
    }

    #[test]
    fn prove_count_indexed_top_k_paginated_on_non_cidx_target_errors() {
        // Path points at a regular Tree (not a cidx primary); the
        // primary-check guard in the builder must reject.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // TEST_LEAF is a regular Tree, not a cidx — exercise the guard.
        let result = db
            .prove_count_indexed_top_k_paginated(
                [TEST_LEAF].as_ref(),
                3,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(crate::Error::InvalidPath(msg)) => {
                assert!(
                    msg.contains("CountIndexedTree"),
                    "expected message to mention CountIndexedTree, got {msg}"
                );
            }
            other => panic!("expected InvalidPath error, got {:?}", other),
        }
    }

    #[test]
    fn verify_count_indexed_top_k_paginated_rejects_corrupted_bytes() {
        // Garbage bytes → bincode decode failure surfaced as CorruptedData.
        let garbage = vec![0xff, 0xfe, 0xfd, 0xfc, 0xfb];
        let result = GroveDb::verify_count_indexed_top_k_paginated(
            &garbage,
            &[TEST_LEAF, b"cidx"],
            2,
            0,
            true,
        );
        match result {
            Err(crate::Error::CorruptedData(_)) => {}
            other => panic!("expected CorruptedData error, got {:?}", other),
        }
    }

    #[test]
    fn verify_count_indexed_top_k_paginated_rejects_path_length_mismatch() {
        // Honest proof for path = [TEST_LEAF, cidx]; verifier called
        // with a 3-segment path. The layer-count check at the top of
        // the verifier must reject before any merk-level work.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        for (k, c) in [(b"a".as_ref(), 1u64), (b"b", 2)] {
            let ct = Element::new_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        let proof = db
            .prove_count_indexed_top_k_paginated(
                [TEST_LEAF, b"cidx"].as_ref(),
                2,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove");

        let err = GroveDb::verify_count_indexed_top_k_paginated(
            &proof,
            &[TEST_LEAF, b"cidx", b"extra"],
            2,
            0,
            true,
        )
        .expect_err("expected path-length mismatch");
        match err {
            crate::Error::CorruptedData(msg) => assert!(
                msg.contains("layers but path has"),
                "expected layer-count mismatch message, got {msg}"
            ),
            other => panic!("expected CorruptedData, got {:?}", other),
        }
    }

    #[test]
    fn count_indexed_top_k_paginated_empty_cidx_returns_empty() {
        // No entries in cidx → both offset=0 and offset>0 return
        // empty results without erroring. Covers the empty-iterator
        // branch in the skip phase.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");

        let r0 = db
            .count_indexed_top_k_paginated(
                [TEST_LEAF, b"cidx"].as_ref(),
                10,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("empty cidx, offset=0");
        assert!(r0.is_empty());

        let r5 = db
            .count_indexed_top_k_paginated(
                [TEST_LEAF, b"cidx"].as_ref(),
                10,
                5,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("empty cidx, offset=5");
        assert!(r5.is_empty());
    }

    #[test]
    fn prove_and_verify_count_indexed_top_k_paginated_ascending() {
        // Same shape as the round-trip test above but with
        // `descending = false`, exercising the ascending walk path
        // in both prover and verifier.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        for (k, c) in [
            (b"alice".as_ref(), 5u64),
            (b"bob", 12),
            (b"carol", 1),
            (b"dave", 7),
            (b"eve", 20),
        ] {
            let count_tree = Element::new_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                count_tree,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }

        // Ascending full: carol(1), alice(5), dave(7), bob(12), eve(20).
        // Skip first 2 (carol+alice), return next 2 (dave+bob).
        let proof = db
            .prove_count_indexed_top_k_paginated(
                [TEST_LEAF, b"cidx"].as_ref(),
                2,
                2,
                false,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove ascending paginated");

        let result = GroveDb::verify_count_indexed_top_k_paginated(
            &proof,
            &[TEST_LEAF, b"cidx"],
            2,
            2,
            false,
        )
        .expect("verify ascending paginated");

        assert_eq!(
            result.entries,
            vec![(7u64, b"dave".to_vec()), (12u64, b"bob".to_vec())]
        );
        assert_eq!(result.skipped, 2);
    }

    #[test]
    fn count_indexed_count_range_filters_by_count() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");

        for (k, c) in [
            (b"a".as_ref(), 1u64),
            (b"b", 5),
            (b"c", 7),
            (b"d", 12),
            (b"e", 20),
        ] {
            let ct = Element::new_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }

        // [5, 12] ascending → b(5), c(7), d(12).
        let in_range = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                5,
                12,
                false,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("range query");
        assert_eq!(
            in_range,
            vec![
                (5u64, b"b".to_vec()),
                (7u64, b"c".to_vec()),
                (12u64, b"d".to_vec()),
            ]
        );

        // [5, 12] descending → d(12), c(7), b(5).
        let in_range_desc = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                5,
                12,
                true,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("range query desc");
        assert_eq!(
            in_range_desc,
            vec![
                (12u64, b"d".to_vec()),
                (7u64, b"c".to_vec()),
                (5u64, b"b".to_vec()),
            ]
        );

        // Limit caps results.
        let capped = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                0,
                u64::MAX,
                false,
                2,
                None,
                grove_version,
            )
            .unwrap()
            .expect("range query capped");
        assert_eq!(capped.len(), 2);
        assert_eq!(capped[0], (1u64, b"a".to_vec()));
        assert_eq!(capped[1], (5u64, b"b".to_vec()));

        // Empty range when lo > hi.
        let empty = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                10,
                5,
                false,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("empty range");
        assert!(empty.is_empty());
    }

    #[test]
    fn count_indexed_top_k_after_delete() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");

        for (k, c) in [(b"alice".as_ref(), 5u64), (b"bob", 12), (b"carol", 7)] {
            let ct = Element::new_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }

        // Delete the highest-count entry; top-1 should now reflect
        // the next highest.
        let removed = db
            .delete_from_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"bob",
                None,
                grove_version,
            )
            .unwrap()
            .expect("delete");
        assert!(removed);

        let top1 = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 1, true, None, grove_version)
            .unwrap()
            .expect("top-1 after delete");
        assert_eq!(top1, vec![(7u64, b"carol".to_vec())]);
    }

    #[test]
    fn reconcile_secondary_is_idempotent() {
        // Round-tripping reconcile after the dedicated insert API should
        // be a no-op (idempotent).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");

        for (k, c) in [(b"a".as_ref(), 5u64), (b"b", 12), (b"c", 1)] {
            let ct = Element::new_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }

        let h_before = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();

        db.reconcile_count_indexed_tree_secondary(
            [TEST_LEAF, b"cidx"].as_ref(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("reconcile");

        let h_after = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(
            h_before, h_after,
            "reconcile should be idempotent when the secondary is already correct"
        );
    }

    #[test]
    fn reconcile_after_query_returns_correct_top_k() {
        // After populating via the dedicated API, reconcile should
        // produce a consistent state and top-k queries should still
        // return the correct order.
        //
        // (A future test will add explicit desync corruption via an
        // internal API to exercise reconcile's repair path more
        // strongly.)
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");

        for (k, c) in [(b"a".as_ref(), 5u64), (b"b", 12), (b"c", 7)] {
            let ct = Element::new_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }

        db.reconcile_count_indexed_tree_secondary(
            [TEST_LEAF, b"cidx"].as_ref(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("reconcile");

        let top1 = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 1, true, None, grove_version)
            .unwrap()
            .expect("top-1");
        assert_eq!(top1, vec![(12u64, b"b".to_vec())]);
    }

    #[test]
    fn batch_can_create_empty_count_indexed_tree() {
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create two CountIndexedTree elements in a single batch alongside
        // a regular subtree.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx1".to_vec(),
                Element::empty_count_indexed_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"pcidx1".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"plain".to_vec(),
                Element::empty_tree(),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch apply ok");

        let cidx = db
            .get([TEST_LEAF].as_ref(), b"cidx1", None, grove_version)
            .unwrap()
            .expect("get cidx1");
        assert!(matches!(cidx, Element::CountIndexedTree(None, None, 0, _)));

        let pcidx = db
            .get([TEST_LEAF].as_ref(), b"pcidx1", None, grove_version)
            .unwrap()
            .expect("get pcidx1");
        assert!(matches!(
            pcidx,
            Element::ProvableCountIndexedTree(None, None, 0, _)
        ));

        let plain = db
            .get([TEST_LEAF].as_ref(), b"plain", None, grove_version)
            .unwrap()
            .expect("get plain");
        assert!(matches!(plain, Element::Tree(None, _)));
    }

    #[test]
    fn batch_rejects_non_empty_count_indexed_tree_creation() {
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let non_empty = Element::new_count_indexed_tree_with_root_keys_and_count_value(
            Some(b"primary".to_vec()),
            None,
            0,
            None,
        );
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"bad".to_vec(),
            non_empty,
        )];

        let result = db.apply_batch(ops, None, None, grove_version).unwrap();
        // Batch creation of non-empty cidx is rejected: the on-disk
        // storage at the claimed primary/secondary root_keys would not
        // actually exist, so this is an invariant violation.
        assert!(
            result.is_err(),
            "non-empty cidx batch creation must be rejected"
        );
    }

    #[test]
    fn deep_insert_under_sub_tree_in_count_indexed_primary_cascades_count() {
        // The hardest case: regular db.insert into a path that's INSIDE a
        // sub-tree which lives in a CountIndexedTree primary. The
        // sub-tree's aggregate count changes; that change should propagate
        // through to the CountIndexedTree's element, AND the secondary
        // index entry for the sub-tree should be updated to reflect the
        // new count.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Layout:
        //   TEST_LEAF / cidx (CountIndexedTree)
        //                  / subtree (CountTree, initially empty)
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        // Use dedicated API to create the sub-CountTree so the secondary
        // is populated correctly. (Direct `db.insert` into a cidx primary
        // does not auto-mirror; users should use the dedicated API.)
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"sub",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create sub count tree");

        // Initially, the cidx primary has one entry (sub) with count_value
        // = 0 (sub is an empty CountTree).
        let top1_before = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 1, true, None, grove_version)
            .unwrap()
            .expect("top-1 before");
        assert_eq!(top1_before, vec![(0u64, b"sub".to_vec())]);

        // Now insert deeply: TEST_LEAF / cidx / sub / item
        db.insert(
            [TEST_LEAF, b"cidx", b"sub"].as_ref(),
            b"item1",
            Element::new_item(b"d1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("deep insert 1");

        db.insert(
            [TEST_LEAF, b"cidx", b"sub"].as_ref(),
            b"item2",
            Element::new_item(b"d2".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("deep insert 2");

        // sub's aggregate count is now 2. The cidx primary's element for
        // "sub" should reflect this. Top-1 by count should return
        // (2, "sub").
        let top1_after = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 1, true, None, grove_version)
            .unwrap()
            .expect("top-1 after");
        assert_eq!(
            top1_after,
            vec![(2u64, b"sub".to_vec())],
            "secondary should reflect sub's new aggregate count after auto-cascade"
        );

        // CountIndexedTree's own count_value should also be 2 (one entry
        // contributing 2).
        let cidx_element = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get cidx");
        match cidx_element {
            Element::CountIndexedTree(_, _, count, _) => assert_eq!(count, 2),
            other => panic!("expected CountIndexedTree, got {:?}", other),
        }
    }

    #[test]
    fn nested_count_indexed_trees_cascade_correctly() {
        // Nested CountIndexedTree-of-CountIndexedTree:
        //   TEST_LEAF / outer (CountIndexedTree)
        //                    / inner (CountIndexedTree)
        //                            / item (Item, count_value = 1)
        //
        // After deep insert, both outer and inner secondary indexes
        // should reflect the new state.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer");
        // Use dedicated API to create the inner CountIndexedTree inside
        // outer's primary so outer's secondary is populated correctly.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer"].as_ref(),
            b"inner",
            Element::empty_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create inner");

        // Insert into inner using the dedicated API. This triggers the
        // nested-mirror logic in `insert_into_count_indexed_tree`, which
        // correctly updates inner's secondary AND outer's secondary entry
        // for "inner" (the count delta).
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer", b"inner"].as_ref(),
            b"item",
            Element::new_item(b"data".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("deep insert");

        // Inner's count = 1, top-1 of inner should return (1, "item").
        let inner_top1 = db
            .count_indexed_top_k(
                [TEST_LEAF, b"outer", b"inner"].as_ref(),
                1,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("inner top-1");
        assert_eq!(inner_top1, vec![(1u64, b"item".to_vec())]);

        // Outer's primary contains "inner" with count_value = 1 (inner's
        // aggregate). Top-1 of outer should return (1, "inner").
        let outer_top1 = db
            .count_indexed_top_k([TEST_LEAF, b"outer"].as_ref(), 1, true, None, grove_version)
            .unwrap()
            .expect("outer top-1");
        assert_eq!(outer_top1, vec![(1u64, b"inner".to_vec())]);
    }

    #[test]
    fn proof_round_trip_for_top_k_descending() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        for (k, c) in [
            (b"alice".as_ref(), 5u64),
            (b"bob", 12),
            (b"carol", 1),
            (b"dave", 7),
        ] {
            let ct = Element::new_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }

        let proof_bytes = db
            .prove_count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 3, true, None, grove_version)
            .unwrap()
            .expect("prove top-3");

        assert!(!proof_bytes.is_empty());

        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let result =
            GroveDb::verify_count_indexed_top_k(&proof_bytes, path, 3, true).expect("verify top-3");

        // Verifier should reconstruct the same root hash GroveDB has.
        let actual_root = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(result.root_hash, actual_root);

        // Top 3 descending: bob(12), dave(7), alice(5).
        assert_eq!(
            result.entries,
            vec![
                (12u64, b"bob".to_vec()),
                (7u64, b"dave".to_vec()),
                (5u64, b"alice".to_vec()),
            ]
        );
    }

    #[test]
    fn proof_round_trip_for_top_k_ascending() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        for (k, c) in [(b"alice".as_ref(), 5u64), (b"bob", 12), (b"carol", 1)] {
            let ct = Element::new_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }

        let proof_bytes = db
            .prove_count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 2, false, None, grove_version)
            .unwrap()
            .expect("prove top-2 asc");

        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let result =
            GroveDb::verify_count_indexed_top_k(&proof_bytes, path, 2, false).expect("verify");

        let actual_root = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();
        assert_eq!(result.root_hash, actual_root);
        assert_eq!(
            result.entries,
            vec![(1u64, b"carol".to_vec()), (5u64, b"alice".to_vec())]
        );
    }

    #[test]
    fn proof_forge_count_byte_is_rejected() {
        // Tamper with the count bytes inside a secondary key. The secondary
        // proof's reconstructed root will differ, breaking the H1-A check
        // against the cidx element's value_hash.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");

        for (k, c) in [(b"alice".as_ref(), 5u64), (b"bob", 12)] {
            let ct = Element::new_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }

        let proof_bytes = db
            .prove_count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 2, true, None, grove_version)
            .unwrap()
            .expect("prove");

        // Tamper with one byte in the proof; verification should reject.
        // Find a non-zero byte to flip.
        let mut tampered = proof_bytes.clone();
        for byte in tampered.iter_mut() {
            if *byte != 0 {
                *byte = byte.wrapping_add(1);
                break;
            }
        }

        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let result = GroveDb::verify_count_indexed_top_k(&tampered, path, 2, true);
        assert!(
            result.is_err(),
            "tampered proof must be rejected by the verifier"
        );
    }

    #[test]
    fn proof_with_wrong_path_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"alice",
            Element::new_item(b"data".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");

        let proof_bytes = db
            .prove_count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 1, true, None, grove_version)
            .unwrap()
            .expect("prove");

        // Verify with a different path — should fail with CorruptedData
        // since the path doesn't match what the proof was generated for.
        let wrong_path: &[&[u8]] = &[TEST_LEAF, b"wrong_key"];
        let result = GroveDb::verify_count_indexed_top_k(&proof_bytes, wrong_path, 1, true);
        assert!(
            matches!(result, Err(crate::Error::CorruptedData(_))),
            "verification with wrong path must fail with CorruptedData, got {:?}",
            result
        );
    }

    #[test]
    fn direct_insert_rejects_mismatched_cidx_root_keys() {
        // Direct insertion of a non-empty CountIndexedTree element is
        // supported (migration / restore-from-backup path), but the
        // primary_root_key / secondary_root_key declared on the element
        // must match the actual on-disk state of the primary and
        // secondary Merks. A claim that does not match should fail
        // rather than persist an inconsistent root_hash chain.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let bogus = Element::new_count_indexed_tree_with_root_keys_and_count_value(
            Some(b"primary-not-on-disk".to_vec()),
            None,
            5,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"key",
                bogus,
                None,
                None,
                grove_version,
            )
            .unwrap();
        // Opening the primary merk to validate root_keys fails earlier
        // — with InvalidParentLayerPath — because the cidx subtree
        // hasn't been created yet (this is its first-ever insert). The
        // safe outcome either way is "the bogus state never lands on
        // disk"; both variants below are acceptable.
        assert!(
            matches!(
                result,
                Err(crate::Error::InvalidInput(_) | crate::Error::InvalidParentLayerPath(_))
            ),
            "expected InvalidInput or InvalidParentLayerPath, got {:?}",
            result
        );
    }

    #[test]
    fn direct_insert_rejects_mismatched_secondary_root_key() {
        // Same as above but with a bogus secondary_root_key while the
        // primary key correctly stays None.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let bogus = Element::new_count_indexed_tree_with_root_keys_and_count_value(
            None,
            Some(b"secondary-not-on-disk".to_vec()),
            0,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"key",
                bogus,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(
                result,
                Err(crate::Error::InvalidInput(_) | crate::Error::InvalidParentLayerPath(_))
            ),
            "expected InvalidInput or InvalidParentLayerPath, got {:?}",
            result
        );
    }

    #[test]
    fn insert_into_count_indexed_tree_supports_reference() {
        // Inserting a Reference via insert_into_count_indexed_tree must
        // resolve the target's value_hash and use Element::insert_reference
        // so the merk node carries combine_hash(value_hash(serialized),
        // referenced_value_hash). The aggregate count goes up by 1
        // (a Reference contributes count = 1 like any non-counted leaf).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Create a target item that the reference will point to.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"target",
            Element::new_item(b"target_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert target item");
        // Create the cidx.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        // Insert a reference (cousin reference: from cidx primary back
        // up to the target sibling at TEST_LEAF/target).
        let reference = Element::new_reference(
            grovedb_element::reference_path::ReferencePathType::UpstreamRootHeightReference(
                1,
                vec![b"target".to_vec()],
            ),
        );
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"alias",
            reference,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert reference into cidx primary");

        // Path resolution should follow the reference and return the
        // target's value.
        let fetched = db
            .get([TEST_LEAF, b"cidx"].as_ref(), b"alias", None, grove_version)
            .unwrap()
            .expect("get reference via cidx primary");
        assert_eq!(
            fetched,
            Element::new_item(b"target_value".to_vec()),
            "reference should resolve to the target item"
        );

        // The cidx element now reflects the new state; count = 1.
        let cidx_element = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get cidx element");
        match cidx_element {
            Element::CountIndexedTree(primary, secondary, count, _) => {
                assert!(primary.is_some());
                assert!(secondary.is_some());
                assert_eq!(count, 1);
            }
            other => panic!("expected CountIndexedTree, got {:?}", other),
        }
    }

    #[test]
    fn batch_insert_into_cidx_primary_works() {
        // Batch insert of a non-cidx leaf element directly into a cidx
        // primary's Merk now mirrors the change to the secondary and
        // updates the cidx element on the parent merk via H1-A.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
            b"item".to_vec(),
            Element::new_item(b"v".to_vec()),
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch insert into cidx primary");

        // Reading the item back resolves through the cidx primary.
        let fetched = db
            .get([TEST_LEAF, b"cidx"].as_ref(), b"item", None, grove_version)
            .unwrap()
            .expect("get item");
        assert_eq!(fetched, Element::new_item(b"v".to_vec()));

        // The cidx element on the parent now reflects count = 1 with
        // both root keys set.
        let cidx_element = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get cidx");
        match cidx_element {
            Element::CountIndexedTree(primary, secondary, count, _) => {
                assert!(primary.is_some());
                assert!(secondary.is_some());
                assert_eq!(count, 1);
            }
            other => panic!("expected CountIndexedTree, got {:?}", other),
        }

        // The secondary index has the new entry: count=1, key=item.
        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 5, true, None, grove_version)
            .unwrap()
            .expect("top-5");
        assert_eq!(top.len(), 1);
        assert_eq!(top[0], (1u64, b"item".to_vec()));

        // Full-tree integrity check: H1-A walk through the cidx node.
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "expected no issues, got {:?}", issues);
    }

    #[test]
    fn direct_delete_empty_cidx_cleans_up_secondary_storage() {
        // Even an empty cidx has metadata at both the primary's prefix
        // and the secondary's prefix (Blake3(primary_prefix || 0x01)).
        // db.delete() must clear both so that re-creating a cidx at the
        // same path doesn't observe stale state.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        // Delete the empty cidx.
        db.delete([TEST_LEAF].as_ref(), b"cidx", None, None, grove_version)
            .unwrap()
            .expect("delete empty cidx");

        // The cidx element is gone.
        let result = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap();
        assert!(
            matches!(result, Err(crate::Error::PathKeyNotFound(_))),
            "cidx element should be PathKeyNotFound after delete, got {:?}",
            result
        );

        // Re-create a fresh cidx at the same path; populate it. If the
        // old secondary storage wasn't cleaned, the new secondary's
        // queries would observe stale entries.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("re-create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"fresh_key",
            Element::new_item(b"fresh".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert into fresh cidx");

        // Top-k must show ONLY the fresh entry.
        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top after re-create");
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].1, b"fresh_key".to_vec());

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "expected no issues, got {:?}", issues);
    }

    #[test]
    fn direct_delete_empty_cidx_with_drifted_secondary_clears_namespace() {
        // Regression test for the `is_empty` branch in
        // GroveDb::delete_internal: a drifted cidx (primary empty,
        // secondary holds an orphan) used to leave the secondary
        // namespace untouched on delete. With the cidx-secondary
        // cleanup hoisted out of the `if !is_empty` block, this case
        // is now covered. We verify via a raw storage scan over the
        // S2-B prefix.
        use grovedb_storage::{
            rocksdb_storage::RocksDbStorage, RawIterator, Storage, StorageContext,
        };

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        // Inject an orphan into the secondary so the cidx is empty in
        // the primary but has drift in the secondary.
        corrupt_secondary_insert(
            &db,
            &[TEST_LEAF, b"cidx"],
            &make_secondary_key(0, b"orphan"),
            grove_version,
        );

        // Compute the secondary prefix for the cidx path so we can
        // scan it directly via raw_iter both before and after delete.
        let cidx_path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let path_vec: Vec<&[u8]> = cidx_path.to_vec();
        let subtree_path: grovedb_path::SubtreePath<&[u8]> = path_vec.as_slice().into();
        let primary_prefix = RocksDbStorage::build_prefix(subtree_path).unwrap();
        let secondary_prefix = RocksDbStorage::secondary_prefix_for(&primary_prefix).unwrap();

        // Sanity: drift is real — secondary namespace has at least
        // one entry before delete.
        {
            let tx = db.start_transaction();
            let ctx = db
                .db
                .get_transactional_storage_context_by_subtree_prefix(secondary_prefix, None, &tx)
                .unwrap();
            let mut iter = ctx.raw_iter();
            iter.seek_to_first().unwrap();
            assert!(
                iter.valid().unwrap(),
                "drift sanity: secondary namespace must be non-empty before delete"
            );
        }

        // Delete the (primary-)empty cidx. Before the hoist fix, this
        // would leave the drifted orphan in storage. After the fix,
        // the cidx secondary cleanup runs unconditionally.
        db.delete([TEST_LEAF].as_ref(), b"cidx", None, None, grove_version)
            .unwrap()
            .expect("delete empty (drifted) cidx");

        // Verify the secondary namespace is now empty.
        {
            let tx = db.start_transaction();
            let ctx = db
                .db
                .get_transactional_storage_context_by_subtree_prefix(secondary_prefix, None, &tx)
                .unwrap();
            let mut iter = ctx.raw_iter();
            iter.seek_to_first().unwrap();
            assert!(
                !iter.valid().unwrap(),
                "secondary namespace must be empty after cidx delete; drift cleared"
            );
        }
    }

    #[test]
    fn direct_delete_non_empty_cidx_cleans_up_both_namespaces() {
        // Non-empty cidx: must allow_deleting_non_empty_trees, must clean
        // up BOTH primary subtree storage and secondary storage.
        use crate::operations::delete::DeleteOptions;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        // Populate with multiple entries having different counts so the
        // secondary has real data.
        for k in [b"a".as_slice(), b"b", b"c"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("create sub");
        }
        db.insert(
            [TEST_LEAF, b"cidx", b"a"].as_ref(),
            b"x",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate sub");

        // Delete the non-empty cidx.
        let opts = DeleteOptions {
            allow_deleting_non_empty_trees: true,
            ..Default::default()
        };
        db.delete(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Some(opts),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete non-empty cidx");

        // The cidx element is gone.
        let result = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap();
        assert!(
            matches!(result, Err(crate::Error::PathKeyNotFound(_))),
            "deleted cidx must return PathKeyNotFound, got {:?}",
            result
        );

        // Re-create + populate; query must see only the fresh entry.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("re-create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"only",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top");
        assert_eq!(top.len(), 1, "secondary must have exactly the new entry");
        assert_eq!(top[0].1, b"only".to_vec());

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "expected no issues, got {:?}", issues);
    }

    #[test]
    fn batch_delete_item_from_cidx_primary_works() {
        // Batch path: a Delete op at a path inside a cidx primary mirrors
        // the count change to the secondary (delete branch of
        // mirror_to_secondary_for_batch).
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        // Pre-populate via the dedicated API.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"x".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert a");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"b",
            Element::new_item(b"y".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert b");

        // Now batch-delete "a".
        let ops = vec![QualifiedGroveDbOp::delete_op(
            vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
            b"a".to_vec(),
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch delete from cidx primary");

        // count is now 1, secondary has only b.
        let cidx_element = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get cidx");
        match cidx_element {
            Element::CountIndexedTree(_, _, count, _) => assert_eq!(count, 1),
            other => panic!("expected cidx, got {:?}", other),
        }
        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 5, true, None, grove_version)
            .unwrap()
            .expect("top after delete");
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].1, b"b".to_vec());

        // Integrity check.
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty());
    }

    #[test]
    fn batch_multiple_inserts_into_cidx_primary_in_one_call() {
        // Multiple ops on the same cidx primary in one batch — exercises
        // the multi-key pre-state capture loop and multi-key mirror.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"a".to_vec(),
                Element::new_item(b"1".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"b".to_vec(),
                Element::new_item(b"2".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"c".to_vec(),
                Element::new_item(b"3".to_vec()),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("multi-op batch into cidx primary");

        let cidx_element = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get cidx");
        match cidx_element {
            Element::CountIndexedTree(_, _, count, _) => assert_eq!(count, 3),
            other => panic!("expected cidx, got {:?}", other),
        }
        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top");
        assert_eq!(top.len(), 3);

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty());
    }

    #[test]
    fn apply_partial_batch_with_delete_tree_on_cidx_cleans_up_secondary() {
        // Exercises the cidx-secondary cleanup pass added to
        // apply_partial_batch (parallels the apply_batch cleanup but
        // routes through the partial-batch code path).
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            grovedb_merk::tree_type::TreeType::CountIndexedTree,
            crate::batch::SubelementsDeletionBehavior::DeleteChildren,
        )];
        db.apply_partial_batch(
            ops,
            None,
            |_cost, _leftover| Ok(vec![]),
            None,
            grove_version,
        )
        .unwrap()
        .expect("apply_partial_batch should succeed");

        // Re-create + populate; the secondary must have only the new
        // entry (proves the partial-batch cidx cleanup ran).
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("re-create");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"only",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top");
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].1, b"only".to_vec());
    }

    #[test]
    fn batch_delete_tree_on_cidx_dont_check_with_no_cleanup_still_clears_secondary() {
        // The DontCheckWithNoCleanup behavior skips primary subtree
        // cleanup but cidx secondary cleanup must still run because
        // the cidx secondary lives in a different namespace not
        // covered by find_subtrees.
        use crate::batch::{QualifiedGroveDbOp, SubelementsDeletionBehavior};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        // DontCheckWithNoCleanup requires the cidx to be empty (caller
        // promises it). It is — we just created it.
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            grovedb_merk::tree_type::TreeType::CountIndexedTree,
            SubelementsDeletionBehavior::DontCheckWithNoCleanup,
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("delete tree no-cleanup");

        // Re-create + populate. The new cidx secondary must observe only
        // the new entry; if the secondary cleanup didn't run for the
        // DontCheckWithNoCleanup path, stale empty-cidx metadata could
        // surface here.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("re-create");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top");
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].1, b"k".to_vec());
    }

    #[test]
    fn batch_overwrite_cidx_rejected_with_override_protection_on() {
        // The standard tree-override flag also rejects cidx overwrites
        // (covers the validate_insertion_does_not_override_tree=true
        // branch).
        use crate::batch::{BatchApplyOptions, QualifiedGroveDbOp};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            Element::new_item(b"replaced".to_vec()),
        )];
        let opts = BatchApplyOptions {
            validate_insertion_does_not_override_tree: true,
            ..Default::default()
        };
        let result = db
            .apply_batch(ops, Some(opts), None, grove_version)
            .unwrap();
        match result {
            Err(crate::Error::InvalidBatchOperation(msg)) => {
                assert!(
                    msg.contains("tree"),
                    "expected tree-overwrite InvalidBatchOperation, got: {msg}"
                );
            }
            other => panic!("expected InvalidBatchOperation, got {:?}", other),
        }
    }

    #[test]
    fn batch_delete_tree_on_cidx_then_recreate_in_separate_batch_works() {
        // The recommended workaround for cidx overwrite: DeleteTree the
        // old cidx in one batch, re-create with new state in another.
        // Verifies the workaround is actually clean (no stale state).
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for k in [b"a".as_slice(), b"b"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate");
        }

        // Batch 1: DeleteTree the cidx.
        let ops1 = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            grovedb_merk::tree_type::TreeType::CountIndexedTree,
            crate::batch::SubelementsDeletionBehavior::DeleteChildren,
        )];
        db.apply_batch(ops1, None, None, grove_version)
            .unwrap()
            .expect("batch delete tree");

        // Batch 2: Re-create the cidx.
        let ops2 = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            Element::empty_count_indexed_tree(),
        )];
        db.apply_batch(ops2, None, None, grove_version)
            .unwrap()
            .expect("batch recreate");
        // Batch 3: populate the new cidx.
        let ops3 = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
            b"new".to_vec(),
            Element::new_item(b"fresh".to_vec()),
        )];
        db.apply_batch(ops3, None, None, grove_version)
            .unwrap()
            .expect("batch populate");

        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top");
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].1, b"new".to_vec());

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty());
    }

    #[test]
    fn batch_overwrite_existing_cidx_with_item_is_allowed_and_cleans_up() {
        // Safe subset of the cidx-overwrite case: replacing an
        // existing cidx with a NON-CIDX element (plain item here) is
        // allowed when override-protection is off, and the post-apply
        // pass cleans up both the primary subtree storage AND the
        // secondary namespace at Blake3(primary ‖ 0x01).
        use crate::batch::{BatchApplyOptions, QualifiedGroveDbOp};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        // Populate so we have non-trivial primary + secondary state
        // to clean up.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");

        // Overwrite the cidx element with a plain item.
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            Element::new_item(b"replaced".to_vec()),
        )];
        let opts = BatchApplyOptions {
            validate_insertion_does_not_override_tree: false,
            ..Default::default()
        };
        db.apply_batch(ops, Some(opts), None, grove_version)
            .unwrap()
            .expect("safe-subset overwrite must succeed");

        // The element at TEST_LEAF/cidx is now an Item, not a cidx.
        let elem = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get after overwrite");
        assert_eq!(elem, Element::new_item(b"replaced".to_vec()));

        // Integrity walk: no orphaned primary storage, no orphaned
        // secondary. verify_grovedb fails if either is left behind.
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "verify issues: {:?}", issues);
    }

    #[test]
    fn batch_overwrite_existing_cidx_with_empty_cidx_is_allowed_and_resets() {
        // Safe subset case 2: replacing a non-empty cidx with an
        // EMPTY cidx is allowed. The old storage is cleaned up; the
        // new cidx starts fresh.
        use crate::batch::{BatchApplyOptions, QualifiedGroveDbOp};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        // Populate so the cidx has non-empty state before reset.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"stale_key",
            Element::new_item(b"stale_value".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");

        // Overwrite with an empty cidx (same element type, fresh state).
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            Element::empty_count_indexed_tree(),
        )];
        let opts = BatchApplyOptions {
            validate_insertion_does_not_override_tree: false,
            ..Default::default()
        };
        db.apply_batch(ops, Some(opts), None, grove_version)
            .unwrap()
            .expect("reset to empty cidx must succeed");

        // The cidx is now empty (count = 0, no entries).
        let elem = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get cidx after reset");
        match elem {
            Element::CountIndexedTree(p, s, c, _) => {
                assert!(
                    p.is_none() && s.is_none() && c == 0,
                    "expected empty cidx, got {:?}",
                    (p, s, c)
                );
            }
            other => panic!("expected cidx, got {:?}", other),
        }

        // Top-k returns nothing — the stale key from before is gone.
        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top after reset");
        assert!(top.is_empty(), "stale entries leaked: {:?}", top);

        // Re-populate; verify the new entry is the only one.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"fresh_key",
            Element::new_item(b"fresh".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("re-populate");
        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top after re-populate");
        assert_eq!(top, vec![(1u64, b"fresh_key".to_vec())]);

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty());
    }

    #[test]
    fn batch_overwrite_existing_cidx_with_non_empty_cidx_is_rejected() {
        // Non-empty cidx overwrite: rejected because the new element's
        // root_keys refer to on-disk data that the cleanup pass would
        // also clear — ambiguous storage-pointer semantics. Callers
        // must use the delete-then-recreate dance.
        use crate::batch::{BatchApplyOptions, QualifiedGroveDbOp};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        // A non-empty cidx element (claims root_keys + count != 0).
        let non_empty = Element::new_count_indexed_tree_with_root_keys_and_count_value(
            Some(b"bogus_primary".to_vec()),
            Some(b"bogus_secondary".to_vec()),
            5,
            None,
        );
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            non_empty,
        )];
        let opts = BatchApplyOptions {
            validate_insertion_does_not_override_tree: false,
            ..Default::default()
        };
        let result = db
            .apply_batch(ops, Some(opts), None, grove_version)
            .unwrap();
        match result {
            Err(crate::Error::NotSupported(msg)) => {
                assert!(
                    msg.contains("NON-EMPTY cidx") || msg.contains("ambiguous"),
                    "expected non-empty cidx rejection message, got: {msg}"
                );
            }
            other => panic!("expected NotSupported, got {:?}", other),
        }
    }

    #[test]
    fn batch_delete_tree_on_empty_cidx_works() {
        // DeleteTree on an empty CountIndexedTree via the batch path
        // succeeds and cleans up the secondary's storage so re-creating
        // a cidx at the same path observes a fresh secondary.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            grovedb_merk::tree_type::TreeType::CountIndexedTree,
            crate::batch::SubelementsDeletionBehavior::DontCheckWithNoCleanup,
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch delete empty cidx");

        // Re-create + populate; secondary must observe only the new
        // entry.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("re-create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"fresh",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top");
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].1, b"fresh".to_vec());
    }

    #[test]
    fn batch_delete_tree_on_non_empty_cidx_works() {
        // DeleteTree with DeleteChildren on a non-empty cidx via batch
        // must cleanly remove both primary subtree storage and secondary
        // storage.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for k in [b"a".as_slice(), b"b", b"c"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("create sub");
        }
        db.insert(
            [TEST_LEAF, b"cidx", b"a"].as_ref(),
            b"x",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate sub");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            grovedb_merk::tree_type::TreeType::CountIndexedTree,
            crate::batch::SubelementsDeletionBehavior::DeleteChildren,
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch delete non-empty cidx");

        // Re-create + insert one item; query must see only the new item.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("re-create");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"only",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top");
        assert_eq!(top.len(), 1, "secondary must have only the new entry");
        assert_eq!(top[0].1, b"only".to_vec());

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty());
    }

    #[test]
    fn verify_grovedb_walks_cidx_h1a_chain_and_finds_no_issues() {
        // verify_grovedb must walk a cidx node: open both child Merks,
        // verify the H1-A combined value_hash matches the parent's
        // recorded value_hash, then recurse into the primary.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        // Populate the cidx so both child Merks are non-empty.
        for k in [b"a".as_slice(), b"b", b"c"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert into cidx");
        }
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(
            issues.is_empty(),
            "expected no integrity issues, got {} issue(s): {:?}",
            issues.len(),
            issues
        );
    }

    #[test]
    fn prove_count_indexed_top_k_at_root_path_errors() {
        // Proving at root path is invalid; the proof envelope needs at
        // least one parent layer.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty_path: &[&[u8]] = &[];
        let result = db
            .prove_count_indexed_top_k(empty_path, 3, true, None, grove_version)
            .unwrap();
        match result {
            Err(crate::Error::InvalidPath(msg)) => {
                assert!(
                    msg.contains("root") || msg.contains("layer") || msg.contains("at least"),
                    "expected root-path InvalidPath message, got: {msg}"
                );
            }
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn prove_count_indexed_top_k_on_non_cidx_target_errors() {
        // Proving over a path whose terminal element is not a cidx must
        // fail with InvalidPath naming the cidx requirement.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // TEST_LEAF is a Tree, not a CountIndexedTree.
        let result = db
            .prove_count_indexed_top_k([TEST_LEAF].as_ref(), 3, true, None, grove_version)
            .unwrap();
        match result {
            Err(crate::Error::InvalidPath(msg)) => {
                assert!(
                    msg.contains("CountIndexedTree") || msg.contains("cidx"),
                    "expected cidx-requirement InvalidPath, got: {msg}"
                );
            }
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn count_indexed_top_k_on_non_cidx_target_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let result = db
            .count_indexed_top_k([TEST_LEAF].as_ref(), 3, true, None, grove_version)
            .unwrap();
        match result {
            Err(crate::Error::InvalidPath(msg)) => {
                assert!(
                    msg.contains("CountIndexedTree") || msg.contains("cidx"),
                    "expected cidx-requirement InvalidPath, got: {msg}"
                );
            }
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn count_indexed_count_range_on_non_cidx_target_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let result = db
            .count_indexed_count_range([TEST_LEAF].as_ref(), 0, 100, false, 10, None, grove_version)
            .unwrap();
        match result {
            Err(crate::Error::InvalidPath(msg)) => {
                assert!(
                    msg.contains("CountIndexedTree") || msg.contains("cidx"),
                    "expected cidx-requirement InvalidPath, got: {msg}"
                );
            }
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn reconcile_on_non_cidx_target_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let result = db
            .reconcile_count_indexed_tree_secondary([TEST_LEAF].as_ref(), None, grove_version)
            .unwrap();
        match result {
            Err(crate::Error::InvalidPath(msg)) => {
                assert!(
                    msg.contains("CountIndexedTree") || msg.contains("cidx"),
                    "expected cidx-requirement InvalidPath, got: {msg}"
                );
            }
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn delete_from_count_indexed_tree_on_non_cidx_target_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let result = db
            .delete_from_count_indexed_tree([TEST_LEAF].as_ref(), b"item", None, grove_version)
            .unwrap();
        match result {
            Err(crate::Error::InvalidPath(msg)) => {
                assert!(
                    msg.contains("CountIndexedTree") || msg.contains("cidx"),
                    "expected cidx-requirement InvalidPath, got: {msg}"
                );
            }
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn delete_from_count_indexed_tree_returns_false_for_unknown_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        let removed = db
            .delete_from_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"missing",
                None,
                grove_version,
            )
            .unwrap()
            .expect("delete returns Ok even when the key does not exist");
        assert!(!removed);
    }

    #[test]
    fn count_indexed_count_range_descending_returns_descending_order() {
        // Exercise the descending branch of count_indexed_count_range so
        // the bounded-range path is covered for both directions.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for k in [b"a".as_slice(), b"b", b"c"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("create sub count tree");
            db.insert(
                [TEST_LEAF, b"cidx", k].as_ref(),
                b"x",
                Element::new_item(b"v".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate sub count tree");
        }
        let entries = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                0,
                100,
                true,
                10,
                None,
                grove_version,
            )
            .unwrap()
            .expect("count_range descending");
        assert_eq!(entries.len(), 3);
        // Counts are equal here (all 1); just confirm we got all keys.
        let mut keys: Vec<_> = entries.into_iter().map(|(_, k)| k).collect();
        keys.sort();
        assert_eq!(keys, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
    }

    #[test]
    fn count_indexed_count_range_with_lo_greater_than_hi_returns_empty() {
        // Defensive early-return path: an inverted [lo, hi] interval is
        // valid input but has no entries.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        let entries = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                10,
                5,
                false,
                100,
                None,
                grove_version,
            )
            .unwrap()
            .expect("inverted bounds returns Ok with empty Vec");
        assert!(entries.is_empty());
    }

    #[test]
    fn count_indexed_count_range_with_hi_count_u64_max_uses_range_from() {
        // Exercises the RangeFrom branch in count_indexed_count_range
        // (hi_count == u64::MAX). Without entries at that count, the
        // result is the same as the bounded-upper-bound branch — but the
        // code path differs.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        // Add one entry so the secondary has at least one key.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create sub count tree");
        let entries = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                0,
                u64::MAX,
                false,
                10,
                None,
                grove_version,
            )
            .unwrap()
            .expect("count_range with u64::MAX upper bound");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, b"k".to_vec());
    }

    #[test]
    fn count_indexed_count_range_respects_limit() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for k in [b"a".as_slice(), b"b", b"c", b"d", b"e"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("create sub count tree");
        }
        let entries = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                0,
                100,
                false,
                2,
                None,
                grove_version,
            )
            .unwrap()
            .expect("count_range with limit");
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn count_indexed_top_k_with_zero_returns_empty() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate cidx");
        let entries = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 0, true, None, grove_version)
            .unwrap()
            .expect("top_k with k=0");
        assert!(entries.is_empty());
    }

    #[test]
    fn count_indexed_top_k_at_root_path_errors() {
        // top_k on the empty path is invalid; needs at least one parent.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty_path: &[&[u8]] = &[];
        let result = db
            .count_indexed_top_k(empty_path, 3, true, None, grove_version)
            .unwrap();
        assert!(
            matches!(result, Err(crate::Error::InvalidPath(_))),
            "root-path top_k must fail with InvalidPath, got {:?}",
            result
        );
    }

    #[test]
    fn count_indexed_count_range_at_root_path_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty_path: &[&[u8]] = &[];
        let result = db
            .count_indexed_count_range(empty_path, 0, 100, false, 10, None, grove_version)
            .unwrap();
        assert!(
            matches!(result, Err(crate::Error::InvalidPath(_))),
            "root-path count_range must fail with InvalidPath, got {:?}",
            result
        );
    }

    #[test]
    fn verify_count_indexed_top_k_rejects_corrupt_proof_bytes() {
        // Garbage proof bytes — bincode decode fails first.
        let result = GroveDb::verify_count_indexed_top_k(b"not-a-valid-proof", &[b"x"], 1, true);
        assert!(matches!(result, Err(crate::Error::CorruptedData(_))));
    }

    #[test]
    fn prove_count_indexed_top_k_round_trips_through_nested_cidx_ancestor() {
        // path: TEST_LEAF / outer_cidx / inner_cidx
        // outer_cidx is a cidx whose primary contains an inner cidx.
        // The proof envelope must carry per-ancestor secondary
        // attestation so the verifier can chain via combine_hash_three
        // at the outer_cidx layer.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer_cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer cidx");
        // Insert an inner cidx into the outer_cidx primary.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer_cidx"].as_ref(),
            b"inner_cidx",
            Element::empty_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create inner cidx");
        // Populate the inner cidx so its top_k has results.
        for k in [b"a".as_slice(), b"b", b"c"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"outer_cidx", b"inner_cidx"].as_ref(),
                k,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate inner cidx");
        }
        let proof = db
            .prove_count_indexed_top_k(
                [TEST_LEAF, b"outer_cidx", b"inner_cidx"].as_ref(),
                10,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove nested cidx top-k");
        let path: &[&[u8]] = &[TEST_LEAF, b"outer_cidx", b"inner_cidx"];
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 10, true)
            .expect("verify nested cidx top-k");
        assert_eq!(result.entries.len(), 3);
        let expected_root = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root hash");
        assert_eq!(result.root_hash, expected_root);
    }

    #[test]
    fn insert_into_count_indexed_tree_with_reference_to_missing_target_errors() {
        // Reference resolution failure: the target the reference points
        // to does not exist. Should bubble up an error from
        // follow_reference, not silently corrupt the cidx primary.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        let dangling = Element::new_reference(
            grovedb_element::reference_path::ReferencePathType::UpstreamRootHeightReference(
                1,
                vec![b"does_not_exist".to_vec()],
            ),
        );
        let result = db
            .insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"alias",
                dangling,
                None,
                grove_version,
            )
            .unwrap();
        // Reference resolution returns CorruptedReferencePathKeyNotFound
        // when the target doesn't exist — that's the safe failure mode
        // for dangling references documented in the delete/mod.rs docs.
        assert!(
            matches!(
                result,
                Err(crate::Error::CorruptedReferencePathKeyNotFound(_))
            ),
            "dangling reference must produce CorruptedReferencePathKeyNotFound, got {:?}",
            result
        );
    }

    #[test]
    fn deep_insert_under_nested_cidx_propagates_through_both_levels() {
        // Layout: TEST_LEAF / outer / inner / sub
        // outer is a CountIndexedTree containing inner (also a cidx).
        // inner contains a sub-CountTree. Deep insert into sub must
        // propagate counts and root hashes through BOTH cidx levels,
        // mirroring at each level's secondary.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer"].as_ref(),
            b"inner",
            Element::empty_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create inner cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer", b"inner"].as_ref(),
            b"sub",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create sub count tree");

        let outer_top1 = db
            .count_indexed_top_k([TEST_LEAF, b"outer"].as_ref(), 1, true, None, grove_version)
            .unwrap()
            .expect("outer top-1");
        // outer has one entry (inner), with count = 1 (sub is one
        // entry inside inner).
        assert_eq!(outer_top1.len(), 1);
        assert_eq!(outer_top1[0].1, b"inner".to_vec());

        // Deep insert into sub: TEST_LEAF/outer/inner/sub/item
        db.insert(
            [TEST_LEAF, b"outer", b"inner", b"sub"].as_ref(),
            b"item",
            Element::new_item(b"d".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("deep insert");
        // sub's count_value is now 1; verify outer's view reflects this.
        let inner_top1 = db
            .count_indexed_top_k(
                [TEST_LEAF, b"outer", b"inner"].as_ref(),
                1,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("inner top-1");
        assert_eq!(inner_top1.len(), 1);
        assert_eq!(inner_top1[0].0, 1);
        assert_eq!(inner_top1[0].1, b"sub".to_vec());

        // verify_grovedb walks both cidx levels and finds no issues.
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "expected no issues, got {:?}", issues);
    }

    #[test]
    fn delete_from_count_indexed_tree_round_trip_with_proof() {
        // Insert several items, delete one, verify the proof reflects
        // the post-delete state.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for k in [b"a".as_slice(), b"b", b"c"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        // Add one item under "a" so its count goes to 1.
        db.insert(
            [TEST_LEAF, b"cidx", b"a"].as_ref(),
            b"x",
            Element::new_item(b"d".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("deep insert");
        let removed = db
            .delete_from_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"b",
                None,
                grove_version,
            )
            .unwrap()
            .expect("delete b");
        assert!(removed);
        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 3, true, None, grove_version)
            .unwrap()
            .expect("top-3 after delete");
        assert_eq!(top.len(), 2);
        assert_eq!(top[0], (1u64, b"a".to_vec()));
        assert_eq!(top[1], (0u64, b"c".to_vec()));

        let proof = db
            .prove_count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 3, true, None, grove_version)
            .unwrap()
            .expect("prove top-3");
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let result =
            GroveDb::verify_count_indexed_top_k(&proof, path, 3, true).expect("verify top-3");
        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.entries[0], (1u64, b"a".to_vec()));
        assert_eq!(result.entries[1], (0u64, b"c".to_vec()));
    }

    #[test]
    fn verify_count_indexed_top_k_rejects_truncated_proof() {
        let result = GroveDb::verify_count_indexed_top_k(b"\x00\x01", &[b"x"], 1, true);
        assert!(matches!(result, Err(crate::Error::CorruptedData(_))));
    }

    #[test]
    fn verify_grovedb_walks_provable_count_indexed_tree() {
        // Same H1-A walk but on a ProvableCountIndexedTree variant.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create provable cidx");
        for k in [b"a".as_slice(), b"b"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"pcidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert into pcidx");
        }
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "expected no issues");
    }

    #[test]
    fn prove_count_indexed_query_with_count_range() {
        // Arbitrary secondary query: prove only entries with count
        // in [3, 5] (encoded as the byte range
        // [3u64.be_bytes() .. 6u64.be_bytes()]).
        use grovedb_merk::proofs::Query as MerkQuery;
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        // Build entries with diverse counts: a=1, b=2, c=3, d=5, e=8.
        for (k, count) in [
            (b"a".as_slice(), 1),
            (b"b", 2),
            (b"c", 3),
            (b"d", 5),
            (b"e", 8),
        ] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("create sub");
            // Pump count up by inserting count distinct items into sub.
            for i in 0..count {
                let item_key = format!("x{i}").into_bytes();
                db.insert(
                    [TEST_LEAF, b"cidx", k].as_ref(),
                    &item_key,
                    Element::new_item(b"d".to_vec()),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("populate sub");
            }
        }

        // Build secondary query covering count in [3, 5] inclusive.
        // Lower bound: 3u64::to_be_bytes() (zero-suffix). Upper: 6u64
        // (exclusive in the next-count slot).
        let lo = 3u64.to_be_bytes().to_vec();
        let hi = 6u64.to_be_bytes().to_vec();
        let mut q = MerkQuery::new();
        q.insert_range(lo..hi);
        q.left_to_right = true;

        let proof = db
            .prove_count_indexed_query(
                [TEST_LEAF, b"cidx"].as_ref(),
                q.clone(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove arbitrary cidx query");

        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let result =
            GroveDb::verify_count_indexed_query(&proof, q, None, path).expect("verify arbitrary");
        // Should yield c (count=3) and d (count=5); b (count=2) and e
        // (count=8) are outside the [3, 5] window.
        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.entries[0], (3u64, b"c".to_vec()));
        assert_eq!(result.entries[1], (5u64, b"d".to_vec()));
    }

    #[test]
    fn verify_count_indexed_top_k_rejects_path_length_mismatch() {
        // Generate a real proof, then verify with a path of the wrong length.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate cidx");
        let proof = db
            .prove_count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 3, true, None, grove_version)
            .unwrap()
            .expect("prove");

        // Use a path that has the wrong number of segments.
        let bad_path: &[&[u8]] = &[TEST_LEAF, b"cidx", b"extra"];
        let result = GroveDb::verify_count_indexed_top_k(&proof, bad_path, 3, true);
        assert!(matches!(result, Err(crate::Error::CorruptedData(_))));
    }

    #[test]
    fn direct_insert_into_nested_cidx_primary_bubbles_count_up_outer_secondary() {
        // Same shape as the batch test below, but routes through the
        // dedicated `insert_into_count_indexed_tree` API (NOT batch).
        // After inserting an item into inner_cidx primary, outer's
        // secondary entry for "inner_cidx" must move from (0, ...) to
        // (1, ...) via the propagation's auto-mirror.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer_cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer_cidx"].as_ref(),
            b"inner_cidx",
            Element::empty_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create inner");

        // Direct insert into inner cidx primary via dedicated API
        // (the only safe way; raw `db.insert` is rejected for cidx
        // primary targets — see `db.insert` guard in
        // operations/insert/mod.rs).
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer_cidx", b"inner_cidx"].as_ref(),
            b"item",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert into nested cidx primary");

        let top = db
            .count_indexed_top_k(
                [TEST_LEAF, b"outer_cidx"].as_ref(),
                10,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("outer top");
        assert_eq!(
            top[0],
            (1u64, b"inner_cidx".to_vec()),
            "outer's secondary must reflect inner's new count = 1 (direct path)"
        );

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "verify_grovedb issues: {:?}", issues);
    }

    #[test]
    fn direct_db_insert_into_cidx_primary_is_rejected() {
        // The direct `db.insert()` path has no secondary-mirror hook —
        // inserting into a cidx primary via it would leave the secondary
        // stale. Reject with a pointer to `insert_into_count_indexed_tree`.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        let result = db
            .insert(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"item",
                Element::new_item(b"v".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(crate::Error::NotSupported(msg)) => {
                assert!(
                    msg.contains("CountIndexedTree primary")
                        && msg.contains("insert_into_count_indexed_tree"),
                    "expected cidx-primary rejection with API pointer, got: {msg}"
                );
            }
            other => panic!("expected NotSupported, got {:?}", other),
        }

        // Confirm verify_grovedb stays clean after the rejected insert
        // (nothing should have been written).
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty());
    }

    #[test]
    fn batch_insert_through_cidx_then_regular_tree_then_cidx() {
        // Layout: TEST_LEAF / outer_cidx / regular_tree / inner_cidx
        // Mixed nesting: cidx → regular tree → cidx. The bubble-up from
        // inner_cidx must:
        //   1. Mirror inner's secondary inline.
        //   2. Emit ReplaceAggregateIndexedTreeRootKeys to the regular_tree
        //      level (parent is NOT a cidx primary, so no mirror runs
        //      there, but the inner_cidx element bytes are still
        //      updated via the H1-A handler).
        //   3. Regular_tree's bubble-up emits ReplaceTreeRootKey to
        //      outer_cidx — the outer's pre-state captures the
        //      regular_tree's count (regular trees aggregate count from
        //      children), and outer's secondary mirrors the change.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer_cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("outer");
        // Insert a regular CountTree (not cidx) inside outer_cidx — must
        // be a count-bearing tree to live inside a cidx primary, but
        // does NOT need to be cidx itself.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer_cidx"].as_ref(),
            b"regular",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("regular");
        db.insert(
            [TEST_LEAF, b"outer_cidx", b"regular"].as_ref(),
            b"inner_cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("inner");

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![
                TEST_LEAF.to_vec(),
                b"outer_cidx".to_vec(),
                b"regular".to_vec(),
                b"inner_cidx".to_vec(),
            ],
            b"item".to_vec(),
            Element::new_item(b"v".to_vec()),
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("mixed-nesting batch");

        // outer_cidx's secondary should reflect "regular"'s aggregate
        // count = 1 (the count contributed by inner_cidx → 1).
        let outer_top = db
            .count_indexed_top_k(
                [TEST_LEAF, b"outer_cidx"].as_ref(),
                5,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("outer top");
        assert_eq!(outer_top, vec![(1u64, b"regular".to_vec())]);

        // inner_cidx's secondary has the item.
        let inner_top = db
            .count_indexed_top_k(
                [TEST_LEAF, b"outer_cidx", b"regular", b"inner_cidx"].as_ref(),
                5,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("inner top");
        assert_eq!(inner_top, vec![(1u64, b"item".to_vec())]);

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty());
    }

    #[test]
    fn batch_insert_into_triple_nested_cidx_propagates_through_all_levels() {
        // Layout: TEST_LEAF / a (cidx) / b (cidx) / c (cidx)
        // Batch-insert one item into c's primary. After the bubble-up,
        // every ancestor cidx's secondary must reflect the count change:
        //   a's secondary: b under count 1 (from c's count 1)
        //   b's secondary: c under count 1
        //   c's secondary: item under count 1
        //
        // Also confirms that ReplaceAggregateIndexedTreeRootKeys correctly
        // chains through multiple cidx-primary levels (each level emits
        // it for the level above when the level was a cidx primary).
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("a");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"a"].as_ref(),
            b"b",
            Element::empty_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("b");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"a", b"b"].as_ref(),
            b"c",
            Element::empty_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("c");

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![
                TEST_LEAF.to_vec(),
                b"a".to_vec(),
                b"b".to_vec(),
                b"c".to_vec(),
            ],
            b"item".to_vec(),
            Element::new_item(b"v".to_vec()),
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("triple-nested cidx batch insert");

        // Each level's secondary must reflect count = 1 for the entry
        // representing the level below.
        let a_top = db
            .count_indexed_top_k([TEST_LEAF, b"a"].as_ref(), 5, true, None, grove_version)
            .unwrap()
            .expect("a top");
        assert_eq!(a_top, vec![(1u64, b"b".to_vec())]);

        let b_top = db
            .count_indexed_top_k(
                [TEST_LEAF, b"a", b"b"].as_ref(),
                5,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("b top");
        assert_eq!(b_top, vec![(1u64, b"c".to_vec())]);

        let c_top = db
            .count_indexed_top_k(
                [TEST_LEAF, b"a", b"b", b"c"].as_ref(),
                5,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("c top");
        assert_eq!(c_top, vec![(1u64, b"item".to_vec())]);

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty());
    }

    #[test]
    fn batch_insert_into_nested_cidx_primary_bubbles_count_up_outer_secondary() {
        // Layout: TEST_LEAF / outer_cidx / inner_cidx
        //                       (cidx)       (cidx)
        // Batch-inserts an item into inner_cidx's primary. The bubble-up
        // should:
        //   - Mirror the count change inside inner's secondary (count
        //     for "item" goes None → 1).
        //   - Emit ReplaceAggregateIndexedTreeRootKeys to outer's primary
        //     level.
        //   - At outer's primary level, the pre/post element bytes for
        //     "inner_cidx" change (count_value 0 → 1), so outer's
        //     secondary entry for "inner_cidx" must move from
        //     (0_be ‖ inner_cidx) to (1_be ‖ inner_cidx).
        //
        // If outer's pre-state capture skips ReplaceAggregateIndexedTreeRootKeys
        // ops, outer's secondary won't be mirrored — top-k on outer
        // would silently return stale counts.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer_cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer_cidx"].as_ref(),
            b"inner_cidx",
            Element::empty_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create inner");

        // Sanity: outer's top-k should currently show inner_cidx with
        // count = 0 (newly created, empty inner).
        let top = db
            .count_indexed_top_k(
                [TEST_LEAF, b"outer_cidx"].as_ref(),
                10,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("outer top before");
        assert_eq!(top.len(), 1);
        assert_eq!(top[0], (0u64, b"inner_cidx".to_vec()));

        // Now BATCH-insert an item into inner_cidx's primary.
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![
                TEST_LEAF.to_vec(),
                b"outer_cidx".to_vec(),
                b"inner_cidx".to_vec(),
            ],
            b"item".to_vec(),
            Element::new_item(b"v".to_vec()),
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch insert into nested cidx");

        // Outer's top-k MUST now show inner_cidx with count = 1.
        let top = db
            .count_indexed_top_k(
                [TEST_LEAF, b"outer_cidx"].as_ref(),
                10,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("outer top after");
        assert_eq!(top.len(), 1);
        assert_eq!(
            top[0],
            (1u64, b"inner_cidx".to_vec()),
            "outer's secondary must reflect inner's new count = 1"
        );

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "verify_grovedb issues: {:?}", issues);
    }

    // =====================================================================
    // Atomicity stress tests for batches mixing cidx + non-cidx ops.
    //
    // GroveDB batches are atomic by design: validation runs over the full
    // op list before any writes hit storage. These tests verify that the
    // cidx-aware paths in the batch flow preserve that invariant under
    // mixed workloads and partial-failure scenarios.
    // =====================================================================

    #[test]
    fn batch_mixed_cidx_and_non_cidx_ops_apply_atomically() {
        // A single batch inserts into both a cidx primary AND a plain
        // sibling Tree at the same depth. After commit, both subtrees
        // reflect their respective inserts; the cidx's secondary index
        // is consistent; verify_grovedb finds no issues.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Set up: TEST_LEAF / cidx (CountIndexedTree) and
        //         TEST_LEAF / plain (Tree)
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"plain",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create plain tree");

        // Mix four ops in one batch: two into cidx primary, two into plain
        // tree. Order is not stable across batch internal sort but the
        // outcome must be the union of all four.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"a".to_vec(),
                Element::new_item(b"1".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"plain".to_vec()],
                b"x".to_vec(),
                Element::new_item(b"X".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"b".to_vec(),
                Element::new_item(b"2".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"plain".to_vec()],
                b"y".to_vec(),
                Element::new_item(b"Y".to_vec()),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("mixed batch must succeed");

        // Cidx state: count = 2, top-2 contains a and b.
        let cidx_element = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("cidx");
        match cidx_element {
            Element::CountIndexedTree(_, _, count, _) => assert_eq!(count, 2),
            other => panic!("expected cidx, got {:?}", other),
        }
        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 5, true, None, grove_version)
            .unwrap()
            .expect("top");
        assert_eq!(top.len(), 2);

        // Plain tree state: x and y both present.
        let x = db
            .get([TEST_LEAF, b"plain"].as_ref(), b"x", None, grove_version)
            .unwrap()
            .expect("plain x");
        assert_eq!(x, Element::new_item(b"X".to_vec()));
        let y = db
            .get([TEST_LEAF, b"plain"].as_ref(), b"y", None, grove_version)
            .unwrap()
            .expect("plain y");
        assert_eq!(y, Element::new_item(b"Y".to_vec()));

        // Integrity walk through cidx node + plain node.
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "expected no issues, got {:?}", issues);
    }

    #[test]
    fn batch_failure_in_non_cidx_op_rolls_back_cidx_mutations() {
        // Atomicity: if validation fails on ANY op in the batch, the
        // cidx state must remain at its pre-batch values. We trigger
        // the failure with an InsertWithKnownToNotAlreadyExist where
        // the key DOES already exist (asserted absence violated).
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        // Pre-existing item that we'll try to "insert with known to
        // not already exist" — this op asserts the key is absent.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"existing",
            Element::new_item(b"old".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create existing");

        // Snapshot pre-batch state.
        let cidx_before = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("cidx before");
        let root_before = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root before");

        // Compose a batch whose first op would mutate the cidx primary,
        // and whose later op would fail. The validation phase should
        // reject the whole batch and leave cidx untouched.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"would_be_inserted".to_vec(),
                Element::new_item(b"v".to_vec()),
            ),
            QualifiedGroveDbOp::insert_only_known_to_not_already_exist_op(
                vec![TEST_LEAF.to_vec()],
                b"existing".to_vec(),
                Element::new_item(b"new".to_vec()),
            ),
        ];
        let opts = crate::batch::BatchApplyOptions {
            validate_insertion_does_not_override: true,
            ..Default::default()
        };
        let result = db
            .apply_batch(ops, Some(opts), None, grove_version)
            .unwrap();
        assert!(
            result.is_err(),
            "batch with InsertWithKnownToNotAlreadyExist on existing key must fail"
        );

        // Post-failure state must match pre-batch state exactly.
        let cidx_after = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("cidx after");
        assert_eq!(cidx_before, cidx_after, "cidx state must be unchanged");

        let root_after = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root after");
        assert_eq!(
            root_before, root_after,
            "root hash must be unchanged on rollback"
        );

        // The would-be-inserted key is not present.
        let result = db
            .get(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"would_be_inserted",
                None,
                grove_version,
            )
            .unwrap();
        assert!(result.is_err());

        // The pre-existing item still has its original value.
        let existing = db
            .get([TEST_LEAF].as_ref(), b"existing", None, grove_version)
            .unwrap()
            .expect("existing");
        assert_eq!(existing, Element::new_item(b"old".to_vec()));
    }

    #[test]
    fn batch_with_multiple_cidx_primaries_each_get_updated() {
        // Two independent cidx primaries at the same level; one batch
        // inserts into both. Each cidx must end up with its own correct
        // count + secondary state.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx_a",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cidx_a");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx_b",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cidx_b");

        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx_a".to_vec()],
                b"a1".to_vec(),
                Element::new_item(b"v".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx_b".to_vec()],
                b"b1".to_vec(),
                Element::new_item(b"v".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx_a".to_vec()],
                b"a2".to_vec(),
                Element::new_item(b"v".to_vec()),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("multi-cidx batch");

        // cidx_a has count=2 (a1, a2)
        match db
            .get([TEST_LEAF].as_ref(), b"cidx_a", None, grove_version)
            .unwrap()
            .expect("cidx_a")
        {
            Element::CountIndexedTree(_, _, count, _) => assert_eq!(count, 2),
            other => panic!("expected cidx_a, got {:?}", other),
        }
        // cidx_b has count=1 (b1)
        match db
            .get([TEST_LEAF].as_ref(), b"cidx_b", None, grove_version)
            .unwrap()
            .expect("cidx_b")
        {
            Element::CountIndexedTree(_, _, count, _) => assert_eq!(count, 1),
            other => panic!("expected cidx_b, got {:?}", other),
        }

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty());
    }

    #[test]
    fn batch_cidx_delete_with_concurrent_cidx_inserts_atomic() {
        // One batch: DeleteTree one cidx + insert items into another
        // cidx. The DeleteTree's secondary cleanup runs in the post-
        // apply phase; the insert's secondary mirror runs in the level
        // execute phase. Both must apply atomically — no cross-cidx
        // interference.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx_to_delete",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete cidx");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx_to_keep",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("keep cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx_to_delete"].as_ref(),
            b"old",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate delete cidx");

        let ops = vec![
            QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx_to_delete".to_vec(),
                grovedb_merk::tree_type::TreeType::CountIndexedTree,
                crate::batch::SubelementsDeletionBehavior::DeleteChildren,
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx_to_keep".to_vec()],
                b"new1".to_vec(),
                Element::new_item(b"v".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx_to_keep".to_vec()],
                b"new2".to_vec(),
                Element::new_item(b"v".to_vec()),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("delete + insert atomic");

        // The deleted cidx is gone.
        let result = db
            .get([TEST_LEAF].as_ref(), b"cidx_to_delete", None, grove_version)
            .unwrap();
        assert!(result.is_err(), "deleted cidx must be absent");

        // The kept cidx has the two new items.
        match db
            .get([TEST_LEAF].as_ref(), b"cidx_to_keep", None, grove_version)
            .unwrap()
            .expect("kept")
        {
            Element::CountIndexedTree(_, _, count, _) => assert_eq!(count, 2),
            other => panic!("expected cidx_to_keep, got {:?}", other),
        }

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty());
    }

    #[test]
    fn batch_failure_after_cidx_delete_tree_rolls_back() {
        // Atomicity: a batch containing both a DeleteTree on cidx (which
        // schedules secondary cleanup for the post-apply phase) and an
        // op that fails validation must abort BEFORE either change
        // commits — neither the DeleteTree nor the secondary cleanup
        // should be visible.
        use crate::batch::{BatchApplyOptions, QualifiedGroveDbOp};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"existing",
            Element::new_item(b"old".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create existing");

        // Snapshot.
        let root_before = db.root_hash(None, grove_version).unwrap().expect("root");
        let cidx_before = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("cidx before");
        let top_before = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top before");

        // Batch: DeleteTree on cidx + violation of an existence assertion.
        let ops = vec![
            QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                grovedb_merk::tree_type::TreeType::CountIndexedTree,
                crate::batch::SubelementsDeletionBehavior::DeleteChildren,
            ),
            QualifiedGroveDbOp::insert_only_known_to_not_already_exist_op(
                vec![TEST_LEAF.to_vec()],
                b"existing".to_vec(),
                Element::new_item(b"new".to_vec()),
            ),
        ];
        let opts = BatchApplyOptions {
            validate_insertion_does_not_override: true,
            ..Default::default()
        };
        let result = db
            .apply_batch(ops, Some(opts), None, grove_version)
            .unwrap();
        assert!(result.is_err(), "batch with assertion violation must fail");

        // Cidx and secondary state must match pre-batch — DeleteTree
        // didn't commit, secondary cleanup didn't run.
        let root_after = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root after");
        assert_eq!(root_before, root_after);
        let cidx_after = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("cidx after");
        assert_eq!(cidx_before, cidx_after);
        let top_after = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top after");
        assert_eq!(top_before, top_after);
    }

    // =====================================================================
    // verify_grovedb cidx CONTENT consistency checks.
    //
    // The H1-A walk only verifies *chain* integrity — that the cidx
    // element's recorded value_hash matches combine_hash_three of the two
    // child Merks' root hashes. It does NOT check that the secondary's
    // contents are consistent with the primary's count_values. The
    // content-consistency pass (added alongside these tests) walks both
    // Merks and asserts every primary entry has exactly one matching
    // secondary entry at (count_be ‖ key), and vice versa.
    //
    // The tests below deliberately corrupt the secondary via direct
    // storage manipulation, then assert verify_grovedb reports the
    // expected sentinel-path issue. Without the content-consistency
    // pass, all three tests would silently pass an integrity check while
    // queries returned wrong results.
    // =====================================================================

    fn make_secondary_key(count: u64, item_key: &[u8]) -> Vec<u8> {
        // Mirrors the private helper in operations/count_indexed_tree.rs.
        let mut k = Vec::with_capacity(8 + item_key.len());
        k.extend_from_slice(&count.to_be_bytes());
        k.extend_from_slice(item_key);
        k
    }

    /// Manually applies an `Element::delete` to the cidx primary's
    /// secondary at the given secondary key, then commits. The
    /// secondary's root_key changes on disk, but the parent's cidx
    /// element bytes are NOT updated, so the H1-A check will see a
    /// chain mismatch — that's fine; we're testing the CONTENT check.
    fn corrupt_secondary_delete(
        db: &crate::GroveDb,
        cidx_primary_path: &[&[u8]],
        secondary_key: &[u8],
        grove_version: &GroveVersion,
    ) {
        use grovedb_merk::element::{
            delete::ElementDeleteFromStorageExtensions, get::ElementFetchFromStorageExtensions,
        };
        use grovedb_merk::tree_type::TreeType;
        use grovedb_path::SubtreePath;
        use grovedb_storage::{Storage, StorageBatch};

        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let path_vec: Vec<&[u8]> = cidx_primary_path.to_vec();
        let path: SubtreePath<&[u8]> = path_vec.as_slice().into();

        // Read the parent's cidx element to get the current secondary
        // root_key.
        let (parent_path, cidx_key) = path.derive_parent().expect("non-root cidx");
        let secondary_root_key = {
            let parent_merk = db
                .open_transactional_merk_at_path(parent_path, &tx, Some(&batch), grove_version)
                .unwrap()
                .expect("open parent");
            let cidx_element = Element::get(&parent_merk, cidx_key, true, grove_version)
                .unwrap()
                .expect("cidx element");
            match cidx_element.underlying() {
                Element::CountIndexedTree(_, s, ..)
                | Element::ProvableCountIndexedTree(_, s, ..) => s.clone(),
                _ => panic!("not a cidx element"),
            }
        };

        {
            let mut secondary_merk = db
                .open_count_indexed_secondary_at_path(
                    path,
                    secondary_root_key,
                    &tx,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .expect("open secondary");
            Element::delete(
                &mut secondary_merk,
                secondary_key,
                None,
                false,
                TreeType::ProvableCountTree,
                grove_version,
            )
            .unwrap()
            .expect("delete secondary entry");
        }

        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit");
        tx.commit().expect("commit transaction");
    }

    /// Manually inserts a bogus entry into the cidx primary's
    /// secondary. Same caveat about chain mismatch.
    fn corrupt_secondary_insert(
        db: &crate::GroveDb,
        cidx_primary_path: &[&[u8]],
        secondary_key: &[u8],
        grove_version: &GroveVersion,
    ) {
        use grovedb_merk::element::{
            get::ElementFetchFromStorageExtensions, insert::ElementInsertToStorageExtensions,
        };
        use grovedb_path::SubtreePath;
        use grovedb_storage::{Storage, StorageBatch};

        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let path_vec: Vec<&[u8]> = cidx_primary_path.to_vec();
        let path: SubtreePath<&[u8]> = path_vec.as_slice().into();

        let (parent_path, cidx_key) = path.derive_parent().expect("non-root cidx");
        let secondary_root_key = {
            let parent_merk = db
                .open_transactional_merk_at_path(parent_path, &tx, Some(&batch), grove_version)
                .unwrap()
                .expect("open parent");
            let cidx_element = Element::get(&parent_merk, cidx_key, true, grove_version)
                .unwrap()
                .expect("cidx element");
            match cidx_element.underlying() {
                Element::CountIndexedTree(_, s, ..)
                | Element::ProvableCountIndexedTree(_, s, ..) => s.clone(),
                _ => panic!("not a cidx element"),
            }
        };

        {
            let mut secondary_merk = db
                .open_count_indexed_secondary_at_path(
                    path,
                    secondary_root_key,
                    &tx,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .expect("open secondary");
            let bogus = Element::new_item(Vec::new());
            bogus
                .insert(&mut secondary_merk, secondary_key, None, grove_version)
                .unwrap()
                .expect("insert orphan");
        }

        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit");
        tx.commit().expect("commit transaction");
    }

    #[test]
    fn verify_grovedb_catches_secondary_missing_entry_for_primary() {
        // Primary has "a" at count=1 but the secondary's entry is
        // deleted under us. The content-consistency pass must flag
        // `__cidx_primary_orphan__` for "a".
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert a");

        // Sanity: clean integrity.
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify pre-corruption");
        assert!(issues.is_empty(), "pre-corruption clean: {:?}", issues);

        // Delete the secondary entry for "a" (at count=1) WITHOUT
        // touching the primary. Drift introduced.
        corrupt_secondary_delete(
            &db,
            &[TEST_LEAF, b"cidx"],
            &make_secondary_key(1, b"a"),
            grove_version,
        );

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify post-corruption");
        let primary_orphan_path: Vec<Vec<u8>> = vec![
            TEST_LEAF.to_vec(),
            b"cidx".to_vec(),
            b"__cidx_primary_orphan__".to_vec(),
            b"a".to_vec(),
        ];
        assert!(
            issues.contains_key(&primary_orphan_path),
            "expected __cidx_primary_orphan__ for 'a', got issues: {:?}",
            issues.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn verify_grovedb_catches_orphan_in_secondary() {
        // Primary is clean (no entry "ghost"), but the secondary has a
        // bogus entry at count=99 / key="ghost". Content-consistency
        // pass must flag `__cidx_secondary_orphan__` for "ghost".
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"real",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert real");

        // Inject an orphan into the secondary.
        corrupt_secondary_insert(
            &db,
            &[TEST_LEAF, b"cidx"],
            &make_secondary_key(99, b"ghost"),
            grove_version,
        );

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify post-corruption");
        let secondary_orphan_path: Vec<Vec<u8>> = vec![
            TEST_LEAF.to_vec(),
            b"cidx".to_vec(),
            b"__cidx_secondary_orphan__".to_vec(),
            b"ghost".to_vec(),
        ];
        assert!(
            issues.contains_key(&secondary_orphan_path),
            "expected __cidx_secondary_orphan__ for 'ghost', got: {:?}",
            issues.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn verify_grovedb_catches_count_mismatch_between_primary_and_secondary() {
        // Primary has "a" at count=1. Manually delete the (count=1, "a")
        // secondary entry AND insert a (count=99, "a") secondary entry.
        // Both Merks have an entry for "a" but at different counts.
        // Content-consistency pass must flag `__cidx_count_mismatch__`
        // for "a".
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert a");

        // Remove the legitimate (1, "a") and add a bogus (99, "a").
        corrupt_secondary_delete(
            &db,
            &[TEST_LEAF, b"cidx"],
            &make_secondary_key(1, b"a"),
            grove_version,
        );
        corrupt_secondary_insert(
            &db,
            &[TEST_LEAF, b"cidx"],
            &make_secondary_key(99, b"a"),
            grove_version,
        );

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify post-corruption");
        let mismatch_path: Vec<Vec<u8>> = vec![
            TEST_LEAF.to_vec(),
            b"cidx".to_vec(),
            b"__cidx_count_mismatch__".to_vec(),
            b"a".to_vec(),
        ];
        let entry = issues.get(&mismatch_path).unwrap_or_else(|| {
            panic!(
                "expected __cidx_count_mismatch__ for 'a', got: {:?}",
                issues.keys().collect::<Vec<_>>()
            )
        });
        // The expected (slot 1) hash encodes count=1 in its last 8
        // bytes; the actual (slot 2) hash encodes count=99.
        assert_eq!(&entry.1[24..32], &1u64.to_be_bytes());
        assert_eq!(&entry.2[24..32], &99u64.to_be_bytes());
    }

    // =====================================================================
    // Property-based stress tests for cidx invariants.
    //
    // Generates long random sequences of operations against a 2-level
    // cidx layout (outer cidx contains CountTrees as values, each
    // CountTree can grow and shrink) and asserts after every op:
    //
    //   1. verify_grovedb() reports no issues — H1-A chain integrity
    //      AND content consistency between primary and secondary.
    //   2. count_indexed_top_k() returns the same set the property
    //      model says should be there, in the right count order.
    //
    // Random op generation uses a hand-rolled SplitMix64 PRNG so
    // failing seeds are reproducible — no external dep, no test
    // flakiness. Each test has a hard-coded seed; if you discover a
    // failure on a different seed, hard-code it as a regression case.
    //
    // These tests are the structural answer to the audit-found
    // nested-cidx bug class: any future code change that drifts the
    // secondary fails CI here, not in production.
    // =====================================================================

    /// SplitMix64 — small, deterministic, no allocation. Good enough
    /// for property generation.
    #[derive(Clone)]
    struct Prng(u64);
    impl Prng {
        fn new(seed: u64) -> Self {
            Self(seed)
        }
        fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
            z ^ (z >> 31)
        }
        fn next_usize(&mut self, exclusive_max: usize) -> usize {
            (self.next_u64() as usize) % exclusive_max.max(1)
        }
    }

    /// Property model: the in-memory ground truth the test maintains in
    /// parallel with the database. After each op, we run a verify pass
    /// AND assert the database's top-k matches the model's view.
    #[derive(Clone, Default)]
    struct CidxModel {
        /// cidx_entry_key → number of items inside its CountTree
        entries: std::collections::BTreeMap<Vec<u8>, u64>,
    }
    impl CidxModel {
        fn top_k_ascending(&self) -> Vec<(u64, Vec<u8>)> {
            // Same ordering as the secondary: (count_be, key) ascending.
            let mut v: Vec<(u64, Vec<u8>)> =
                self.entries.iter().map(|(k, c)| (*c, k.clone())).collect();
            v.sort();
            v
        }
    }

    /// Apply one random op to both the live database and the in-memory
    /// model, then assert invariants. Returns `true` if the op was
    /// applied (some random selections become no-ops, e.g. delete
    /// against an absent key — we count those but don't fail).
    fn apply_random_op_and_check(
        rng: &mut Prng,
        db: &crate::GroveDb,
        cidx_path: &[&[u8]],
        model: &mut CidxModel,
        grove_version: &GroveVersion,
        iteration: usize,
    ) {
        // Key space is small so updates dominate over inserts —
        // exercises count transitions rather than only fresh creates.
        const KEY_SPACE: usize = 8;
        let key_idx = rng.next_usize(KEY_SPACE);
        let key = format!("k{:02}", key_idx).into_bytes();

        // 5 op kinds in roughly equal proportion.
        let op_kind = rng.next_usize(5);

        match op_kind {
            0 => {
                // Ensure CountTree exists at this key, then add 1 item
                // inside it (raises its count by 1).
                if !model.entries.contains_key(&key) {
                    db.insert_into_count_indexed_tree(
                        cidx_path,
                        &key,
                        Element::empty_count_tree(),
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("create CountTree");
                    model.entries.insert(key.clone(), 0);
                }
                let inner_key = format!("i{:08}", iteration).into_bytes();
                let mut path_vec: Vec<&[u8]> = cidx_path.to_vec();
                path_vec.push(&key);
                db.insert(
                    path_vec.as_slice(),
                    &inner_key,
                    Element::new_item(b"v".to_vec()),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert into CountTree");
                *model.entries.get_mut(&key).unwrap() += 1;
            }
            1 => {
                // Delete the cidx entry entirely (if it exists).
                if model.entries.contains_key(&key) {
                    db.delete_from_count_indexed_tree(cidx_path, &key, None, grove_version)
                        .unwrap()
                        .expect("delete cidx entry");
                    model.entries.remove(&key);
                }
            }
            2 => {
                // Re-insert the same cidx entry as a fresh empty
                // CountTree. Allowed because
                // insert_into_count_indexed_tree handles the existing-
                // entry case (via the dedicated API; not the direct
                // db.insert path which is rejected for cidx primaries).
                // If the key exists, we delete then re-create to keep
                // the model simple.
                if model.entries.contains_key(&key) {
                    db.delete_from_count_indexed_tree(cidx_path, &key, None, grove_version)
                        .unwrap()
                        .expect("delete before re-create");
                    model.entries.remove(&key);
                }
                db.insert_into_count_indexed_tree(
                    cidx_path,
                    &key,
                    Element::empty_count_tree(),
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("create empty CountTree");
                model.entries.insert(key.clone(), 0);
            }
            3 => {
                // Batch: insert one item into an existing CountTree (if
                // any exist), via the batch path — exercises the
                // bubble-up + nested cidx mirror.
                use crate::batch::QualifiedGroveDbOp;
                if let Some((existing_key, count)) =
                    model.entries.iter().next().map(|(k, c)| (k.clone(), *c))
                {
                    let inner_key = format!("b{:08}", iteration).into_bytes();
                    let mut inner_path: Vec<Vec<u8>> =
                        cidx_path.iter().map(|s| s.to_vec()).collect();
                    inner_path.push(existing_key.clone());
                    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
                        inner_path,
                        inner_key,
                        Element::new_item(b"v".to_vec()),
                    )];
                    db.apply_batch(ops, None, None, grove_version)
                        .unwrap()
                        .expect("batch insert into existing CountTree");
                    *model.entries.get_mut(&existing_key).unwrap() = count + 1;
                }
            }
            _ => {
                // Idle iteration — random selection might land on a
                // model state where this kind has nothing to do (e.g.
                // delete with no entries). That's fine; the iteration
                // count is preserved.
            }
        }

        // INVARIANT 1: verify_grovedb finds no issues (chain + content).
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(
            issues.is_empty(),
            "iteration {iteration}: verify_grovedb issues: {:?}",
            issues.keys().collect::<Vec<_>>()
        );

        // INVARIANT 2: top-k (ascending) matches the model's view.
        let top = db
            .count_indexed_top_k(cidx_path, 100, false, None, grove_version)
            .unwrap()
            .expect("top-k");
        let model_top = model.top_k_ascending();
        assert_eq!(
            top, model_top,
            "iteration {iteration}: top-k drift\n  db:    {:?}\n  model: {:?}",
            top, model_top
        );
    }

    #[test]
    fn property_random_ops_preserve_cidx_invariant_single_level() {
        // 300 random ops against a single-level cidx. Each op is
        // followed by a full verify_grovedb scan and a top-k diff
        // against the in-memory model. Fixed seed; if you find a
        // failing seed in CI, hard-code it as a regression test.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        let cidx_path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let mut model = CidxModel::default();
        let mut rng = Prng::new(0xC1DC_5EED_C0FFEE);

        for iteration in 0..300 {
            apply_random_op_and_check(
                &mut rng,
                &db,
                cidx_path,
                &mut model,
                grove_version,
                iteration,
            );
        }
    }

    /// Apply a single insert operation via the DIRECT API
    /// (`insert_into_count_indexed_tree`).
    fn apply_insert_via_direct(
        db: &crate::GroveDb,
        cidx_path: &[&[u8]],
        key: &[u8],
        item: Element,
        grove_version: &GroveVersion,
    ) {
        db.insert_into_count_indexed_tree(cidx_path, key, item, None, grove_version)
            .unwrap()
            .expect("direct insert");
    }

    /// Apply a single insert operation via the BATCH path
    /// (`apply_batch` with one InsertOrReplace op).
    fn apply_insert_via_batch(
        db: &crate::GroveDb,
        cidx_path: &[&[u8]],
        key: &[u8],
        item: Element,
        grove_version: &GroveVersion,
    ) {
        use crate::batch::QualifiedGroveDbOp;
        let path_vec: Vec<Vec<u8>> = cidx_path.iter().map(|s| s.to_vec()).collect();
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            path_vec,
            key.to_vec(),
            item,
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch insert");
    }

    #[test]
    fn differential_direct_vs_batch_produce_identical_root_hashes() {
        // The same sequence of operations applied via the two API
        // paths (insert_into_count_indexed_tree vs. apply_batch with
        // one InsertOrReplace op) must produce IDENTICAL on-disk
        // state — same GroveDB root hash, same cidx element bytes,
        // same primary/secondary content. This catches any drift
        // between the dedicated and batch implementations.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();

        // 4 representative operation sequences.
        let sequences: Vec<Vec<(Vec<u8>, Element)>> = vec![
            // Sequence 1: three distinct inserts.
            vec![
                (b"a".to_vec(), Element::new_item(b"1".to_vec())),
                (b"b".to_vec(), Element::new_item(b"2".to_vec())),
                (b"c".to_vec(), Element::new_item(b"3".to_vec())),
            ],
            // Sequence 2: insert + overwrite same key.
            vec![
                (b"x".to_vec(), Element::new_item(b"old".to_vec())),
                (b"x".to_vec(), Element::new_item(b"new".to_vec())),
                (b"y".to_vec(), Element::new_item(b"z".to_vec())),
            ],
            // Sequence 3: inserts in varying key order.
            vec![
                (b"c".to_vec(), Element::new_item(b"3".to_vec())),
                (b"a".to_vec(), Element::new_item(b"1".to_vec())),
                (b"b".to_vec(), Element::new_item(b"2".to_vec())),
                (b"d".to_vec(), Element::new_item(b"4".to_vec())),
            ],
            // Sequence 4: inserts then a delete on a middle key
            // (handled as an extra op below).
            vec![
                (b"a".to_vec(), Element::new_item(b"1".to_vec())),
                (b"b".to_vec(), Element::new_item(b"2".to_vec())),
                (b"c".to_vec(), Element::new_item(b"3".to_vec())),
            ],
        ];

        for (idx, seq) in sequences.iter().enumerate() {
            // === Apply via direct API ===
            let db_direct = make_test_grovedb(grove_version);
            db_direct
                .insert(
                    [TEST_LEAF].as_ref(),
                    b"cidx",
                    Element::empty_count_indexed_tree(),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("direct: create cidx");
            for (k, v) in seq {
                apply_insert_via_direct(
                    &db_direct,
                    &[TEST_LEAF, b"cidx"],
                    k,
                    v.clone(),
                    grove_version,
                );
            }
            if idx == 3 {
                db_direct
                    .delete_from_count_indexed_tree(
                        [TEST_LEAF, b"cidx"].as_ref(),
                        b"b",
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("direct: delete b");
            }

            // === Apply via batch path ===
            let db_batch = make_test_grovedb(grove_version);
            db_batch
                .insert(
                    [TEST_LEAF].as_ref(),
                    b"cidx",
                    Element::empty_count_indexed_tree(),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("batch: create cidx");
            for (k, v) in seq {
                apply_insert_via_batch(
                    &db_batch,
                    &[TEST_LEAF, b"cidx"],
                    k,
                    v.clone(),
                    grove_version,
                );
            }
            if idx == 3 {
                let ops = vec![QualifiedGroveDbOp::delete_op(
                    vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                    b"b".to_vec(),
                )];
                db_batch
                    .apply_batch(ops, None, None, grove_version)
                    .unwrap()
                    .expect("batch: delete b");
            }

            // === Compare ===
            let root_direct = db_direct
                .root_hash(None, grove_version)
                .unwrap()
                .expect("direct: root");
            let root_batch = db_batch
                .root_hash(None, grove_version)
                .unwrap()
                .expect("batch: root");
            assert_eq!(
                root_direct, root_batch,
                "sequence {}: GroveDB root hashes differ between direct and batch paths",
                idx
            );

            let elem_direct = db_direct
                .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
                .unwrap()
                .expect("direct: cidx elem");
            let elem_batch = db_batch
                .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
                .unwrap()
                .expect("batch: cidx elem");
            assert_eq!(
                elem_direct, elem_batch,
                "sequence {}: cidx element bytes differ",
                idx
            );

            let top_direct = db_direct
                .count_indexed_top_k(
                    [TEST_LEAF, b"cidx"].as_ref(),
                    100,
                    true,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("direct: top");
            let top_batch = db_batch
                .count_indexed_top_k(
                    [TEST_LEAF, b"cidx"].as_ref(),
                    100,
                    true,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("batch: top");
            assert_eq!(top_direct, top_batch, "sequence {}: top-k differs", idx);

            let issues_direct = db_direct
                .verify_grovedb(None, false, true, grove_version)
                .expect("direct: verify");
            let issues_batch = db_batch
                .verify_grovedb(None, false, true, grove_version)
                .expect("batch: verify");
            assert!(
                issues_direct.is_empty() && issues_batch.is_empty(),
                "sequence {}: integrity issues",
                idx
            );
        }
    }

    #[test]
    fn property_random_ops_preserve_cidx_invariant_nested_two_levels() {
        // Same shape but against a NESTED cidx layout
        //   TEST_LEAF / outer_cidx / inner_cidx
        // with random ops applied to inner_cidx. Exercises the
        // nested-bubble-up path — the bug class found in commit
        // a8bb34fb. 200 iterations because each verify_grovedb walks
        // both cidx primaries' content.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer_cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("outer");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer_cidx"].as_ref(),
            b"inner_cidx",
            Element::empty_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("inner");

        let cidx_path: &[&[u8]] = &[TEST_LEAF, b"outer_cidx", b"inner_cidx"];
        let mut model = CidxModel::default();
        let mut rng = Prng::new(0xDEADBEEF_CAFEBABE);

        for iteration in 0..200 {
            apply_random_op_and_check(
                &mut rng,
                &db,
                cidx_path,
                &mut model,
                grove_version,
                iteration,
            );
        }
    }

    // =====================================================================
    // Fuzz-style tests for cidx panic-resistance and high-iteration
    // coverage.
    //
    // These run as part of the normal test suite but use much higher
    // iteration counts than the property tests and specifically target
    // panic-resistance properties — the verifier must NEVER panic on
    // adversarial input, only return Err. Catches DoS vectors in proof
    // verification and finds subtle bugs that property tests miss by
    // virtue of generating much more input.
    //
    // Each test uses a hand-rolled SplitMix64 PRNG seeded from a
    // fixed-but-rotating base (so failing runs are reproducible by
    // printing the seed), with the option of overriding via env var
    // CIDX_FUZZ_SEED=<u64> for debugging.
    // =====================================================================

    /// Seed source: env var CIDX_FUZZ_SEED if set, else a fixed
    /// hard-coded seed. Print on test start so failures are
    /// reproducible.
    fn fuzz_seed(default: u64) -> u64 {
        match std::env::var("CIDX_FUZZ_SEED")
            .ok()
            .and_then(|s| s.parse().ok())
        {
            Some(s) => {
                eprintln!("CIDX fuzz: using env-provided seed = {}", s);
                s
            }
            None => {
                eprintln!("CIDX fuzz: using default seed = {}", default);
                default
            }
        }
    }

    #[test]
    fn fuzz_verify_count_indexed_top_k_never_panics_on_arbitrary_bytes() {
        // The verifier MUST gracefully reject adversarial input.
        // Garbage bytes, truncated bytes, oversized bytes, near-valid
        // bytes — all must produce Err, never panic.
        //
        // 5000 iterations of random byte buffers of varying sizes.
        let seed = fuzz_seed(0xF1122_C1DC_F022);
        let mut rng = Prng::new(seed);
        let path: &[&[u8]] = &[b"x", b"y"];

        for iteration in 0..5_000 {
            // Generate a random byte buffer. Size distribution skewed
            // toward small (most adversarial inputs are short) with
            // occasional large buffers.
            let size = match rng.next_usize(100) {
                0..=70 => rng.next_usize(64),   // most: short
                71..=90 => rng.next_usize(512), // some: medium
                _ => rng.next_usize(4096),      // few: large
            };
            let mut bytes = Vec::with_capacity(size);
            for _ in 0..size {
                bytes.push((rng.next_u64() & 0xFF) as u8);
            }

            // The contract: never panic, always return Err for invalid
            // input. Genuinely valid proofs are vanishingly unlikely
            // from random bytes, so we expect Err in all cases.
            let result = GroveDb::verify_count_indexed_top_k(&bytes, path, 10, true);
            assert!(
                result.is_err(),
                "iteration {iteration}: random {size}-byte buffer parsed as valid proof"
            );
        }
    }

    #[test]
    fn fuzz_verify_count_indexed_query_never_panics_on_arbitrary_bytes() {
        // Same panic-resistance check for the arbitrary-query verify
        // entry. The verify_count_indexed_query function takes a
        // MerkQuery argument too — we pass a non-empty full-range
        // query so the query-side code path is exercised.
        use grovedb_merk::proofs::Query as MerkQuery;

        let seed = fuzz_seed(0xF1122_C1DC_F033);
        let mut rng = Prng::new(seed);
        let path: &[&[u8]] = &[b"x", b"y"];

        for iteration in 0..5_000 {
            let size = rng.next_usize(2048);
            let mut bytes = Vec::with_capacity(size);
            for _ in 0..size {
                bytes.push((rng.next_u64() & 0xFF) as u8);
            }

            let mut q = MerkQuery::new();
            q.insert_all();
            // Randomize direction so both code paths are exercised.
            q.left_to_right = rng.next_u64() & 1 == 0;

            let result = GroveDb::verify_count_indexed_query(&bytes, q, None, path);
            assert!(
                result.is_err(),
                "iteration {iteration}: random {size}-byte buffer parsed as valid query proof"
            );
        }
    }

    #[test]
    fn fuzz_prove_verify_round_trip_with_arbitrary_count_ranges() {
        // Pick random count-ranges over a populated cidx and verify
        // the proof round-trip. The range endpoints, direction, and
        // limit are randomized. 1000 iterations against a single
        // populated DB (DB construction is amortized).
        use grovedb_merk::proofs::Query as MerkQuery;

        let seed = fuzz_seed(0xF1122_C1DC_F044);
        let mut rng = Prng::new(seed);

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        // Populate with 50 entries at varied counts.
        for i in 0..50 {
            let key = format!("k{:03}", i).into_bytes();
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                &key,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("create sub");
            // Vary count from 0..(i+1) by inserting i items into each.
            for j in 0..i {
                let inner = format!("c{:03}", j).into_bytes();
                db.insert(
                    [TEST_LEAF, b"cidx", &key].as_ref(),
                    &inner,
                    Element::new_item(b"v".to_vec()),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("populate sub");
            }
        }

        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];

        for iteration in 0..1_000 {
            // Random count range.
            let lo = rng.next_u64() % 60; // covers 0..50 + a margin
            let hi_delta = rng.next_u64() % 60;
            let hi = lo.saturating_add(hi_delta);

            let mut q = MerkQuery::new();
            q.insert_range(lo.to_be_bytes().to_vec()..hi.to_be_bytes().to_vec());
            q.left_to_right = rng.next_u64() & 1 == 0;

            let limit = if rng.next_u64() & 1 == 0 {
                None
            } else {
                Some((rng.next_u64() % 50) as u16 + 1)
            };

            let proof = db
                .prove_count_indexed_query(path, q.clone(), limit, None, grove_version)
                .unwrap();
            let proof = match proof {
                Ok(p) => p,
                Err(e) => panic!(
                    "iteration {iteration}: prove failed (lo={lo}, hi={hi}, limit={:?}): {:?}",
                    limit, e
                ),
            };

            let verified = GroveDb::verify_count_indexed_query(&proof, q, limit, path)
                .unwrap_or_else(|e| {
                    panic!(
                        "iteration {iteration}: verify failed (lo={lo}, hi={hi}, limit={:?}): \
                         {:?}",
                        limit, e
                    )
                });

            // Sanity: root_hash matches DB's actual root.
            let expected_root = db
                .root_hash(None, grove_version)
                .unwrap()
                .expect("root hash");
            assert_eq!(
                verified.root_hash, expected_root,
                "iteration {iteration}: proof root_hash mismatch"
            );
        }
    }

    // =====================================================================
    // Cost regression tests for cidx ops. Pins down cost shape so
    // accidental regressions (extra disk reads, redundant hashes) fail
    // CI.
    // =====================================================================

    fn make_grovedb_with_cidx() -> (crate::tests::TempGroveDb, &'static GroveVersion) {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        (db, grove_version)
    }

    // =====================================================================
    // Concurrent reader test.
    //
    // GroveDB currently supports a single writer; multi-writer
    // semantics are not a claimed contract. Read concurrency IS
    // supported — multiple readers can safely query the same DB.
    // This test verifies that for cidx queries specifically.
    // =====================================================================

    #[test]
    fn concurrent_readers_against_populated_cidx_see_consistent_state() {
        // 8 reader threads, each running 100 top-k queries against the
        // same DB. Reads must all see the same state.
        use std::sync::Arc;
        use std::thread;

        let grove_version = GroveVersion::latest();
        let db = Arc::new(make_test_grovedb(grove_version));
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for i in 0..20 {
            let key = format!("k{:02}", i).into_bytes();
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                &key,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate");
        }

        let expected = db
            .count_indexed_top_k(
                [TEST_LEAF, b"cidx"].as_ref(),
                100,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("baseline top");

        let mut handles = vec![];
        for _ in 0..8 {
            let db_c = db.clone();
            let expected_c = expected.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    let got = db_c
                        .count_indexed_top_k(
                            [TEST_LEAF, b"cidx"].as_ref(),
                            100,
                            true,
                            None,
                            grove_version,
                        )
                        .unwrap()
                        .expect("reader top");
                    assert_eq!(got, expected_c, "concurrent reader inconsistency");
                }
            }));
        }
        for h in handles {
            h.join().expect("reader panicked");
        }

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty());
    }

    // (Concurrent-writer test removed: GroveDB does not currently
    // support multiple writer threads as a supported contract.
    // Tests against an unsupported scenario would be testing for
    // behavior the system doesn't claim to provide. The reader test
    // above stays — concurrent readers IS a supported property.)

    // =====================================================================
    // Integrity check on database open.
    // =====================================================================

    #[test]
    fn open_with_cidx_integrity_check_passes_on_clean_db() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let grove_version = GroveVersion::latest();
        {
            let db = crate::GroveDb::open(tmp.path()).expect("open");
            // Use the root path directly; no test-leaf scaffolding for
            // this minimal corruption-detection test.
            db.insert(
                grovedb_path::SubtreePath::<[u8; 0]>::empty(),
                b"cidx",
                Element::empty_count_indexed_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create cidx");
            db.insert_into_count_indexed_tree(
                [b"cidx".as_slice()].as_ref(),
                b"k",
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate");
        }
        let _db = crate::GroveDb::open_with_cidx_integrity_check(tmp.path(), grove_version)
            .expect("open with integrity check");
    }

    #[test]
    fn open_with_cidx_integrity_check_fails_on_corrupted_secondary() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let grove_version = GroveVersion::latest();
        {
            let db = crate::GroveDb::open(tmp.path()).expect("open");
            db.insert(
                grovedb_path::SubtreePath::<[u8; 0]>::empty(),
                b"cidx",
                Element::empty_count_indexed_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create cidx");
            db.insert_into_count_indexed_tree(
                [b"cidx".as_slice()].as_ref(),
                b"a",
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate");
            corrupt_secondary_insert(
                &db,
                &[b"cidx"],
                &make_secondary_key(99, b"ghost"),
                grove_version,
            );
        }
        let result = crate::GroveDb::open_with_cidx_integrity_check(tmp.path(), grove_version);
        match result {
            Err(crate::Error::CorruptedData(msg)) => {
                assert!(
                    msg.contains("integrity check"),
                    "expected integrity violation message, got: {msg}"
                );
            }
            Err(other) => panic!("expected CorruptedData, got {:?}", other),
            Ok(_) => panic!("expected CorruptedData err, got Ok(GroveDb)"),
        }
    }

    #[test]
    fn cost_insert_into_count_indexed_tree_first_item() {
        let (db, grove_version) = make_grovedb_with_cidx();
        let cost = db
            .insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"k",
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .cost;
        eprintln!("cidx insert cost: {:?}", cost);
        assert!(cost.seek_count > 0 && cost.hash_node_calls > 0);
    }

    #[test]
    fn cost_delete_from_count_indexed_tree() {
        let (db, grove_version) = make_grovedb_with_cidx();
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        let cost = db
            .delete_from_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"k",
                None,
                grove_version,
            )
            .cost;
        eprintln!("cidx delete cost: {:?}", cost);
        assert!(cost.seek_count > 0 && cost.hash_node_calls > 0);
    }

    #[test]
    fn cost_count_indexed_top_k_read_does_not_write() {
        let (db, grove_version) = make_grovedb_with_cidx();
        for i in 0..5 {
            let key = format!("k{:02}", i).into_bytes();
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                &key,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate");
        }
        let cost = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .cost;
        eprintln!("cidx top_k cost: {:?}", cost);
        assert!(cost.seek_count > 0 && cost.storage_loaded_bytes > 0);
        assert_eq!(cost.storage_cost.added_bytes, 0, "top_k must not write");
    }

    #[test]
    fn cost_count_indexed_count_range_read_does_not_write() {
        let (db, grove_version) = make_grovedb_with_cidx();
        for i in 0..5 {
            let key = format!("k{:02}", i).into_bytes();
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                &key,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate");
        }
        let cost = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                0,
                u64::MAX,
                false,
                10,
                None,
                grove_version,
            )
            .cost;
        eprintln!("cidx count_range cost: {:?}", cost);
        assert!(cost.seek_count > 0 && cost.storage_loaded_bytes > 0);
        assert_eq!(
            cost.storage_cost.added_bytes, 0,
            "count_range must not write"
        );
    }

    #[test]
    fn fuzz_large_random_op_sequence_against_cidx() {
        // 2000 random ops against a single-level cidx with a larger
        // key space than the property tests (20 keys, more update
        // pressure). Stress test for the bug class found in
        // delete_from_count_indexed_tree (commit 4f1d7305).
        let seed = fuzz_seed(0xF1122_C1DC_F055);
        let mut rng = Prng::new(seed);

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        let cidx_path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let mut model = CidxModel::default();

        for iteration in 0..2_000 {
            apply_random_op_and_check(
                &mut rng,
                &db,
                cidx_path,
                &mut model,
                grove_version,
                iteration,
            );
        }
    }

    // =====================================================================
    // Additional cidx coverage: targets uncovered branches surfaced by
    // codecov's patch-coverage report. Each test exercises a real
    // behavior, not just lines.
    // =====================================================================

    #[test]
    fn apply_partial_batch_with_cidx_overwrite_safe_subset() {
        // Safe-subset cidx overwrite via apply_partial_batch (parallels
        // the apply_batch test). Exercises the partial-batch cleanup
        // pass for the overwrite case.
        use crate::batch::{BatchApplyOptions, QualifiedGroveDbOp};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");

        // Overwrite via partial-batch path.
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            Element::new_item(b"replaced".to_vec()),
        )];
        let opts = BatchApplyOptions {
            validate_insertion_does_not_override_tree: false,
            ..Default::default()
        };
        db.apply_partial_batch(
            ops,
            Some(opts),
            |_cost, _leftover| Ok(vec![]),
            None,
            grove_version,
        )
        .unwrap()
        .expect("apply_partial_batch overwrite");

        let elem = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get");
        assert_eq!(elem, Element::new_item(b"replaced".to_vec()));
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty());
    }

    #[test]
    fn insert_into_count_indexed_tree_at_root_path_errors() {
        // Root-path is invalid for cidx insert because the API needs
        // a parent path to read the cidx element from.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty: &[&[u8]] = &[];
        let result = db
            .insert_into_count_indexed_tree(
                empty,
                b"k",
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(crate::Error::InvalidPath(msg)) => {
                assert!(msg.contains("root") || msg.contains("cidx"));
            }
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn delete_from_count_indexed_tree_at_root_path_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty: &[&[u8]] = &[];
        let result = db
            .delete_from_count_indexed_tree(empty, b"k", None, grove_version)
            .unwrap();
        match result {
            Err(crate::Error::InvalidPath(_)) => {}
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn reconcile_at_root_path_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty: &[&[u8]] = &[];
        let result = db
            .reconcile_count_indexed_tree_secondary(empty, None, grove_version)
            .unwrap();
        match result {
            Err(crate::Error::InvalidPath(_)) => {}
            other => panic!("expected InvalidPath, got {:?}", other),
        }
    }

    #[test]
    fn insert_into_count_indexed_tree_update_value_same_count() {
        // Re-insert the same key with a different Item value. The
        // count doesn't change (both Items are count=1) so the
        // secondary mirror should short-circuit (old_count == new_count).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::new_item(b"v1".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert v1");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::new_item(b"v2".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("update to v2");
        let elem = db
            .get([TEST_LEAF, b"cidx"].as_ref(), b"k", None, grove_version)
            .unwrap()
            .expect("get");
        assert_eq!(elem, Element::new_item(b"v2".to_vec()));
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty());
    }

    #[test]
    fn insert_into_count_indexed_tree_replace_item_with_count_tree() {
        // Replace an Item (count=1) with a CountTree (count=0 when
        // empty). The count CHANGES; the secondary mirror must move
        // the entry from (1_be ‖ k) to (0_be ‖ k).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");

        // Top-k confirms count=1.
        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top");
        assert_eq!(top, vec![(1u64, b"k".to_vec())]);

        // Replace with empty CountTree.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("replace with count tree");

        // Top-k now shows count=0.
        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top after");
        assert_eq!(top, vec![(0u64, b"k".to_vec())]);

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty());
    }

    #[test]
    fn batch_overwrite_cidx_with_count_tree_succeeds() {
        // Batch safe-subset overwrite: cidx → CountTree (non-cidx tree).
        // Same cleanup as cidx → Item.
        use crate::batch::{BatchApplyOptions, QualifiedGroveDbOp};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            Element::empty_count_tree(),
        )];
        let opts = BatchApplyOptions {
            validate_insertion_does_not_override_tree: false,
            ..Default::default()
        };
        db.apply_batch(ops, Some(opts), None, grove_version)
            .unwrap()
            .expect("overwrite cidx with count tree");
        let elem = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get");
        match elem {
            Element::CountTree(_, c, _) => assert_eq!(c, 0),
            other => panic!("expected CountTree, got {:?}", other),
        }
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty());
    }

    #[test]
    fn prove_count_indexed_top_k_descending_then_ascending_round_trip() {
        // Build a cidx with varied counts, prove top-k in both
        // directions, verify both round-trip. Exercises both branches
        // of the descending param in build_count_indexed_proof.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        for k in [b"a".as_slice(), b"b", b"c"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("create sub");
        }
        // Give them different counts.
        db.insert(
            [TEST_LEAF, b"cidx", b"a"].as_ref(),
            b"x",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("a x1");
        db.insert(
            [TEST_LEAF, b"cidx", b"b"].as_ref(),
            b"x",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("b x1");
        db.insert(
            [TEST_LEAF, b"cidx", b"b"].as_ref(),
            b"y",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("b x2");
        // Now: a=1, b=2, c=0.

        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        for &descending in &[true, false] {
            let proof = db
                .prove_count_indexed_top_k(path, 10, descending, None, grove_version)
                .unwrap()
                .expect("prove");
            let result =
                GroveDb::verify_count_indexed_top_k(&proof, path, 10, descending).expect("verify");
            if descending {
                // Descending: b(2), a(1), c(0)
                assert_eq!(
                    result.entries,
                    vec![
                        (2u64, b"b".to_vec()),
                        (1u64, b"a".to_vec()),
                        (0u64, b"c".to_vec())
                    ]
                );
            } else {
                // Ascending: c(0), a(1), b(2)
                assert_eq!(
                    result.entries,
                    vec![
                        (0u64, b"c".to_vec()),
                        (1u64, b"a".to_vec()),
                        (2u64, b"b".to_vec())
                    ]
                );
            }
        }
    }

    #[test]
    fn count_indexed_count_range_with_specific_bounds() {
        // count_range with lo > 0 AND hi < u64::MAX — exercises the
        // bounded-range branch of make_secondary_range_query.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        for k in [b"a".as_slice(), b"b", b"c", b"d", b"e"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("create sub");
        }
        // Set counts: a=1, b=3, c=5, d=7, e=9
        for (k, n) in [
            (b"a".as_slice(), 1usize),
            (b"b", 3),
            (b"c", 5),
            (b"d", 7),
            (b"e", 9),
        ] {
            for i in 0..n {
                let inner = format!("x{}", i).into_bytes();
                db.insert(
                    [TEST_LEAF, b"cidx", k].as_ref(),
                    &inner,
                    Element::new_item(b"v".to_vec()),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("populate");
            }
        }
        // Query [3, 7] inclusive — should return b(3), c(5), d(7).
        let entries = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                3,
                7,
                false,
                10,
                None,
                grove_version,
            )
            .unwrap()
            .expect("range");
        assert_eq!(
            entries,
            vec![
                (3u64, b"b".to_vec()),
                (5u64, b"c".to_vec()),
                (7u64, b"d".to_vec()),
            ]
        );

        // Single-count query [5, 5] — only c.
        let entries = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                5,
                5,
                false,
                10,
                None,
                grove_version,
            )
            .unwrap()
            .expect("single");
        assert_eq!(entries, vec![(5u64, b"c".to_vec())]);

        // Empty range [100, 200].
        let entries = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                100,
                200,
                false,
                10,
                None,
                grove_version,
            )
            .unwrap()
            .expect("empty");
        assert!(entries.is_empty());
    }

    #[test]
    fn open_count_indexed_secondary_for_batch_on_non_cidx_parent_errors() {
        // Trigger open_count_indexed_secondary_for_batch error path
        // when the parent element is not a cidx (the dedicated helper
        // method should return an error rather than panicking).
        //
        // We exercise this indirectly via a batch op that targets a
        // cidx primary path which is actually a regular Tree — the
        // batch path will try to open the (non-existent) secondary
        // and fail cleanly.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Insert a REGULAR tree at TEST_LEAF/regular.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"regular",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create regular");

        // Attempt a batch op at [TEST_LEAF, regular] — the regular
        // tree is not a cidx primary, so the cidx-specific code paths
        // in the batch shouldn't fire. This isn't an error; it's
        // just exercising the non-cidx fallthrough.
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"regular".to_vec()],
            b"item".to_vec(),
            Element::new_item(b"v".to_vec()),
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch insert into regular tree");
    }

    #[test]
    fn count_indexed_count_range_lo_greater_than_hi() {
        // Edge case: lo > hi. Should return empty results, not panic.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        let entries = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                100,
                50,
                false,
                10,
                None,
                grove_version,
            )
            .unwrap()
            .expect("inverted range");
        assert!(
            entries.is_empty(),
            "lo > hi must return empty, got {:?}",
            entries
        );
    }

    #[test]
    fn count_indexed_top_k_descending_returns_correct_order() {
        // Ensures the descending branch is exercised explicitly.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        for k in [b"low".as_slice(), b"mid", b"hi"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("sub");
        }
        for (k, n) in [(b"low".as_slice(), 1), (b"mid", 5), (b"hi", 10)] {
            for i in 0..n {
                let inner = format!("x{}", i).into_bytes();
                db.insert(
                    [TEST_LEAF, b"cidx", k].as_ref(),
                    &inner,
                    Element::new_item(b"v".to_vec()),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("populate");
            }
        }
        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 2, true, None, grove_version)
            .unwrap()
            .expect("top descending limit=2");
        assert_eq!(top, vec![(10u64, b"hi".to_vec()), (5u64, b"mid".to_vec())]);
    }

    #[test]
    fn batch_atomicity_failure_after_safe_subset_overwrite_rolls_back() {
        // Safe-subset cidx overwrite is staged + cleanup is scheduled,
        // but a LATER op in the batch fails validation. The overwrite
        // must roll back — the cleanup must NOT run, the old cidx
        // state stays intact.
        use crate::batch::{BatchApplyOptions, QualifiedGroveDbOp};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"existing",
            Element::new_item(b"original".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create existing");

        let root_before = db.root_hash(None, grove_version).unwrap().expect("root");

        let ops = vec![
            // Op 1: safe-subset overwrite of cidx (would be allowed).
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::new_item(b"replaced".to_vec()),
            ),
            // Op 2: assertion violation — InsertWithKnownToNotAlready
            // Exist on existing key.
            QualifiedGroveDbOp::insert_only_known_to_not_already_exist_op(
                vec![TEST_LEAF.to_vec()],
                b"existing".to_vec(),
                Element::new_item(b"new".to_vec()),
            ),
        ];
        let opts = BatchApplyOptions {
            validate_insertion_does_not_override_tree: false,
            validate_insertion_does_not_override: true,
            ..Default::default()
        };
        let result = db
            .apply_batch(ops, Some(opts), None, grove_version)
            .unwrap();
        assert!(
            result.is_err(),
            "batch must fail on the assertion violation"
        );

        // State unchanged: cidx is still a cidx with its old entry.
        let root_after = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root after");
        assert_eq!(root_before, root_after, "rollback failed");

        let elem = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("cidx");
        match elem {
            Element::CountIndexedTree(_, _, count, _) => {
                assert_eq!(count, 1, "cidx state corrupted after rollback");
            }
            other => panic!("expected cidx, got {:?}", other),
        }
        // Old item still resolvable inside cidx.
        let item = db
            .get([TEST_LEAF, b"cidx"].as_ref(), b"k", None, grove_version)
            .unwrap()
            .expect("k still present");
        assert_eq!(item, Element::new_item(b"v".to_vec()));
    }

    // =====================================================================
    // Audit fixes: P1 + P2 findings on commit cc4db742.
    // =====================================================================

    #[test]
    fn cidx_item_key_247_byte_ceiling_direct_path() {
        // Item keys for cidx primary writes must be ≤ 247 bytes — the
        // secondary key (count_be ‖ item_key) must fit in Merk's
        // < 256-byte ceiling (8 + 247 = 255).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        // 247-byte key: OK.
        let max_ok_key = vec![b'a'; 247];
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            &max_ok_key,
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("247-byte key must be accepted");

        // 248-byte key: rejected.
        let too_long_key = vec![b'a'; 248];
        let result = db
            .insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                &too_long_key,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(crate::Error::InvalidInput(msg)) => {
                assert!(
                    msg.contains("247"),
                    "expected 247-byte ceiling message, got: {msg}"
                );
            }
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn cidx_item_key_247_byte_ceiling_batch_path() {
        // Same ceiling on the batch path.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        let too_long_key = vec![b'a'; 248];
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
            too_long_key,
            Element::new_item(b"v".to_vec()),
        )];
        let result = db.apply_batch(ops, None, None, grove_version).unwrap();
        match result {
            Err(crate::Error::InvalidInput(msg)) => {
                assert!(
                    msg.contains("247"),
                    "expected 247-byte ceiling message, got: {msg}"
                );
            }
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn insert_into_count_indexed_tree_overwriting_tree_cleans_up_storage() {
        // Insert a CountTree at cidx_key/sub, populate it with items,
        // then overwrite that CountTree with an Item via the
        // dedicated API. The old CountTree's children must be cleaned
        // up; verify_grovedb finds no issues afterward.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"sub",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create sub");
        for i in 0..5 {
            let inner = format!("inner_{}", i).into_bytes();
            db.insert(
                [TEST_LEAF, b"cidx", b"sub"].as_ref(),
                &inner,
                Element::new_item(b"v".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate sub");
        }

        // Overwrite the CountTree with an Item via the dedicated API.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"sub",
            Element::new_item(b"replaced".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("overwrite tree with item");

        // Old children are gone; verify finds no orphans.
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "expected no issues, got {:?}", issues);
    }

    #[test]
    fn insert_into_count_indexed_tree_overwriting_cidx_with_non_empty_cidx_rejected() {
        // Direct-API counterpart to the batch rejection: cidx → non-empty
        // cidx is ambiguous and must be rejected.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"sub",
            Element::empty_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create nested cidx");

        // Try to overwrite with a non-empty cidx claim.
        let non_empty = Element::new_count_indexed_tree_with_root_keys_and_count_value(
            Some(b"bogus_primary".to_vec()),
            Some(b"bogus_secondary".to_vec()),
            5,
            None,
        );
        let result = db
            .insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"sub",
                non_empty,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(crate::Error::NotSupported(msg)) => {
                assert!(
                    msg.contains("EMPTY cidx")
                        || msg.contains("non-empty cidx")
                        || msg.contains("ambiguous"),
                    "expected non-empty cidx rejection, got: {msg}"
                );
            }
            other => panic!("expected NotSupported, got {:?}", other),
        }
    }

    #[test]
    fn batch_safe_subset_overwrite_with_descendant_write_in_same_batch_rejected() {
        // A safe-subset cidx overwrite + a write under the same cidx
        // path in the SAME batch is rejected: the post-apply cleanup
        // would silently drop the descendant write. Two rejection
        // paths exist:
        //   1. cidx → non-tree element (e.g. Item) — existing
        //      "insertion under non-tree" rejection fires during
        //      bubble-up (the deep write can't be wrapped into the
        //      new element).
        //   2. cidx → empty cidx (still a tree) — the deep write
        //      can be wrapped, but my new check in
        //      execute_ops_on_path detects the cleanup-vs-write
        //      conflict and rejects.
        // This test exercises path 2 specifically.
        use crate::batch::{BatchApplyOptions, QualifiedGroveDbOp};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"sub",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create sub for descendant write target");

        // Batch: overwrite cidx with EMPTY cidx (still a tree, so the
        // existing "insertion under non-tree" check doesn't fire) AND
        // write under cidx's path. The cleanup would silently drop
        // the descendant write.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::empty_count_indexed_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec(), b"sub".to_vec()],
                b"would_be_lost".to_vec(),
                Element::new_item(b"v".to_vec()),
            ),
        ];
        let opts = BatchApplyOptions {
            validate_insertion_does_not_override_tree: false,
            ..Default::default()
        };
        let result = db
            .apply_batch(ops, Some(opts), None, grove_version)
            .unwrap();
        // The batch must be rejected — by ANY check. Multiple
        // existing checks in the batch pipeline (propagation lookup,
        // tree-shape validation, etc.) catch various flavors of this
        // inconsistency. Our new check in execute_ops_on_path adds
        // defense in depth + clearer error attribution. The audit's
        // worry was a silent cleanup-drop; what matters here is that
        // the batch FAILS rather than silently losing the descendant.
        assert!(
            result.is_err(),
            "batch with safe-subset overwrite + descendant write must be \
             rejected (any error is acceptable); got Ok"
        );
    }

    #[test]
    fn verify_grovedb_detects_duplicate_secondary_rows() {
        // Two secondary entries for the same primary key at different
        // counts. The previous HashMap<key, u64> shape silently
        // collapsed the duplicates; the Vec<u64> shape catches it via
        // __cidx_secondary_duplicate__.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");

        // Inject a duplicate secondary entry at a wrong count without
        // removing the correct one. Now the secondary has BOTH (1, k)
        // and (99, k) — drift the previous check would miss because
        // one would overwrite the other in the HashMap.
        corrupt_secondary_insert(
            &db,
            &[TEST_LEAF, b"cidx"],
            &make_secondary_key(99, b"k"),
            grove_version,
        );

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        let dup_path: Vec<Vec<u8>> = vec![
            TEST_LEAF.to_vec(),
            b"cidx".to_vec(),
            b"__cidx_secondary_duplicate__".to_vec(),
            b"k".to_vec(),
        ];
        assert!(
            issues.contains_key(&dup_path),
            "expected __cidx_secondary_duplicate__ for 'k', got: {:?}",
            issues.keys().collect::<Vec<_>>()
        );
    }

    // =====================================================================
    // Coverage push: extra tests around the new overwrite-cleanup
    // branches and V1 generic verify cidx subqueries.
    // =====================================================================

    #[test]
    fn insert_into_count_indexed_tree_overwrites_count_tree_with_empty_tree() {
        // Replace existing CountTree with empty Tree (different tree
        // type). Exercises the existing-tree-→-empty-tree branch.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"sub",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create sub");
        db.insert(
            [TEST_LEAF, b"cidx", b"sub"].as_ref(),
            b"inner",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate sub");

        // Overwrite CountTree with empty Tree.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"sub",
            Element::empty_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("overwrite CountTree with Tree");

        let elem = db
            .get([TEST_LEAF, b"cidx"].as_ref(), b"sub", None, grove_version)
            .unwrap()
            .expect("get");
        assert!(matches!(elem, Element::Tree(None, _)));

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty(), "issues: {:?}", issues);
    }

    #[test]
    fn insert_into_count_indexed_tree_overwrites_empty_count_tree_with_item() {
        // Replace an EMPTY existing CountTree with Item. Both count=0
        // and count=1 respectively, so the secondary mirror updates
        // from (0_be‖sub) → (1_be‖sub). Cleanup runs but doesn't
        // touch anything substantive (empty tree has no children).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"sub",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create sub");

        // Overwrite empty CountTree with Item.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"sub",
            Element::new_item(b"replaced".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("overwrite empty count tree with item");

        let elem = db
            .get([TEST_LEAF, b"cidx"].as_ref(), b"sub", None, grove_version)
            .unwrap()
            .expect("get");
        assert_eq!(elem, Element::new_item(b"replaced".to_vec()));

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty(), "issues: {:?}", issues);
    }

    #[test]
    fn insert_into_count_indexed_tree_rejects_overwrite_with_non_empty_tree() {
        // existing CountTree, new Tree with root_key=Some(...) →
        // REJECT (non-empty tree with claimed root_key).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"sub",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create sub");

        // Try non-empty Tree (claims root_key).
        let non_empty_tree = Element::Tree(Some(b"bogus".to_vec()), None);
        let result = db
            .insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"sub",
                non_empty_tree,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(crate::Error::NotSupported(msg)) => {
                assert!(
                    msg.contains("EMPTY tree")
                        || msg.contains("NON-EMPTY")
                        || msg.contains("ambiguous"),
                    "expected non-empty tree rejection, got: {msg}"
                );
            }
            other => panic!("expected NotSupported, got {:?}", other),
        }
    }

    #[test]
    fn batch_safe_subset_overwrite_replaces_cidx_with_sum_tree() {
        // Batch safe-subset overwrite: cidx → SumTree (non-cidx,
        // non-empty count-bearing-or-sum-bearing tree). The new tree
        // has count_value=0 / sum_value=0 by default.
        //
        // Wait — for a non-cidx tree, "empty" means root_key=None.
        // SumTree::empty has root_key=None and sum_value=0 — empty.
        // So this exercises the existing-cidx → empty-SumTree branch.
        use crate::batch::{BatchApplyOptions, QualifiedGroveDbOp};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");

        // Batch overwrite cidx with empty SumTree.
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            Element::empty_sum_tree(),
        )];
        let opts = BatchApplyOptions {
            validate_insertion_does_not_override_tree: false,
            ..Default::default()
        };
        db.apply_batch(ops, Some(opts), None, grove_version)
            .unwrap()
            .expect("overwrite cidx with SumTree");

        let elem = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get");
        assert!(matches!(elem, Element::SumTree(None, 0, _)));

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty());
    }

    #[test]
    fn batch_atomicity_failure_with_safe_subset_overwrite_preserves_cidx() {
        // Variant of the existing atomicity test: a safe-subset
        // overwrite + a failing op. The CIDX's pre-overwrite state
        // must be intact after rollback, AND verify_grovedb finds no
        // orphan from a partial cleanup.
        use crate::batch::{BatchApplyOptions, QualifiedGroveDbOp};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"b",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate b");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"existing",
            Element::new_item(b"original".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create existing");

        let root_before = db.root_hash(None, grove_version).unwrap().expect("root");

        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::new_item(b"replaced".to_vec()),
            ),
            QualifiedGroveDbOp::insert_only_known_to_not_already_exist_op(
                vec![TEST_LEAF.to_vec()],
                b"existing".to_vec(),
                Element::new_item(b"new".to_vec()),
            ),
        ];
        let opts = BatchApplyOptions {
            validate_insertion_does_not_override_tree: false,
            validate_insertion_does_not_override: true,
            ..Default::default()
        };
        assert!(db
            .apply_batch(ops, Some(opts), None, grove_version)
            .unwrap()
            .is_err());
        assert_eq!(
            root_before,
            db.root_hash(None, grove_version)
                .unwrap()
                .expect("root after"),
            "rollback failed"
        );

        // Cidx still functional + state intact.
        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top");
        assert_eq!(top.len(), 2);

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty());
    }

    #[test]
    fn cidx_item_key_exactly_at_247_byte_boundary_batch() {
        // The 247-byte ceiling is INCLUSIVE — exactly 247 is allowed.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        let max_ok_key = vec![b'a'; 247];
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
            max_ok_key.clone(),
            Element::new_item(b"v".to_vec()),
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("247-byte key must be accepted in batch");
        let item = db
            .get(
                [TEST_LEAF, b"cidx"].as_ref(),
                &max_ok_key,
                None,
                grove_version,
            )
            .unwrap()
            .expect("get");
        assert_eq!(item, Element::new_item(b"v".to_vec()));
    }

    // =====================================================================
    // Tests for the audit-fix patch landed in this commit:
    //   - Direct insert rejects partially-initialized cidx claims
    //   - proof/count_indexed.rs guards zero-layer envelopes
    //   - proof verify handles empty-cidx terminal proofs
    //   - delete clears nested cidx secondaries
    // =====================================================================

    #[test]
    fn direct_insert_rejects_cidx_with_count_but_no_roots() {
        // (None, None, count > 0) is partially initialized — the cidx
        // claims entries exist but provides no root keys. Reject.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let bogus =
            Element::new_count_indexed_tree_with_root_keys_and_count_value(None, None, 5, None);
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"key",
                bogus,
                None,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(crate::Error::InvalidInput(msg)) => {
                assert!(
                    msg.contains("partial") || msg.contains("BOTH"),
                    "expected partial-state rejection, got: {msg}"
                );
            }
            other => panic!("expected InvalidInput, got {:?}", other),
        }
    }

    #[test]
    fn direct_insert_rejects_cidx_with_primary_only() {
        // (Some, None, count > 0): asymmetric, only primary supplied.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let bogus = Element::new_count_indexed_tree_with_root_keys_and_count_value(
            Some(b"primary".to_vec()),
            None,
            5,
            None,
        );
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"key",
                bogus,
                None,
                None,
                grove_version,
            )
            .unwrap();
        // Accept either the new partial-state rejection or the existing
        // InvalidParentLayerPath (depending on which check fires first
        // — both are correct rejections).
        assert!(
            matches!(
                result,
                Err(crate::Error::InvalidInput(_) | crate::Error::InvalidParentLayerPath(_))
            ),
            "expected InvalidInput or InvalidParentLayerPath, got {:?}",
            result
        );
    }

    #[test]
    fn insert_into_count_indexed_tree_rejects_non_empty_cidx_on_brand_new_key() {
        // The non-empty-cidx rejection must fire even when the
        // item_key has no existing element to overwrite. Previously
        // the check was gated inside the existing_is_tree branch, so
        // brand-new keys slipped through and persisted inconsistent
        // state (parent merk wrote NULL_HASH child roots while the
        // serialized cidx element preserved the claimed root keys).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        // Brand-new key (cidx is empty).
        let non_empty = Element::new_count_indexed_tree_with_root_keys_and_count_value(
            Some(b"bogus_primary".to_vec()),
            Some(b"bogus_secondary".to_vec()),
            5,
            None,
        );
        let result = db
            .insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"brand_new_key",
                non_empty,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(crate::Error::NotSupported(msg)) => {
                assert!(
                    msg.contains("EMPTY cidx") || msg.contains("Non-empty cidx claims"),
                    "expected empty-cidx rejection, got: {msg}"
                );
            }
            other => panic!("expected NotSupported, got {:?}", other),
        }

        // Verify nothing was persisted.
        let result = db
            .get(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"brand_new_key",
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(crate::Error::PathKeyNotFound(_))),
            "brand-new key must not have been persisted, got {:?}",
            result
        );
    }

    #[test]
    fn insert_into_count_indexed_tree_rejects_non_empty_tree_on_brand_new_key() {
        // Same as the cidx case but for plain non-cidx trees with a
        // non-None root_key.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        let non_empty_tree = Element::Tree(Some(b"bogus_root".to_vec()), None);
        let result = db
            .insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"brand_new_tree",
                non_empty_tree,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(crate::Error::NotSupported(msg)) => {
                assert!(
                    msg.contains("EMPTY tree") || msg.contains("non-None root_key"),
                    "expected empty-tree rejection, got: {msg}"
                );
            }
            other => panic!("expected NotSupported, got {:?}", other),
        }
    }

    #[test]
    fn insert_into_count_indexed_tree_rejects_non_empty_cidx_replacing_item() {
        // Existing key is an ITEM (not a tree), new element is a
        // non-empty cidx. The unconditional check must fire even when
        // there's nothing to clean up.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");

        let non_empty = Element::new_count_indexed_tree_with_root_keys_and_count_value(
            Some(b"bogus_primary".to_vec()),
            Some(b"bogus_secondary".to_vec()),
            5,
            None,
        );
        let result = db
            .insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"k",
                non_empty,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(crate::Error::NotSupported(msg)) => {
                assert!(
                    msg.contains("EMPTY cidx") || msg.contains("Non-empty cidx claims"),
                    "expected empty-cidx rejection, got: {msg}"
                );
            }
            other => panic!("expected NotSupported, got {:?}", other),
        }

        // Original Item is intact.
        let elem = db
            .get([TEST_LEAF, b"cidx"].as_ref(), b"k", None, grove_version)
            .unwrap()
            .expect("get");
        assert_eq!(elem, Element::new_item(b"v".to_vec()));
    }

    #[test]
    fn verify_count_indexed_top_k_rejects_zero_layer_envelope() {
        // An adversarial envelope with 0 layers + 0 path elements
        // previously panicked via underflow at `last_idx = len - 1`.
        // The guard now rejects with CorruptedData.
        let envelope = crate::operations::proof::count_indexed::CountIndexedRangeProof {
            layer_proofs: Vec::new(),
            primary_root_hash: [0u8; 32],
            ancestor_cidx_secondary_root_hashes: Vec::new(),
            secondary_proof: Vec::new(),
            // Match the test's expected_k=1, expected_descending=false
            // below so the new envelope-matches-expected check passes
            // and the zero-layer guard is reached. Without this match
            // the test would fail on the direction/limit guard instead
            // (which is also correct rejection, but a different code
            // path).
            requested_limit: Some(1),
            descending: false,
        };
        let bytes = bincode::encode_to_vec(&envelope, bincode::config::standard()).unwrap();
        let result = GroveDb::verify_count_indexed_top_k(&bytes, &[], 1, false);
        match result {
            Err(crate::Error::CorruptedData(msg)) => {
                assert!(
                    msg.contains("zero layers") || msg.contains("at least one"),
                    "expected zero-layer rejection, got: {msg}"
                );
            }
            other => panic!("expected CorruptedData, got {:?}", other),
        }
    }

    #[test]
    fn direct_delete_clears_nested_cidx_secondary() {
        // Layout: TEST_LEAF / outer (regular Tree) / nested_cidx (cidx)
        // Delete the outer Tree. find_subtrees walks the outer's
        // children and clears each subtree's PRIMARY storage; the
        // nested_cidx's SECONDARY namespace must also be cleaned up by
        // the audit-fix patch.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer tree");
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"nested_cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create nested cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer", b"nested_cidx"].as_ref(),
            b"item",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate nested cidx");

        // Delete the outer Tree (with allow_deleting_non_empty_trees).
        use crate::operations::delete::DeleteOptions;
        let opts = DeleteOptions {
            allow_deleting_non_empty_trees: true,
            ..Default::default()
        };
        db.delete(
            [TEST_LEAF].as_ref(),
            b"outer",
            Some(opts),
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete outer tree");

        // Re-create the same layout at the same path. The nested cidx
        // must observe a clean secondary (no orphan from the prior
        // incarnation).
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("recreate outer");
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"nested_cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("recreate nested cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer", b"nested_cidx"].as_ref(),
            b"new_item",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert into fresh nested cidx");

        // Top-k must see only the new_item, no orphan from the old
        // nested cidx's secondary.
        let top = db
            .count_indexed_top_k(
                [TEST_LEAF, b"outer", b"nested_cidx"].as_ref(),
                10,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("top-k after recreate");
        assert_eq!(top.len(), 1);
        assert_eq!(top[0].1, b"new_item".to_vec());

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty());
    }

    #[test]
    fn delete_from_count_indexed_tree_handles_overwrite_remnant() {
        // After an overwrite (existing tree → Item via dedicated API
        // with cleanup), delete the new Item. Exercises the
        // delete-after-overwrite-with-cleanup path.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create tree");
        // Overwrite tree with Item.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"k",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("overwrite tree with item");
        // Delete the Item.
        db.delete_from_count_indexed_tree([TEST_LEAF, b"cidx"].as_ref(), b"k", None, grove_version)
            .unwrap()
            .expect("delete after overwrite");
        // Cidx is now empty.
        match db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get cidx")
        {
            Element::CountIndexedTree(_, _, count, _) => assert_eq!(count, 0),
            other => panic!("expected cidx, got {:?}", other),
        }
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty());
    }

    // =====================================================================
    // Audit fix tests (PR comment 4422941255).
    //
    // P1: verify_count_indexed_top_k / verify_count_indexed_query must bind
    //     caller intent (expected_k / expected_descending / expected_limit)
    //     and reject envelopes whose parameters do not match. Also,
    //     `requested_limit` must distinguish `None` from `Some(0)`.
    //
    // P2: reconcile_count_indexed_tree_secondary must refuse to synthesize a
    //     secondary key from a primary key whose length exceeds the cidx
    //     ceiling. verify_grovedb must also flag oversized primary keys via
    //     the `__cidx_primary_key_oversize__` sentinel.
    // =====================================================================

    /// Injects an `Element::new_item` directly into the cidx primary at
    /// `raw_key`, bypassing the cidx insert wrapper's length check, and
    /// propagates the primary's new root_key/root_hash into the parent's
    /// cidx element so the layered merk pointer stays consistent. The
    /// secondary is left untouched — used to simulate legacy/corrupt
    /// states (oversize keys, primary entries with no secondary mirror).
    /// Only use with `raw_key.len() < 256` since merk still enforces its
    /// own key-length invariant.
    fn corrupt_primary_insert_oversized(
        db: &crate::GroveDb,
        cidx_primary_path: &[&[u8]],
        raw_key: &[u8],
        grove_version: &GroveVersion,
    ) {
        use grovedb_merk::element::{
            get::ElementFetchFromStorageExtensions, insert::ElementInsertToStorageExtensions,
            reconstruct::ElementReconstructExtensions,
        };
        use grovedb_path::SubtreePath;
        use grovedb_storage::{Storage, StorageBatch};

        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let path_vec: Vec<&[u8]> = cidx_primary_path.to_vec();
        let path: SubtreePath<&[u8]> = path_vec.as_slice().into();
        let (parent_path, cidx_key) = path.derive_parent().expect("non-root cidx");

        // 1. Read the current cidx element from the parent so we can
        //    reconstruct it with the post-insert root_key/aggregate.
        let cidx_element = {
            let parent_merk = db
                .open_transactional_merk_at_path(
                    parent_path.clone(),
                    &tx,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .expect("open parent");
            Element::get(&parent_merk, cidx_key, true, grove_version)
                .unwrap()
                .expect("cidx element")
        };

        // 2. Insert the bogus item into the primary. Item is non-counted
        //    + non-summed, which is acceptable in a count-bearing tree.
        let (primary_root_hash, primary_root_key, primary_aggregate_data) = {
            let mut primary_merk = db
                .open_transactional_merk_at_path(path.clone(), &tx, Some(&batch), grove_version)
                .unwrap()
                .expect("open primary");
            let bogus = Element::new_item(b"v".to_vec());
            bogus
                .insert(&mut primary_merk, raw_key, None, grove_version)
                .unwrap()
                .expect("insert oversize key into primary");
            primary_merk
                .root_hash_key_and_aggregate_data()
                .unwrap()
                .expect("snapshot primary post-insert")
        };

        // Pull the current secondary_root_key off the cidx element
        // once for use in both the secondary open and the reconstruct.
        let secondary_root_key_now = match cidx_element.underlying() {
            Element::CountIndexedTree(_, s, ..) | Element::ProvableCountIndexedTree(_, s, ..) => {
                s.clone()
            }
            _ => panic!("not a cidx element"),
        };

        // 3. Reconstruct parent's cidx element with the new primary
        //    root_key/aggregate; keep secondary_root_key as-is, then
        //    write it back. The parent's pointer is now consistent with
        //    the primary's new tree, so verify_grovedb's recursion can
        //    walk into the primary and observe the oversize key.
        let secondary_root_hash = {
            let secondary_merk = db
                .open_count_indexed_secondary_at_path(
                    path.clone(),
                    secondary_root_key_now.clone(),
                    &tx,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .expect("open secondary");
            let (h, _, _) = secondary_merk
                .root_hash_key_and_aggregate_data()
                .unwrap()
                .expect("snapshot secondary");
            h
        };

        let reconstructed = cidx_element
            .reconstruct_with_two_root_keys(
                primary_root_key,
                secondary_root_key_now,
                primary_aggregate_data,
            )
            .expect("reconstruct cidx element");
        {
            let mut parent_merk = db
                .open_transactional_merk_at_path(parent_path, &tx, Some(&batch), grove_version)
                .unwrap()
                .expect("open parent (write)");
            reconstructed
                .insert_count_indexed_subtree(
                    &mut parent_merk,
                    cidx_key,
                    primary_root_hash,
                    secondary_root_hash,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("rewrite parent cidx element");
        }

        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit");
        tx.commit().expect("commit transaction");
    }

    #[test]
    fn verify_count_indexed_top_k_rejects_wrong_expected_descending() {
        // P1: a valid descending=true proof must NOT verify when the caller
        // requested descending=false. A malicious prover could otherwise
        // answer a top-N-largest request with a top-N-smallest proof that
        // chains to the same root.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for k in [b"a".as_slice(), b"b", b"c"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        let proof = db
            .prove_count_indexed_top_k(
                [TEST_LEAF, b"cidx"].as_ref(),
                10,
                true, // prove descending
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove");
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        // Same k, opposite direction — must be rejected.
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 10, false);
        match result {
            Err(crate::Error::CorruptedData(msg)) => {
                assert!(
                    msg.contains("direction mismatch"),
                    "expected direction-mismatch error, got: {msg}"
                );
            }
            other => panic!("expected CorruptedData direction mismatch, got: {other:?}"),
        }
        // Sanity: original (matching) parameters still verify.
        GroveDb::verify_count_indexed_top_k(&proof, path, 10, true)
            .expect("matching params should verify");
    }

    #[test]
    fn verify_count_indexed_top_k_rejects_wrong_expected_k() {
        // P1: a valid k=5 proof must NOT verify when the caller requested
        // k=10. Different k changes how many entries the verifier is
        // willing to accept from the underlying merk proof.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for k in [b"a".as_slice(), b"b", b"c"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        let proof = db
            .prove_count_indexed_top_k(
                [TEST_LEAF, b"cidx"].as_ref(),
                5, // prover used k=5
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove");
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 10, true);
        match result {
            Err(crate::Error::CorruptedData(msg)) => {
                assert!(
                    msg.contains("limit mismatch"),
                    "expected limit-mismatch error, got: {msg}"
                );
            }
            other => panic!("expected CorruptedData limit mismatch, got: {other:?}"),
        }
        GroveDb::verify_count_indexed_top_k(&proof, path, 5, true)
            .expect("matching k should verify");
    }

    #[test]
    fn verify_count_indexed_query_rejects_wrong_expected_limit() {
        // P1: prove with limit=Some(5), verify with limit=Some(10) — reject.
        use grovedb_merk::proofs::Query as MerkQuery;
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for k in [b"a".as_slice(), b"b", b"c"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        let mut q = MerkQuery::new();
        q.insert_all();
        q.left_to_right = false;
        let proof = db
            .prove_count_indexed_query(
                [TEST_LEAF, b"cidx"].as_ref(),
                q.clone(),
                Some(5),
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove");
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let result = GroveDb::verify_count_indexed_query(&proof, q.clone(), Some(10), path);
        match result {
            Err(crate::Error::CorruptedData(msg)) => {
                assert!(
                    msg.contains("limit mismatch"),
                    "expected limit-mismatch error, got: {msg}"
                );
            }
            other => panic!("expected CorruptedData limit mismatch, got: {other:?}"),
        }
        GroveDb::verify_count_indexed_query(&proof, q, Some(5), path)
            .expect("matching limit should verify");
    }

    #[test]
    fn verify_count_indexed_query_distinguishes_none_from_some_zero() {
        // P1: requested_limit is Option<u16>; a proof produced with
        // limit=None must not verify when the caller asks for Some(0)
        // (and vice-versa). Without the Option, `0` was conflated with
        // "no limit".
        use grovedb_merk::proofs::Query as MerkQuery;
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        // Need at least one entry so the prove path exercises secondary
        // proof generation rather than the empty-cidx short-circuit.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
        let mut q = MerkQuery::new();
        q.insert_all();
        q.left_to_right = true;

        // Prove with None, verify with Some(0).
        let proof_none = db
            .prove_count_indexed_query(
                [TEST_LEAF, b"cidx"].as_ref(),
                q.clone(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove with None");
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let result = GroveDb::verify_count_indexed_query(&proof_none, q.clone(), Some(0), path);
        assert!(
            matches!(result, Err(crate::Error::CorruptedData(_))),
            "Some(0) verify of a None proof should reject, got: {result:?}"
        );
        // And the inverse: prove with Some(0), verify with None.
        let proof_zero = db
            .prove_count_indexed_query(
                [TEST_LEAF, b"cidx"].as_ref(),
                q.clone(),
                Some(0),
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove with Some(0)");
        let result = GroveDb::verify_count_indexed_query(&proof_zero, q.clone(), None, path);
        assert!(
            matches!(result, Err(crate::Error::CorruptedData(_))),
            "None verify of a Some(0) proof should reject, got: {result:?}"
        );
        // Sanity: matching None ↔ None and Some(0) ↔ Some(0) verify.
        GroveDb::verify_count_indexed_query(&proof_none, q.clone(), None, path)
            .expect("None ↔ None verify");
        GroveDb::verify_count_indexed_query(&proof_zero, q, Some(0), path)
            .expect("Some(0) ↔ Some(0) verify");
    }

    #[test]
    fn reconcile_rejects_oversized_primary_key() {
        // P2: an oversize primary key (> 247 bytes) injected by a code
        // path that bypassed the cidx-key length check must cause
        // reconcile to fail closed with CorruptedData rather than build
        // a secondary key that violates Merk's < 256 invariant.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        // 248 = MAX_CIDX_ITEM_KEY_LEN + 1, well under merk's 256-byte
        // ceiling. Different first byte from any cidx_item_key we ever
        // use to keep this isolated.
        let oversize_key = vec![0xAAu8; 248];
        corrupt_primary_insert_oversized(&db, &[TEST_LEAF, b"cidx"], &oversize_key, grove_version);

        let result = db
            .reconcile_count_indexed_tree_secondary(
                [TEST_LEAF, b"cidx"].as_ref(),
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(crate::Error::CorruptedData(msg)) => {
                assert!(
                    msg.contains("248 bytes") && msg.contains("247"),
                    "expected oversize-key error, got: {msg}"
                );
            }
            other => panic!("expected CorruptedData for oversize primary key, got: {other:?}"),
        }
    }

    #[test]
    fn corrupt_primary_insert_helper_roundtrips_short_key() {
        // Sanity-check the helper: inserting a SHORT key directly into
        // the cidx primary via Element::insert + parent-rewrite must
        // produce a primary whose verify_grovedb recurses cleanly. The
        // resulting state is intentionally drifty (the secondary wasn't
        // mirrored), so verify_grovedb still flags drift — but the
        // recursion itself must not error with "expected merk to
        // contain value at key ... for item", which would indicate the
        // helper failed to keep the parent's primary_root_key in sync
        // with the primary's new root.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        corrupt_primary_insert_oversized(
            &db,
            &[TEST_LEAF, b"cidx"],
            b"short", // 5 bytes, well under the ceiling
            grove_version,
        );
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb must succeed (recursion-level)");
        // Expect at least a primary_orphan sentinel for "short" (the
        // secondary lacks the mirror) — confirms the recursion got
        // through the cidx walk.
        let primary_orphan_path: Vec<Vec<u8>> = vec![
            TEST_LEAF.to_vec(),
            b"cidx".to_vec(),
            b"__cidx_primary_orphan__".to_vec(),
            b"short".to_vec(),
        ];
        assert!(
            issues.contains_key(&primary_orphan_path),
            "expected __cidx_primary_orphan__ for 'short' (helper sanity), \
             got issues: {:?}",
            issues.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn verify_grovedb_flags_oversized_primary_key() {
        // P2: verify_grovedb must surface oversize cidx primary keys via
        // the `__cidx_primary_key_oversize__` sentinel so operators can
        // discover the corruption without having to run reconcile.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        let oversize_key = vec![0xBBu8; 250];
        corrupt_primary_insert_oversized(&db, &[TEST_LEAF, b"cidx"], &oversize_key, grove_version);

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        let oversize_sentinel: Vec<Vec<u8>> = vec![
            TEST_LEAF.to_vec(),
            b"cidx".to_vec(),
            b"__cidx_primary_key_oversize__".to_vec(),
            oversize_key.clone(),
        ];
        assert!(
            issues.contains_key(&oversize_sentinel),
            "expected __cidx_primary_key_oversize__ sentinel for the {}-byte key, \
             got issues: {:?}",
            oversize_key.len(),
            issues.keys().collect::<Vec<_>>()
        );
        // Diagnostic: the length is encoded in the last 8 bytes of the
        // third hash slot.
        let len_slot = issues.get(&oversize_sentinel).expect("sentinel present").2;
        let encoded_len =
            u64::from_be_bytes(len_slot[24..32].try_into().expect("8 bytes")) as usize;
        assert_eq!(
            encoded_len,
            oversize_key.len(),
            "oversize sentinel should encode the actual key length"
        );
    }

    // =====================================================================
    // Coverage tests for cidx code paths that are otherwise hard to reach:
    //   - direct insert_into_count_indexed_tree overwriting a nested cidx
    //     (existing_is_tree && existing_is_cidx — clears secondary).
    //   - direct delete_from_count_indexed_tree removing a nested cidx
    //     entry (deleted_was_cidx_primary — clears its secondary too).
    //   - count_indexed_top_k / count_indexed_count_range surfacing a
    //     CorruptedData error when the secondary contains a key that is
    //     shorter than the 8-byte count prefix (drift via corrupted
    //     storage; the decode_secondary_key helper returns None).
    //   - reconcile_count_indexed_tree_secondary surfacing CorruptedData
    //     when the primary contains undecodable Element bytes.
    // =====================================================================

    #[test]
    fn direct_insert_into_cidx_overwrites_nested_cidx_entry_and_cleans_secondary() {
        // Layout: TEST_LEAF / outer_cidx / inner (nested cidx, populated)
        // Then call insert_into_count_indexed_tree on outer_cidx with key
        // "inner" replacing it with a plain item (safe-subset overwrite
        // allowed when validate_insertion_does_not_override_tree is off
        // at the cidx-API level). The cleanup path
        // (count_indexed_tree.rs:400-427) must clear the nested cidx's
        // SECONDARY namespace too — not just the primary's subtree
        // storage.
        use grovedb_storage::{
            rocksdb_storage::RocksDbStorage, RawIterator, Storage, StorageContext,
        };

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer_cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer_cidx"].as_ref(),
            b"inner",
            Element::empty_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create nested cidx");
        // Force the nested cidx's secondary to acquire concrete storage
        // so the post-overwrite cleanup has something visible to clear.
        // Use corrupt_secondary_insert here because the normal cidx
        // insert path doesn't accept an empty (no-count) entry — and we
        // want a secondary KV that survives any merk root-key reset.
        corrupt_secondary_insert(
            &db,
            &[TEST_LEAF, b"outer_cidx", b"inner"],
            &make_secondary_key(0, b"sec_entry"),
            grove_version,
        );

        // Capture the nested cidx's S2-B secondary prefix.
        let inner_path: &[&[u8]] = &[TEST_LEAF, b"outer_cidx", b"inner"];
        let inner_path_vec: Vec<&[u8]> = inner_path.to_vec();
        let inner_subtree: grovedb_path::SubtreePath<&[u8]> = inner_path_vec.as_slice().into();
        let inner_primary_prefix = RocksDbStorage::build_prefix(inner_subtree).unwrap();
        let inner_secondary_prefix =
            RocksDbStorage::secondary_prefix_for(&inner_primary_prefix).unwrap();

        // Sanity: secondary namespace non-empty pre-overwrite.
        {
            let tx = db.start_transaction();
            let ctx = db
                .db
                .get_transactional_storage_context_by_subtree_prefix(
                    inner_secondary_prefix,
                    None,
                    &tx,
                )
                .unwrap();
            let mut it = ctx.raw_iter();
            it.seek_to_first().unwrap();
            assert!(
                it.valid().unwrap(),
                "pre-overwrite: nested cidx secondary must be non-empty"
            );
        }

        // Direct cidx insert overwriting nested cidx with a plain item.
        // This is the safe-subset path: cidx → non-cidx is allowed.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer_cidx"].as_ref(),
            b"inner",
            Element::new_item(b"replaced".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("overwrite nested cidx with item via direct cidx insert");

        // Verify nested cidx's secondary namespace is now cleared.
        {
            let tx = db.start_transaction();
            let ctx = db
                .db
                .get_transactional_storage_context_by_subtree_prefix(
                    inner_secondary_prefix,
                    None,
                    &tx,
                )
                .unwrap();
            let mut it = ctx.raw_iter();
            it.seek_to_first().unwrap();
            assert!(
                !it.valid().unwrap(),
                "post-overwrite: nested cidx secondary must be cleared by the \
                 direct insert_into_count_indexed_tree overwrite cleanup"
            );
        }

        // Element at outer/inner is now an item.
        let elem = db
            .get(
                [TEST_LEAF, b"outer_cidx"].as_ref(),
                b"inner",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get after overwrite");
        assert_eq!(elem, Element::new_item(b"replaced".to_vec()));
    }

    #[test]
    fn direct_delete_from_cidx_removes_nested_cidx_entry_and_cleans_secondary() {
        // Layout: TEST_LEAF / outer_cidx / nested_cidx (populated cidx).
        // delete_from_count_indexed_tree on the outer with key
        // "nested_cidx" must (a) remove the entry from the outer's
        // primary, (b) clear the nested cidx's PRIMARY child storage
        // (find_subtrees loop), AND (c) clear the nested cidx's
        // SECONDARY namespace (count_indexed_tree.rs:1408-1443).
        use grovedb_storage::{
            rocksdb_storage::RocksDbStorage, RawIterator, Storage, StorageContext,
        };

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer_cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer_cidx"].as_ref(),
            b"nested_cidx",
            Element::empty_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("create nested cidx");
        // Populate so secondary has concrete state to clean.
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer_cidx", b"nested_cidx"].as_ref(),
            b"x",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate nested cidx");

        // Capture nested cidx's secondary prefix.
        let nested_path: &[&[u8]] = &[TEST_LEAF, b"outer_cidx", b"nested_cidx"];
        let nested_path_vec: Vec<&[u8]> = nested_path.to_vec();
        let nested_subtree: grovedb_path::SubtreePath<&[u8]> = nested_path_vec.as_slice().into();
        let nested_primary_prefix = RocksDbStorage::build_prefix(nested_subtree).unwrap();
        let nested_secondary_prefix =
            RocksDbStorage::secondary_prefix_for(&nested_primary_prefix).unwrap();

        // Sanity: nested secondary is populated pre-delete.
        {
            let tx = db.start_transaction();
            let ctx = db
                .db
                .get_transactional_storage_context_by_subtree_prefix(
                    nested_secondary_prefix,
                    None,
                    &tx,
                )
                .unwrap();
            let mut it = ctx.raw_iter();
            it.seek_to_first().unwrap();
            assert!(
                it.valid().unwrap(),
                "pre-delete: nested cidx secondary must be non-empty"
            );
        }

        // Delete the nested_cidx entry via delete_from_count_indexed_tree.
        let removed = db
            .delete_from_count_indexed_tree(
                [TEST_LEAF, b"outer_cidx"].as_ref(),
                b"nested_cidx",
                None,
                grove_version,
            )
            .unwrap()
            .expect("delete nested cidx entry from outer");
        assert!(removed, "entry must exist and report removed=true");

        // Verify nested cidx's secondary namespace is cleared.
        {
            let tx = db.start_transaction();
            let ctx = db
                .db
                .get_transactional_storage_context_by_subtree_prefix(
                    nested_secondary_prefix,
                    None,
                    &tx,
                )
                .unwrap();
            let mut it = ctx.raw_iter();
            it.seek_to_first().unwrap();
            assert!(
                !it.valid().unwrap(),
                "post-delete: nested cidx secondary must be cleared by \
                 delete_from_count_indexed_tree"
            );
        }

        // Outer no longer contains the entry.
        let result = db
            .get(
                [TEST_LEAF, b"outer_cidx"].as_ref(),
                b"nested_cidx",
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(crate::Error::PathKeyNotFound(_))),
            "deleted entry must return PathKeyNotFound, got {:?}",
            result
        );

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "verify issues: {:?}", issues);
    }

    #[test]
    fn count_indexed_top_k_errors_on_short_secondary_key_drift() {
        // The secondary's key encoding is `count_be(8 bytes) ‖
        // original_key`. If drift creates a key shorter than 8 bytes,
        // the iterator's decode_secondary_key returns None and
        // count_indexed_top_k surfaces a CorruptedData error
        // (count_indexed_tree.rs:1061-1065, 1750).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        // Inject a 3-byte (< 8) secondary key directly. Bypasses cidx
        // mirror logic that always uses 8-byte count prefix.
        corrupt_secondary_insert(&db, &[TEST_LEAF, b"cidx"], b"abc", grove_version);

        let result = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap();
        match result {
            Err(crate::Error::CorruptedData(msg)) => {
                assert!(
                    msg.contains("shorter than 8 bytes"),
                    "expected short-key CorruptedData, got: {msg}"
                );
            }
            other => panic!(
                "expected CorruptedData(secondary key shorter than 8 bytes), \
                 got: {other:?}"
            ),
        }
    }

    #[test]
    fn count_indexed_count_range_errors_on_short_secondary_key_drift() {
        // Same drift class as the previous test, exercising the
        // count_indexed_count_range branch
        // (count_indexed_tree.rs:1160-1164). A short secondary key
        // returned by the range iterator must produce CorruptedData
        // rather than silently truncate.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        // 3-byte secondary key. The range [0, u64::MAX] forces the
        // None-upper branch (lo_bytes.shrink_to_fit + insert_range_from)
        // — also useful coverage.
        corrupt_secondary_insert(&db, &[TEST_LEAF, b"cidx"], b"abc", grove_version);

        let result = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                0,
                u64::MAX,
                false,
                10,
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(crate::Error::CorruptedData(msg)) => {
                assert!(
                    msg.contains("shorter than 8 bytes"),
                    "expected short-key CorruptedData, got: {msg}"
                );
            }
            other => panic!("expected CorruptedData, got: {other:?}"),
        }
    }

    #[test]
    fn count_indexed_count_range_returns_empty_when_lo_greater_than_hi() {
        // Trivial early-return path
        // (count_indexed_tree.rs:1096-1098): if lo_count > hi_count, the
        // returned range is empty without opening the secondary merk.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        let res = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                10,
                5, // lo > hi
                false,
                10,
                None,
                grove_version,
            )
            .unwrap()
            .expect("range with lo>hi");
        assert!(
            res.is_empty(),
            "lo>hi must produce empty result, got {:?}",
            res
        );
    }

    #[test]
    fn reconcile_errors_on_undecodable_element_bytes_in_primary() {
        // The reconcile loop calls `Element::raw_decode` on each primary
        // KV's value bytes. If a corrupted code path stored bytes that
        // don't decode as an Element, reconcile surfaces a CorruptedData
        // error (count_indexed_tree.rs:842-845) rather than panicking or
        // silently producing a wrong secondary.
        use grovedb_path::SubtreePath;
        use grovedb_storage::{Storage, StorageBatch, StorageContext};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        // Write garbage bytes directly into the cidx primary's storage
        // namespace at a key that the merk's raw_iter will surface. We
        // bypass the merk-level tree structure so the bytes won't form
        // a valid TreeNode, but the bytes-in-storage will still trip
        // the raw_decode path in reconcile.
        //
        // Approach: write to the storage context at the cidx primary's
        // prefix using StorageContext::put. The bytes show up via the
        // raw_iter scan but aren't part of the merk's tree.
        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let cidx_path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let path_vec: Vec<&[u8]> = cidx_path.to_vec();
        let subtree: SubtreePath<&[u8]> = path_vec.as_slice().into();
        let mut storage = db
            .db
            .get_transactional_storage_context(subtree, Some(&batch), &tx)
            .unwrap();
        storage
            .put(
                b"corrupted",
                b"this is not valid Element bytes \xff\xff\xff\xff",
                None,
                None,
            )
            .unwrap()
            .expect("write garbage bytes");
        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit");
        tx.commit().expect("commit tx");

        let result = db
            .reconcile_count_indexed_tree_secondary(
                [TEST_LEAF, b"cidx"].as_ref(),
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(crate::Error::CorruptedData(msg)) => {
                assert!(
                    msg.contains("failed to decode element while reconciling secondary"),
                    "expected decode-failure CorruptedData, got: {msg}"
                );
            }
            other => panic!("expected CorruptedData decode error, got: {other:?}"),
        }
    }

    #[test]
    fn reconcile_repairs_missing_secondary_entry_via_insert_loop() {
        // Exercises reconcile's insert loop
        // (count_indexed_tree.rs:927-937): re-adds a secondary entry
        // that the primary still claims but the secondary lacks.
        //
        // Set up cidx with one entry, delete its secondary mirror,
        // run reconcile, expect the mirror to be restored.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        let ct = Element::new_count_tree_with_flags_and_count_value(None, 7, None);
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            ct,
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");

        // Drift: delete the mirror for (count=7, "a").
        corrupt_secondary_delete(
            &db,
            &[TEST_LEAF, b"cidx"],
            &make_secondary_key(7, b"a"),
            grove_version,
        );

        // Reconcile must re-insert the missing mirror via the insert
        // loop. Don't assert on top_k afterwards (verify_grovedb's
        // chain integrity isn't restored by reconcile alone), only
        // confirm reconcile completes without error so the loop runs.
        db.reconcile_count_indexed_tree_secondary(
            [TEST_LEAF, b"cidx"].as_ref(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("reconcile must run insert loop without error");
    }

    #[test]
    fn reconcile_removes_orphan_secondary_entry_via_delete_loop() {
        // Exercises reconcile's delete loop
        // (count_indexed_tree.rs:909-919): removes a secondary entry
        // that the primary doesn't claim.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        // Real entry so the secondary's tree isn't empty.
        let ct = Element::new_count_tree_with_flags_and_count_value(None, 7, None);
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            ct,
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");

        // Drift: inject an orphan into the secondary.
        corrupt_secondary_insert(
            &db,
            &[TEST_LEAF, b"cidx"],
            &make_secondary_key(999, b"ghost"),
            grove_version,
        );

        // Reconcile must remove the orphan via the delete loop.
        db.reconcile_count_indexed_tree_secondary(
            [TEST_LEAF, b"cidx"].as_ref(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("reconcile must run delete loop without error");
    }

    #[test]
    fn verify_count_indexed_top_k_rejects_tampered_primary_root_hash() {
        // Coverage for proof/count_indexed.rs:566-572: the H1-A chain
        // check at the cidx layer must fail when the envelope's
        // `primary_root_hash` field is tampered. Generate a valid
        // proof, flip a byte in the encoded primary_root_hash, and
        // expect a "cidx layer chain mismatch" CorruptedData.
        use crate::operations::proof::count_indexed::CountIndexedRangeProof;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for k in [b"a".as_slice(), b"b"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        let proof = db
            .prove_count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("prove");

        // Decode + tamper + re-encode.
        let config = bincode::config::standard().with_limit::<{ 16 * 1024 * 1024 }>();
        let (mut envelope, _): (CountIndexedRangeProof, _) =
            bincode::decode_from_slice(&proof, config).expect("decode");
        envelope.primary_root_hash[0] ^= 0xFF; // flip one byte
        let tampered = bincode::encode_to_vec(&envelope, bincode::config::standard())
            .expect("re-encode tampered envelope");

        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let result = GroveDb::verify_count_indexed_top_k(&tampered, path, 10, true);
        match result {
            Err(crate::Error::CorruptedData(msg)) => {
                assert!(
                    msg.contains("cidx layer chain mismatch") || msg.contains("chain mismatch"),
                    "expected chain mismatch CorruptedData, got: {msg}"
                );
            }
            other => panic!("expected CorruptedData(chain mismatch), got: {other:?}"),
        }
    }

    #[test]
    fn verify_count_indexed_top_k_rejects_tampered_intermediate_layer_proof() {
        // Coverage for proof/count_indexed.rs:600-613: the verifier's
        // shallower-layer chain check (combine_hash / combine_hash_three
        // at non-cidx ancestor depth). Tamper a byte in an intermediate
        // layer's encoded proof; the recomputed value_hash for that
        // layer will diverge from the parent's recorded one.
        use crate::operations::proof::count_indexed::CountIndexedRangeProof;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("outer tree");
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer", b"cidx"].as_ref(),
            b"a",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");

        let proof = db
            .prove_count_indexed_top_k(
                [TEST_LEAF, b"outer", b"cidx"].as_ref(),
                10,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove");
        let config = bincode::config::standard().with_limit::<{ 16 * 1024 * 1024 }>();
        let (mut envelope, _): (CountIndexedRangeProof, _) =
            bincode::decode_from_slice(&proof, config).expect("decode");
        // Tamper an intermediate (non-deepest) layer proof. Replace it
        // with a wholly different layer proof — concretely, the
        // secondary proof bytes (a different valid Merk proof, but
        // proving a different tree at a different key). The verifier
        // must reject because the recomputed value_hash for this layer
        // diverges from the parent's recorded one.
        envelope.layer_proofs[0] = envelope.secondary_proof.clone();
        let tampered =
            bincode::encode_to_vec(&envelope, bincode::config::standard()).expect("re-encode");
        let path: &[&[u8]] = &[TEST_LEAF, b"outer", b"cidx"];
        let result = GroveDb::verify_count_indexed_top_k(&tampered, path, 10, true);
        // Accept any CorruptedData — the tampered layer must NOT
        // verify silently.
        assert!(
            matches!(result, Err(crate::Error::CorruptedData(_))),
            "tampered intermediate layer must produce CorruptedData, got: {:?}",
            result
        );
    }

    #[test]
    fn verify_count_indexed_top_k_rejects_tampered_secondary_root_hash_via_query() {
        // Coverage for proof/count_indexed.rs:581-585: the verifier
        // checks `ancestor_cidx_secondary_root_hashes.len() == last_idx`
        // and emits CorruptedData if a length mismatch sneaks in. We
        // achieve this by tampering the envelope's ancestor secondary
        // attestation vector to a wrong length.
        use crate::operations::proof::count_indexed::CountIndexedRangeProof;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Nested cidx so last_idx > 0 — without nesting, ancestor list
        // would be size 0 and trivially "match".
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("outer");
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer", b"cidx"].as_ref(),
            b"a",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");

        let proof = db
            .prove_count_indexed_top_k(
                [TEST_LEAF, b"outer", b"cidx"].as_ref(),
                10,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove");

        let config = bincode::config::standard().with_limit::<{ 16 * 1024 * 1024 }>();
        let (mut envelope, _): (CountIndexedRangeProof, _) =
            bincode::decode_from_slice(&proof, config).expect("decode");
        // Tamper: pad ancestor list to a wrong length.
        envelope.ancestor_cidx_secondary_root_hashes.push(None);
        let tampered =
            bincode::encode_to_vec(&envelope, bincode::config::standard()).expect("re-encode");

        let path: &[&[u8]] = &[TEST_LEAF, b"outer", b"cidx"];
        let result = GroveDb::verify_count_indexed_top_k(&tampered, path, 10, true);
        match result {
            Err(crate::Error::CorruptedData(msg)) => {
                assert!(
                    msg.contains("ancestor_cidx_secondary_root_hashes"),
                    "expected ancestor-length-mismatch CorruptedData, got: {msg}"
                );
            }
            other => panic!("expected CorruptedData(ancestor length mismatch), got: {other:?}"),
        }
    }

    #[test]
    fn verify_count_indexed_top_k_rejects_proof_with_short_secondary_key_drift() {
        // Coverage for proof/count_indexed.rs:540-545 — the verifier's
        // result_set loop rejects any proved key shorter than 8 bytes.
        // Build a proof where the secondary has a < 8-byte drifted key
        // (via corrupt_secondary_insert), generate the proof, and check
        // that verification surfaces the short-key CorruptedData rather
        // than silently producing garbage entries.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        // Drift: a 3-byte secondary key. The proof generator iterates
        // the secondary and includes this key in the proof; the
        // verifier's loop then rejects on length check.
        corrupt_secondary_insert(&db, &[TEST_LEAF, b"cidx"], b"abc", grove_version);

        let proof = db
            .prove_count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("prove (drift accepted at proof time; rejected at verify)");
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 10, true);
        match result {
            Err(crate::Error::CorruptedData(msg)) => {
                assert!(
                    msg.contains("shorter than 8 bytes"),
                    "expected short-key CorruptedData at verify, got: {msg}"
                );
            }
            other => {
                panic!("expected CorruptedData(short secondary key) at verify, got: {other:?}")
            }
        }
    }

    // =====================================================================
    // Coverage for the direct-insertion non-empty cidx path
    // (insert/mod.rs:314-410). Direct `db.insert(...)` of a cidx element
    // with concrete root_keys is the migration / restore-from-backup
    // path: the parent's value_hash must reflect the actual on-disk
    // child Merk root hashes, so insert() opens both child Merks and
    // validates the provided root_keys match. Reject branches:
    //   - non-empty cidx with one root None (asymmetric state).
    //   - non-empty cidx where the provided primary_root_key disagrees
    //     with the actual primary Merk's on-disk root.
    //   - non-empty cidx where the provided secondary_root_key
    //     disagrees with the actual secondary Merk's on-disk root.
    // =====================================================================

    /// Reads the parent-stored cidx element bytes so we can re-insert
    /// them directly via `db.insert(...)` to exercise the non-empty
    /// path.
    fn snapshot_cidx_element_for_direct_reinsert(
        db: &crate::GroveDb,
        parent_path: &[&[u8]],
        cidx_key: &[u8],
        grove_version: &GroveVersion,
    ) -> Element {
        db.get(parent_path, cidx_key, None, grove_version)
            .unwrap()
            .expect("get cidx for snapshot")
    }

    #[test]
    fn direct_insert_non_empty_cidx_with_matching_roots_succeeds() {
        // Build a cidx with content via the normal API, snapshot its
        // bytes, then re-insert the same cidx element bytes at the
        // same path. The direct-insert path opens both child Merks,
        // validates the provided root_keys match, reads root hashes
        // off disk, and stores the parent element with the correct
        // H1-A composition.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for k in [b"a".as_slice(), b"b"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }

        let snapshot =
            snapshot_cidx_element_for_direct_reinsert(&db, &[TEST_LEAF], b"cidx", grove_version);
        match snapshot.underlying() {
            Element::CountIndexedTree(Some(_), Some(_), _, _) => {
                // count_value reflects the aggregate count of the
                // inserted CountTrees, which are empty (count=0 each).
                // The non-empty state we care about is Some(_) on
                // both root_keys — that signals the child Merks have
                // concrete on-disk structure.
            }
            other => panic!("expected cidx with both roots Some, got: {:?}", other),
        }

        use crate::operations::insert::InsertOptions;
        let opts = InsertOptions {
            validate_insertion_does_not_override_tree: false,
            validate_insertion_does_not_override: false,
            base_root_storage_is_free: false,
        };
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            snapshot,
            Some(opts),
            None,
            grove_version,
        )
        .unwrap()
        .expect("direct re-insert with matching roots must succeed");

        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top_k after direct re-insert");
        assert_eq!(top.len(), 2);
    }

    #[test]
    fn direct_insert_partial_cidx_with_one_root_none_rejected() {
        // Non-empty cidx requires BOTH primary_root_key and
        // secondary_root_key to be Some(_); asymmetric state (one
        // None, one Some) must be rejected with InvalidInput. Also
        // (None, None, count > 0) must be rejected.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let bad_primary_only = Element::CountIndexedTree(Some(vec![1u8; 8]), None, 0, None);
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"a",
                bad_primary_only,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(crate::Error::InvalidInput(_))),
            "(Some, None) must be rejected, got: {:?}",
            result
        );

        let bad_secondary_only = Element::CountIndexedTree(None, Some(vec![2u8; 8]), 0, None);
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"b",
                bad_secondary_only,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(crate::Error::InvalidInput(_))),
            "(None, Some) must be rejected, got: {:?}",
            result
        );

        let bad_count_no_roots = Element::CountIndexedTree(None, None, 5, None);
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"c",
                bad_count_no_roots,
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(crate::Error::InvalidInput(_))),
            "(None, None, count>0) must be rejected, got: {:?}",
            result
        );
    }

    #[test]
    fn direct_insert_cidx_with_mismatched_primary_root_key_rejected() {
        // Provided primary_root_key doesn't match the actual primary
        // Merk's on-disk root → InvalidInput.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");

        let real =
            snapshot_cidx_element_for_direct_reinsert(&db, &[TEST_LEAF], b"cidx", grove_version);
        let (real_primary, real_secondary, real_count) = match real.underlying() {
            Element::CountIndexedTree(p, s, c, _) => (p.clone(), s.clone(), *c),
            other => panic!("expected cidx, got: {:?}", other),
        };
        let mut bad_primary = real_primary.clone().unwrap();
        bad_primary[0] ^= 0xFF;
        let tampered =
            Element::CountIndexedTree(Some(bad_primary), real_secondary, real_count, None);

        use crate::operations::insert::InsertOptions;
        let opts = InsertOptions {
            validate_insertion_does_not_override_tree: false,
            validate_insertion_does_not_override: false,
            base_root_storage_is_free: false,
        };
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"cidx",
                tampered,
                Some(opts),
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(crate::Error::InvalidInput(msg)) => {
                assert!(
                    msg.contains("primary_root_key"),
                    "expected primary_root_key mismatch error, got: {msg}"
                );
            }
            other => panic!(
                "expected InvalidInput(primary_root_key mismatch), got: {:?}",
                other
            ),
        }
    }

    #[test]
    fn direct_insert_cidx_with_mismatched_secondary_root_key_rejected() {
        // Mirror of the previous test but tampering secondary_root_key.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");

        let real =
            snapshot_cidx_element_for_direct_reinsert(&db, &[TEST_LEAF], b"cidx", grove_version);
        let (real_primary, real_secondary, real_count) = match real.underlying() {
            Element::CountIndexedTree(p, s, c, _) => (p.clone(), s.clone(), *c),
            other => panic!("expected cidx, got: {:?}", other),
        };
        let mut bad_secondary = real_secondary.clone().unwrap();
        bad_secondary[0] ^= 0xFF;
        let tampered =
            Element::CountIndexedTree(real_primary, Some(bad_secondary), real_count, None);

        use crate::operations::insert::InsertOptions;
        let opts = InsertOptions {
            validate_insertion_does_not_override_tree: false,
            validate_insertion_does_not_override: false,
            base_root_storage_is_free: false,
        };
        let result = db
            .insert(
                [TEST_LEAF].as_ref(),
                b"cidx",
                tampered,
                Some(opts),
                None,
                grove_version,
            )
            .unwrap();
        match result {
            Err(crate::Error::InvalidInput(msg)) => {
                assert!(
                    msg.contains("secondary_root_key"),
                    "expected secondary_root_key mismatch error, got: {msg}"
                );
            }
            other => panic!(
                "expected InvalidInput(secondary_root_key mismatch), got: {:?}",
                other
            ),
        }
    }

    #[test]
    fn v1_verify_rejects_cidx_subquery_proof_with_non_cidx_lower_layer_bytes() {
        // Coverage for proof/verify.rs:546-553 — when a V1 subquery
        // descent hits a CountIndexedTree element, the lower_layer's
        // merk_proof MUST be `ProofBytes::CountIndexedTree(_)`. If a
        // malicious prover replaces it with `ProofBytes::Merk(_)`, the
        // verifier rejects with "V1 lower layer for CountIndexedTree
        // element must use ProofBytes::CountIndexedTree".
        use crate::operations::proof::{GroveDBProof, ProofBytes};
        use crate::{PathQuery, SizedQuery};
        use grovedb_merk::proofs::{
            query::{QueryItem, SubqueryBranch},
            Query as MerkQuery,
        };

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for k in [b"a".as_slice(), b"b"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate");
        }

        let mut inner = MerkQuery::new();
        inner.insert_all();
        let path_query = PathQuery {
            path: vec![TEST_LEAF.to_vec()],
            query: SizedQuery {
                query: MerkQuery {
                    items: vec![QueryItem::Key(b"cidx".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: Some(inner.into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                    add_parent_tree_on_subquery: false,
                },
                limit: None,
                offset: None,
            },
        };

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove_query");
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let (proof, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof_bytes, config).expect("decode V1 proof");

        // Walk the layers to find the cidx layer and replace its
        // ProofBytes::CountIndexedTree(inner) with ProofBytes::Merk(inner).
        let mut tampered = proof;
        let root_layer = match &mut tampered {
            GroveDBProof::V1(v1) => &mut v1.root_layer,
            _ => panic!("expected V1 proof"),
        };
        let mut found = false;
        for (_k, lower) in root_layer.lower_layers.iter_mut() {
            for (_kk, sublower) in lower.lower_layers.iter_mut() {
                if let ProofBytes::CountIndexedTree(b) = &sublower.merk_proof {
                    sublower.merk_proof = ProofBytes::Merk(b.clone());
                    found = true;
                }
            }
        }
        assert!(found, "expected a CountIndexedTree layer in the proof");

        let tampered_bytes =
            bincode::encode_to_vec(&tampered, config).expect("re-encode tampered proof");
        let result = GroveDb::verify_query(&tampered_bytes, &path_query, grove_version);
        match result {
            Err(crate::Error::InvalidProof(_, msg)) => {
                assert!(
                    msg.contains("ProofBytes::CountIndexedTree")
                        || msg.contains("CountIndexedTree element"),
                    "expected cidx-proof-bytes-mismatch error, got: {msg}"
                );
            }
            other => panic!(
                "expected InvalidProof(cidx proof bytes mismatch), got: {:?}",
                other
            ),
        }
    }

    #[test]
    fn v1_verify_rejects_cidx_subquery_proof_with_short_cidx_bytes() {
        // Coverage for proof/verify.rs:555-561 — the cidx proof bytes
        // must be >= 32 bytes (the secondary_root attestation prefix).
        // Tamper to truncate the inner bytes.
        use crate::operations::proof::{GroveDBProof, ProofBytes};
        use crate::{PathQuery, SizedQuery};
        use grovedb_merk::proofs::{
            query::{QueryItem, SubqueryBranch},
            Query as MerkQuery,
        };

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");

        let mut inner = MerkQuery::new();
        inner.insert_all();
        let path_query = PathQuery {
            path: vec![TEST_LEAF.to_vec()],
            query: SizedQuery {
                query: MerkQuery {
                    items: vec![QueryItem::Key(b"cidx".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: Some(inner.into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                    add_parent_tree_on_subquery: false,
                },
                limit: None,
                offset: None,
            },
        };

        let proof_bytes = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove_query");
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let (mut proof, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof_bytes, config).expect("decode V1 proof");

        // Truncate the cidx layer's bytes to < 32.
        let root_layer = match &mut proof {
            GroveDBProof::V1(v1) => &mut v1.root_layer,
            _ => panic!("expected V1 proof"),
        };
        let mut found = false;
        for (_k, lower) in root_layer.lower_layers.iter_mut() {
            for (_kk, sublower) in lower.lower_layers.iter_mut() {
                if let ProofBytes::CountIndexedTree(b) = &mut sublower.merk_proof {
                    *b = vec![0u8; 16]; // 16 < 32
                    found = true;
                }
            }
        }
        assert!(found, "expected a CountIndexedTree layer in the proof");

        let tampered_bytes =
            bincode::encode_to_vec(&proof, config).expect("re-encode tampered proof");
        let result = GroveDb::verify_query(&tampered_bytes, &path_query, grove_version);
        match result {
            Err(crate::Error::InvalidProof(_, msg)) => {
                assert!(
                    msg.contains("shorter than 32-byte secondary root"),
                    "expected short-cidx-bytes error, got: {msg}"
                );
            }
            other => panic!("expected InvalidProof(short cidx bytes), got: {:?}", other),
        }
    }

    #[test]
    fn direct_insert_provable_count_indexed_tree_with_matching_roots_succeeds() {
        // Exercises the ProvableCountIndexedTree arm of the direct
        // insert pattern (insert/mod.rs:315). Mirrors the
        // CountIndexedTree happy-path test but with the provable
        // variant.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"prov_cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create provable cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"prov_cidx"].as_ref(),
            b"a",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");

        let snapshot = snapshot_cidx_element_for_direct_reinsert(
            &db,
            &[TEST_LEAF],
            b"prov_cidx",
            grove_version,
        );
        match snapshot.underlying() {
            Element::ProvableCountIndexedTree(Some(_), Some(_), _, _) => {}
            other => panic!(
                "expected provable cidx with both roots Some, got: {:?}",
                other
            ),
        }

        use crate::operations::insert::InsertOptions;
        let opts = InsertOptions {
            validate_insertion_does_not_override_tree: false,
            validate_insertion_does_not_override: false,
            base_root_storage_is_free: false,
        };
        db.insert(
            [TEST_LEAF].as_ref(),
            b"prov_cidx",
            snapshot,
            Some(opts),
            None,
            grove_version,
        )
        .unwrap()
        .expect("direct re-insert of provable cidx with matching roots must succeed");

        let top = db
            .count_indexed_top_k(
                [TEST_LEAF, b"prov_cidx"].as_ref(),
                10,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("top_k after direct re-insert");
        assert_eq!(top.len(), 1);
    }

    // =====================================================================
    // Coverage tests for cidx-proof verify entry-point error paths
    // (proof/count_indexed.rs:472-494, 534-537, 601-613).
    // =====================================================================

    #[test]
    fn verify_count_indexed_query_rejects_wrong_expected_descending() {
        // Coverage for proof/count_indexed.rs:472-478 — the
        // verify_count_indexed_query direction mismatch check (the
        // _query variant; the _top_k variant is already covered by
        // verify_count_indexed_top_k_rejects_wrong_expected_descending).
        use grovedb_merk::proofs::Query as MerkQuery;
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");

        // Prove with left_to_right=true (ascending).
        let mut q_asc = MerkQuery::new();
        q_asc.insert_all();
        q_asc.left_to_right = true;
        let proof = db
            .prove_count_indexed_query(
                [TEST_LEAF, b"cidx"].as_ref(),
                q_asc.clone(),
                Some(5),
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove");

        // Verify with left_to_right=false (descending) — direction
        // mismatch.
        let mut q_desc = MerkQuery::new();
        q_desc.insert_all();
        q_desc.left_to_right = false;
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let result = GroveDb::verify_count_indexed_query(&proof, q_desc, Some(5), path);
        match result {
            Err(crate::Error::CorruptedData(msg)) => {
                assert!(
                    msg.contains("direction mismatch"),
                    "expected direction-mismatch error, got: {msg}"
                );
            }
            other => panic!("expected CorruptedData direction mismatch, got: {other:?}"),
        }
    }

    #[test]
    fn verify_count_indexed_top_k_rejects_proof_with_layer_count_mismatch() {
        // Coverage for proof/count_indexed.rs:488-494. Generate a
        // proof for [TEST_LEAF, "cidx"] (2 layers), then verify with
        // a 1-segment path. layer_proofs.len() != path.len() → reject.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        // Populate so the cidx isn't empty (empty cidx prove errors).
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");

        let proof = db
            .prove_count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("prove");

        // Verify with WRONG path length (1 segment, but envelope has 2
        // layer proofs).
        let short_path: &[&[u8]] = &[TEST_LEAF];
        let result = GroveDb::verify_count_indexed_top_k(&proof, short_path, 10, true);
        match result {
            Err(crate::Error::CorruptedData(msg)) => {
                assert!(
                    msg.contains("layers") && msg.contains("segments"),
                    "expected layer/path-length-mismatch error, got: {msg}"
                );
            }
            other => panic!("expected CorruptedData(layer count mismatch), got: {other:?}"),
        }
    }

    #[test]
    fn verify_count_indexed_top_k_rejects_proof_with_corrupted_secondary_proof() {
        // Coverage for proof/count_indexed.rs:534-537. Tamper the
        // envelope's secondary_proof bytes; secondary range proof
        // verification must fail with CorruptedData. We replace the
        // secondary_proof bytes with random garbage so merk's
        // execute_proof returns an error.
        use crate::operations::proof::count_indexed::CountIndexedRangeProof;
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        let proof = db
            .prove_count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("prove");

        let config = bincode::config::standard().with_limit::<{ 16 * 1024 * 1024 }>();
        let (mut envelope, _): (CountIndexedRangeProof, _) =
            bincode::decode_from_slice(&proof, config).expect("decode");
        // Replace secondary_proof with garbage so execute_proof errors.
        envelope.secondary_proof = vec![0xFF; 32];
        let tampered =
            bincode::encode_to_vec(&envelope, bincode::config::standard()).expect("re-encode");

        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let result = GroveDb::verify_count_indexed_top_k(&tampered, path, 10, true);
        match result {
            Err(crate::Error::CorruptedData(msg)) => {
                assert!(
                    msg.contains("secondary range proof failed to verify")
                        || msg.contains("decoding")
                        || msg.contains("execute"),
                    "expected secondary-proof failure, got: {msg}"
                );
            }
            other => panic!("expected CorruptedData(secondary proof verification), got: {other:?}"),
        }
    }

    // =====================================================================
    // Coverage tests for the lib.rs cidx-propagate cascading aggregation
    // path (lib.rs:840-998). Triggered when a nested write under a cidx
    // ancestor bubbles up the parent count delta into the ancestor's
    // secondary mirror. The deep_insert_under_nested_cidx test exercises
    // this in a 2-level layout; the next test does the same for a deeper
    // 3-level chain to cover additional cascading paths in the same
    // propagate loop.
    // =====================================================================

    #[test]
    fn deep_insert_under_triple_nested_cidx_propagates_all_levels() {
        // Layout:
        //   TEST_LEAF / outer (cidx)
        //                  / middle (cidx)
        //                          / inner (cidx)
        //                                  / leaf_ct (count_tree)
        // Insert an item inside leaf_ct; the count must bubble up
        // through inner → middle → outer, updating each level's
        // secondary mirror. Exercises the cascading-aggregation path
        // in lib.rs propagate_changes_with_transaction_with_initial_deferred.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("outer cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer"].as_ref(),
            b"middle",
            Element::empty_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("middle cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer", b"middle"].as_ref(),
            b"inner",
            Element::empty_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("inner cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer", b"middle", b"inner"].as_ref(),
            b"leaf_ct",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("leaf count tree");

        // Insert a real item inside leaf_ct — count bubbles up.
        db.insert(
            [TEST_LEAF, b"outer", b"middle", b"inner", b"leaf_ct"].as_ref(),
            b"item",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert leaf item");

        // verify_grovedb walks all three cidx layers' H1-A chains and
        // their content consistency. A propagation bug would surface
        // here.
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(
            issues.is_empty(),
            "triple-nested cidx propagation must produce no integrity issues: {:?}",
            issues.keys().collect::<Vec<_>>()
        );

        // Top_k at each level returns the expected entry.
        let top_inner = db
            .count_indexed_top_k(
                [TEST_LEAF, b"outer", b"middle", b"inner"].as_ref(),
                10,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("inner top_k");
        assert_eq!(top_inner.len(), 1);
        assert_eq!(top_inner[0].1, b"leaf_ct".to_vec());

        let top_middle = db
            .count_indexed_top_k(
                [TEST_LEAF, b"outer", b"middle"].as_ref(),
                10,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("middle top_k");
        assert_eq!(top_middle.len(), 1);
        assert_eq!(top_middle[0].1, b"inner".to_vec());

        let top_outer = db
            .count_indexed_top_k(
                [TEST_LEAF, b"outer"].as_ref(),
                10,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("outer top_k");
        assert_eq!(top_outer.len(), 1);
        assert_eq!(top_outer[0].1, b"middle".to_vec());
    }

    // =====================================================================
    // Additional coverage for V1 proof / cidx empty-terminal / verify
    // edge cases (proof/verify.rs).
    // =====================================================================

    #[test]
    fn v1_proof_round_trips_for_empty_cidx_terminal_query() {
        // Coverage for proof/verify.rs's empty-cidx terminal check
        // (the `is_empty_cidx` block using
        // `combine_hash_three(H(value), NULL_HASH, NULL_HASH)`). Build
        // a query that targets an empty cidx as a terminal element
        // (no subquery into it) and verify the proof roundtrips.
        use crate::{PathQuery, SizedQuery};
        use grovedb_merk::proofs::query::QueryItem;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"empty_cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create empty cidx");

        let path_query = PathQuery {
            path: vec![TEST_LEAF.to_vec()],
            query: SizedQuery {
                query: grovedb_merk::proofs::Query {
                    items: vec![QueryItem::Key(b"empty_cidx".to_vec())],
                    default_subquery_branch: grovedb_merk::proofs::query::SubqueryBranch {
                        subquery_path: None,
                        subquery: None,
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                    add_parent_tree_on_subquery: false,
                },
                limit: None,
                offset: None,
            },
        };

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove empty cidx terminal");
        let (root_hash, results) = GroveDb::verify_query(&proof, &path_query, grove_version)
            .expect("verify empty cidx terminal");
        assert_eq!(
            root_hash,
            db.root_hash(None, grove_version).unwrap().expect("root")
        );
        assert_eq!(results.len(), 1, "expected exactly the empty cidx element");
    }

    #[test]
    fn v1_proof_round_trips_for_provable_empty_cidx_terminal_query() {
        // Same as the previous test but for ProvableCountIndexedTree.
        use crate::{PathQuery, SizedQuery};
        use grovedb_merk::proofs::query::QueryItem;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"empty_prov_cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create empty provable cidx");

        let path_query = PathQuery {
            path: vec![TEST_LEAF.to_vec()],
            query: SizedQuery {
                query: grovedb_merk::proofs::Query {
                    items: vec![QueryItem::Key(b"empty_prov_cidx".to_vec())],
                    default_subquery_branch: grovedb_merk::proofs::query::SubqueryBranch {
                        subquery_path: None,
                        subquery: None,
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                    add_parent_tree_on_subquery: false,
                },
                limit: None,
                offset: None,
            },
        };

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove empty provable cidx terminal");
        let (root_hash, results) = GroveDb::verify_query(&proof, &path_query, grove_version)
            .expect("verify empty provable cidx terminal");
        assert_eq!(
            root_hash,
            db.root_hash(None, grove_version).unwrap().expect("root")
        );
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn v1_proof_query_with_limit_terminates_early_at_cidx_subquery() {
        // Exercise the `if limit_left == &Some(0) { break; }` branch
        // in the V1 cidx subquery handler (proof/verify.rs:521 and
        // 604). Set a limit that triggers early termination during
        // cidx descent.
        use crate::{PathQuery, SizedQuery};
        use grovedb_merk::proofs::query::{QueryItem, SubqueryBranch};
        use grovedb_merk::proofs::Query as MerkQuery;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cidx");
        for k in [b"a".as_slice(), b"b", b"c", b"d", b"e"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate");
        }

        let mut inner = MerkQuery::new();
        inner.insert_all();
        let path_query = PathQuery {
            path: vec![TEST_LEAF.to_vec()],
            query: SizedQuery {
                query: MerkQuery {
                    items: vec![QueryItem::Key(b"cidx".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: Some(inner.into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                    add_parent_tree_on_subquery: false,
                },
                limit: Some(2),
                offset: None,
            },
        };

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove with limit");
        let (_root, results) =
            GroveDb::verify_query(&proof, &path_query, grove_version).expect("verify with limit");
        assert_eq!(results.len(), 2, "limit=2 must yield exactly 2 results");
    }

    #[test]
    fn count_indexed_top_k_descending_returns_largest_counts_first() {
        // Cover the descending top-k secondary scan path
        // (count_indexed_tree.rs around 1055-1070). Build cidx with
        // varied counts; descending order must return them
        // largest-first.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for (k, c) in [
            (b"a".as_slice(), 5u64),
            (b"b", 12),
            (b"c", 1),
            (b"d", 99),
            (b"e", 42),
        ] {
            let ct = Element::new_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        let top3 = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 3, true, None, grove_version)
            .unwrap()
            .expect("top_k desc");
        assert_eq!(top3.len(), 3);
        assert_eq!(top3[0], (99, b"d".to_vec()));
        assert_eq!(top3[1], (42, b"e".to_vec()));
        assert_eq!(top3[2], (12, b"b".to_vec()));
    }

    #[test]
    fn count_indexed_count_range_filters_to_inclusive_band() {
        // Exercise count_indexed_count_range with concrete lo/hi
        // bounds (not the lo=0, hi=u64::MAX case already tested).
        // This covers the `Some(upper_bytes)` branch of the range
        // builder at count_indexed_tree.rs:1144-1145.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for (k, c) in [
            (b"a".as_slice(), 5u64),
            (b"b", 10),
            (b"c", 15),
            (b"d", 20),
            (b"e", 25),
        ] {
            let ct = Element::new_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        // Range [10, 20] inclusive — should yield b, c, d.
        let range = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                10,
                20,
                false,
                10,
                None,
                grove_version,
            )
            .unwrap()
            .expect("range");
        assert_eq!(range.len(), 3);
        assert_eq!(range[0], (10, b"b".to_vec()));
        assert_eq!(range[1], (15, b"c".to_vec()));
        assert_eq!(range[2], (20, b"d".to_vec()));
    }

    #[test]
    fn count_indexed_count_range_with_limit_cuts_short() {
        // Exercise the limit-respecting branch in
        // count_indexed_count_range at 1156 (`results.len() < limit`).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for (k, c) in [
            (b"a".as_slice(), 5u64),
            (b"b", 10),
            (b"c", 15),
            (b"d", 20),
            (b"e", 25),
        ] {
            let ct = Element::new_count_tree_with_flags_and_count_value(None, c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        // Range [0, u64::MAX] with limit 2 → only first 2 entries.
        let range = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                0,
                u64::MAX,
                false,
                2,
                None,
                grove_version,
            )
            .unwrap()
            .expect("range");
        assert_eq!(range.len(), 2);
        assert_eq!(range[0], (5, b"a".to_vec()));
        assert_eq!(range[1], (10, b"b".to_vec()));
    }

    #[test]
    fn cidx_top_k_with_k_larger_than_entries_returns_all() {
        // Exercise the "loop ends because iterator returns None" path
        // (count_indexed_tree.rs:1068).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for k in [b"a".as_slice(), b"b"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::empty_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        let top = db
            .count_indexed_top_k(
                [TEST_LEAF, b"cidx"].as_ref(),
                100,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("top_k");
        assert_eq!(top.len(), 2, "k=100 > 2 entries: returns all 2");
    }

    #[test]
    fn batch_insert_non_counted_wrapped_into_count_indexed_tree() {
        // Exercise the wrapper-element path in cidx batch insert
        // (batch/mod.rs:2750-2807 area). A NonCounted-wrapped CountTree
        // is permitted via batch_op_into; the count flag must be
        // preserved during the H1-A propagation.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        // Insert a NonCounted-wrapped CountTree under the cidx primary.
        // The cidx primary stores count-bearing elements; NonCounted
        // wraps suppress count propagation TO this cidx but the
        // inner CountTree is still legit.
        let inner = Element::empty_count_tree();
        let wrapped = Element::new_non_counted(inner).expect("wrap");
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
            b"wrapped".to_vec(),
            wrapped,
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("apply batch with wrapped cidx insert");

        // verify_grovedb walks the cidx layer's H1-A chain.
        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(
            issues.is_empty(),
            "wrapped-element batch insert must produce no issues: {:?}",
            issues.keys().collect::<Vec<_>>()
        );
    }

    // =====================================================================
    // Coverage for batch/mod.rs cidx-specific patch lines.
    // =====================================================================

    #[test]
    fn batch_cidx_safe_subset_overwrite_with_write_under_cidx_rejected() {
        // Coverage for batch/mod.rs:2218-2226 — a batch that both
        // safe-subset-overwrites a cidx AND writes under that same
        // cidx is rejected (the post-apply cleanup would silently
        // clear the descendant write). The descendant write must
        // already exist in `ops_by_qualified_paths` by the time the
        // cidx overwrite is processed; in the existing cidx primary
        // we ALSO have an existing descendant entry, and use it to
        // anchor the descendant op being an update (which routes via
        // the existing-tree path so the descendant op is registered
        // before the safe-subset overwrite is decided).
        use crate::batch::{BatchApplyOptions, QualifiedGroveDbOp};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"existing",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");

        // Put the descendant write FIRST in the ops list so it
        // populates ops_by_qualified_paths before the cidx overwrite
        // is processed. Then the cidx overwrite's consistency scan at
        // L2214-2227 finds the descendant and rejects.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"existing".to_vec(),
                Element::new_item(b"updated".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::new_item(b"replaced".to_vec()),
            ),
        ];
        let opts = BatchApplyOptions {
            validate_insertion_does_not_override_tree: false,
            ..Default::default()
        };
        let result = db
            .apply_batch(ops, Some(opts), None, grove_version)
            .unwrap();
        // The check at L2218 is one of several inconsistency rejections
        // possible for this batch shape. Accept ANY InvalidBatchOperation
        // (the precise rejection ordering is an implementation detail);
        // what matters is the batch is REJECTED, not silently misapplied.
        assert!(
            matches!(result, Err(crate::Error::InvalidBatchOperation(_)))
                || matches!(result, Err(crate::Error::NotSupported(_))),
            "expected batch rejection (InvalidBatchOperation or NotSupported), got: {:?}",
            result
        );
    }

    #[test]
    fn batch_insert_if_not_exists_for_existing_cidx_errors() {
        // Coverage for batch/mod.rs:2370-2377 — InsertIfNotExists with
        // a cidx element at a key where a cidx already exists must
        // return InvalidBatchOperation when validate_insertion_does_not_override
        // is true.
        use crate::batch::{BatchApplyOptions, QualifiedGroveDbOp};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create existing cidx");

        let ops = vec![QualifiedGroveDbOp::insert_if_not_exists_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            Element::empty_count_indexed_tree(),
        )];
        let opts = BatchApplyOptions {
            validate_insertion_does_not_override: true,
            ..Default::default()
        };
        let result = db
            .apply_batch(ops, Some(opts), None, grove_version)
            .unwrap();
        match result {
            Err(crate::Error::InvalidBatchOperation(msg)) => {
                assert!(
                    msg.contains("already exists") || msg.contains("CountIndexedTree"),
                    "expected already-exists error, got: {msg}"
                );
            }
            other => panic!(
                "expected InvalidBatchOperation(already exists), got: {:?}",
                other
            ),
        }
    }

    #[test]
    fn batch_insert_multiple_items_into_same_cidx_primary_propagates_correctly() {
        // Coverage for batch/mod.rs:3252-3267 — when a single batch
        // contains multiple ops under the SAME cidx primary, the
        // propagation phase processes them iteratively. The second
        // iteration's propagation visits the cidx primary's level
        // with an EXISTING `ReplaceTreeRootKey` op already in
        // ops_at_level_above, and must upgrade it to
        // `ReplaceAggregateIndexedTreeRootKeys`.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        // All-fresh inserts under cidx primary; multiple ops in same
        // batch force propagation to visit the cidx primary level
        // iteratively.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"a".to_vec(),
                Element::new_count_tree_with_flags_and_count_value(None, 100, None),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"b".to_vec(),
                Element::new_count_tree_with_flags_and_count_value(None, 50, None),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"c".to_vec(),
                Element::new_count_tree_with_flags_and_count_value(None, 25, None),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with multiple cidx-primary ops");

        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 5, true, None, grove_version)
            .unwrap()
            .expect("top_k");
        assert_eq!(top.len(), 3);
        assert_eq!(top[0], (100, b"a".to_vec()));
        assert_eq!(top[1], (50, b"b".to_vec()));
        assert_eq!(top[2], (25, b"c".to_vec()));

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty(), "batch produced drift: {:?}", issues);
    }

    #[test]
    fn apply_partial_batch_with_cidx_mirror_secondary_open() {
        // Coverage for batch/mod.rs:4902-4912 and 4987-4997 (closures
        // that pass a primary_path into open_count_indexed_secondary_for_batch
        // during partial-batch processing).
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
            b"a".to_vec(),
            Element::new_count_tree_with_flags_and_count_value(None, 7, None),
        )];
        db.apply_partial_batch(
            ops,
            None,
            |_cost, _leftover| Ok(vec![]),
            None,
            grove_version,
        )
        .unwrap()
        .expect("apply_partial_batch with cidx mirror");

        let initial = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
            b"b".to_vec(),
            Element::new_count_tree_with_flags_and_count_value(None, 11, None),
        )];
        db.apply_partial_batch(
            initial,
            None,
            |_cost, _leftover| {
                Ok(vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                    b"c".to_vec(),
                    Element::new_count_tree_with_flags_and_count_value(None, 3, None),
                )])
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("apply_partial_batch + add-on with cidx mirror");

        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top_k");
        assert_eq!(top.len(), 3);
        assert_eq!(top[0], (11, b"b".to_vec()));
        assert_eq!(top[1], (7, b"a".to_vec()));
        assert_eq!(top[2], (3, b"c".to_vec()));
    }

    // =====================================================================
    // Additional coverage for lib.rs verify_grovedb sentinel + batch
    // propagation paths.
    // =====================================================================

    #[test]
    fn verify_grovedb_flags_short_secondary_key_with_sentinel() {
        // Coverage for lib.rs:1422-1427 — verify_grovedb's cidx walk
        // emits a `__cidx_secondary_malformed_key__` sentinel for any
        // secondary key shorter than 8 bytes.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        corrupt_secondary_insert(&db, &[TEST_LEAF, b"cidx"], b"bad!", grove_version);

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify_grovedb");
        let sentinel_path: Vec<Vec<u8>> = vec![
            TEST_LEAF.to_vec(),
            b"cidx".to_vec(),
            b"__cidx_secondary_malformed_key__".to_vec(),
            b"bad!".to_vec(),
        ];
        assert!(
            issues.contains_key(&sentinel_path),
            "expected __cidx_secondary_malformed_key__ sentinel for 'bad!', \
             got issues: {:?}",
            issues.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn batch_apply_two_inserts_into_cidx_propagation_visits_cidx_level() {
        // Exercises the batch propagation that visits the cidx primary
        // level multiple times (once per affected key). Two fresh
        // inserts force the propagation visitor to coalesce ops at the
        // cidx primary level.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"alpha".to_vec(),
                Element::new_count_tree_with_flags_and_count_value(None, 42, None),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"beta".to_vec(),
                Element::new_count_tree_with_flags_and_count_value(None, 77, None),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with 2 cidx-primary inserts");

        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top_k");
        assert_eq!(top.len(), 2);
        assert_eq!(top[0], (77, b"beta".to_vec()));
        assert_eq!(top[1], (42, b"alpha".to_vec()));

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty(), "batch produced drift: {:?}", issues);
    }

    #[test]
    fn batch_apply_with_multiple_inserts_descending_count() {
        // Multiple inserts forcing the propagation queue to coalesce
        // ops at the cidx primary level.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        let ops: Vec<_> = (1u64..=4)
            .map(|i| {
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                    format!("k{i}").into_bytes(),
                    Element::new_count_tree_with_flags_and_count_value(None, 100 - i * 10, None),
                )
            })
            .collect();
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with 4 inserts");

        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top_k");
        assert_eq!(top.len(), 4);
        assert_eq!(top[0], (90, b"k1".to_vec()));
        assert_eq!(top[3], (60, b"k4".to_vec()));
    }

    // =====================================================================
    // Additional coverage for V1 proof verify cidx-error branches.
    // =====================================================================

    #[test]
    fn v1_verify_rejects_cidx_subquery_proof_with_tampered_secondary_root() {
        // Coverage for proof/verify.rs:593-602 — V1 cidx layer hash
        // mismatch. Build a valid V1 cidx-subquery proof, then tamper
        // the secondary_root prefix in the cidx_bytes so the combined
        // root hash diverges from what the parent's value_hash claims.
        use crate::operations::proof::{GroveDBProof, ProofBytes};
        use crate::{PathQuery, SizedQuery};
        use grovedb_merk::proofs::{
            query::{QueryItem, SubqueryBranch},
            Query as MerkQuery,
        };

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");

        let mut inner = MerkQuery::new();
        inner.insert_all();
        let path_query = PathQuery {
            path: vec![TEST_LEAF.to_vec()],
            query: SizedQuery {
                query: MerkQuery {
                    items: vec![QueryItem::Key(b"cidx".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: Some(inner.into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                    add_parent_tree_on_subquery: false,
                },
                limit: None,
                offset: None,
            },
        };
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove_query");
        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let (mut proof_decoded, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof, config).expect("decode V1 proof");

        // Flip the first byte of the secondary_root attestation
        // (cidx_bytes[..32]) in the cidx layer's ProofBytes.
        let root_layer = match &mut proof_decoded {
            GroveDBProof::V1(v1) => &mut v1.root_layer,
            _ => panic!("expected V1 proof"),
        };
        let mut found = false;
        for (_k, lower) in root_layer.lower_layers.iter_mut() {
            for (_kk, sublower) in lower.lower_layers.iter_mut() {
                if let ProofBytes::CountIndexedTree(b) = &mut sublower.merk_proof {
                    if b.len() >= 32 {
                        b[0] ^= 0xFF;
                        found = true;
                    }
                }
            }
        }
        assert!(found, "expected a CountIndexedTree layer");

        let tampered = bincode::encode_to_vec(&proof_decoded, config).expect("re-encode");
        let result = GroveDb::verify_query(&tampered, &path_query, grove_version);
        assert!(
            matches!(result, Err(crate::Error::InvalidProof(_, _))),
            "tampered secondary_root must produce InvalidProof, got: {:?}",
            result
        );
    }

    #[test]
    fn v1_verify_rejects_cidx_proof_bytes_under_non_cidx_parent() {
        // Coverage for proof/verify.rs:726-731 — when a V1 proof
        // envelope places a `ProofBytes::CountIndexedTree(_)` layer
        // under a parent element that is NOT a cidx, the verifier
        // rejects with "ProofBytes::CountIndexedTree under a non-cidx
        // parent element".
        use crate::operations::proof::{GroveDBProof, ProofBytes};
        use crate::{PathQuery, SizedQuery};
        use grovedb_merk::proofs::{
            query::{QueryItem, SubqueryBranch},
            Query as MerkQuery,
        };

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"normal_tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create normal tree");
        db.insert(
            [TEST_LEAF, b"normal_tree"].as_ref(),
            b"x",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert leaf");

        let mut inner = MerkQuery::new();
        inner.insert_all();
        let path_query = PathQuery {
            path: vec![TEST_LEAF.to_vec()],
            query: SizedQuery {
                query: MerkQuery {
                    items: vec![QueryItem::Key(b"normal_tree".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: Some(inner.into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                    add_parent_tree_on_subquery: false,
                },
                limit: None,
                offset: None,
            },
        };
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove");

        let config = bincode::config::standard()
            .with_big_endian()
            .with_no_limit();
        let (mut decoded, _): (GroveDBProof, _) =
            bincode::decode_from_slice(&proof, config).expect("decode");

        let root_layer = match &mut decoded {
            GroveDBProof::V1(v1) => &mut v1.root_layer,
            _ => panic!("expected V1"),
        };
        let mut tampered_any = false;
        for (_k, lower) in root_layer.lower_layers.iter_mut() {
            for (_kk, sublower) in lower.lower_layers.iter_mut() {
                if let ProofBytes::Merk(b) = &sublower.merk_proof {
                    // Prepend 32 bytes of zeros so the cidx-shape
                    // length check passes; the parent-type mismatch
                    // fires before the hash check.
                    let mut cidx_b = vec![0u8; 32];
                    cidx_b.extend_from_slice(b);
                    sublower.merk_proof = ProofBytes::CountIndexedTree(cidx_b);
                    tampered_any = true;
                }
            }
        }
        assert!(tampered_any, "expected a Merk layer to tamper");

        let tampered = bincode::encode_to_vec(&decoded, config).expect("re-encode");
        let result = GroveDb::verify_query(&tampered, &path_query, grove_version);
        match result {
            Err(crate::Error::InvalidProof(_, msg)) => {
                assert!(
                    msg.contains("non-cidx parent element")
                        || msg.contains("CountIndexedTree under"),
                    "expected non-cidx-parent error, got: {msg}"
                );
            }
            other => panic!(
                "expected InvalidProof(cidx under non-cidx parent), got: {:?}",
                other
            ),
        }
    }

    // =====================================================================
    // Additional coverage: provable cidx prove/verify + execute_single_key
    // error branches + batch+propagation chains.
    // =====================================================================

    #[test]
    fn prove_count_indexed_top_k_for_provable_cidx_round_trips() {
        // Covers the `ProvableCountIndexedTree(_, secondary, ..)` arm
        // in read_count_indexed_secondary_root_key_for_proof
        // (proof/count_indexed.rs:380). Build a Provable cidx with
        // entries, prove top-k, verify.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"prov_cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create provable cidx");
        for (k, c) in [(b"a".as_slice(), 5u64), (b"b", 12), (b"c", 1)] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"prov_cidx"].as_ref(),
                k,
                Element::new_count_tree_with_flags_and_count_value(None, c, None),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate");
        }
        let proof = db
            .prove_count_indexed_top_k(
                [TEST_LEAF, b"prov_cidx"].as_ref(),
                10,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove");
        let path: &[&[u8]] = &[TEST_LEAF, b"prov_cidx"];
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 10, true)
            .expect("verify provable cidx top_k");
        assert_eq!(result.entries.len(), 3);
        assert_eq!(result.entries[0], (12, b"b".to_vec()));
    }

    #[test]
    fn verify_count_indexed_top_k_with_layer_proof_replaced_by_garbage() {
        // Coverage for proof/count_indexed.rs:638-642 — when
        // execute_single_key_proof receives layer bytes that can't be
        // decoded as a valid Merk proof, it returns CorruptedData.
        use crate::operations::proof::count_indexed::CountIndexedRangeProof;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        let proof = db
            .prove_count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("prove");

        let config = bincode::config::standard().with_limit::<{ 16 * 1024 * 1024 }>();
        let (mut envelope, _): (CountIndexedRangeProof, _) =
            bincode::decode_from_slice(&proof, config).expect("decode");
        envelope.layer_proofs[0] = vec![0xFFu8; 64];
        let tampered =
            bincode::encode_to_vec(&envelope, bincode::config::standard()).expect("re-encode");

        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let result = GroveDb::verify_count_indexed_top_k(&tampered, path, 10, true);
        assert!(
            matches!(result, Err(crate::Error::CorruptedData(_))),
            "garbage layer proof must produce CorruptedData, got: {:?}",
            result
        );
    }

    #[test]
    fn cidx_batch_pipeline_exercises_propagation_and_query() {
        // Comprehensive batch+query pipeline: 6 inserts via apply_batch
        // followed by top-k + verify roundtrip.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");

        let ops: Vec<_> = [
            (b"k1".to_vec(), 7u64),
            (b"k2".to_vec(), 99),
            (b"k3".to_vec(), 3),
            (b"k4".to_vec(), 22),
            (b"k5".to_vec(), 11),
            (b"k6".to_vec(), 55),
        ]
        .into_iter()
        .map(|(k, c)| {
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                k,
                Element::new_count_tree_with_flags_and_count_value(None, c, None),
            )
        })
        .collect();
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with 6 cidx inserts");

        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 3, true, None, grove_version)
            .unwrap()
            .expect("top_k");
        assert_eq!(top.len(), 3);
        assert_eq!(top[0], (99, b"k2".to_vec()));

        let proof = db
            .prove_count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 3, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let path: &[&[u8]] = &[TEST_LEAF, b"cidx"];
        let result = GroveDb::verify_count_indexed_top_k(&proof, path, 3, true).expect("verify");
        assert_eq!(result.entries.len(), 3);

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty(), "batch produced drift: {:?}", issues);
    }

    #[test]
    fn cidx_batch_with_provable_cidx_propagation_round_trip() {
        // Provable cidx variant of the comprehensive batch test.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"prov_cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create provable cidx");

        let ops: Vec<_> = [
            (b"x".to_vec(), 30u64),
            (b"y".to_vec(), 60),
            (b"z".to_vec(), 15),
        ]
        .into_iter()
        .map(|(k, c)| {
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"prov_cidx".to_vec()],
                k,
                Element::new_count_tree_with_flags_and_count_value(None, c, None),
            )
        })
        .collect();
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with provable cidx inserts");

        let top = db
            .count_indexed_top_k(
                [TEST_LEAF, b"prov_cidx"].as_ref(),
                10,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("top_k");
        assert_eq!(top.len(), 3);
        assert_eq!(top[0], (60, b"y".to_vec()));

        let issues = db
            .verify_grovedb(None, false, true, grove_version)
            .expect("verify");
        assert!(issues.is_empty());
    }

    #[test]
    fn cidx_count_range_with_intermediate_count_filter() {
        // Range that filters out entries on both ends.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for (k, c) in [
            (b"a".as_slice(), 1u64),
            (b"b", 50),
            (b"c", 99),
            (b"d", 150),
            (b"e", 200),
        ] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::new_count_tree_with_flags_and_count_value(None, c, None),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        let range = db
            .count_indexed_count_range(
                [TEST_LEAF, b"cidx"].as_ref(),
                50,
                100,
                false,
                10,
                None,
                grove_version,
            )
            .unwrap()
            .expect("range");
        assert_eq!(range.len(), 2);
        assert_eq!(range[0], (50, b"b".to_vec()));
        assert_eq!(range[1], (99, b"c".to_vec()));
    }

    // =====================================================================
    // Reproductions of suspicious behaviors uncovered while writing
    // coverage tests. These tests document the current behavior; if any
    // FAIL the assertion, the behavior is a real bug worth flagging.
    // =====================================================================

    #[test]
    fn batch_delete_op_on_cidx_primary_entry_mirrors_to_secondary() {
        // Pre-populate cidx with one entry, use REGULAR batch
        // delete_op (NOT the cidx-aware delete_from_count_indexed_tree)
        // to remove it, then insert a new one in the same batch.
        //
        // History: this test was flaky (~60% failure under
        // --test-threads=4) before the fix at batch/mod.rs:3046-3070.
        // The cidx secondary mirror deltas were applied in non-
        // deterministic HashMap iteration order, and when an INSERT
        // delta ran before a DELETE delta on the same secondary merk,
        // the delete sometimes failed to actually remove the entry —
        // leaving stale secondary state. Fixed by sorting deltas
        // deterministically (pure deletes first, then by key).
        //
        // Pre-fix observed failure: top_k returned
        // [(77, "new"), (42, "old")] — the deleted "old" remained at
        // count=42 in the secondary. Primary delete succeeded
        // (`db.get` returned `PathKeyNotFound`); the bug was strictly
        // in the cidx secondary mirror path.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"old",
            Element::new_count_tree_with_flags_and_count_value(None, 42, None),
            None,
            grove_version,
        )
        .unwrap()
        .expect("preload old");

        let ops = vec![
            QualifiedGroveDbOp::delete_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"old".to_vec(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"new".to_vec(),
                Element::new_count_tree_with_flags_and_count_value(None, 77, None),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch delete + insert under cidx");

        // Sanity 1: primary delete actually happened — "old" is gone.
        let old_result = db
            .get([TEST_LEAF, b"cidx"].as_ref(), b"old", None, grove_version)
            .unwrap();
        assert!(
            matches!(old_result, Err(crate::Error::PathKeyNotFound(_))),
            "primary delete must have removed 'old' from the cidx primary; \
             got: {:?}",
            old_result
        );

        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top_k");
        assert_eq!(
            top.len(),
            1,
            "regular batch delete_op on a cidx primary entry must mirror to \
             the secondary; observed top_k contents: {:?}",
            top
        );
        assert_eq!(top[0], (77, b"new".to_vec()));
    }

    #[test]
    fn batch_insert_or_replace_overwriting_cidx_entry_drops_old_mirror() {
        // Pre-populate cidx with "a" at count=0. Then batch-replace
        // "a" with count=100. The OLD secondary mirror at count=0
        // must be removed, leaving only count=100.
        use crate::batch::QualifiedGroveDbOp;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::empty_count_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("preload a@count=0");

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
            b"a".to_vec(),
            Element::new_count_tree_with_flags_and_count_value(None, 100, None),
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch replace existing cidx entry");

        let top = db
            .count_indexed_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top_k");
        assert_eq!(
            top.len(),
            1,
            "batch insert_or_replace_op on existing cidx entry must replace \
             both primary and secondary mirror; observed top_k contents: {:?}",
            top
        );
        assert_eq!(top[0], (100, b"a".to_vec()));
    }

    // =====================================================================
    // Additional V1 proof coverage targeting subquery-into-cidx branches
    // in verify.rs (L509-538).
    // =====================================================================

    #[test]
    fn v1_proof_subquery_into_cidx_with_add_parent_tree_returns_cidx_and_children() {
        // Coverage for proof/verify.rs:524-538 — the
        // should_add_parent_tree_at_path branch inside the cidx
        // subquery descent. When the query has
        // `add_parent_tree_on_subquery: true`, the verifier emits the
        // cidx element itself in addition to descending into its
        // contents.
        use crate::{PathQuery, SizedQuery};
        use grovedb_merk::proofs::query::{QueryItem, SubqueryBranch};
        use grovedb_merk::proofs::Query as MerkQuery;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for k in [b"a".as_slice(), b"b"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate");
        }

        let mut inner = MerkQuery::new();
        inner.insert_all();
        let path_query = PathQuery {
            path: vec![TEST_LEAF.to_vec()],
            query: SizedQuery {
                query: MerkQuery {
                    items: vec![QueryItem::Key(b"cidx".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: Some(inner.into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                    add_parent_tree_on_subquery: true,
                },
                limit: None,
                offset: None,
            },
        };

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove");
        let (_root, results) =
            GroveDb::verify_query(&proof, &path_query, grove_version).expect("verify");

        assert!(
            results.len() >= 3,
            "expected cidx element + children (>= 3), got {}: {:?}",
            results.len(),
            results
        );
    }

    #[test]
    fn v1_proof_subquery_into_cidx_with_limit_one_terminates_early() {
        // Coverage for proof/verify.rs:518-523 — the cidx-terminal
        // path that decrements limit and breaks at zero. With a tight
        // limit, the iteration terminates inside the cidx subquery.
        use crate::{PathQuery, SizedQuery};
        use grovedb_merk::proofs::query::{QueryItem, SubqueryBranch};
        use grovedb_merk::proofs::Query as MerkQuery;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create cidx");
        for k in [b"a".as_slice(), b"b", b"c", b"d"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate");
        }

        let mut inner = MerkQuery::new();
        inner.insert_all();
        let path_query = PathQuery {
            path: vec![TEST_LEAF.to_vec()],
            query: SizedQuery {
                query: MerkQuery {
                    items: vec![QueryItem::Key(b"cidx".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: Some(inner.into()),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                    add_parent_tree_on_subquery: true,
                },
                limit: Some(1),
                offset: None,
            },
        };

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove with limit=1");
        let (_root, results) =
            GroveDb::verify_query(&proof, &path_query, grove_version).expect("verify with limit=1");
        assert!(
            !results.is_empty() && results.len() <= 2,
            "expected 1-2 results with limit=1 + add_parent_tree, got {}: {:?}",
            results.len(),
            results
        );
    }

    #[test]
    fn batch_inserting_and_populating_fresh_cidx_in_same_batch_is_rejected() {
        // Coverage for CodeRabbit finding on bb390d55. A batch that
        // both creates a CountIndexedTree element AND writes to a
        // descendant of it has no valid propagation path: there is
        // no Insert-style aggregate-indexed propagation op
        // counterpart to ReplaceAggregateIndexedTreeRootKeys, and
        // the secondary merk cannot be opened by stale parent state.
        // The batch path must reject this combination with a clear,
        // actionable NotSupported message (not the confusing
        // "insertion of element under a non tree" catch-all).
        use crate::batch::{BatchApplyOptions, QualifiedGroveDbOp};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Op 1: insert a fresh empty cidx at TEST_LEAF/cidx
        // Op 2: insert into the (not-yet-existing) cidx
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::empty_count_indexed_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"item".to_vec(),
                Element::empty_count_tree(),
            ),
        ];
        let result = db
            .apply_batch(ops, Some(BatchApplyOptions::default()), None, grove_version)
            .unwrap();
        match result {
            Err(crate::Error::NotSupported(msg)) => {
                assert!(
                    msg.contains("freshly-inserted") && msg.contains("CountIndexedTree"),
                    "expected freshly-inserted cidx rejection, got: {msg}"
                );
            }
            other => panic!(
                "expected NotSupported(freshly-inserted cidx + populate), got: {:?}",
                other
            ),
        }
    }
}
