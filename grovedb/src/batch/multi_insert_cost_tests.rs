//! Multi insert cost tests

#[cfg(feature = "full")]
mod tests {
    use std::{ops::Add, option::Option::None};

    use grovedb_costs::{
        storage_cost::{removal::StorageRemovedBytes::NoStorageRemoval, StorageCost},
        OperationCost,
    };
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::GroveDbOp,
        reference_path::ReferencePathType::{SiblingReference, UpstreamFromElementHeightReference},
        tests::{common::EMPTY_PATH, make_empty_grovedb},
        Element,
    };

    #[test]
    fn test_batch_two_insert_empty_tree_same_level_added_bytes_match_non_batch() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let non_batch_cost_1 = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_tree(),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        let non_batch_cost_2 = db
            .insert(
                EMPTY_PATH,
                b"key2",
                Element::empty_tree(),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        let non_batch_cost = non_batch_cost_1.add(non_batch_cost_2);
        tx.rollback().expect("expected to rollback");
        let ops = vec![
            GroveDbOp::insert_or_replace_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_or_replace_op(vec![], b"key2".to_vec(), Element::empty_tree()),
        ];
        let cost = db.apply_batch(ops, None, Some(&tx), grove_version).cost;
        assert_eq!(
            non_batch_cost.storage_cost.added_bytes,
            cost.storage_cost.added_bytes
        );
        assert_eq!(non_batch_cost.storage_cost.removed_bytes, NoStorageRemoval);
        assert_eq!(cost.storage_cost.removed_bytes, NoStorageRemoval);
    }

    #[test]
    fn test_batch_three_inserts_elements_same_level_added_bytes_match_non_batch() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let non_batch_cost_1 = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_tree(),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        let non_batch_cost_2 = db
            .insert(
                EMPTY_PATH,
                b"key2",
                Element::new_item_with_flags(b"pizza".to_vec(), Some([0, 1].to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        let non_batch_cost_3 = db
            .insert(
                EMPTY_PATH,
                b"key3",
                Element::new_reference(SiblingReference(b"key2".to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        let non_batch_cost = non_batch_cost_1.add(non_batch_cost_2).add(non_batch_cost_3);
        tx.rollback().expect("expected to rollback");
        let ops = vec![
            GroveDbOp::insert_or_replace_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_or_replace_op(
                vec![],
                b"key2".to_vec(),
                Element::new_item_with_flags(b"pizza".to_vec(), Some([0, 1].to_vec())),
            ),
            GroveDbOp::insert_or_replace_op(
                vec![],
                b"key3".to_vec(),
                Element::new_reference(SiblingReference(b"key2".to_vec())),
            ),
        ];
        let cost = db.apply_batch(ops, None, Some(&tx), grove_version).cost;
        assert_eq!(
            non_batch_cost.storage_cost.added_bytes,
            cost.storage_cost.added_bytes
        );
        assert_eq!(non_batch_cost.storage_cost.removed_bytes, NoStorageRemoval);
        assert_eq!(cost.storage_cost.removed_bytes, NoStorageRemoval);
    }

    #[test]
    fn test_batch_four_inserts_elements_multi_level_added_bytes_match_non_batch() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let non_batch_cost_1 = db
            .insert(
                EMPTY_PATH,
                b"key1",
                Element::empty_tree(),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        let non_batch_cost_2 = db
            .insert(
                [b"key1".as_slice()].as_ref(),
                b"key2",
                Element::new_item_with_flags(b"pizza".to_vec(), Some([0, 1].to_vec())),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        let non_batch_cost_3 = db
            .insert(
                [b"key1".as_slice()].as_ref(),
                b"key3",
                Element::empty_tree(),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        let non_batch_cost_4 = db
            .insert(
                [b"key1".as_slice(), b"key3".as_slice()].as_ref(),
                b"key4",
                Element::new_reference(UpstreamFromElementHeightReference(
                    1,
                    vec![b"key2".to_vec()],
                )),
                None,
                Some(&tx),
                grove_version,
            )
            .cost;
        let non_batch_cost = non_batch_cost_1
            .add(non_batch_cost_2)
            .add(non_batch_cost_3)
            .add(non_batch_cost_4);
        tx.rollback().expect("expected to rollback");
        let ops = vec![
            GroveDbOp::insert_or_replace_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::new_item_with_flags(b"pizza".to_vec(), Some([0, 1].to_vec())),
            ),
            GroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec()],
                b"key3".to_vec(),
                Element::empty_tree(),
            ),
            GroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec(), b"key3".to_vec()],
                b"key4".to_vec(),
                Element::new_reference(UpstreamFromElementHeightReference(
                    1,
                    vec![b"key2".to_vec()],
                )),
            ),
        ];
        let cost = db
            .apply_batch(ops, None, Some(&tx), grove_version)
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
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![
            GroveDbOp::insert_or_replace_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_or_replace_op(vec![], b"key2".to_vec(), Element::empty_tree()),
        ];
        let cost_result = db.apply_batch(ops, None, Some(&tx), grove_version);
        cost_result.value.expect("expected to execute batch");
        let cost = cost_result.cost;
        // Explanation for 214 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 38
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for empty tree value
        //   1 for Basic Merk
        // 32 for node hash
        // 0 for value hash
        // 2 byte for the value_size (required space for 98 + up to 256 for child key)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total (37 + 38 + 40) * 2 = 230

        // Hashes
        // 2 trees
        // 2 * 5 hashes per node
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 4,
                storage_cost: StorageCost {
                    added_bytes: 230,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 12,
            }
        );
    }

    #[test]
    fn test_batch_root_two_insert_tree_cost_different_level() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![
            GroveDbOp::insert_or_replace_op(vec![], b"key1".to_vec(), Element::empty_tree()),
            GroveDbOp::insert_or_replace_op(
                vec![b"key1".to_vec()],
                b"key2".to_vec(),
                Element::empty_tree(),
            ),
        ];
        let cost_result = db.apply_batch(ops, None, Some(&tx), grove_version);
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
        //   1 for Basic Merk
        // 32 for node hash
        // 0 for value hash
        // 2 byte for the value_size (required space for 98 + up to 256 for child key)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total (37 + 38 + 40) * 2 = 230

        // Hashes
        // 2 trees
        // 2 node hash
        // 1 combine hash
        // 1
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 4,
                storage_cost: StorageCost {
                    added_bytes: 230,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 12,
            }
        );
    }
}
