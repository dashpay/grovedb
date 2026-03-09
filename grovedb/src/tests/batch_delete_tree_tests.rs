//! Tests for batch DeleteTree cleanup (H1) and emptiness check (H2).
//!
//! H1: Batch DeleteTree for standard Merk trees must clean up child subtree
//! storage, not just remove the parent key.
//!
//! H2: Batch DeleteTree must consult `allow_deleting_non_empty_trees` before
//! deleting a non-empty tree.

#[cfg(feature = "minimal")]
mod tests {
    use grovedb_merk::tree_type::TreeType;
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::{BatchApplyOptions, QualifiedGroveDbOp, SubelementsDeletionBehavior},
        tests::{common::EMPTY_PATH, make_empty_grovedb},
        Element, Error,
    };

    // ===================================================================
    // H2: Batch DeleteTree should respect allow_deleting_non_empty_trees
    // ===================================================================

    #[test]
    fn test_batch_delete_tree_non_empty_should_fail_when_not_allowed() {
        // When `allow_deleting_non_empty_trees` is false (default) and
        // `deleting_non_empty_trees_returns_error` is true (default),
        // deleting a non-empty tree via batch should return an error.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Insert a tree at root level
        db.insert(
            EMPTY_PATH,
            b"parent_tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert parent tree");

        // Insert an item inside the tree to make it non-empty
        db.insert(
            [b"parent_tree".as_ref()].as_ref(),
            b"child_item",
            Element::new_item(b"value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert child item");

        // Try to delete the non-empty tree via batch with Error mode
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"parent_tree".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Error,
        )];

        let batch_options = Some(BatchApplyOptions {
            ..Default::default()
        });

        let result = db
            .apply_batch(ops, batch_options, None, grove_version)
            .unwrap();

        assert!(
            result.is_err(),
            "batch DeleteTree on a non-empty tree with allow_deleting_non_empty_trees=false \
             should fail, but got: {:?}",
            result,
        );
        match result {
            Err(Error::DeletingNonEmptyTree(_)) => { /* expected */ }
            Err(e) => panic!("expected DeletingNonEmptyTree error, got: {:?}", e),
            Ok(()) => panic!("expected error but got Ok"),
        }
    }

    #[test]
    fn test_batch_delete_tree_non_empty_succeeds_when_allowed() {
        // When `allow_deleting_non_empty_trees` is true,
        // deleting a non-empty tree via batch should succeed.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Insert a tree at root level
        db.insert(
            EMPTY_PATH,
            b"parent_tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert parent tree");

        // Insert an item inside the tree to make it non-empty
        db.insert(
            [b"parent_tree".as_ref()].as_ref(),
            b"child_item",
            Element::new_item(b"value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert child item");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"parent_tree".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DontCheck,
        )];

        let batch_options = Some(BatchApplyOptions {
            ..Default::default()
        });

        db.apply_batch(ops, batch_options, None, grove_version)
            .unwrap()
            .expect("batch delete of non-empty tree should succeed when allowed");

        // Verify tree is gone
        let result = db
            .get(EMPTY_PATH, b"parent_tree", None, grove_version)
            .unwrap();
        assert!(result.is_err(), "parent tree should have been deleted");
    }

    #[test]
    fn test_batch_delete_empty_tree_succeeds() {
        // Deleting an empty tree should always succeed regardless of options.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Insert an empty tree at root level
        db.insert(
            EMPTY_PATH,
            b"empty_tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert empty tree");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"empty_tree".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Error,
        )];

        let batch_options = Some(BatchApplyOptions {
            ..Default::default()
        });

        db.apply_batch(ops, batch_options, None, grove_version)
            .unwrap()
            .expect("batch delete of empty tree should succeed");

        // Verify tree is gone
        let result = db
            .get(EMPTY_PATH, b"empty_tree", None, grove_version)
            .unwrap();
        assert!(result.is_err(), "empty tree should have been deleted");
    }

    // ===================================================================
    // H1: Batch DeleteTree must clean up child subtree storage
    // ===================================================================

    #[test]
    fn test_batch_delete_tree_cleans_up_subtree_storage() {
        // When we delete a tree that has nested subtrees via batch,
        // the storage for those subtrees should be cleaned up.
        // Then inserting a new tree at the same path should work without
        // corruption from stale data.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Step 1: Create a tree with nested subtrees
        db.insert(
            EMPTY_PATH,
            b"outer",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert outer tree");

        db.insert(
            [b"outer".as_ref()].as_ref(),
            b"inner",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert inner tree");

        db.insert(
            [b"outer".as_ref(), b"inner".as_ref()].as_ref(),
            b"item1",
            Element::new_item(b"old_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item into inner tree");

        // Step 2: Delete the outer tree via batch (with DontCheck for non-empty subtrees)
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"outer".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DontCheck,
        )];

        let batch_options = Some(BatchApplyOptions {
            ..Default::default()
        });

        db.apply_batch(ops, batch_options.clone(), None, grove_version)
            .unwrap()
            .expect("batch delete tree should succeed");

        // Verify that outer tree is gone
        let outer_get = db.get(EMPTY_PATH, b"outer", None, grove_version).unwrap();
        assert!(outer_get.is_err(), "outer tree should have been deleted");

        // Step 3: Insert a new tree at the same path
        db.insert(
            EMPTY_PATH,
            b"outer",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert new outer tree at same path");

        // Step 4: The new tree should be empty (no stale data from old inner tree)
        db.insert(
            [b"outer".as_ref()].as_ref(),
            b"new_inner",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert new inner tree");

        db.insert(
            [b"outer".as_ref(), b"new_inner".as_ref()].as_ref(),
            b"new_item",
            Element::new_item(b"new_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item into new inner tree");

        // Verify the new data is correct
        let result = db
            .get(
                [b"outer".as_ref(), b"new_inner".as_ref()].as_ref(),
                b"new_item",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get new item");
        assert_eq!(result, Element::new_item(b"new_value".to_vec()));

        // The old item should NOT be accessible at the old path
        // (the inner tree no longer exists, so this path is invalid)
        let old_result = db
            .get(
                [b"outer".as_ref(), b"inner".as_ref()].as_ref(),
                b"item1",
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            old_result.is_err(),
            "old inner tree data should not be accessible after delete and re-insert"
        );
    }

    #[test]
    fn test_batch_delete_tree_then_reinsert_produces_clean_tree() {
        // This test verifies that after batch-deleting a tree with children,
        // re-inserting at the same path produces a genuinely empty tree
        // with no leftover data from previous children.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Create structure: root -> parent -> child (with item)
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"parent".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"parent".to_vec()],
                b"child".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"parent".to_vec(), b"child".to_vec()],
                b"key1".to_vec(),
                Element::new_item(b"data".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch insert tree structure");

        // Verify structure was created
        let val = db
            .get(
                [b"parent".as_ref(), b"child".as_ref()].as_ref(),
                b"key1",
                None,
                grove_version,
            )
            .unwrap()
            .expect("verify item exists");
        assert_eq!(val, Element::new_item(b"data".to_vec()));

        // Delete the parent tree via batch
        let delete_ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"parent".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DontCheck,
        )];

        let batch_options = Some(BatchApplyOptions {
            ..Default::default()
        });

        db.apply_batch(delete_ops, batch_options, None, grove_version)
            .unwrap()
            .expect("batch delete parent tree");

        // Re-insert parent as empty tree
        db.insert(
            EMPTY_PATH,
            b"parent",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("re-insert parent tree");

        // Verify the re-inserted tree is truly empty by checking
        // that the old child subtree does not leak through
        let child_get = db
            .get([b"parent".as_ref()].as_ref(), b"child", None, grove_version)
            .unwrap();
        assert!(
            child_get.is_err(),
            "re-inserted tree should not contain old child subtree data"
        );
    }

    // ===================================================================
    // H2: Skip mode — deleting_non_empty_trees_returns_error = false
    // ===================================================================

    #[test]
    fn test_batch_delete_tree_non_empty_skip_mode_silently_skips() {
        // When `allow_deleting_non_empty_trees` is false and
        // `deleting_non_empty_trees_returns_error` is also false,
        // the DeleteTree op for a non-empty tree should be silently skipped.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Insert a tree with a child item to make it non-empty
        db.insert(
            EMPTY_PATH,
            b"skip_tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert tree");

        db.insert(
            [b"skip_tree".as_ref()].as_ref(),
            b"child",
            Element::new_item(b"data".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert child");

        // Attempt to delete the non-empty tree in skip mode
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"skip_tree".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Skip,
        )];

        let batch_options = Some(BatchApplyOptions {
            ..Default::default()
        });

        // Should succeed (no error) but the tree should still exist
        db.apply_batch(ops, batch_options, None, grove_version)
            .unwrap()
            .expect("skip mode should not return error");

        // The tree should still exist because the delete was skipped
        let result = db
            .get(EMPTY_PATH, b"skip_tree", None, grove_version)
            .unwrap();
        assert!(
            result.is_ok(),
            "tree should still exist after skip-mode DeleteTree on non-empty tree"
        );

        // Child should still be accessible
        let child_result = db
            .get(
                [b"skip_tree".as_ref()].as_ref(),
                b"child",
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            child_result.is_ok(),
            "child should still be accessible after skipped delete"
        );
    }

    #[test]
    fn test_batch_delete_tree_skip_mode_allows_empty_tree_deletion() {
        // Even in skip mode, an empty tree should still be deleted.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"empty_tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert empty tree");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"empty_tree".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Skip,
        )];

        let batch_options = Some(BatchApplyOptions {
            ..Default::default()
        });

        db.apply_batch(ops, batch_options, None, grove_version)
            .unwrap()
            .expect("empty tree deletion should succeed in skip mode");

        let result = db
            .get(EMPTY_PATH, b"empty_tree", None, grove_version)
            .unwrap();
        assert!(
            result.is_err(),
            "empty tree should have been deleted even in skip mode"
        );
    }

    // ===================================================================
    // Batch DeleteTree with simultaneous child deletion (is_empty_tree_except)
    // ===================================================================

    #[test]
    fn test_batch_delete_tree_with_simultaneous_child_delete() {
        // When the batch contains both a Delete of the child and a DeleteTree
        // of the parent, the emptiness check should account for the child
        // deletion (is_empty_tree_except). The tree should be considered
        // empty because the child is also being deleted in the same batch.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Create a tree with one child
        db.insert(
            EMPTY_PATH,
            b"parent",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert parent");

        db.insert(
            [b"parent".as_ref()].as_ref(),
            b"child",
            Element::new_item(b"value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert child");

        // Batch: delete the child item AND delete the parent tree
        // The emptiness check should see the child as "being deleted" and
        // consider the tree empty.
        let ops = vec![
            QualifiedGroveDbOp::delete_op(vec![b"parent".to_vec()], b"child".to_vec()),
            QualifiedGroveDbOp::delete_tree_op(
                vec![],
                b"parent".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::Error,
            ),
        ];

        let batch_options = Some(BatchApplyOptions {
            ..Default::default()
        });

        // Should succeed: the child is being deleted in the same batch,
        // so the tree is effectively empty.
        db.apply_batch(ops, batch_options, None, grove_version)
            .unwrap()
            .expect("batch delete with simultaneous child deletion should succeed");

        // Verify parent is gone
        let result = db.get(EMPTY_PATH, b"parent", None, grove_version).unwrap();
        assert!(result.is_err(), "parent tree should have been deleted");
    }

    #[test]
    fn test_batch_delete_tree_with_partial_child_delete_still_non_empty() {
        // When only some children are deleted in the same batch, the tree
        // should still be considered non-empty and the delete should fail.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Create a tree with two children
        db.insert(
            EMPTY_PATH,
            b"parent",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert parent");

        db.insert(
            [b"parent".as_ref()].as_ref(),
            b"child1",
            Element::new_item(b"value1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert child1");

        db.insert(
            [b"parent".as_ref()].as_ref(),
            b"child2",
            Element::new_item(b"value2".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert child2");

        // Only delete one child in the batch, then try to delete the parent tree.
        // The tree still has child2, so it should fail.
        let ops = vec![
            QualifiedGroveDbOp::delete_op(vec![b"parent".to_vec()], b"child1".to_vec()),
            QualifiedGroveDbOp::delete_tree_op(
                vec![],
                b"parent".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::Error,
            ),
        ];

        let batch_options = Some(BatchApplyOptions {
            ..Default::default()
        });

        let result = db
            .apply_batch(ops, batch_options, None, grove_version)
            .unwrap();

        assert!(
            result.is_err(),
            "should fail when only some children are deleted: {:?}",
            result,
        );
        match result {
            Err(Error::DeletingNonEmptyTree(_)) => { /* expected */ }
            Err(e) => panic!("expected DeletingNonEmptyTree, got: {:?}", e),
            Ok(()) => panic!("expected error but got Ok"),
        }
    }

    // ===================================================================
    // Partial batch: emptiness checks and cleanup
    // ===================================================================

    #[test]
    fn test_partial_batch_delete_tree_non_empty_should_fail_when_not_allowed() {
        // The partial batch path has its own copy of the emptiness check logic.
        // Verify it also enforces non-empty tree deletion restrictions.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree_a",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert tree");

        db.insert(
            [b"tree_a".as_ref()].as_ref(),
            b"item",
            Element::new_item(b"val".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert item");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"tree_a".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Error,
        )];

        let batch_options = Some(BatchApplyOptions {
            ..Default::default()
        });

        let result = db
            .apply_partial_batch(
                ops,
                batch_options,
                |_cost, _left_over_ops| Ok(vec![]),
                Some(&tx),
                grove_version,
            )
            .unwrap();

        assert!(
            result.is_err(),
            "partial batch should fail for non-empty tree: {:?}",
            result,
        );
        match result {
            Err(Error::DeletingNonEmptyTree(_)) => { /* expected */ }
            Err(e) => panic!("expected DeletingNonEmptyTree, got: {:?}", e),
            Ok(()) => panic!("expected error but got Ok"),
        }
    }

    #[test]
    fn test_partial_batch_delete_tree_non_empty_skip_mode() {
        // Partial batch skip mode: non-empty tree should be skipped, not errored.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree_b",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert tree");

        db.insert(
            [b"tree_b".as_ref()].as_ref(),
            b"item",
            Element::new_item(b"val".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert item");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"tree_b".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Skip,
        )];

        let batch_options = Some(BatchApplyOptions {
            ..Default::default()
        });

        db.apply_partial_batch(
            ops,
            batch_options,
            |_cost, _left_over_ops| Ok(vec![]),
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("partial batch skip mode should succeed");

        // Tree should still exist
        let result = db
            .get(EMPTY_PATH, b"tree_b", Some(&tx), grove_version)
            .unwrap();
        assert!(
            result.is_ok(),
            "tree should still exist after skipped partial batch delete"
        );
    }

    #[test]
    fn test_partial_batch_delete_tree_non_empty_succeeds_when_allowed() {
        // Partial batch: deleting a non-empty tree should succeed when allowed,
        // exercising the merk cleanup path in apply_partial_batch.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        // Create a tree with a nested subtree
        db.insert(
            EMPTY_PATH,
            b"outer",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert outer");

        db.insert(
            [b"outer".as_ref()].as_ref(),
            b"inner",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert inner");

        db.insert(
            [b"outer".as_ref(), b"inner".as_ref()].as_ref(),
            b"data",
            Element::new_item(b"some_value".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert data");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"outer".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DontCheck,
        )];

        let batch_options = Some(BatchApplyOptions {
            ..Default::default()
        });

        db.apply_partial_batch(
            ops,
            batch_options,
            |_cost, _left_over_ops| Ok(vec![]),
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("partial batch delete non-empty tree");

        // Tree should be gone
        let result = db
            .get(EMPTY_PATH, b"outer", Some(&tx), grove_version)
            .unwrap();
        assert!(result.is_err(), "tree should have been deleted");

        // Re-insert and verify clean state (no stale data)
        db.insert(
            EMPTY_PATH,
            b"outer",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("re-insert outer");

        let inner_get = db
            .get(
                [b"outer".as_ref()].as_ref(),
                b"inner",
                Some(&tx),
                grove_version,
            )
            .unwrap();
        assert!(
            inner_get.is_err(),
            "re-inserted tree should be clean with no stale inner subtree"
        );
    }

    #[test]
    fn test_partial_batch_delete_tree_with_simultaneous_child_delete() {
        // Partial batch version of the is_empty_tree_except test.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"parent",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert parent");

        db.insert(
            [b"parent".as_ref()].as_ref(),
            b"only_child",
            Element::new_item(b"data".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert only child");

        // Delete the only child and the parent tree in the same batch
        let ops = vec![
            QualifiedGroveDbOp::delete_op(vec![b"parent".to_vec()], b"only_child".to_vec()),
            QualifiedGroveDbOp::delete_tree_op(
                vec![],
                b"parent".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::Error,
            ),
        ];

        let batch_options = Some(BatchApplyOptions {
            ..Default::default()
        });

        db.apply_partial_batch(
            ops,
            batch_options,
            |_cost, _left_over_ops| Ok(vec![]),
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("partial batch: should succeed since child is also being deleted");

        let result = db
            .get(EMPTY_PATH, b"parent", Some(&tx), grove_version)
            .unwrap();
        assert!(result.is_err(), "parent should have been deleted");
    }

    // ===================================================================
    // Transactional batch: emptiness check + Merk cleanup
    // ===================================================================

    #[test]
    fn test_batch_delete_tree_non_empty_error_with_transaction() {
        // Same as the non-transactional test but uses a transaction,
        // exercising the apply_batch_with_element_flags_update code path
        // which is slightly different (uses storage_batch).
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"tree_tx",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert tree");

        db.insert(
            [b"tree_tx".as_ref()].as_ref(),
            b"item",
            Element::new_item(b"val".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert item");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"tree_tx".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Error,
        )];

        let batch_options = Some(BatchApplyOptions {
            ..Default::default()
        });

        let result = db
            .apply_batch(ops, batch_options, Some(&tx), grove_version)
            .unwrap();

        match result {
            Err(Error::DeletingNonEmptyTree(_)) => { /* expected */ }
            Err(e) => panic!("expected DeletingNonEmptyTree, got: {:?}", e),
            Ok(()) => panic!("expected error but got Ok"),
        }
    }

    #[test]
    fn test_batch_delete_tree_cleans_up_deeply_nested_subtrees() {
        // Verify that recursive cleanup works for 3+ levels of nesting.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Create: root -> level1 -> level2 -> level3 -> item
        db.insert(
            EMPTY_PATH,
            b"l1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert l1");
        db.insert(
            [b"l1".as_ref()].as_ref(),
            b"l2",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert l2");
        db.insert(
            [b"l1".as_ref(), b"l2".as_ref()].as_ref(),
            b"l3",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert l3");
        db.insert(
            [b"l1".as_ref(), b"l2".as_ref(), b"l3".as_ref()].as_ref(),
            b"item",
            Element::new_item(b"deep_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert deep item");

        // Delete the top-level tree
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"l1".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DontCheck,
        )];

        let batch_options = Some(BatchApplyOptions {
            ..Default::default()
        });

        db.apply_batch(ops, batch_options, None, grove_version)
            .unwrap()
            .expect("delete deeply nested tree");

        // Re-insert and verify clean state at all levels
        db.insert(
            EMPTY_PATH,
            b"l1",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("re-insert l1");

        // l2 should not exist in the re-inserted tree
        let l2_get = db
            .get([b"l1".as_ref()].as_ref(), b"l2", None, grove_version)
            .unwrap();
        assert!(l2_get.is_err(), "l2 should not exist in re-inserted tree");
    }

    // ===================================================================
    // DeleteChildren mode — non-empty tree should be deleted with cleanup
    // ===================================================================

    #[test]
    fn test_batch_delete_tree_delete_children_mode_deletes_non_empty_tree() {
        // When SubelementsDeletionBehavior::DeleteChildren is used,
        // the emptiness check runs but a non-empty tree should still
        // be deleted (children cleaned up).
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Create a tree with children
        db.insert(
            EMPTY_PATH,
            b"tree_dc",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert tree");

        db.insert(
            [b"tree_dc".as_ref()].as_ref(),
            b"child1",
            Element::new_item(b"val1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert child1");

        db.insert(
            [b"tree_dc".as_ref()].as_ref(),
            b"child2",
            Element::new_item(b"val2".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert child2");

        // Delete the non-empty tree with DeleteChildren mode
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"tree_dc".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DeleteChildren,
        )];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("DeleteChildren mode should delete non-empty tree");

        // Tree should be gone
        assert!(
            db.get(EMPTY_PATH, b"tree_dc", None, grove_version)
                .unwrap()
                .is_err(),
            "tree should have been deleted"
        );

        // Re-insert and verify no stale data
        db.insert(
            EMPTY_PATH,
            b"tree_dc",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("re-insert tree");

        assert!(
            db.get(
                [b"tree_dc".as_ref()].as_ref(),
                b"child1",
                None,
                grove_version
            )
            .unwrap()
            .is_err(),
            "old children should not exist in re-inserted tree"
        );
    }

    #[test]
    fn test_batch_delete_tree_delete_children_mode_empty_tree_succeeds() {
        // DeleteChildren mode on an empty tree should succeed normally.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"empty_dc",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert empty tree");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"empty_dc".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DeleteChildren,
        )];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("DeleteChildren on empty tree should succeed");

        assert!(
            db.get(EMPTY_PATH, b"empty_dc", None, grove_version)
                .unwrap()
                .is_err(),
            "empty tree should have been deleted"
        );
    }

    #[test]
    fn test_partial_batch_delete_tree_delete_children_mode() {
        // Partial batch path: DeleteChildren on a non-empty tree.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"ptree_dc",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert tree");

        db.insert(
            [b"ptree_dc".as_ref()].as_ref(),
            b"child",
            Element::new_item(b"val".to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert child");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"ptree_dc".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DeleteChildren,
        )];

        db.apply_partial_batch(
            ops,
            None,
            |_cost, _left_over_ops| Ok(vec![]),
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("partial batch DeleteChildren should succeed");

        assert!(
            db.get(EMPTY_PATH, b"ptree_dc", Some(&tx), grove_version)
                .unwrap()
                .is_err(),
            "tree should have been deleted"
        );
    }

    // ===================================================================
    // apply_operations_without_batching: exercises non-batch fallback
    // ===================================================================

    #[test]
    fn test_without_batching_delete_tree_dont_check() {
        // Exercise the non-batch fallback path for DeleteTree with DontCheck.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"nb_tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert tree");

        db.insert(
            [b"nb_tree".as_ref()].as_ref(),
            b"item",
            Element::new_item(b"val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"nb_tree".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DontCheck,
        )];

        db.apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap()
            .expect("non-batch DontCheck should succeed");

        assert!(
            db.get(EMPTY_PATH, b"nb_tree", None, grove_version)
                .unwrap()
                .is_err(),
            "tree should have been deleted"
        );
    }

    #[test]
    fn test_without_batching_delete_tree_error_mode() {
        // Exercise the non-batch fallback path for DeleteTree with Error mode.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"nb_err",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert tree");

        db.insert(
            [b"nb_err".as_ref()].as_ref(),
            b"item",
            Element::new_item(b"val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");

        // Error mode on non-empty tree: should fail
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"nb_err".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Error,
        )];

        let result = db
            .apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap();

        match result {
            Err(Error::DeletingNonEmptyTree(_)) => { /* expected */ }
            Err(e) => panic!("expected DeletingNonEmptyTree, got: {:?}", e),
            Ok(()) => panic!("expected error but got Ok"),
        }
    }

    #[test]
    fn test_without_batching_delete_tree_delete_children_mode() {
        // Exercise the non-batch fallback path for DeleteTree with
        // DeleteChildren mode.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"nb_dc",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert tree");

        db.insert(
            [b"nb_dc".as_ref()].as_ref(),
            b"item",
            Element::new_item(b"val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"nb_dc".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DeleteChildren,
        )];

        db.apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap()
            .expect("non-batch DeleteChildren should succeed");

        assert!(
            db.get(EMPTY_PATH, b"nb_dc", None, grove_version)
                .unwrap()
                .is_err(),
            "tree should have been deleted"
        );
    }

    #[test]
    fn test_without_batching_delete_tree_skip_mode() {
        // Exercise the non-batch fallback for DeleteTree with Skip mode.
        // Skip mode maps to allow=false, error=false in DeleteOptions,
        // which makes delete() silently skip non-empty trees.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"nb_skip",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert tree");

        db.insert(
            [b"nb_skip".as_ref()].as_ref(),
            b"item",
            Element::new_item(b"val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"nb_skip".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Skip,
        )];

        db.apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap()
            .expect("non-batch Skip should succeed");

        // Tree should still exist (skip mode)
        assert!(
            db.get(EMPTY_PATH, b"nb_skip", None, grove_version)
                .unwrap()
                .is_ok(),
            "tree should still exist after skip-mode non-batch delete"
        );
    }

    #[test]
    fn test_without_batching_delete_tree_empty_tree_with_error_mode() {
        // Error mode on an empty tree should succeed.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"nb_empty",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert tree");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"nb_empty".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Error,
        )];

        db.apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap()
            .expect("non-batch Error mode on empty tree should succeed");

        assert!(
            db.get(EMPTY_PATH, b"nb_empty", None, grove_version)
                .unwrap()
                .is_err(),
            "empty tree should have been deleted"
        );
    }

    // ===================================================================
    // apply_operations_without_batching with BatchApplyOptions (covers
    // as_delete_options() in options.rs)
    // ===================================================================

    #[test]
    fn test_without_batching_delete_item_with_options() {
        // Exercise as_delete_options() by passing Some(BatchApplyOptions)
        // when deleting a non-tree element via apply_operations_without_batching.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        db.insert(
            EMPTY_PATH,
            b"my_tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert tree");

        db.insert(
            [b"my_tree".as_ref()].as_ref(),
            b"item",
            Element::new_item(b"val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");

        // Delete the item (not a tree) via apply_operations_without_batching
        // with explicit options to exercise as_delete_options().
        let ops = vec![QualifiedGroveDbOp::delete_op(
            vec![b"my_tree".to_vec()],
            b"item".to_vec(),
        )];

        let batch_options = Some(BatchApplyOptions::default());

        db.apply_operations_without_batching(ops, batch_options, None, grove_version)
            .unwrap()
            .expect("delete item with options should succeed");

        assert!(
            db.get([b"my_tree".as_ref()].as_ref(), b"item", None, grove_version)
                .unwrap()
                .is_err(),
            "item should have been deleted"
        );
    }

    // ===================================================================
    // Debug formatting
    // ===================================================================

    #[test]
    fn test_delete_tree_op_debug_format() {
        // Verify the Debug impl includes the SubelementsDeletionBehavior.
        let op = QualifiedGroveDbOp::delete_tree_op(
            vec![b"root".to_vec()],
            b"key".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DeleteChildren,
        );
        let debug_str = format!("{:?}", op);
        assert!(
            debug_str.contains("DeleteChildren"),
            "debug format should include the behavior variant, got: {}",
            debug_str
        );

        let op2 = QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"k".to_vec(),
            TreeType::SumTree,
            SubelementsDeletionBehavior::Skip,
        );
        let debug_str2 = format!("{:?}", op2);
        assert!(
            debug_str2.contains("Skip"),
            "debug format should include Skip, got: {}",
            debug_str2
        );
    }

    // ===================================================================
    // Non-Merk tree emptiness checks (CommitmentTree, MmrTree, etc.)
    // ===================================================================

    #[test]
    fn test_batch_delete_non_merk_tree_error_when_non_empty() {
        // Exercise the non-Merk branch of the emptiness check in
        // apply_batch. CommitmentTree with data should fail with Error mode.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"ct_err",
            Element::empty_commitment_tree(4).expect("valid chunk_power"),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert commitment tree");

        // Populate with one entry
        db.commitment_tree_insert_raw(
            EMPTY_PATH,
            b"ct_err",
            [1u8; 32],
            [2u8; 32],
            vec![0u8; 216],
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert commitment tree data");

        // Try to delete with Error mode — should fail because non-empty
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"ct_err".to_vec(),
            grovedb_merk::tree_type::TreeType::CommitmentTree(4),
            SubelementsDeletionBehavior::Error,
        )];

        let result = db.apply_batch(ops, None, Some(&tx), grove_version).unwrap();

        match result {
            Err(Error::DeletingNonEmptyTree(_)) => { /* expected */ }
            Err(e) => panic!("expected DeletingNonEmptyTree, got: {:?}", e),
            Ok(()) => panic!("expected error but got Ok"),
        }
    }

    #[test]
    fn test_batch_delete_non_merk_tree_skip_when_non_empty() {
        // Non-Merk tree with Skip mode: should silently skip when non-empty.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"ct_skip",
            Element::empty_commitment_tree(4).expect("valid chunk_power"),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert commitment tree");

        db.commitment_tree_insert_raw(
            EMPTY_PATH,
            b"ct_skip",
            [1u8; 32],
            [2u8; 32],
            vec![0u8; 216],
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert commitment tree data");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"ct_skip".to_vec(),
            grovedb_merk::tree_type::TreeType::CommitmentTree(4),
            SubelementsDeletionBehavior::Skip,
        )];

        db.apply_batch(ops, None, Some(&tx), grove_version)
            .unwrap()
            .expect("Skip mode should succeed");

        // Tree should still exist
        assert!(
            db.get(EMPTY_PATH, b"ct_skip", Some(&tx), grove_version)
                .unwrap()
                .is_ok(),
            "commitment tree should still exist after skipped delete"
        );
    }

    #[test]
    fn test_partial_batch_delete_non_merk_tree_error_when_non_empty() {
        // Same as above but via partial batch path.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"ct_perr",
            Element::empty_commitment_tree(4).expect("valid chunk_power"),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert commitment tree");

        db.commitment_tree_insert_raw(
            EMPTY_PATH,
            b"ct_perr",
            [1u8; 32],
            [2u8; 32],
            vec![0u8; 216],
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert commitment tree data");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"ct_perr".to_vec(),
            grovedb_merk::tree_type::TreeType::CommitmentTree(4),
            SubelementsDeletionBehavior::Error,
        )];

        let result = db
            .apply_partial_batch(
                ops,
                None,
                |_cost, _left_over_ops| Ok(vec![]),
                Some(&tx),
                grove_version,
            )
            .unwrap();

        match result {
            Err(Error::DeletingNonEmptyTree(_)) => { /* expected */ }
            Err(e) => panic!("expected DeletingNonEmptyTree, got: {:?}", e),
            Ok(()) => panic!("expected error but got Ok"),
        }
    }

    #[test]
    fn test_batch_delete_non_merk_tree_delete_children_mode() {
        // Non-Merk tree with DeleteChildren mode: should succeed even
        // when non-empty and proceed with deletion.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"ct_dc",
            Element::empty_commitment_tree(4).expect("valid chunk_power"),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert commitment tree");

        db.commitment_tree_insert_raw(
            EMPTY_PATH,
            b"ct_dc",
            [1u8; 32],
            [2u8; 32],
            vec![0u8; 216],
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert commitment tree data");

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"ct_dc".to_vec(),
            grovedb_merk::tree_type::TreeType::CommitmentTree(4),
            SubelementsDeletionBehavior::DeleteChildren,
        )];

        db.apply_batch(ops, None, Some(&tx), grove_version)
            .unwrap()
            .expect("DeleteChildren on non-empty non-Merk tree should succeed");

        assert!(
            db.get(EMPTY_PATH, b"ct_dc", Some(&tx), grove_version)
                .unwrap()
                .is_err(),
            "commitment tree should have been deleted"
        );
    }

    #[test]
    fn test_batch_delete_tree_skip_mode_mixed_batch() {
        // Batch with a DeleteTree of a non-empty tree (skipped) mixed with
        // other operations that should still succeed.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Create two trees: one non-empty, one empty
        db.insert(
            EMPTY_PATH,
            b"non_empty",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert non_empty");
        db.insert(
            [b"non_empty".as_ref()].as_ref(),
            b"child",
            Element::new_item(b"data".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert child");

        db.insert(
            EMPTY_PATH,
            b"empty_one",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert empty_one");

        // Batch: delete non-empty tree (should be skipped) and delete empty tree
        // (should succeed), plus insert an item
        let ops = vec![
            QualifiedGroveDbOp::delete_tree_op(
                vec![],
                b"non_empty".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::Skip,
            ),
            QualifiedGroveDbOp::delete_tree_op(
                vec![],
                b"empty_one".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::Skip,
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"new_item".to_vec(),
                Element::new_item(b"hello".to_vec()),
            ),
        ];

        let batch_options = Some(BatchApplyOptions {
            ..Default::default()
        });

        db.apply_batch(ops, batch_options, None, grove_version)
            .unwrap()
            .expect("mixed batch should succeed");

        // non_empty tree should still exist (skipped)
        assert!(
            db.get(EMPTY_PATH, b"non_empty", None, grove_version)
                .unwrap()
                .is_ok(),
            "non-empty tree should still exist"
        );

        // empty_one should be gone
        assert!(
            db.get(EMPTY_PATH, b"empty_one", None, grove_version)
                .unwrap()
                .is_err(),
            "empty tree should have been deleted"
        );

        // new_item should exist
        assert_eq!(
            db.get(EMPTY_PATH, b"new_item", None, grove_version)
                .unwrap()
                .expect("get new_item"),
            Element::new_item(b"hello".to_vec()),
        );
    }
}
