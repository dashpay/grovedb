#[cfg(test)]
mod tests {
    use std::{ops::Add, option::Option::None};

    use costs::{
        storage_cost::{
            removal::{
                StorageRemovedBytes,
                StorageRemovedBytes::{
                    BasicStorageRemoval, NoStorageRemoval, SectionedStorageRemoval,
                },
            },
            transition::OperationStorageTransitionType,
            StorageCost,
        },
        OperationCost,
    };
    use integer_encoding::VarInt;
    use intmap::IntMap;

    use crate::{
        batch::GroveDbOp,
        reference_path::{
            ReferencePathType,
            ReferencePathType::{SiblingReference, UpstreamFromElementHeightReference},
        },
        tests::{make_empty_grovedb, make_test_grovedb, ANOTHER_TEST_LEAF, TEST_LEAF},
        Element, ElementFlags, PathQuery,
    };

    #[test]
    fn test_batch_two_insert_empty_tree_same_level_added_bytes_match_non_batch() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let non_batch_cost_1 = db
            .insert(vec![], b"key1", Element::empty_tree(), None, Some(&tx))
            .cost;
        let non_batch_cost_2 = db
            .insert(vec![], b"key2", Element::empty_tree(), None, Some(&tx))
            .cost;
        let non_batch_cost = non_batch_cost_1.add(non_batch_cost_2);
        tx.rollback().expect("expected to rollback");
        let ops = vec![
            GroveDbOp::insert_run_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_run_op(vec![], b"key2".to_vec(), Element::empty_tree()),
        ];
        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert_eq!(
            non_batch_cost.storage_cost.added_bytes,
            cost.storage_cost.added_bytes
        );
        assert_eq!(non_batch_cost.storage_cost.removed_bytes, NoStorageRemoval);
        assert_eq!(cost.storage_cost.removed_bytes, NoStorageRemoval);
    }

    #[test]
    fn test_batch_three_inserts_elements_same_level_added_bytes_match_non_batch() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let non_batch_cost_1 = db
            .insert(vec![], b"key1", Element::empty_tree(), None, Some(&tx))
            .cost;
        let non_batch_cost_2 = db
            .insert(
                vec![],
                b"key2",
                Element::new_item_with_flags(b"pizza".to_vec(), Some([0, 1].to_vec())),
                None,
                Some(&tx),
            )
            .cost;
        let non_batch_cost_3 = db
            .insert(
                vec![],
                b"key3",
                Element::new_reference(SiblingReference(b"key2".to_vec())),
                None,
                Some(&tx),
            )
            .cost;
        let non_batch_cost = non_batch_cost_1.add(non_batch_cost_2).add(non_batch_cost_3);
        tx.rollback().expect("expected to rollback");
        let ops = vec![
            GroveDbOp::insert_run_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_run_op(
                vec![],
                b"key2".to_vec(),
                Element::new_item_with_flags(b"pizza".to_vec(), Some([0, 1].to_vec())),
            ),
            GroveDbOp::insert_run_op(
                vec![],
                b"key3".to_vec(),
                Element::new_reference(SiblingReference(b"key2".to_vec())),
            ),
        ];
        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert_eq!(
            non_batch_cost.storage_cost.added_bytes,
            cost.storage_cost.added_bytes
        );
        assert_eq!(non_batch_cost.storage_cost.removed_bytes, NoStorageRemoval);
        assert_eq!(cost.storage_cost.removed_bytes, NoStorageRemoval);
    }

    #[test]
    fn test_batch_four_inserts_elements_multi_level_added_bytes_match_non_batch() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let non_batch_cost_1 = db
            .insert(vec![], b"key1", Element::empty_tree(), None, Some(&tx))
            .cost;
        let non_batch_cost_2 = db
            .insert(
                vec![b"key1".as_slice()],
                b"key2",
                Element::new_item_with_flags(b"pizza".to_vec(), Some([0, 1].to_vec())),
                None,
                Some(&tx),
            )
            .cost;
        let non_batch_cost_3 = db
            .insert(
                vec![b"key1".as_slice()],
                b"key3",
                Element::empty_tree(),
                None,
                Some(&tx),
            )
            .cost;
        let non_batch_cost_4 = db
            .insert(
                vec![b"key1".as_slice(), b"key3".as_slice()],
                b"key4",
                Element::new_reference(UpstreamFromElementHeightReference(
                    1,
                    vec![b"key2".to_vec()],
                )),
                None,
                Some(&tx),
            )
            .cost;
        let non_batch_cost = non_batch_cost_1
            .add(non_batch_cost_2)
            .add(non_batch_cost_3)
            .add(non_batch_cost_4);
        tx.rollback().expect("expected to rollback");
        let ops = vec![
            GroveDbOp::insert_run_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::new_item_with_flags(b"pizza".to_vec(), Some([0, 1].to_vec())),
            ),
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_run_op(
                vec![b"key1".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                Element::new_reference(UpstreamFromElementHeightReference(
                    1,
                    vec![b"key2".to_vec()],
                )),
            ),
        ];
        let cost = db
            .apply_batch(ops, None, Some(&tx))
            .cost_as_result()
            .expect("expected to apply batch");
        assert_eq!(
            non_batch_cost.storage_cost.added_bytes,
            cost.storage_cost.added_bytes
        );
        assert_eq!(non_batch_cost.storage_cost.removed_bytes, NoStorageRemoval);
        assert_eq!(cost.storage_cost.removed_bytes, NoStorageRemoval);
    }


    #[test]
    fn test_batch_root_two_insert_tree_cost_same_level() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![GroveDbOp::insert_run_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        ),
                       GroveDbOp::insert_run_op(
                           vec![],
                           b"key2".to_vec(),
                           Element::empty_tree(),
                       )];
        let cost_result = db.apply_batch(ops, None, Some(&tx));
        cost_result.value.expect("expected to execute batch");
        let cost = cost_result.cost;
        // Explanation for 214 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 37
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for empty tree value
        // 32 for node hash
        // 0 for value hash
        // 2 byte for the value_size (required space for 98 + up to 256 for child key)

        // Parent Hook -> 39
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2

        // Total (37 + 37 + 39) * 2 = 226
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 2, // todo: this seems too little
                storage_cost: StorageCost {
                    added_bytes: 226,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 10,
            }
        );
    }

    #[test]
    fn test_batch_root_two_insert_tree_cost_different_level() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![GroveDbOp::insert_run_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        ),
                       GroveDbOp::insert_run_op(
                           vec![b"key1".to_vec()],
                           b"key2".to_vec(),
                           Element::empty_tree(),
                       )];
        let cost_result = db.apply_batch(ops, None, Some(&tx));
        cost_result.value.expect("expected to execute batch");
        let cost = cost_result.cost;
        // Explanation for 214 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 37
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for empty tree value
        // 32 for node hash
        // 0 for value hash
        // 2 byte for the value_size (required space for 98 + up to 256 for child key)

        // Parent Hook -> 39
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2

        // Total (37 + 37 + 39) * 2 = 226
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 2, // todo: this seems too little
                storage_cost: StorageCost {
                    added_bytes: 226,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 10,
            }
        );
    }
}
