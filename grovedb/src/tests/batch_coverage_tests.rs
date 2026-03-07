//! Targeted coverage tests for the batch module.
//!
//! These tests exercise uncovered code paths in `grovedb/src/batch/mod.rs`
//! and `just_in_time_reference_update.rs`.

#[cfg(feature = "minimal")]
mod tests {
    use grovedb_merk::tree::AggregateData;
    use grovedb_merk::tree_type::TreeType;
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::{
            key_info::KeyInfo::KnownKey, BatchApplyOptions, GroveOp, KeyInfoPath, NonMerkTreeMeta,
            QualifiedGroveDbOp,
        },
        reference_path::ReferencePathType,
        tests::{common::EMPTY_PATH, make_empty_grovedb, make_test_grovedb, TEST_LEAF},
        Element, Error,
    };

    // ===================================================================
    // 3. Batch with Patch operation
    // ===================================================================

    #[test]
    fn test_batch_patch_updates_item() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert initial item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"patch_key",
            Element::new_item(b"original".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert initial item for patch");

        // Patch the item with new data
        let patched_element = Element::new_item(b"patched_val".to_vec());
        let change_in_bytes = b"patched_val".len() as i32 - b"original".len() as i32;

        let ops = vec![QualifiedGroveDbOp::patch_op(
            vec![TEST_LEAF.to_vec()],
            b"patch_key".to_vec(),
            patched_element.clone(),
            change_in_bytes,
        )];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch patch should succeed");

        // Verify the element was updated
        let result = db
            .get([TEST_LEAF].as_ref(), b"patch_key", None, grove_version)
            .unwrap()
            .expect("get patched element");
        assert_eq!(
            result, patched_element,
            "element should be updated after patch"
        );
    }

    // ===================================================================
    // 4. Batch with RefreshReference operation
    // ===================================================================

    #[test]
    fn test_batch_refresh_reference() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a target item
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

        // Insert a reference to the target
        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref1",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"target".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert reference");

        // Batch-apply RefreshReference
        let ops = vec![QualifiedGroveDbOp::refresh_reference_op(
            vec![TEST_LEAF.to_vec()],
            b"ref1".to_vec(),
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"target".to_vec()]),
            Some(5),
            None,
            true,
        )];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch refresh reference should succeed");

        // Verify the reference still resolves
        let result = db
            .get([TEST_LEAF].as_ref(), b"ref1", None, grove_version)
            .unwrap()
            .expect("get reference after refresh");
        assert_eq!(
            result,
            Element::new_item(b"target_value".to_vec()),
            "reference should still resolve to target after refresh"
        );
    }

    // ===================================================================
    // 5. Batch with DeleteTree operation
    // ===================================================================

    #[test]
    fn test_batch_delete_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Create a tree with some items
        db.insert(
            EMPTY_PATH,
            b"tree_to_del",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert tree");

        // Apply DeleteTree via batch
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"tree_to_del".to_vec(),
            TreeType::NormalTree,
        )];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch delete tree should succeed");

        // Verify the tree no longer exists
        let result = db
            .get(EMPTY_PATH, b"tree_to_del", None, grove_version)
            .unwrap();
        assert!(result.is_err(), "tree should not exist after batch delete");
    }

    #[test]
    fn test_batch_delete_non_empty_tree_with_allow_option() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Create a tree with a child item
        db.insert(
            EMPTY_PATH,
            b"tree_with_items",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert tree");
        db.insert(
            [b"tree_with_items"].as_ref(),
            b"child",
            Element::new_item(b"value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert child item");

        // Delete non-empty tree with allow_deleting_non_empty_trees = true
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![],
            b"tree_with_items".to_vec(),
            TreeType::NormalTree,
        )];

        let options = Some(BatchApplyOptions {
            allow_deleting_non_empty_trees: true,
            ..Default::default()
        });

        db.apply_batch(ops, options, None, grove_version)
            .unwrap()
            .expect("deleting non-empty tree with option should succeed");

        // Verify gone
        let result = db
            .get(EMPTY_PATH, b"tree_with_items", None, grove_version)
            .unwrap();
        assert!(result.is_err(), "tree should not exist after batch delete");
    }

    // ===================================================================
    // 6. apply_operations_without_batching
    // ===================================================================

    #[test]
    fn test_apply_operations_without_batching_inserts() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"unbatched1".to_vec(),
                Element::new_item(b"val1".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"unbatched2".to_vec(),
                Element::new_item(b"val2".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"unbatched3".to_vec(),
                Element::new_item(b"val3".to_vec()),
            ),
        ];

        db.apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap()
            .expect("apply_operations_without_batching should succeed");

        // Verify all elements were inserted
        for (key, val) in [
            (b"unbatched1", b"val1"),
            (b"unbatched2", b"val2"),
            (b"unbatched3", b"val3"),
        ] {
            let result = db
                .get([TEST_LEAF].as_ref(), key.as_slice(), None, grove_version)
                .unwrap()
                .expect("element should exist");
            assert_eq!(
                result,
                Element::new_item(val.to_vec()),
                "element at key {} should match",
                String::from_utf8_lossy(key)
            );
        }
    }

    #[test]
    fn test_apply_operations_without_batching_delete() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert first
        db.insert(
            [TEST_LEAF].as_ref(),
            b"to_delete",
            Element::new_item(b"value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item for deletion");

        // Delete via apply_operations_without_batching
        let ops = vec![QualifiedGroveDbOp::delete_op(
            vec![TEST_LEAF.to_vec()],
            b"to_delete".to_vec(),
        )];

        db.apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap()
            .expect("apply_operations_without_batching delete should succeed");

        // Verify deletion
        let result = db
            .get([TEST_LEAF].as_ref(), b"to_delete", None, grove_version)
            .unwrap();
        assert!(result.is_err(), "element should be gone after deletion");
    }

    #[test]
    fn test_apply_operations_without_batching_unsupported_op_fails() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // RefreshReference is not supported in apply_operations_without_batching
        let ops = vec![QualifiedGroveDbOp::refresh_reference_op(
            vec![TEST_LEAF.to_vec()],
            b"ref1".to_vec(),
            ReferencePathType::AbsolutePathReference(vec![TEST_LEAF.to_vec(), b"target".to_vec()]),
            Some(5),
            None,
            true,
        )];

        let result = db
            .apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap();
        assert!(
            result.is_err(),
            "unsupported op in apply_operations_without_batching should fail"
        );
    }

    #[test]
    fn test_apply_operations_without_batching_replace() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert initial item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"replace_key",
            Element::new_item(b"old".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert initial item");

        // Replace via unbatched ops (Replace variant uses the InsertOrReplace path)
        let ops = vec![QualifiedGroveDbOp::replace_op(
            vec![TEST_LEAF.to_vec()],
            b"replace_key".to_vec(),
            Element::new_item(b"new".to_vec()),
        )];

        db.apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap()
            .expect("replace op should succeed");

        let result = db
            .get([TEST_LEAF].as_ref(), b"replace_key", None, grove_version)
            .unwrap()
            .expect("get replaced element");
        assert_eq!(
            result,
            Element::new_item(b"new".to_vec()),
            "element should be updated after replace"
        );
    }

    #[test]
    fn test_apply_operations_without_batching_insert_only() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // InsertOnly should succeed when key does not exist
        let ops = vec![QualifiedGroveDbOp::insert_only_op(
            vec![TEST_LEAF.to_vec()],
            b"insert_only_key".to_vec(),
            Element::new_item(b"val".to_vec()),
        )];

        db.apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap()
            .expect("insert_only should succeed for new key");

        let result = db
            .get(
                [TEST_LEAF].as_ref(),
                b"insert_only_key",
                None,
                grove_version,
            )
            .unwrap()
            .expect("element should exist");
        assert_eq!(result, Element::new_item(b"val".to_vec()));

        // InsertOnly should fail when key already exists
        let ops = vec![QualifiedGroveDbOp::insert_only_op(
            vec![TEST_LEAF.to_vec()],
            b"insert_only_key".to_vec(),
            Element::new_item(b"val2".to_vec()),
        )];

        let result = db
            .apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap();
        assert!(
            result.is_err(),
            "insert_only should fail when key already exists"
        );
    }

    #[test]
    fn test_apply_operations_without_batching_delete_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create a subtree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"subtree_to_delete",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert subtree");

        // DeleteTree via unbatched ops
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"subtree_to_delete".to_vec(),
            TreeType::NormalTree,
        )];

        db.apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap()
            .expect("delete_tree should succeed");

        let result = db
            .get(
                [TEST_LEAF].as_ref(),
                b"subtree_to_delete",
                None,
                grove_version,
            )
            .unwrap();
        assert!(result.is_err(), "subtree should be gone after delete_tree");
    }

    // ===================================================================
    // 7. batch_structure.rs validation (from_ops)
    // ===================================================================

    // These tests exercise from_ops indirectly through apply_batch, since
    // from_ops is pub(super) and not directly accessible from tests.

    #[test]
    fn test_apply_batch_rejects_replace_tree_root_key_via_from_ops() {
        // When consistency check is disabled, from_ops should still reject
        // internal-only ops.
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let op = QualifiedGroveDbOp {
            path: KeyInfoPath(vec![]),
            key: Some(KnownKey(b"test".to_vec())),
            op: GroveOp::ReplaceTreeRootKey {
                hash: [0u8; 32],
                root_key: None,
                aggregate_data: AggregateData::NoAggregateData,
            },
        };

        let options = Some(BatchApplyOptions {
            disable_operation_consistency_check: true,
            ..Default::default()
        });

        let result = db.apply_batch(vec![op], options, None, grove_version).value;
        match result {
            Err(Error::InvalidBatchOperation(msg)) => {
                assert!(
                    msg.contains("internal operations only"),
                    "error should mention 'internal operations only', got: {}",
                    msg
                );
            }
            Err(other) => {
                panic!(
                    "expected InvalidBatchOperation from from_ops, got: {:?}",
                    other
                );
            }
            Ok(()) => {
                panic!("expected error for internal-only op with consistency check disabled");
            }
        }
    }

    #[test]
    fn test_apply_batch_rejects_insert_tree_with_root_hash_via_from_ops() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let op = QualifiedGroveDbOp {
            path: KeyInfoPath(vec![]),
            key: Some(KnownKey(b"test".to_vec())),
            op: GroveOp::InsertTreeWithRootHash {
                hash: [0u8; 32],
                root_key: None,
                flags: None,
                aggregate_data: AggregateData::NoAggregateData,
            },
        };

        let options = Some(BatchApplyOptions {
            disable_operation_consistency_check: true,
            ..Default::default()
        });

        let result = db.apply_batch(vec![op], options, None, grove_version).value;
        match result {
            Err(Error::InvalidBatchOperation(msg)) => {
                assert!(
                    msg.contains("internal operations only"),
                    "error should mention 'internal operations only', got: {}",
                    msg
                );
            }
            Err(other) => {
                panic!(
                    "expected InvalidBatchOperation from from_ops, got: {:?}",
                    other
                );
            }
            Ok(()) => {
                panic!("expected error for internal-only InsertTreeWithRootHash");
            }
        }
    }

    #[test]
    fn test_apply_batch_rejects_insert_non_merk_tree_via_from_ops() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let op = QualifiedGroveDbOp {
            path: KeyInfoPath(vec![]),
            key: Some(KnownKey(b"test".to_vec())),
            op: GroveOp::InsertNonMerkTree {
                hash: [0u8; 32],
                root_key: None,
                flags: None,
                aggregate_data: AggregateData::NoAggregateData,
                meta: NonMerkTreeMeta::MmrTree { mmr_size: 0 },
            },
        };

        let options = Some(BatchApplyOptions {
            disable_operation_consistency_check: true,
            ..Default::default()
        });

        let result = db.apply_batch(vec![op], options, None, grove_version).value;
        match result {
            Err(Error::InvalidBatchOperation(msg)) => {
                assert!(
                    msg.contains("internal operations only"),
                    "error should mention 'internal operations only', got: {}",
                    msg
                );
            }
            Err(other) => {
                panic!(
                    "expected InvalidBatchOperation from from_ops, got: {:?}",
                    other
                );
            }
            Ok(()) => {
                panic!("expected error for internal-only InsertNonMerkTree");
            }
        }
    }

    // ===================================================================
    // 9. just_in_time_reference_update: reference and target in same batch
    // ===================================================================

    #[test]
    fn test_batch_insert_reference_and_target_same_batch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert both a target item and a reference to it in the same batch
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"target_item".to_vec(),
                Element::new_item(b"hello".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"ref_to_target".to_vec(),
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"target_item".to_vec(),
                ])),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with reference and target should succeed");

        // Verify the reference resolves
        let result = db
            .get([TEST_LEAF].as_ref(), b"ref_to_target", None, grove_version)
            .unwrap()
            .expect("reference should resolve");
        assert_eq!(
            result,
            Element::new_item(b"hello".to_vec()),
            "reference should resolve to the target item value"
        );
    }

    // ===================================================================
    // 10. Batch with multiple tree types
    // ===================================================================

    #[test]
    fn test_batch_mixed_tree_types_in_single_batch() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Insert multiple tree types and an item in a single batch
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"normal_tree".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"sum_tree".to_vec(),
                Element::empty_sum_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"item1".to_vec(),
                Element::new_item(b"value1".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with mixed element types should succeed");

        // Verify all elements exist
        let tree = db
            .get(EMPTY_PATH, b"normal_tree", None, grove_version)
            .unwrap()
            .expect("normal tree should exist");
        assert!(
            matches!(tree, Element::Tree(..)),
            "should be a Tree element"
        );

        let sum_tree = db
            .get(EMPTY_PATH, b"sum_tree", None, grove_version)
            .unwrap()
            .expect("sum tree should exist");
        assert!(
            matches!(sum_tree, Element::SumTree(..)),
            "should be a SumTree element"
        );

        let item = db
            .get(EMPTY_PATH, b"item1", None, grove_version)
            .unwrap()
            .expect("item should exist");
        assert_eq!(
            item,
            Element::new_item(b"value1".to_vec()),
            "item value should match"
        );
    }

    #[test]
    fn test_batch_insert_items_under_different_tree_types() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Create trees first
        db.insert(
            EMPTY_PATH,
            b"normal",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert normal tree");
        db.insert(
            EMPTY_PATH,
            b"sumtree",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum tree");

        // Insert items under both tree types in a single batch
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"normal".to_vec()],
                b"nkey".to_vec(),
                Element::new_item(b"nval".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"sumtree".to_vec()],
                b"skey".to_vec(),
                Element::new_sum_item(42),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch insert under different tree types should succeed");

        let normal_item = db
            .get([b"normal"].as_ref(), b"nkey", None, grove_version)
            .unwrap()
            .expect("item under normal tree");
        assert_eq!(normal_item, Element::new_item(b"nval".to_vec()));

        let sum_item = db
            .get([b"sumtree"].as_ref(), b"skey", None, grove_version)
            .unwrap()
            .expect("item under sum tree");
        assert_eq!(sum_item, Element::new_sum_item(42));
    }

    // ===================================================================
    // 11. Batch error: insert into non-existent path
    // ===================================================================

    #[test]
    fn test_batch_insert_into_nonexistent_path_fails() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Try to insert into a path where the parent tree does not exist
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"nonexistent_parent".to_vec()],
            b"key1".to_vec(),
            Element::new_item(b"value".to_vec()),
        )];

        let result = db.apply_batch(ops, None, None, grove_version).unwrap();
        assert!(
            result.is_err(),
            "inserting into a non-existent path should fail"
        );
    }

    #[test]
    fn test_batch_insert_deep_nonexistent_path_fails() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // TEST_LEAF exists, but TEST_LEAF/nonexistent does not
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"nonexistent".to_vec()],
            b"key1".to_vec(),
            Element::new_item(b"value".to_vec()),
        )];

        let result = db.apply_batch(ops, None, None, grove_version).unwrap();
        assert!(
            result.is_err(),
            "inserting into a deep non-existent path should fail"
        );
    }

    // ===================================================================
    // 12. Batch with transaction rollback
    // ===================================================================

    #[test]
    fn test_batch_transaction_rollback() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Start a transaction
        let tx = db.start_transaction();

        // Apply a batch within the transaction
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"tx_key".to_vec(),
            Element::new_item(b"tx_value".to_vec()),
        )];

        db.apply_batch(ops, None, Some(&tx), grove_version)
            .unwrap()
            .expect("batch in transaction should succeed");

        // Verify element exists within the transaction
        let in_tx_result = db
            .get([TEST_LEAF].as_ref(), b"tx_key", Some(&tx), grove_version)
            .unwrap()
            .expect("element should exist in transaction");
        assert_eq!(in_tx_result, Element::new_item(b"tx_value".to_vec()));

        // Rollback the transaction
        db.rollback_transaction(&tx)
            .expect("rollback should succeed");

        // Verify element does NOT exist outside the transaction
        let after_rollback = db
            .get([TEST_LEAF].as_ref(), b"tx_key", None, grove_version)
            .unwrap();
        assert!(
            after_rollback.is_err(),
            "element should not exist after transaction rollback"
        );
    }

    #[test]
    fn test_batch_transaction_commit() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Start a transaction
        let tx = db.start_transaction();

        // Apply a batch within the transaction
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"commit_key".to_vec(),
            Element::new_item(b"commit_value".to_vec()),
        )];

        db.apply_batch(ops, None, Some(&tx), grove_version)
            .unwrap()
            .expect("batch in transaction should succeed");

        // Commit the transaction
        db.commit_transaction(tx)
            .unwrap()
            .expect("commit should succeed");

        // Verify element exists after commit (no transaction)
        let result = db
            .get([TEST_LEAF].as_ref(), b"commit_key", None, grove_version)
            .unwrap()
            .expect("element should exist after commit");
        assert_eq!(result, Element::new_item(b"commit_value".to_vec()));
    }

    // ===================================================================
    // 14. GroveOp ordering tests
    // ===================================================================

    #[test]
    fn test_grove_op_ordering() {
        // DeleteTree = 0, Delete = 2, InsertOrReplace = 8, InsertOnly = 9
        let delete_tree = GroveOp::DeleteTree(TreeType::NormalTree);
        let delete = GroveOp::Delete;
        let insert_or_replace = GroveOp::InsertOrReplace {
            element: Element::new_item(b"test".to_vec()),
        };
        let insert_only = GroveOp::InsertOnly {
            element: Element::new_item(b"test".to_vec()),
        };

        assert!(
            delete_tree < delete,
            "DeleteTree (0) should be less than Delete (2)"
        );
        assert!(
            delete < insert_or_replace,
            "Delete (2) should be less than InsertOrReplace (8)"
        );
        assert!(
            insert_or_replace < insert_only,
            "InsertOrReplace (8) should be less than InsertOnly (9)"
        );
    }

    // ===================================================================
    // 15. KnownKeysPath tests
    // ===================================================================

    // Note: KnownKeysPath has a private inner field so we test its behavior
    // indirectly through KeyInfoPath comparisons and via the batch system.

    // ===================================================================
    // 16. Empty batch is a no-op
    // ===================================================================

    #[test]
    fn test_apply_batch_empty_is_noop() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Apply empty batch -- should succeed trivially
        db.apply_batch(vec![], None, None, grove_version)
            .unwrap()
            .expect("empty batch should succeed");
    }

    // ===================================================================
    // 17. Batch InsertOnly for new elements
    // ===================================================================

    #[test]
    fn test_batch_insert_only_new_elements() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let ops = vec![
            QualifiedGroveDbOp::insert_only_op(
                vec![TEST_LEAF.to_vec()],
                b"only1".to_vec(),
                Element::new_item(b"val1".to_vec()),
            ),
            QualifiedGroveDbOp::insert_only_op(
                vec![TEST_LEAF.to_vec()],
                b"only2".to_vec(),
                Element::new_item(b"val2".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("insert only for new elements should succeed");

        let v1 = db
            .get([TEST_LEAF].as_ref(), b"only1", None, grove_version)
            .unwrap()
            .expect("element 1 should exist");
        assert_eq!(v1, Element::new_item(b"val1".to_vec()));

        let v2 = db
            .get([TEST_LEAF].as_ref(), b"only2", None, grove_version)
            .unwrap()
            .expect("element 2 should exist");
        assert_eq!(v2, Element::new_item(b"val2".to_vec()));
    }

    // ===================================================================
    // 18. BatchApplyOptions: disable_operation_consistency_check
    // ===================================================================

    #[test]
    fn test_batch_apply_options_disable_consistency_check() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Create duplicate operations (same path+key, same op) which normally
        // fail consistency. With consistency check disabled, from_ops still
        // processes them (last one wins for same key in BTreeMap).
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"dup_key".to_vec(),
                Element::new_item(b"first".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"dup_key".to_vec(),
                Element::new_item(b"second".to_vec()),
            ),
        ];

        let options = Some(BatchApplyOptions {
            disable_operation_consistency_check: true,
            ..Default::default()
        });

        // This should succeed because consistency check is disabled
        db.apply_batch(ops, options, None, grove_version)
            .unwrap()
            .expect("batch with disabled consistency check should succeed");

        // Verify the element (last-writer-wins in BTreeMap)
        let result = db
            .get([TEST_LEAF].as_ref(), b"dup_key", None, grove_version)
            .unwrap()
            .expect("element should exist");
        // Both ops have the same GroveOp discriminant (InsertOrReplace = 8),
        // so BTreeMap ordering depends on the GroveOp Ord implementation.
        // Since they have the same to_u8(), the last inserted wins in the
        // iteration. The actual value depends on implementation details.
        assert!(
            result == Element::new_item(b"first".to_vec())
                || result == Element::new_item(b"second".to_vec()),
            "one of the duplicate values should be present"
        );
    }

    // ===================================================================
    // 19. Batch creating nested trees
    // ===================================================================

    #[test]
    fn test_batch_create_nested_trees() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Create parent and child tree in a single batch
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
                b"item".to_vec(),
                Element::new_item(b"deep_value".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with nested trees should succeed");

        // Verify the deeply nested item
        let result = db
            .get(
                [b"parent".as_ref(), b"child".as_ref()].as_ref(),
                b"item",
                None,
                grove_version,
            )
            .unwrap()
            .expect("deeply nested item should exist");
        assert_eq!(result, Element::new_item(b"deep_value".to_vec()));
    }

    // ===================================================================
    // 20. Batch delete and re-insert in separate batches
    // ===================================================================

    #[test]
    fn test_batch_delete_then_reinsert() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"toggle_key",
            Element::new_item(b"original".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert original item");

        // Delete it via batch
        let ops = vec![QualifiedGroveDbOp::delete_op(
            vec![TEST_LEAF.to_vec()],
            b"toggle_key".to_vec(),
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch delete should succeed");

        // Verify deletion
        let result = db
            .get([TEST_LEAF].as_ref(), b"toggle_key", None, grove_version)
            .unwrap();
        assert!(result.is_err(), "item should be deleted");

        // Re-insert via batch
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"toggle_key".to_vec(),
            Element::new_item(b"reinserted".to_vec()),
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch re-insert should succeed");

        let result = db
            .get([TEST_LEAF].as_ref(), b"toggle_key", None, grove_version)
            .unwrap()
            .expect("re-inserted item should exist");
        assert_eq!(result, Element::new_item(b"reinserted".to_vec()));
    }

    // ===================================================================
    // 21. Batch with many operations at same path
    // ===================================================================

    #[test]
    fn test_batch_many_inserts_same_path() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let ops: Vec<QualifiedGroveDbOp> = (0..50)
            .map(|i| {
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    format!("key_{:03}", i).into_bytes(),
                    Element::new_item(format!("val_{:03}", i).into_bytes()),
                )
            })
            .collect();

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with many inserts should succeed");

        // Spot-check a few
        for i in [0, 25, 49] {
            let key = format!("key_{:03}", i);
            let expected_val = format!("val_{:03}", i);
            let result = db
                .get([TEST_LEAF].as_ref(), key.as_bytes(), None, grove_version)
                .unwrap()
                .expect("element should exist");
            assert_eq!(
                result,
                Element::new_item(expected_val.into_bytes()),
                "element at {} should match",
                key
            );
        }
    }

    // ===================================================================
    // 22. Batch with Delete + Insert for different keys at same path
    // ===================================================================

    #[test]
    fn test_batch_mixed_delete_and_insert_different_keys() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Pre-insert some items
        db.insert(
            [TEST_LEAF].as_ref(),
            b"keep",
            Element::new_item(b"keep_val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert keep item");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"remove",
            Element::new_item(b"remove_val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert remove item");

        // Batch: delete one key, insert another, at the same path
        let ops = vec![
            QualifiedGroveDbOp::delete_op(vec![TEST_LEAF.to_vec()], b"remove".to_vec()),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"new_key".to_vec(),
                Element::new_item(b"new_val".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with mixed delete and insert should succeed");

        // Verify
        let keep = db
            .get([TEST_LEAF].as_ref(), b"keep", None, grove_version)
            .unwrap()
            .expect("keep should still exist");
        assert_eq!(keep, Element::new_item(b"keep_val".to_vec()));

        let removed = db
            .get([TEST_LEAF].as_ref(), b"remove", None, grove_version)
            .unwrap();
        assert!(removed.is_err(), "remove should be gone");

        let new = db
            .get([TEST_LEAF].as_ref(), b"new_key", None, grove_version)
            .unwrap()
            .expect("new_key should exist");
        assert_eq!(new, Element::new_item(b"new_val".to_vec()));
    }

    // ===================================================================
    // 23. Batch: SumTree + SumItem propagation (occupied entry path)
    // ===================================================================

    #[test]
    fn test_batch_insert_sum_tree_and_sum_items_same_batch() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Insert a SumTree at root and SumItems under it in the same batch.
        // This exercises the Element::SumTree occupied-entry propagation arm.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"my_sum_tree".to_vec(),
                Element::empty_sum_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"my_sum_tree".to_vec()],
                b"s1".to_vec(),
                Element::new_sum_item(10),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"my_sum_tree".to_vec()],
                b"s2".to_vec(),
                Element::new_sum_item(20),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with sum tree + sum items should succeed");

        let s1 = db
            .get([b"my_sum_tree"].as_ref(), b"s1", None, grove_version)
            .unwrap()
            .expect("s1 should exist");
        assert_eq!(s1, Element::new_sum_item(10));

        let s2 = db
            .get([b"my_sum_tree"].as_ref(), b"s2", None, grove_version)
            .unwrap()
            .expect("s2 should exist");
        assert_eq!(s2, Element::new_sum_item(20));
    }

    // ===================================================================
    // 24. Batch: BigSumTree propagation (occupied entry path)
    // ===================================================================

    #[test]
    fn test_batch_insert_big_sum_tree_and_items_same_batch() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Insert a BigSumTree and items under it in the same batch.
        // This exercises the Element::BigSumTree occupied-entry propagation arm.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"big_sum".to_vec(),
                Element::empty_big_sum_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"big_sum".to_vec()],
                b"bs1".to_vec(),
                Element::new_sum_item(100),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with big sum tree should succeed");

        let item = db
            .get([b"big_sum"].as_ref(), b"bs1", None, grove_version)
            .unwrap()
            .expect("bs1 should exist under big sum tree");
        assert_eq!(item, Element::new_sum_item(100));
    }

    // ===================================================================
    // 25. Batch: CountTree propagation (occupied entry path)
    // ===================================================================

    #[test]
    fn test_batch_insert_count_tree_and_items_same_batch() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Insert a CountTree and items under it in the same batch.
        // This exercises the Element::CountTree occupied-entry propagation arm.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"count_tree".to_vec(),
                Element::empty_count_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"count_tree".to_vec()],
                b"c1".to_vec(),
                Element::new_item(b"val1".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"count_tree".to_vec()],
                b"c2".to_vec(),
                Element::new_item(b"val2".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with count tree should succeed");

        let c1 = db
            .get([b"count_tree"].as_ref(), b"c1", None, grove_version)
            .unwrap()
            .expect("c1 should exist");
        assert_eq!(c1, Element::new_item(b"val1".to_vec()));
    }

    // ===================================================================
    // 26. Batch: CountSumTree propagation (occupied entry path)
    // ===================================================================

    #[test]
    fn test_batch_insert_count_sum_tree_and_items_same_batch() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Insert a CountSumTree and SumItems under it in the same batch.
        // This exercises the Element::CountSumTree occupied-entry propagation arm.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"count_sum".to_vec(),
                Element::empty_count_sum_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"count_sum".to_vec()],
                b"cs1".to_vec(),
                Element::new_sum_item(5),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"count_sum".to_vec()],
                b"cs2".to_vec(),
                Element::new_sum_item(15),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with count sum tree should succeed");

        let cs1 = db
            .get([b"count_sum"].as_ref(), b"cs1", None, grove_version)
            .unwrap()
            .expect("cs1 should exist");
        assert_eq!(cs1, Element::new_sum_item(5));
    }

    // ===================================================================
    // 27. Batch: ProvableCountTree propagation (occupied entry path)
    // ===================================================================

    #[test]
    fn test_batch_insert_provable_count_tree_and_items_same_batch() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Insert a ProvableCountTree and items under it in the same batch.
        // This exercises the Element::ProvableCountTree occupied-entry propagation arm.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"provable_count".to_vec(),
                Element::empty_provable_count_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"provable_count".to_vec()],
                b"pc1".to_vec(),
                Element::new_item(b"pval1".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with provable count tree should succeed");

        let pc1 = db
            .get([b"provable_count"].as_ref(), b"pc1", None, grove_version)
            .unwrap()
            .expect("pc1 should exist");
        assert_eq!(pc1, Element::new_item(b"pval1".to_vec()));
    }

    // ===================================================================
    // 28. Batch: ProvableCountSumTree propagation (occupied entry path)
    // ===================================================================

    #[test]
    fn test_batch_insert_provable_count_sum_tree_and_items_same_batch() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Insert a ProvableCountSumTree and SumItems under it in the same batch.
        // This exercises the ProvableCountSumTree occupied-entry propagation arm.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"prov_count_sum".to_vec(),
                Element::empty_provable_count_sum_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"prov_count_sum".to_vec()],
                b"pcs1".to_vec(),
                Element::new_sum_item(7),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with provable count sum tree should succeed");

        let pcs1 = db
            .get([b"prov_count_sum"].as_ref(), b"pcs1", None, grove_version)
            .unwrap()
            .expect("pcs1 should exist");
        assert_eq!(pcs1, Element::new_sum_item(7));
    }

    // ===================================================================
    // 29. Batch: Replace operation on existing item
    // ===================================================================

    #[test]
    fn test_batch_replace_existing_item() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert initial item
        db.insert(
            [TEST_LEAF].as_ref(),
            b"replace_me",
            Element::new_item(b"old_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert initial item for replace");

        // Use the Replace op variant in batch
        let ops = vec![QualifiedGroveDbOp::replace_op(
            vec![TEST_LEAF.to_vec()],
            b"replace_me".to_vec(),
            Element::new_item(b"new_value".to_vec()),
        )];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch replace should succeed");

        let result = db
            .get([TEST_LEAF].as_ref(), b"replace_me", None, grove_version)
            .unwrap()
            .expect("replaced element should exist");
        assert_eq!(
            result,
            Element::new_item(b"new_value".to_vec()),
            "element should have the replaced value"
        );
    }

    // ===================================================================
    // 30. Batch: InsertOnly when element already exists (with validation)
    // ===================================================================

    #[test]
    fn test_batch_insert_only_existing_element_with_validation_fails() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an item first
        db.insert(
            [TEST_LEAF].as_ref(),
            b"existing",
            Element::new_item(b"original".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert existing item");

        // Try InsertOnly on the same key with validate_insertion_does_not_override
        let ops = vec![QualifiedGroveDbOp::insert_only_op(
            vec![TEST_LEAF.to_vec()],
            b"existing".to_vec(),
            Element::new_item(b"should_fail".to_vec()),
        )];

        let options = Some(BatchApplyOptions {
            validate_insertion_does_not_override: true,
            ..Default::default()
        });

        let result = db.apply_batch(ops, options, None, grove_version).unwrap();
        assert!(
            result.is_err(),
            "InsertOnly on existing element with validation should fail"
        );
    }

    // ===================================================================
    // 30b. InsertOnly enforces no-overwrite even without batch option
    // ===================================================================

    #[test]
    fn test_batch_insert_only_rejects_overwrite_without_explicit_validation_option() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an item first
        db.insert(
            [TEST_LEAF].as_ref(),
            b"existing",
            Element::new_item(b"original".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert existing item");

        // InsertOnly should reject overwrite even with default batch options
        // (validate_insertion_does_not_override = false)
        let ops = vec![QualifiedGroveDbOp::insert_only_op(
            vec![TEST_LEAF.to_vec()],
            b"existing".to_vec(),
            Element::new_item(b"should_fail".to_vec()),
        )];

        let result = db.apply_batch(ops, None, None, grove_version).unwrap();
        assert!(
            result.is_err(),
            "InsertOnly should reject overwrite regardless of batch options"
        );

        // Verify original value is preserved
        let val = db
            .get([TEST_LEAF].as_ref(), b"existing", None, grove_version)
            .unwrap()
            .expect("should get element");
        assert_eq!(val, Element::new_item(b"original".to_vec()));
    }

    // ===================================================================
    // 31. Batch: RefreshReference with trust_refresh_reference = false
    // ===================================================================

    #[test]
    fn test_batch_refresh_reference_untrusted() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert target and reference
        db.insert(
            [TEST_LEAF].as_ref(),
            b"target_untrust",
            Element::new_item(b"target_val".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert target");

        let ref_path = ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"target_untrust".to_vec(),
        ]);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref_untrust",
            Element::new_reference(ref_path.clone()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert reference");

        // RefreshReference with trust_refresh_reference = false.
        // This triggers reading the element from disk to verify.
        let ops = vec![QualifiedGroveDbOp::refresh_reference_op(
            vec![TEST_LEAF.to_vec()],
            b"ref_untrust".to_vec(),
            ref_path,
            Some(5),
            None,
            false, // untrusted: read from disk
        )];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("untrusted refresh reference should succeed");

        // Verify the reference still resolves
        let result = db
            .get([TEST_LEAF].as_ref(), b"ref_untrust", None, grove_version)
            .unwrap()
            .expect("reference should still resolve after untrusted refresh");
        assert_eq!(result, Element::new_item(b"target_val".to_vec()));
    }

    // ===================================================================
    // 32. Batch with element flags and flag update callback
    // ===================================================================

    #[test]
    fn test_batch_with_element_flags_update_callback() {
        use grovedb_costs::storage_cost::removal::StorageRemovedBytes::BasicStorageRemoval;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an item with flags
        let flags = Some(vec![1, 2, 3]);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"flagged",
            Element::new_item_with_flags(b"old_val".to_vec(), flags),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert flagged item");

        // Replace with a new element that has flags, using a flag update callback
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"flagged".to_vec(),
            Element::new_item_with_flags(b"new_val".to_vec(), Some(vec![4, 5, 6])),
        )];

        let mut callback_called = false;
        db.apply_batch_with_element_flags_update(
            ops,
            None,
            |_cost, _old_flags, _new_flags| {
                callback_called = true;
                Ok(false) // do not change the flags
            },
            |_flags, key_bytes_to_remove, value_bytes_to_remove| {
                Ok((
                    BasicStorageRemoval(key_bytes_to_remove),
                    BasicStorageRemoval(value_bytes_to_remove),
                ))
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("batch with flag update callback should succeed");

        assert!(callback_called, "flag update callback should be invoked");

        // Verify the item was updated
        let result = db
            .get([TEST_LEAF].as_ref(), b"flagged", None, grove_version)
            .unwrap()
            .expect("flagged element should exist");
        assert_eq!(
            result,
            Element::new_item_with_flags(b"new_val".to_vec(), Some(vec![4, 5, 6])),
            "element should have new value and flags"
        );
    }

    // ===================================================================
    // 33. Batch with base_root_storage_is_free = false
    // ===================================================================

    #[test]
    fn test_batch_base_root_storage_not_free() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an item with base_root_storage_is_free = false
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"paid_root".to_vec(),
            Element::new_item(b"value".to_vec()),
        )];

        let options = Some(BatchApplyOptions {
            base_root_storage_is_free: false,
            ..Default::default()
        });

        db.apply_batch(ops, options, None, grove_version)
            .unwrap()
            .expect("batch with paid root storage should succeed");

        let result = db
            .get([TEST_LEAF].as_ref(), b"paid_root", None, grove_version)
            .unwrap()
            .expect("element should exist");
        assert_eq!(result, Element::new_item(b"value".to_vec()));
    }

    // ===================================================================
    // 34. Batch: insert tree then items under it, using InsertOnly
    // ===================================================================

    #[test]
    fn test_batch_insert_only_tree_then_items_same_batch() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // InsertOnly for tree and items. The tree insert triggers the
        // occupied-entry propagation path for InsertOnly variant.
        let ops = vec![
            QualifiedGroveDbOp::insert_only_op(vec![], b"new_tree".to_vec(), Element::empty_tree()),
            QualifiedGroveDbOp::insert_only_op(
                vec![b"new_tree".to_vec()],
                b"item1".to_vec(),
                Element::new_item(b"v1".to_vec()),
            ),
            QualifiedGroveDbOp::insert_only_op(
                vec![b"new_tree".to_vec()],
                b"item2".to_vec(),
                Element::new_item(b"v2".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("insert only batch with tree + items should succeed");

        let i1 = db
            .get([b"new_tree"].as_ref(), b"item1", None, grove_version)
            .unwrap()
            .expect("item1 should exist");
        assert_eq!(i1, Element::new_item(b"v1".to_vec()));

        let i2 = db
            .get([b"new_tree"].as_ref(), b"item2", None, grove_version)
            .unwrap()
            .expect("item2 should exist");
        assert_eq!(i2, Element::new_item(b"v2".to_vec()));
    }

    // ===================================================================
    // 35. Batch: apply_partial_batch simple test
    // ===================================================================

    #[test]
    fn test_apply_partial_batch_simple() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // apply_partial_batch allows adding operations after the initial batch
        // pauses at a given height.
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"partial_key".to_vec(),
            Element::new_item(b"partial_value".to_vec()),
        )];

        db.apply_partial_batch(
            ops,
            None,
            |_cost, _leftover| Ok(vec![]), // no additional operations
            None,
            grove_version,
        )
        .unwrap()
        .expect("apply_partial_batch should succeed");

        let result = db
            .get([TEST_LEAF].as_ref(), b"partial_key", None, grove_version)
            .unwrap()
            .expect("element should exist after partial batch");
        assert_eq!(result, Element::new_item(b"partial_value".to_vec()));
    }

    // ===================================================================
    // 36. Batch: apply_partial_batch with add-on operations
    // ===================================================================

    #[test]
    fn test_apply_partial_batch_with_add_on_operations() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Initial batch inserts an item
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"partial_key1".to_vec(),
            Element::new_item(b"val1".to_vec()),
        )];

        let mut addon_called = false;
        db.apply_partial_batch(
            ops,
            None,
            |_cost, _leftover| {
                addon_called = true;
                // Return an add-on operation that inserts a second item
                Ok(vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"addon_key".to_vec(),
                    Element::new_item(b"addon_val".to_vec()),
                )])
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("apply_partial_batch with add-on ops should succeed");

        assert!(addon_called, "add-on operations callback should be invoked");

        let addon_item = db
            .get([TEST_LEAF].as_ref(), b"addon_key", None, grove_version)
            .unwrap()
            .expect("addon_key from add-on operation should exist");
        assert_eq!(addon_item, Element::new_item(b"addon_val".to_vec()));
    }

    // ===================================================================
    // 37. Batch: consistency check detects internal ReplaceNonMerkTreeRoot
    //     when submitted by user
    // ===================================================================

    #[test]
    fn test_batch_consistency_rejects_replace_non_merk_tree_root() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let op = QualifiedGroveDbOp {
            path: KeyInfoPath(vec![]),
            key: Some(KnownKey(b"test".to_vec())),
            op: GroveOp::ReplaceNonMerkTreeRoot {
                hash: [0u8; 32],
                meta: NonMerkTreeMeta::CommitmentTree {
                    total_count: 0,
                    chunk_power: 4,
                },
            },
        };

        // With consistency check enabled (default), this should fail
        let result = db.apply_batch(vec![op], None, None, grove_version).unwrap();
        assert!(
            result.is_err(),
            "ReplaceNonMerkTreeRoot should fail consistency check"
        );
    }

    // ===================================================================
    // 38. Batch: transactional batch with SumTree propagation
    // ===================================================================

    #[test]
    fn test_batch_transaction_sum_tree_propagation() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let tx = db.start_transaction();

        // Create sum tree + items in a batch within a transaction
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"tx_sum".to_vec(),
                Element::empty_sum_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"tx_sum".to_vec()],
                b"ts1".to_vec(),
                Element::new_sum_item(50),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"tx_sum".to_vec()],
                b"ts2".to_vec(),
                Element::new_sum_item(75),
            ),
        ];

        db.apply_batch(ops, None, Some(&tx), grove_version)
            .unwrap()
            .expect("transactional batch with sum tree should succeed");

        // Verify within transaction
        let ts1 = db
            .get([b"tx_sum"].as_ref(), b"ts1", Some(&tx), grove_version)
            .unwrap()
            .expect("ts1 should exist in tx");
        assert_eq!(ts1, Element::new_sum_item(50));

        // Commit and verify outside transaction
        db.commit_transaction(tx)
            .unwrap()
            .expect("commit should succeed");

        let ts2 = db
            .get([b"tx_sum"].as_ref(), b"ts2", None, grove_version)
            .unwrap()
            .expect("ts2 should exist after commit");
        assert_eq!(ts2, Element::new_sum_item(75));
    }

    // ===================================================================
    // 39. Batch: deeply nested tree propagation (3 levels)
    // ===================================================================

    #[test]
    fn test_batch_deep_nested_propagation() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Create 3 levels of nesting in a single batch to exercise multi-level
        // propagation. Each level up must propagate root hashes.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(vec![], b"l1".to_vec(), Element::empty_tree()),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"l1".to_vec()],
                b"l2".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"l1".to_vec(), b"l2".to_vec()],
                b"l3".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"l1".to_vec(), b"l2".to_vec(), b"l3".to_vec()],
                b"deep_item".to_vec(),
                Element::new_item(b"deep_value".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("deep nested batch should succeed");

        let deep = db
            .get(
                [b"l1".as_ref(), b"l2".as_ref(), b"l3".as_ref()].as_ref(),
                b"deep_item",
                None,
                grove_version,
            )
            .unwrap()
            .expect("deep item should exist");
        assert_eq!(deep, Element::new_item(b"deep_value".to_vec()));
    }

    // ===================================================================
    // 40. Batch: SumTree with existing items then update via Replace
    // ===================================================================

    #[test]
    fn test_batch_replace_sum_item_in_existing_sum_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Create sum tree with initial items
        db.insert(
            EMPTY_PATH,
            b"sum_replace",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum tree");

        db.insert(
            [b"sum_replace"].as_ref(),
            b"sr1",
            Element::new_sum_item(10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum item");

        // Replace the sum item via batch
        let ops = vec![QualifiedGroveDbOp::replace_op(
            vec![b"sum_replace".to_vec()],
            b"sr1".to_vec(),
            Element::new_sum_item(99),
        )];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch replace sum item should succeed");

        let result = db
            .get([b"sum_replace"].as_ref(), b"sr1", None, grove_version)
            .unwrap()
            .expect("replaced sum item should exist");
        assert_eq!(result, Element::new_sum_item(99));
    }

    // ===================================================================
    // 41. Batch: insert reference to item that exists on disk (not in batch)
    // ===================================================================

    #[test]
    fn test_batch_insert_reference_to_preexisting_item() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert target item outside the batch
        db.insert(
            [TEST_LEAF].as_ref(),
            b"preexisting_target",
            Element::new_item(b"target_data".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert preexisting target");

        // Insert a reference to it via batch
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"ref_to_preexisting".to_vec(),
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"preexisting_target".to_vec(),
            ])),
        )];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch reference to preexisting item should succeed");

        let result = db
            .get(
                [TEST_LEAF].as_ref(),
                b"ref_to_preexisting",
                None,
                grove_version,
            )
            .unwrap()
            .expect("reference should resolve");
        assert_eq!(result, Element::new_item(b"target_data".to_vec()));
    }

    // ===================================================================
    // 42. Batch: Patch on sum item in sum tree
    // ===================================================================

    #[test]
    fn test_batch_patch_sum_item() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Create sum tree with an item
        db.insert(
            EMPTY_PATH,
            b"patch_sum",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum tree");

        db.insert(
            [b"patch_sum"].as_ref(),
            b"ps1",
            Element::new_sum_item(10),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum item");

        // Patch the sum item
        let ops = vec![QualifiedGroveDbOp::patch_op(
            vec![b"patch_sum".to_vec()],
            b"ps1".to_vec(),
            Element::new_sum_item(25),
            0, // no byte change (sum items are fixed size)
        )];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch patch sum item should succeed");

        let result = db
            .get([b"patch_sum"].as_ref(), b"ps1", None, grove_version)
            .unwrap()
            .expect("patched sum item should exist");
        assert_eq!(result, Element::new_sum_item(25));
    }

    // ===================================================================
    // 43. Batch: delete item in batch (Delete path in execute_ops_on_path)
    // ===================================================================

    #[test]
    fn test_batch_delete_item_exercises_delete_path() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert multiple items
        for i in 0..5 {
            db.insert(
                [TEST_LEAF].as_ref(),
                format!("del_{}", i).as_bytes(),
                Element::new_item(format!("val_{}", i).into_bytes()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert item for deletion");
        }

        // Delete some items via batch
        let ops = vec![
            QualifiedGroveDbOp::delete_op(vec![TEST_LEAF.to_vec()], b"del_1".to_vec()),
            QualifiedGroveDbOp::delete_op(vec![TEST_LEAF.to_vec()], b"del_3".to_vec()),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch delete should succeed");

        // Verify deleted items are gone
        assert!(
            db.get([TEST_LEAF].as_ref(), b"del_1", None, grove_version)
                .unwrap()
                .is_err(),
            "del_1 should be gone"
        );
        assert!(
            db.get([TEST_LEAF].as_ref(), b"del_3", None, grove_version)
                .unwrap()
                .is_err(),
            "del_3 should be gone"
        );

        // Verify remaining items still exist
        assert!(
            db.get([TEST_LEAF].as_ref(), b"del_0", None, grove_version)
                .unwrap()
                .is_ok(),
            "del_0 should still exist"
        );
        assert!(
            db.get([TEST_LEAF].as_ref(), b"del_2", None, grove_version)
                .unwrap()
                .is_ok(),
            "del_2 should still exist"
        );
    }

    // ===================================================================
    // 44. Batch: DeleteTree from sum tree (SumTree tree_type)
    // ===================================================================

    #[test]
    fn test_batch_delete_tree_under_sum_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Create a sum tree with a subtree under it
        db.insert(
            EMPTY_PATH,
            b"parent_sum",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert parent sum tree");

        db.insert(
            [b"parent_sum"].as_ref(),
            b"child_tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert child tree under sum tree");

        // Delete the child tree via batch
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![b"parent_sum".to_vec()],
            b"child_tree".to_vec(),
            TreeType::NormalTree,
        )];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch delete tree under sum tree should succeed");

        assert!(
            db.get([b"parent_sum"].as_ref(), b"child_tree", None, grove_version,)
                .unwrap()
                .is_err(),
            "child tree should be deleted"
        );
    }

    // ===================================================================
    // 45. Batch: insert tree with flags (ElementFlags propagation)
    // ===================================================================

    #[test]
    fn test_batch_insert_tree_with_flags_and_items() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Insert a tree with flags and items below it in the same batch.
        // This tests that flags are preserved during propagation.
        let tree_flags = Some(vec![0xAA, 0xBB]);
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"flagged_tree".to_vec(),
                Element::new_tree_with_flags(None, tree_flags.clone()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"flagged_tree".to_vec()],
                b"child_item".to_vec(),
                Element::new_item(b"child_val".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with flagged tree should succeed");

        // Verify tree exists with flags
        let tree_elem = db
            .get(EMPTY_PATH, b"flagged_tree", None, grove_version)
            .unwrap()
            .expect("flagged tree should exist");
        assert_eq!(
            *tree_elem.get_flags(),
            tree_flags,
            "tree flags should be preserved"
        );

        // Verify child item
        let child = db
            .get(
                [b"flagged_tree"].as_ref(),
                b"child_item",
                None,
                grove_version,
            )
            .unwrap()
            .expect("child item should exist");
        assert_eq!(child, Element::new_item(b"child_val".to_vec()));
    }

    // ===================================================================
    // 46. Batch: multiple operations across different root subtrees
    // ===================================================================

    #[test]
    fn test_batch_operations_across_multiple_root_subtrees() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // TEST_LEAF and ANOTHER_TEST_LEAF exist. Insert into both in same batch.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"k1".to_vec(),
                Element::new_item(b"v1".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![crate::tests::ANOTHER_TEST_LEAF.to_vec()],
                b"k2".to_vec(),
                Element::new_item(b"v2".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch across multiple root subtrees should succeed");

        let v1 = db
            .get([TEST_LEAF].as_ref(), b"k1", None, grove_version)
            .unwrap()
            .expect("k1 should exist under TEST_LEAF");
        assert_eq!(v1, Element::new_item(b"v1".to_vec()));

        let v2 = db
            .get(
                [crate::tests::ANOTHER_TEST_LEAF].as_ref(),
                b"k2",
                None,
                grove_version,
            )
            .unwrap()
            .expect("k2 should exist under ANOTHER_TEST_LEAF");
        assert_eq!(v2, Element::new_item(b"v2".to_vec()));
    }

    // ===================================================================
    // 47. Batch: SumTree with flags + items (flag propagation for SumTree)
    // ===================================================================

    #[test]
    fn test_batch_sum_tree_with_flags_and_items() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        let sum_tree_flags = Some(vec![0x11, 0x22]);
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"flagged_sum".to_vec(),
                Element::empty_sum_tree_with_flags(sum_tree_flags.clone()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"flagged_sum".to_vec()],
                b"fs1".to_vec(),
                Element::new_sum_item(42),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with flagged sum tree should succeed");

        let sum_tree_elem = db
            .get(EMPTY_PATH, b"flagged_sum", None, grove_version)
            .unwrap()
            .expect("flagged sum tree should exist");
        assert_eq!(
            *sum_tree_elem.get_flags(),
            sum_tree_flags,
            "sum tree flags should be preserved"
        );

        let fs1 = db
            .get([b"flagged_sum"].as_ref(), b"fs1", None, grove_version)
            .unwrap()
            .expect("fs1 should exist");
        assert_eq!(fs1, Element::new_sum_item(42));
    }

    // ===================================================================
    // 48. Batch: insert reference and refresh reference in same db
    // ===================================================================

    #[test]
    fn test_batch_insert_and_update_target_with_reference() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Set up initial state: target item + reference
        db.insert(
            [TEST_LEAF].as_ref(),
            b"target_upd",
            Element::new_item(b"initial".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert target");

        db.insert(
            [TEST_LEAF].as_ref(),
            b"ref_upd",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"target_upd".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert reference");

        // In a batch: update the target and refresh the reference.
        // This exercises the follow_reference_get_value_hash code path where
        // the referenced element changes in the same batch.
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"target_upd".to_vec(),
                Element::new_item(b"updated".to_vec()),
            ),
            QualifiedGroveDbOp::refresh_reference_op(
                vec![TEST_LEAF.to_vec()],
                b"ref_upd".to_vec(),
                ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"target_upd".to_vec(),
                ]),
                Some(5),
                None,
                true,
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch update target + refresh ref should succeed");

        // The reference should now resolve to the updated value
        let result = db
            .get([TEST_LEAF].as_ref(), b"ref_upd", None, grove_version)
            .unwrap()
            .expect("reference should resolve after batch update");
        assert_eq!(result, Element::new_item(b"updated".to_vec()));
    }

    // ===================================================================
    // 50. Batch: flag update callback that actually modifies flags
    // ===================================================================

    #[test]
    fn test_batch_flag_update_callback_modifies_flags() {
        use grovedb_costs::storage_cost::removal::StorageRemovedBytes::BasicStorageRemoval;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an item with initial flags
        db.insert(
            [TEST_LEAF].as_ref(),
            b"flag_mod",
            Element::new_item_with_flags(b"data".to_vec(), Some(vec![1, 0, 0])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item with flags");

        // Replace with new flags, using a callback that modifies them
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"flag_mod".to_vec(),
            Element::new_item_with_flags(b"data2".to_vec(), Some(vec![2, 0, 0])),
        )];

        db.apply_batch_with_element_flags_update(
            ops,
            None,
            |_cost, old_flags, new_flags| {
                // Merge: copy the second byte from old flags
                if let Some(old) = old_flags {
                    if old.len() >= 2 && new_flags.len() >= 2 {
                        new_flags[1] = old[1].wrapping_add(1);
                    }
                }
                Ok(true) // indicate flags changed
            },
            |_flags, key_bytes_to_remove, value_bytes_to_remove| {
                Ok((
                    BasicStorageRemoval(key_bytes_to_remove),
                    BasicStorageRemoval(value_bytes_to_remove),
                ))
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("batch with modifying flag callback should succeed");

        // Verify the flags were modified by the callback
        let result = db
            .get([TEST_LEAF].as_ref(), b"flag_mod", None, grove_version)
            .unwrap()
            .expect("flag_mod element should exist");

        let result_flags = result.get_flags().clone();
        assert!(
            result_flags.is_some(),
            "element should have flags after callback"
        );
        let flags = result_flags.expect("just verified is_some");
        assert_eq!(flags[0], 2, "first byte should be from new flags");
        assert_eq!(
            flags[1], 1,
            "second byte should be modified by callback (0+1)"
        );
    }

    // ===================================================================
    // 52. Batch: insert items then delete some in same batch
    //     (exercises mixed ops at same path)
    // ===================================================================

    #[test]
    fn test_batch_insert_and_delete_separate_keys_same_path() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Pre-insert items
        db.insert(
            [TEST_LEAF].as_ref(),
            b"stay1",
            Element::new_item(b"s1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert stay1");

        db.insert(
            [TEST_LEAF].as_ref(),
            b"go1",
            Element::new_item(b"g1".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert go1");

        // Batch: insert new + delete old + replace existing
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"new1".to_vec(),
                Element::new_item(b"n1".to_vec()),
            ),
            QualifiedGroveDbOp::delete_op(vec![TEST_LEAF.to_vec()], b"go1".to_vec()),
            QualifiedGroveDbOp::replace_op(
                vec![TEST_LEAF.to_vec()],
                b"stay1".to_vec(),
                Element::new_item(b"s1_updated".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("mixed batch should succeed");

        assert!(
            db.get([TEST_LEAF].as_ref(), b"go1", None, grove_version)
                .unwrap()
                .is_err(),
            "go1 should be deleted"
        );

        let stay = db
            .get([TEST_LEAF].as_ref(), b"stay1", None, grove_version)
            .unwrap()
            .expect("stay1 should exist");
        assert_eq!(stay, Element::new_item(b"s1_updated".to_vec()));

        let new1 = db
            .get([TEST_LEAF].as_ref(), b"new1", None, grove_version)
            .unwrap()
            .expect("new1 should exist");
        assert_eq!(new1, Element::new_item(b"n1".to_vec()));
    }

    // ===================================================================
    // 53. Batch: insert tree then replace with items (Patch variant)
    // ===================================================================

    #[test]
    fn test_batch_patch_item_with_byte_change() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert initial item with known size
        db.insert(
            [TEST_LEAF].as_ref(),
            b"patch_bytes",
            Element::new_item(b"short".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert initial item");

        // Patch to a longer value
        let new_val = b"a_much_longer_value_here".to_vec();
        let change = new_val.len() as i32 - b"short".len() as i32;

        let ops = vec![QualifiedGroveDbOp::patch_op(
            vec![TEST_LEAF.to_vec()],
            b"patch_bytes".to_vec(),
            Element::new_item(new_val.clone()),
            change,
        )];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch patch with byte change should succeed");

        let result = db
            .get([TEST_LEAF].as_ref(), b"patch_bytes", None, grove_version)
            .unwrap()
            .expect("patched element should exist");
        assert_eq!(result, Element::new_item(new_val));
    }

    // ===================================================================
    // 55. Batch: validate_insertion_does_not_override_tree option
    // ===================================================================

    #[test]
    fn test_batch_validate_insertion_does_not_override_tree() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert a tree at TEST_LEAF/subtree
        db.insert(
            [TEST_LEAF].as_ref(),
            b"subtree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert subtree");

        // Try to insert a non-tree element at the same key with
        // validate_insertion_does_not_override_tree = true.
        // This should fail because an existing tree cannot be overwritten
        // when this validation is enabled (matching the non-batch insert path).
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"subtree".to_vec(),
            Element::new_item(b"new_item".to_vec()),
        )];

        let options = Some(BatchApplyOptions {
            validate_insertion_does_not_override_tree: true,
            ..Default::default()
        });

        let result = db.apply_batch(ops, options, None, grove_version).unwrap();
        assert!(
            result.is_err(),
            "overwriting a tree should fail when validate_insertion_does_not_override_tree is set"
        );

        // Without the validation, it should succeed
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"subtree".to_vec(),
            Element::new_item(b"new_item".to_vec()),
        )];

        let options = Some(BatchApplyOptions {
            validate_insertion_does_not_override_tree: false,
            ..Default::default()
        });

        db.apply_batch(ops, options, None, grove_version)
            .unwrap()
            .expect("replacing a tree should succeed without validation");

        let result = db
            .get([TEST_LEAF].as_ref(), b"subtree", None, grove_version)
            .unwrap()
            .expect("element at 'subtree' should exist after replacement");
        assert_eq!(
            result,
            Element::new_item(b"new_item".to_vec()),
            "tree should have been replaced by item"
        );
    }

    // ===================================================================
    // 56. Batch: empty batch in transaction is a no-op
    // ===================================================================

    #[test]
    fn test_batch_empty_with_transaction() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let tx = db.start_transaction();

        // Empty batch in transaction should succeed trivially
        db.apply_batch(vec![], None, Some(&tx), grove_version)
            .unwrap()
            .expect("empty batch in transaction should succeed");

        db.commit_transaction(tx)
            .unwrap()
            .expect("commit empty transaction should succeed");
    }

    // ===================================================================
    // 57. Batch: InsertOrReplace on existing tree (tree -> tree overwrite)
    // ===================================================================

    #[test]
    fn test_batch_insert_or_replace_existing_tree_with_items() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // TEST_LEAF exists. Insert a subtree, then replace it via batch
        // with items underneath.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"repl_tree",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert initial subtree");

        db.insert(
            [TEST_LEAF, b"repl_tree"].as_ref(),
            b"old_item",
            Element::new_item(b"old".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item under subtree");

        // Replace the tree and insert a new item under it in the same batch
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"repl_tree".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"repl_tree".to_vec()],
                b"new_item".to_vec(),
                Element::new_item(b"new".to_vec()),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch replace tree with items should succeed");

        let new_item = db
            .get(
                [TEST_LEAF, b"repl_tree"].as_ref(),
                b"new_item",
                None,
                grove_version,
            )
            .unwrap()
            .expect("new_item should exist");
        assert_eq!(new_item, Element::new_item(b"new".to_vec()));
    }

    // ===================================================================
    // 58. Batch: apply_operations_without_batching with Replace variant
    // ===================================================================

    #[test]
    fn test_apply_operations_without_batching_with_replace_variant() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert initial
        db.insert(
            [TEST_LEAF].as_ref(),
            b"unbatched_repl",
            Element::new_item(b"old".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert initial");

        // Replace variant (GroveOp::Replace) is handled differently than
        // InsertOrReplace -- both go through the same insert path in
        // apply_operations_without_batching.
        let ops = vec![QualifiedGroveDbOp::replace_op(
            vec![TEST_LEAF.to_vec()],
            b"unbatched_repl".to_vec(),
            Element::new_item(b"replaced".to_vec()),
        )];

        db.apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap()
            .expect("unbatched replace should succeed");

        let result = db
            .get([TEST_LEAF].as_ref(), b"unbatched_repl", None, grove_version)
            .unwrap()
            .expect("element should exist");
        assert_eq!(result, Element::new_item(b"replaced".to_vec()));
    }

    // ===================================================================
    // 59. Batch: insert reference in same batch as item + tree creation
    // ===================================================================

    #[test]
    fn test_batch_create_tree_item_and_reference_same_batch() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();

        // Create everything in a single batch: tree, item, and reference
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                b"ref_tree".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"ref_tree".to_vec()],
                b"item_target".to_vec(),
                Element::new_item(b"target_data".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"ref_tree".to_vec()],
                b"ref_to_item".to_vec(),
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    b"ref_tree".to_vec(),
                    b"item_target".to_vec(),
                ])),
            ),
        ];

        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch with tree + item + reference should succeed");

        // Reference should resolve
        let result = db
            .get([b"ref_tree"].as_ref(), b"ref_to_item", None, grove_version)
            .unwrap()
            .expect("reference should resolve");
        assert_eq!(result, Element::new_item(b"target_data".to_vec()));
    }

    // ===================================================================
    // 60. Batch: split_removal_bytes callback exercised
    // ===================================================================

    #[test]
    fn test_batch_split_removal_bytes_callback() {
        use grovedb_costs::storage_cost::removal::StorageRemovedBytes::BasicStorageRemoval;

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Insert an item with flags
        db.insert(
            [TEST_LEAF].as_ref(),
            b"split_rem",
            Element::new_item_with_flags(b"data".to_vec(), Some(vec![1, 2, 3])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item with flags for removal");

        // Delete it, using a custom split_removal_bytes callback
        let ops = vec![QualifiedGroveDbOp::delete_op(
            vec![TEST_LEAF.to_vec()],
            b"split_rem".to_vec(),
        )];

        db.apply_batch_with_element_flags_update(
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, key_bytes_to_remove, value_bytes_to_remove| {
                // Custom split: just use BasicStorageRemoval
                Ok((
                    BasicStorageRemoval(key_bytes_to_remove),
                    BasicStorageRemoval(value_bytes_to_remove),
                ))
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("batch delete with split_removal_bytes callback should succeed");

        assert!(
            db.get([TEST_LEAF].as_ref(), b"split_rem", None, grove_version)
                .unwrap()
                .is_err(),
            "deleted element should not exist"
        );
    }
}
