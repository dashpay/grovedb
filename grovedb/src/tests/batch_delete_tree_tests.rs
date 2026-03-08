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
        batch::{BatchApplyOptions, QualifiedGroveDbOp},
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

        // Try to delete the non-empty tree via batch with default options
        // (allow_deleting_non_empty_trees: false, deleting_non_empty_trees_returns_error: true)
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"parent_tree".to_vec(),
            TreeType::NormalTree,
        )];

        let batch_options = Some(BatchApplyOptions {
            allow_deleting_non_empty_trees: false,
            deleting_non_empty_trees_returns_error: true,
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
        )];

        let batch_options = Some(BatchApplyOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: true,
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
        )];

        let batch_options = Some(BatchApplyOptions {
            allow_deleting_non_empty_trees: false,
            deleting_non_empty_trees_returns_error: true,
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

        // Step 2: Delete the outer tree via batch (with allow_deleting_non_empty_trees)
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"outer".to_vec(),
            TreeType::NormalTree,
        )];

        let batch_options = Some(BatchApplyOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: true,
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
        )];

        let batch_options = Some(BatchApplyOptions {
            allow_deleting_non_empty_trees: true,
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
}
