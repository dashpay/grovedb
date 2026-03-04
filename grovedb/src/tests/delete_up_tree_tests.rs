//! Delete up tree operation tests

#[cfg(test)]
mod tests {
    use grovedb_path::SubtreePath;
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::delete::DeleteUpTreeOptions,
        tests::{common::EMPTY_PATH, make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    #[test]
    fn delete_up_tree_while_empty_chain_deletion() {
        // Build a chain of nested single-child trees and verify that
        // delete_up_tree_while_empty removes all ancestors up to the
        // stop_path_height boundary.
        //
        // Structure (all under TEST_LEAF):
        //   TEST_LEAF -> level1 -> level2 -> level3 -> leaf_item
        //
        // With stop_path_height=1, the recursion stops when path.len()==1,
        // meaning the deletion at path=[TEST_LEAF] is skipped.
        // Deleted: leaf_item, level3, level2 (3 ops). level1 is NOT deleted
        // because the recursion to path=[TEST_LEAF] with key=level1 sees
        // path.len()==1==stop_path_height and returns None.

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert the chain: TEST_LEAF/level1/level2/level3 + leaf item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"level1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert level1 tree");

        db.insert(
            [TEST_LEAF, b"level1"].as_ref(),
            b"level2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert level2 tree");

        db.insert(
            [TEST_LEAF, b"level1", b"level2"].as_ref(),
            b"level3",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert level3 tree");

        db.insert(
            [TEST_LEAF, b"level1", b"level2", b"level3"].as_ref(),
            b"leaf_item",
            Element::new_item(b"hello".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert leaf item");

        // Verify the item exists before deletion
        let item = db
            .get(
                [TEST_LEAF, b"level1", b"level2", b"level3"].as_ref(),
                b"leaf_item",
                None,
                grove_version,
            )
            .unwrap()
            .expect("should get leaf item");
        assert_eq!(item, Element::new_item(b"hello".to_vec()));

        // Delete up tree: stop_path_height=1 means stop when path.len()==1.
        // Recursion will delete leaf_item, level3, level2 but stop before level1.
        let deleted_count = db
            .delete_up_tree_while_empty(
                [TEST_LEAF, b"level1", b"level2", b"level3"].as_ref(),
                b"leaf_item",
                &DeleteUpTreeOptions {
                    stop_path_height: Some(1),
                    ..Default::default()
                },
                None,
                grove_version,
            )
            .unwrap()
            .expect("should delete up tree successfully");

        // Deleted: leaf_item + level3 + level2 = 3 ops
        assert_eq!(
            deleted_count, 3,
            "should delete leaf item and two now-empty ancestor trees"
        );

        // level1 should still exist (stop boundary prevented its deletion)
        let level1 = db
            .get([TEST_LEAF].as_ref(), b"level1", None, grove_version)
            .unwrap()
            .expect("level1 should still exist at the stop boundary");
        assert!(matches!(level1, Element::Tree(..)));

        // Verify intermediate paths are gone
        assert!(
            matches!(
                db.get(
                    [TEST_LEAF, b"level1"].as_ref(),
                    b"level2",
                    None,
                    grove_version
                )
                .unwrap(),
                Err(Error::PathKeyNotFound(_))
            ),
            "level2 should have been deleted"
        );

        // TEST_LEAF itself should still exist
        let test_leaf = db
            .get(EMPTY_PATH, TEST_LEAF, None, grove_version)
            .unwrap()
            .expect("TEST_LEAF should still exist");
        assert!(matches!(test_leaf, Element::Tree(..)));
    }

    #[test]
    fn delete_up_tree_stops_at_non_empty_ancestor() {
        // Build nested trees where an intermediate ancestor has two children.
        // Deletion should stop at the non-empty ancestor since removing its
        // one child still leaves it non-empty.
        //
        // Structure under TEST_LEAF:
        //   level1 -> level2_a -> leaf_item
        //          -> level2_b   (second child, keeps level1 non-empty)

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"level1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert level1 tree");

        db.insert(
            [TEST_LEAF, b"level1"].as_ref(),
            b"level2_a",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert level2_a tree");

        db.insert(
            [TEST_LEAF, b"level1"].as_ref(),
            b"level2_b",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert level2_b tree");

        db.insert(
            [TEST_LEAF, b"level1", b"level2_a"].as_ref(),
            b"leaf_item",
            Element::new_item(b"data".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert leaf item");

        // Delete up tree with stop at root
        let deleted_count = db
            .delete_up_tree_while_empty(
                [TEST_LEAF, b"level1", b"level2_a"].as_ref(),
                b"leaf_item",
                &DeleteUpTreeOptions {
                    stop_path_height: Some(0),
                    ..Default::default()
                },
                None,
                grove_version,
            )
            .unwrap()
            .expect("should delete up tree successfully");

        // Should delete: leaf_item and level2_a (now empty), but NOT level1
        // because level1 still has level2_b. That is 2 ops.
        assert_eq!(
            deleted_count, 2,
            "should delete leaf item and level2_a only"
        );

        // level2_a should be gone
        assert!(
            matches!(
                db.get(
                    [TEST_LEAF, b"level1"].as_ref(),
                    b"level2_a",
                    None,
                    grove_version
                )
                .unwrap(),
                Err(Error::PathKeyNotFound(_))
            ),
            "level2_a should have been deleted"
        );

        // level2_b should still be present
        let level2_b = db
            .get(
                [TEST_LEAF, b"level1"].as_ref(),
                b"level2_b",
                None,
                grove_version,
            )
            .unwrap()
            .expect("level2_b should still exist");
        assert!(matches!(level2_b, Element::Tree(..)));

        // level1 should still be present
        let level1 = db
            .get([TEST_LEAF].as_ref(), b"level1", None, grove_version)
            .unwrap()
            .expect("level1 should still exist because it has level2_b");
        assert!(matches!(level1, Element::Tree(..)));
    }

    #[test]
    fn delete_up_tree_stop_path_height_various_levels() {
        // Build a chain: TEST_LEAF -> a -> b -> c -> item
        // Path to item is [TEST_LEAF, a, b, c], length=4.
        //
        // stop_path_height=3: recursion stops when path.len()==3
        //   Delete item (at path len 4), then c (at path len 3 == stop) -> stop
        //   Result: 1 op (just item)
        //
        // Wait, let me trace more carefully:
        //   Call 1: path=[T, a, b, c], key=item. path.len()=4 != 3. Delete item.
        //     Recurse: parent_path=[T, a, b], parent_key=c.
        //   Call 2: path=[T, a, b], key=c. path.len()=3 == 3. Return None. Stop.
        //   Result: 1 op (item only).
        //
        // stop_path_height=2:
        //   Call 1: path=[T, a, b, c], key=item. 4!=2. Delete item.
        //     Recurse: path=[T, a, b], key=c.
        //   Call 2: path=[T, a, b], key=c. 3!=2. Delete c.
        //     Recurse: path=[T, a], key=b.
        //   Call 3: path=[T, a], key=b. 2==2. Return None. Stop.
        //   Result: 2 ops (item + c).
        //
        // stop_path_height=1:
        //   Call 1..3 as above, plus:
        //   Call 3: path=[T, a], key=b. 2!=1. Delete b.
        //     Recurse: path=[T], key=a.
        //   Call 4: path=[T], key=a. 1==1. Return None. Stop.
        //   Result: 3 ops (item + c + b).

        let grove_version = GroveVersion::latest();

        // --- stop_path_height = 3: delete only item ---
        {
            let db = make_test_grovedb(grove_version);
            insert_chain(&db, grove_version);

            let deleted_count = db
                .delete_up_tree_while_empty(
                    [TEST_LEAF, b"a", b"b", b"c"].as_ref(),
                    b"item",
                    &DeleteUpTreeOptions {
                        stop_path_height: Some(3),
                        ..Default::default()
                    },
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("should delete with stop_path_height=3");

            assert_eq!(
                deleted_count, 1,
                "stop_path_height=3 should delete only item"
            );

            // item should be gone
            assert!(
                matches!(
                    db.get(
                        [TEST_LEAF, b"a", b"b", b"c"].as_ref(),
                        b"item",
                        None,
                        grove_version
                    )
                    .unwrap(),
                    Err(Error::PathKeyNotFound(_))
                ),
                "item should have been deleted"
            );

            // c should still exist (it's now empty, but the stop prevented its deletion)
            db.get([TEST_LEAF, b"a", b"b"].as_ref(), b"c", None, grove_version)
                .unwrap()
                .expect("c should still exist at stop_path_height=3");
        }

        // --- stop_path_height = 2: delete item and c ---
        {
            let db = make_test_grovedb(grove_version);
            insert_chain(&db, grove_version);

            let deleted_count = db
                .delete_up_tree_while_empty(
                    [TEST_LEAF, b"a", b"b", b"c"].as_ref(),
                    b"item",
                    &DeleteUpTreeOptions {
                        stop_path_height: Some(2),
                        ..Default::default()
                    },
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("should delete with stop_path_height=2");

            assert_eq!(
                deleted_count, 2,
                "stop_path_height=2 should delete item and c"
            );

            // c should be gone
            assert!(
                matches!(
                    db.get([TEST_LEAF, b"a", b"b"].as_ref(), b"c", None, grove_version)
                        .unwrap(),
                    Err(Error::PathKeyNotFound(_))
                ),
                "c should have been deleted"
            );

            // b should still exist
            db.get([TEST_LEAF, b"a"].as_ref(), b"b", None, grove_version)
                .unwrap()
                .expect("b should still exist at stop_path_height=2");
        }

        // --- stop_path_height = 1: delete item, c, and b ---
        {
            let db = make_test_grovedb(grove_version);
            insert_chain(&db, grove_version);

            let deleted_count = db
                .delete_up_tree_while_empty(
                    [TEST_LEAF, b"a", b"b", b"c"].as_ref(),
                    b"item",
                    &DeleteUpTreeOptions {
                        stop_path_height: Some(1),
                        ..Default::default()
                    },
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("should delete with stop_path_height=1");

            assert_eq!(
                deleted_count, 3,
                "stop_path_height=1 should delete item, c, and b"
            );

            // b should be gone
            assert!(
                matches!(
                    db.get([TEST_LEAF, b"a"].as_ref(), b"b", None, grove_version)
                        .unwrap(),
                    Err(Error::PathKeyNotFound(_))
                ),
                "b should have been deleted"
            );

            // a should still exist
            db.get([TEST_LEAF].as_ref(), b"a", None, grove_version)
                .unwrap()
                .expect("a should still exist at stop_path_height=1");

            // TEST_LEAF should still exist
            db.get(EMPTY_PATH, TEST_LEAF, None, grove_version)
                .unwrap()
                .expect("TEST_LEAF should still exist at stop_path_height=1");
        }
    }

    /// Helper: insert chain TEST_LEAF -> a -> b -> c -> item
    fn insert_chain(db: &crate::tests::TempGroveDb, grove_version: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            b"a",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert a");

        db.insert(
            [TEST_LEAF, b"a"].as_ref(),
            b"b",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert b");

        db.insert(
            [TEST_LEAF, b"a", b"b"].as_ref(),
            b"c",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert c");

        db.insert(
            [TEST_LEAF, b"a", b"b", b"c"].as_ref(),
            b"item",
            Element::new_item(b"value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");
    }

    #[test]
    fn delete_up_tree_stop_path_height_exceeds_path_error() {
        // When stop_path_height equals the path length, the function returns
        // None immediately. The caller then converts that None to a
        // DeleteUpTreeStopHeightMoreThanInitialPathSize error.

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a single item under TEST_LEAF
        db.insert(
            [TEST_LEAF].as_ref(),
            b"item",
            Element::new_item(b"data".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        // Path is [TEST_LEAF] which has length 1.
        // Set stop_path_height to 1 (equal to path length) -> the very first
        // call sees path.len()==stop_path_height, returns None, and the caller
        // wraps it as DeleteUpTreeStopHeightMoreThanInitialPathSize.
        let result = db
            .delete_up_tree_while_empty(
                [TEST_LEAF].as_ref(),
                b"item",
                &DeleteUpTreeOptions {
                    stop_path_height: Some(1),
                    ..Default::default()
                },
                None,
                grove_version,
            )
            .unwrap();

        assert!(
            matches!(
                result,
                Err(Error::DeleteUpTreeStopHeightMoreThanInitialPathSize(_))
            ),
            "should return DeleteUpTreeStopHeightMoreThanInitialPathSize error, got: {:?}",
            result
        );

        // Also test with stop_path_height much greater than path length.
        // Path is [TEST_LEAF, sub] (length 2), stop_path_height = 5.
        // The first call has path.len()=2 != 5, so it proceeds with deletion.
        // But path [TEST_LEAF, sub] doesn't exist as a valid tree. Use a
        // deeper path to trigger the error more reliably.
        //
        // Instead, use stop_path_height equal to the path length on a deeper path.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"sub",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert sub");

        db.insert(
            [TEST_LEAF, b"sub"].as_ref(),
            b"leaf",
            Element::new_item(b"x".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert leaf");

        // Path is [TEST_LEAF, sub] (length 2), stop_path_height = 2.
        let result2 = db
            .delete_up_tree_while_empty(
                [TEST_LEAF, b"sub"].as_ref(),
                b"leaf",
                &DeleteUpTreeOptions {
                    stop_path_height: Some(2),
                    ..Default::default()
                },
                None,
                grove_version,
            )
            .unwrap();

        assert!(
            matches!(
                result2,
                Err(Error::DeleteUpTreeStopHeightMoreThanInitialPathSize(_))
            ),
            "should return error when stop_path_height equals path length, got: {:?}",
            result2
        );
    }

    #[test]
    fn delete_up_tree_validate_tree_at_path_exists_error() {
        // When validate_tree_at_path_exists is true and the path does not
        // exist, the operation should return a PathNotFound error.

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Try to delete from a path that does not exist with validation enabled
        let result = db
            .delete_up_tree_while_empty(
                [TEST_LEAF, b"nonexistent_subtree"].as_ref(),
                b"some_key",
                &DeleteUpTreeOptions {
                    validate_tree_at_path_exists: true,
                    stop_path_height: Some(0),
                    ..Default::default()
                },
                None,
                grove_version,
            )
            .unwrap();

        assert!(
            matches!(
                result,
                Err(Error::PathNotFound(_)) | Err(Error::PathParentLayerNotFound(_))
            ),
            "should return path-related error when tree at path does not exist, got: {:?}",
            result
        );
    }

    #[test]
    fn delete_up_tree_while_empty_with_sectional_storage() {
        // Same as chain deletion test but using the sectional storage variant
        // directly, providing a custom split_removal_bytes_function.

        use grovedb_costs::storage_cost::removal::StorageRemovedBytes::BasicStorageRemoval;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Build a chain: TEST_LEAF -> s1 -> s2 -> item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"s1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert s1");

        db.insert(
            [TEST_LEAF, b"s1"].as_ref(),
            b"s2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert s2");

        db.insert(
            [TEST_LEAF, b"s1", b"s2"].as_ref(),
            b"item",
            Element::new_item(b"sectional".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert item");

        // Use delete_up_tree_while_empty_with_sectional_storage directly.
        // stop_path_height=1 means stop when path.len()==1, i.e. at [TEST_LEAF].
        // Recursion:
        //   [T, s1, s2] -> item: delete item (path.len()=3 != 1)
        //   [T, s1]     -> s2:   delete s2   (path.len()=2 != 1)
        //   [T]         -> s1:   path.len()=1 == 1 -> stop
        // Result: 2 ops (item + s2). s1 is NOT deleted.
        let path: SubtreePath<_> = [TEST_LEAF, b"s1", b"s2"].as_ref().into();
        let deleted_count = db
            .delete_up_tree_while_empty_with_sectional_storage(
                path,
                b"item",
                &DeleteUpTreeOptions {
                    stop_path_height: Some(1),
                    ..Default::default()
                },
                None,
                |_flags, removed_key_bytes, removed_value_bytes| {
                    Ok((
                        BasicStorageRemoval(removed_key_bytes),
                        BasicStorageRemoval(removed_value_bytes),
                    ))
                },
                grove_version,
            )
            .unwrap()
            .expect("should delete up tree with sectional storage");

        assert_eq!(
            deleted_count, 2,
            "should delete item and s2 via sectional storage variant"
        );

        // s1 should still exist (stop boundary)
        db.get([TEST_LEAF].as_ref(), b"s1", None, grove_version)
            .unwrap()
            .expect("s1 should still exist at stop boundary");

        // s2 should be gone
        assert!(
            matches!(
                db.get([TEST_LEAF, b"s1"].as_ref(), b"s2", None, grove_version)
                    .unwrap(),
                Err(Error::PathKeyNotFound(_))
            ),
            "s2 should have been deleted"
        );

        // TEST_LEAF should still exist
        db.get(EMPTY_PATH, TEST_LEAF, None, grove_version)
            .unwrap()
            .expect("TEST_LEAF should still exist");
    }

    #[test]
    fn delete_operations_for_delete_up_tree_returns_ops() {
        // Call delete_operations_for_delete_up_tree_while_empty and verify it
        // returns operations without actually modifying the database.

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Build chain: TEST_LEAF -> t1 -> t2 -> val
        db.insert(
            [TEST_LEAF].as_ref(),
            b"t1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert t1");

        db.insert(
            [TEST_LEAF, b"t1"].as_ref(),
            b"t2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert t2");

        db.insert(
            [TEST_LEAF, b"t1", b"t2"].as_ref(),
            b"val",
            Element::new_item(b"keep_me".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert val");

        // Get the operations without applying.
        // stop_path_height=1 means stop when path.len()==1.
        // Recursion:
        //   [T, t1, t2] -> val: delete val  (path.len()=3 != 1)
        //   [T, t1]     -> t2:  delete t2   (path.len()=2 != 1)
        //   [T]         -> t1:  path.len()=1 == 1 -> stop
        // Result: 2 ops (val + t2).
        let path: SubtreePath<_> = [TEST_LEAF, b"t1", b"t2"].as_ref().into();
        let ops = db
            .delete_operations_for_delete_up_tree_while_empty(
                path,
                b"val",
                &DeleteUpTreeOptions {
                    stop_path_height: Some(1),
                    ..Default::default()
                },
                None,
                Vec::new(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("should return delete operations");

        // Should have operations for: val, t2 (t1 is not included due to stop)
        assert_eq!(ops.len(), 2, "should return 2 delete operations");

        // Verify the database is UNCHANGED -- the item should still exist
        let item = db
            .get(
                [TEST_LEAF, b"t1", b"t2"].as_ref(),
                b"val",
                None,
                grove_version,
            )
            .unwrap()
            .expect("val should still exist because ops were not applied");
        assert_eq!(item, Element::new_item(b"keep_me".to_vec()));

        // t1 should also still exist
        db.get([TEST_LEAF].as_ref(), b"t1", None, grove_version)
            .unwrap()
            .expect("t1 should still exist because ops were not applied");
    }

    #[test]
    fn delete_up_tree_single_level() {
        // Test with a single item inside a subtree under TEST_LEAF.
        // After deleting the item with stop_path_height=1, the item is
        // deleted but the parent tree is at the stop boundary.
        //
        // Structure:
        //   TEST_LEAF -> only_child -> my_item
        //
        // Recursion with stop_path_height=1:
        //   [TEST_LEAF, only_child] -> my_item: delete (path.len()=2 != 1)
        //   [TEST_LEAF]             -> only_child: path.len()=1 == 1 -> stop
        // Result: 1 op (my_item).

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a tree under TEST_LEAF
        db.insert(
            [TEST_LEAF].as_ref(),
            b"only_child",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert only_child tree");

        // Insert an item into that tree
        db.insert(
            [TEST_LEAF, b"only_child"].as_ref(),
            b"my_item",
            Element::new_item(b"single_level_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("should insert my_item");

        // Delete up tree with stop_path_height=1
        let deleted_count = db
            .delete_up_tree_while_empty(
                [TEST_LEAF, b"only_child"].as_ref(),
                b"my_item",
                &DeleteUpTreeOptions {
                    stop_path_height: Some(1),
                    ..Default::default()
                },
                None,
                grove_version,
            )
            .unwrap()
            .expect("should delete up tree from single level");

        // Should delete just my_item (only_child is at the stop boundary)
        assert_eq!(
            deleted_count, 1,
            "should delete just the item since parent is at stop boundary"
        );

        // my_item should be gone
        assert!(
            matches!(
                db.get(
                    [TEST_LEAF, b"only_child"].as_ref(),
                    b"my_item",
                    None,
                    grove_version
                )
                .unwrap(),
                Err(Error::PathKeyNotFound(_))
            ),
            "my_item should have been deleted"
        );

        // only_child should still exist (now empty, but stop_path_height
        // prevented its deletion)
        db.get([TEST_LEAF].as_ref(), b"only_child", None, grove_version)
            .unwrap()
            .expect("only_child should still exist at stop boundary");
    }
}
