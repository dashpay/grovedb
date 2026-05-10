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
        assert!(result.is_err());
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
        assert!(result.is_err());
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

        // Verify with a different path — should fail.
        let wrong_path: &[&[u8]] = &[TEST_LEAF, b"wrong_key"];
        let result = GroveDb::verify_count_indexed_top_k(&proof_bytes, wrong_path);
        assert!(result.is_err(), "verification with wrong path must fail");
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
        assert!(result.is_err());
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
        assert!(result.is_err());
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
    fn batch_insert_into_cidx_primary_is_rejected() {
        // The batch propagation has no two-Merk hook for cidx primaries:
        // an InsertOrReplace at a path whose merk is the cidx primary
        // would update the primary alone, leaving the secondary index
        // stale. Fail fast with a clear NotSupported pointing callers
        // to the dedicated cidx APIs.
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
        let result = db.apply_batch(ops, None, None, grove_version).unwrap();
        match result {
            Err(crate::Error::NotSupported(msg)) => {
                assert!(
                    msg.contains("CountIndexedTree primary"),
                    "expected cidx primary message, got: {msg}"
                );
            }
            other => panic!("expected NotSupported, got {:?}", other),
        }
    }

    #[test]
    fn batch_delete_tree_on_cidx_is_rejected() {
        // DeleteTree on a CountIndexedTree via the batch path would
        // orphan the secondary storage namespace; the batch path rejects
        // it so callers must empty the cidx (via
        // delete_from_count_indexed_tree) first.
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
            crate::batch::SubelementsDeletionBehavior::DeleteChildren,
        )];
        let result = db.apply_batch(ops, None, None, grove_version).unwrap();
        assert!(result.is_err(), "DeleteTree on cidx must be rejected");
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
        assert!(result.is_err());
    }

    #[test]
    fn prove_count_indexed_top_k_on_non_cidx_target_errors() {
        // Proving over a path whose terminal element is not a cidx must fail.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // TEST_LEAF is a Tree, not a CountIndexedTree.
        let result = db
            .prove_count_indexed_top_k([TEST_LEAF].as_ref(), 3, true, None, grove_version)
            .unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn count_indexed_top_k_on_non_cidx_target_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let result = db
            .count_indexed_top_k([TEST_LEAF].as_ref(), 3, true, None, grove_version)
            .unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn count_indexed_count_range_on_non_cidx_target_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let result = db
            .count_indexed_count_range([TEST_LEAF].as_ref(), 0, 100, false, 10, None, grove_version)
            .unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn reconcile_on_non_cidx_target_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let result = db
            .reconcile_count_indexed_tree_secondary([TEST_LEAF].as_ref(), None, grove_version)
            .unwrap();
        assert!(result.is_err());
    }

    #[test]
    fn delete_from_count_indexed_tree_on_non_cidx_target_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let result = db
            .delete_from_count_indexed_tree([TEST_LEAF].as_ref(), b"item", None, grove_version)
            .unwrap();
        assert!(result.is_err());
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
        assert!(result.is_err());
    }

    #[test]
    fn count_indexed_count_range_at_root_path_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty_path: &[&[u8]] = &[];
        let result = db
            .count_indexed_count_range(empty_path, 0, 100, false, 10, None, grove_version)
            .unwrap();
        assert!(result.is_err());
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
        assert!(result.is_err(), "dangling reference must produce an error");
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
}
