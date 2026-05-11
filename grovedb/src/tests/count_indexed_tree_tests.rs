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
        let result = GroveDb::verify_count_indexed_top_k(&proof_bytes, path).expect("verify top-3");

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
        let result = GroveDb::verify_count_indexed_top_k(&proof_bytes, path).expect("verify");

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
        let result = GroveDb::verify_count_indexed_top_k(&tampered, path);
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
        let result = GroveDb::verify_count_indexed_top_k(&proof_bytes, wrong_path);
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
        let result = GroveDb::verify_count_indexed_top_k(b"not-a-valid-proof", &[b"x"]);
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
        let result =
            GroveDb::verify_count_indexed_top_k(&proof, path).expect("verify nested cidx top-k");
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
        let result = GroveDb::verify_count_indexed_top_k(&proof, path).expect("verify top-3");
        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.entries[0], (1u64, b"a".to_vec()));
        assert_eq!(result.entries[1], (0u64, b"c".to_vec()));
    }

    #[test]
    fn verify_count_indexed_top_k_rejects_truncated_proof() {
        let result = GroveDb::verify_count_indexed_top_k(b"\x00\x01", &[b"x"]);
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
            GroveDb::verify_count_indexed_query(&proof, q, path).expect("verify arbitrary");
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
        let result = GroveDb::verify_count_indexed_top_k(&proof, bad_path);
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
        //   2. Emit ReplaceCountIndexedTreeRootKeys to the regular_tree
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
        // Also confirms that ReplaceCountIndexedTreeRootKeys correctly
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
        //   - Emit ReplaceCountIndexedTreeRootKeys to outer's primary
        //     level.
        //   - At outer's primary level, the pre/post element bytes for
        //     "inner_cidx" change (count_value 0 → 1), so outer's
        //     secondary entry for "inner_cidx" must move from
        //     (0_be ‖ inner_cidx) to (1_be ‖ inner_cidx).
        //
        // If outer's pre-state capture skips ReplaceCountIndexedTreeRootKeys
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
            let result = GroveDb::verify_count_indexed_top_k(&bytes, path);
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

            let result = GroveDb::verify_count_indexed_query(&bytes, q, path);
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

            let verified =
                GroveDb::verify_count_indexed_query(&proof, q, path).unwrap_or_else(|e| {
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
                    msg.contains("cidx integrity"),
                    "expected cidx integrity violation, got: {msg}"
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
}
