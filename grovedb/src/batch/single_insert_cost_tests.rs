mod tests {
    use costs::{
        storage_cost::{
            removal::{
                Identifier, StorageRemovalPerEpochByIdentifier,
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

    use crate::{batch::GroveDbOp, tests::make_empty_grovedb, Element};

    #[test]
    fn test_batch_one_insert_costs_match_non_batch() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let non_batch_cost = db
            .insert(vec![], b"key1", Element::empty_tree(), None, Some(&tx))
            .cost;
        tx.rollback().expect("expected to rollback");
        let ops = vec![GroveDbOp::insert_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert_eq!(non_batch_cost.storage_cost, cost.storage_cost);
    }

    #[test]
    fn test_batch_root_one_insert_tree_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![GroveDbOp::insert_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let cost_result = db.apply_batch(ops, None, Some(&tx));
        cost_result.value.expect("expected to execute batch");
        let cost = cost_result.cost;
        // Explanation for 113 storage_written_bytes

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

        // Total 37 + 37 + 39 = 113

        // Hash node calls
        // 2 for the node hash
        // 1 for the value hash
        // 1 kv_digest_to_kv_hash

        // Seek Count
        // 1 to load from root tree
        // 1 to insert
        // 1 to update root tree

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3,
                storage_cost: StorageCost {
                    added_bytes: 113,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 4,
            }
        );
    }

    #[test]
    fn test_batch_root_one_insert_item_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![GroveDbOp::insert_op(
            vec![],
            b"0".to_vec(),
            Element::new_item(b"cat".to_vec()),
        )];
        let cost_result = db.apply_batch(ops, None, Some(&tx));
        cost_result.value.expect("expected to execute batch");
        let cost = cost_result.cost;
        // Explanation for 214 storage_written_bytes

        // Key -> 34 bytes
        // 32 bytes for the key prefix
        // 1 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 71
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for required space for bytes
        //   3 for item bytes
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 71)

        // Parent Hook -> 39
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2

        // Root -> 39
        // 1 byte for the root key length size
        // 1 byte for the root value length size
        // 32 for the root key prefix
        // 1 bytes for the key to put in root
        // 1 byte for the root "r"

        // Total 34 + 71 + 36 = 141

        // Hash node calls
        // 2 for the node hash
        // 1 for the value hash
        // 1 kv_digest_to_kv_hash

        // Seek Count
        // 1 to load from root tree
        // 1 to insert
        // 1 to update root tree
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3,
                storage_cost: StorageCost {
                    added_bytes: 141,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 4,
            }
        );
    }

    #[test]
    fn test_batch_root_one_insert_tree_under_parent_item_in_same_merk_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let cost = db
            .insert(
                vec![],
                b"0",
                Element::new_item(b"cat".to_vec()),
                None,
                Some(&tx),
            )
            .cost_as_result()
            .expect("successful root tree leaf insert");

        assert_eq!(cost.storage_cost.added_bytes, 141);

        let ops = vec![GroveDbOp::insert_op(
            vec![],
            b"key1".to_vec(),
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

        // Total 37 + 37 + 39 = 113

        // Replaced bytes

        // Value -> 71
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for required space for bytes
        //   3 for item bytes
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 99)

        // 71 + 36 = 107 (key is not replaced)

        // Hash node calls 6
        // 2 for the node hash
        // 1 for the value hash
        // 1 for the kv_digest_to_kv_hash
        // 2 for the node hash above

        // Seek Count explanation
        // 1 to get root merk
        // 1 to load root tree
        // 1 to insert new item
        // 1 to replace parent tree
        // 1 to update root
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5,
                storage_cost: StorageCost {
                    added_bytes: 113,
                    replaced_bytes: 107,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 73, // todo: verify and explain
                hash_node_calls: 6,
            }
        );
    }

    #[test]
    fn test_batch_root_one_insert_tree_under_parent_tree_in_same_merk_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"0", Element::empty_tree(), None, Some(&tx))
            .unwrap()
            .expect("successful root tree leaf insert");

        let ops = vec![GroveDbOp::insert_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let cost_result = db.apply_batch(ops, None, Some(&tx));
        cost_result.value.expect("expected to execute batch");
        let cost = cost_result.cost;
        // Explanation for 113 storage_written_bytes

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

        // Total 37 + 37 + 39 = 113

        // Replaced bytes

        // 37 + 36 = 73 (key is not replaced)
        // We instead are getting 104, because we are paying for (+ hash - key byte
        // size) this means 31 extra bytes.
        // In reality though we really are replacing 104 bytes. TBD what to do.

        // Hash node calls 7
        // 2 for the node hash
        // 1 for the value hash
        // 1 for the kv_digest_to_kv_hash
        // 2 for the node hash above

        // Seek Count explanation
        // 1 to get root merk
        // 1 to load root tree
        // 1 to insert new item
        // 1 to replace parent tree
        // 1 to update root
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 5,
                storage_cost: StorageCost {
                    added_bytes: 113,
                    replaced_bytes: 104, // todo: this should actually be 73
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 70, // todo: verify and explain
                hash_node_calls: 6,
            }
        );
    }

    #[test]
    fn test_batch_root_one_insert_tree_under_parent_tree_in_different_merk_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"0", Element::empty_tree(), None, Some(&tx))
            .unwrap()
            .expect("successful root tree leaf insert");

        let ops = vec![GroveDbOp::insert_op(
            vec![b"0".to_vec()],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let cost_result = db.apply_batch(ops, None, Some(&tx));
        cost_result.value.expect("expected to execute batch");
        let cost = cost_result.cost;
        // Explanation for 113 storage_written_bytes

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

        // Total 37 + 37 + 39 = 113

        // Replaced bytes

        // 37 + 36 = 73 (key is not replaced)

        //// Hash node calls 10
        // 1 to get the lowest merk
        //
        // 1 to get the middle merk
        // 2 for the node hash
        // 1 for the value hash
        // 1 for the kv_digest_to_kv_hash

        // On the layer above the root key did change
        // meaning we get another 4 hashes 2 + 1 + 1

        //// Seek Count explanation

        // 1 to get merk at lower level
        // 1 to insert new item
        // 1 to get root merk
        // 1 to load root tree
        // 1 to replace parent tree
        // 1 to update root
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6,
                storage_cost: StorageCost {
                    added_bytes: 113,
                    replaced_bytes: 73,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 139, // todo: verify and explain
                hash_node_calls: 10,
            }
        );
    }

    #[test]
    fn test_batch_root_one_insert_cost_right_below_value_required_cost_of_2() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![GroveDbOp::insert_op(
            vec![],
            b"key1".to_vec(),
            Element::new_item([0u8; 60].to_vec()),
        )];
        let cost_result = db.apply_batch(ops, None, Some(&tx));
        cost_result.value.expect("expected to execute batch");
        let cost = cost_result.cost;
        // Explanation for 243 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 128
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for the value size
        //   60 bytes
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 127)

        // Parent Hook -> 39
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2

        // Total 37 + 128 + 39 = 204

        // Hash node calls
        // 2 for the node hash
        // 1 for the value hash
        // 1 kv_digest_to_kv_hash

        // Seek Count
        // 1 to load from root tree
        // 1 to insert
        // 1 to update root tree
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3,
                storage_cost: StorageCost {
                    added_bytes: 204,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 4,
            }
        );
    }

    #[test]
    fn test_batch_root_one_insert_cost_right_above_value_required_cost_of_2() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![GroveDbOp::insert_op(
            vec![],
            b"key1".to_vec(),
            Element::new_item([0u8; 61].to_vec()),
        )];
        let cost_result = db.apply_batch(ops, None, Some(&tx));
        cost_result.value.expect("expected to execute batch");
        let cost = cost_result.cost;
        // Explanation for 243 storage_written_bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 130
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for the value size
        //   61 bytes
        // 32 for node hash
        // 32 for value hash
        // 2 byte for the value_size (required space for 128)

        // Parent Hook -> 39
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2

        // Total 37 + 130 + 39 = 206

        // Hash node calls
        // 2 for the node hash
        // 2 for the value hash
        // 1 kv_digest_to_kv_hash

        // Seek Count
        // 1 to load from root tree
        // 1 to insert
        // 1 to update root tree
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 3,
                storage_cost: StorageCost {
                    added_bytes: 206,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 5,
            }
        );
    }

    #[test]
    fn test_batch_root_one_update_item_bigger_cost_no_flags() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();
        db.insert(vec![], b"tree", Element::empty_tree(), None, None)
            .unwrap()
            .expect("expected to insert tree");

        db.insert(
            vec![b"tree".as_slice()],
            b"key1",
            Element::new_item_with_flags(b"value1".to_vec(), Some(vec![0])),
            None,
            None,
        )
        .unwrap()
        .expect("expected to insert item");

        // We are adding 2 bytes
        let ops = vec![GroveDbOp::insert_op(
            vec![b"tree".to_vec()],
            b"key1".to_vec(),
            Element::new_item_with_flags(b"value100".to_vec(), Some(vec![1])),
        )];

        let cost = db
            .apply_batch_with_element_flags_update(
                ops,
                None,
                |_cost, _old_flags, _new_flags| Ok(false),
                |_flags, _removed_key_bytes, _removed_value_bytes| {
                    Ok((NoStorageRemoval, NoStorageRemoval))
                },
                Some(&tx),
            )
            .cost;

        // Hash node calls

        // Seek Count

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 7, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 2,
                    replaced_bytes: 191, // todo: verify this
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 229, // todo: verify this
                hash_node_calls: 10,       // todo: verify this
            }
        );
    }

    #[test]
    fn test_batch_root_one_update_item_bigger_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();
        db.insert(vec![], b"tree", Element::empty_tree(), None, None)
            .unwrap()
            .expect("expected to insert tree");

        db.insert(
            vec![b"tree".as_slice()],
            b"key1",
            Element::new_item_with_flags(b"value1".to_vec(), Some(vec![0, 0])),
            None,
            None,
        )
        .unwrap()
        .expect("expected to insert item");

        // We are adding 2 bytes
        let ops = vec![GroveDbOp::insert_op(
            vec![b"tree".to_vec()],
            b"key1".to_vec(),
            Element::new_item_with_flags(b"value100".to_vec(), Some(vec![0, 1])),
        )];

        let cost = db
            .apply_batch_with_element_flags_update(
                ops,
                None,
                |cost, old_flags, new_flags| match cost.transition_type() {
                    OperationStorageTransitionType::OperationUpdateBiggerSize => {
                        if new_flags[0] == 0 {
                            new_flags[0] = 1;
                            let new_flags_epoch = new_flags[1];
                            new_flags[1] = old_flags.unwrap()[1];
                            new_flags.push(new_flags_epoch);
                            new_flags.extend(cost.added_bytes.encode_var_vec());
                            assert_eq!(new_flags, &vec![1u8, 0, 1, 2]);
                            Ok(true)
                        } else {
                            assert_eq!(new_flags[0], 1);
                            Ok(false)
                        }
                    }
                    OperationStorageTransitionType::OperationUpdateSmallerSize => {
                        new_flags.extend(vec![1, 2]);
                        Ok(true)
                    }
                    _ => Ok(false),
                },
                |_flags, removed_key_bytes, removed_value_bytes| {
                    Ok((
                        BasicStorageRemoval(removed_key_bytes),
                        BasicStorageRemoval(removed_value_bytes),
                    ))
                },
                Some(&tx),
            )
            .cost;

        // Hash node calls

        // Seek Count

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 7, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 4,
                    replaced_bytes: 192, // todo: verify this
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 230, // todo: verify this
                hash_node_calls: 10,       // todo: verify this
            }
        );
    }

    #[test]
    fn test_batch_root_one_update_item_smaller_cost_no_flags() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();
        db.insert(vec![], b"tree", Element::empty_tree(), None, None)
            .unwrap()
            .expect("expected to insert tree");

        db.insert(
            vec![b"tree".as_slice()],
            b"key1",
            Element::new_item_with_flags(b"value1".to_vec(), Some(vec![0])),
            None,
            None,
        )
        .unwrap()
        .expect("expected to insert item");

        // We are adding 2 bytes
        let ops = vec![GroveDbOp::insert_op(
            vec![b"tree".to_vec()],
            b"key1".to_vec(),
            Element::new_item_with_flags(b"value".to_vec(), Some(vec![1])),
        )];

        let cost = db
            .apply_batch_with_element_flags_update(
                ops,
                None,
                |_cost, _old_flags, _new_flags| Ok(false),
                |_flags, removed_key_bytes, removed_value_bytes| {
                    Ok((
                        BasicStorageRemoval(removed_key_bytes),
                        BasicStorageRemoval(removed_value_bytes),
                    ))
                },
                Some(&tx),
            )
            .cost;

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 7, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 190, // todo: verify this
                    removed_bytes: BasicStorageRemoval(1)
                },
                storage_loaded_bytes: 229, // todo: verify this
                hash_node_calls: 10,       // todo: verify this
            }
        );
    }

    #[test]
    fn test_batch_root_one_update_item_smaller_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();
        db.insert(vec![], b"tree", Element::empty_tree(), None, None)
            .unwrap()
            .expect("expected to insert tree");

        db.insert(
            vec![b"tree".as_slice()],
            b"key1",
            Element::new_item_with_flags(b"value1".to_vec(), Some(vec![0, 0])),
            None,
            None,
        )
        .unwrap()
        .expect("expected to insert item");

        // We are adding 2 bytes
        let ops = vec![GroveDbOp::insert_op(
            vec![b"tree".to_vec()],
            b"key1".to_vec(),
            Element::new_item_with_flags(b"value".to_vec(), Some(vec![0, 1])),
        )];

        let cost = db
            .apply_batch_with_element_flags_update(
                ops,
                None,
                |cost, old_flags, new_flags| match cost.transition_type() {
                    OperationStorageTransitionType::OperationUpdateBiggerSize => {
                        if new_flags[0] == 0 {
                            new_flags[0] = 1;
                            let new_flags_epoch = new_flags[1];
                            new_flags[1] = old_flags.unwrap()[1];
                            new_flags.push(new_flags_epoch);
                            new_flags.extend(cost.added_bytes.encode_var_vec());
                            assert_eq!(new_flags, &vec![1u8, 0, 1, 2]);
                            Ok(true)
                        } else {
                            assert_eq!(new_flags[0], 1);
                            Ok(false)
                        }
                    }
                    OperationStorageTransitionType::OperationUpdateSmallerSize => Ok(true),
                    _ => Ok(false),
                },
                |_flags, _removed_key_bytes, removed_value_bytes| {
                    let mut removed_bytes = StorageRemovalPerEpochByIdentifier::default();
                    // we are removing 1 byte from epoch 0 for an identity
                    let mut removed_bytes_for_identity = IntMap::new();
                    removed_bytes_for_identity.insert(0, removed_value_bytes);
                    removed_bytes.insert(Identifier::default(), removed_bytes_for_identity);
                    Ok((NoStorageRemoval, SectionedStorageRemoval(removed_bytes)))
                },
                Some(&tx),
            )
            .cost;

        let mut removed_bytes = StorageRemovalPerEpochByIdentifier::default();
        // we are removing 1 byte from epoch 0 for an identity
        let mut removed_bytes_for_identity = IntMap::new();
        removed_bytes_for_identity.insert(0, 1);
        removed_bytes.insert(Identifier::default(), removed_bytes_for_identity);

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 7, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 191, // todo: verify this
                    removed_bytes: SectionedStorageRemoval(removed_bytes)
                },
                storage_loaded_bytes: 230, // todo: verify this
                hash_node_calls: 10,       // todo: verify this
            }
        );
    }

    #[test]
    fn test_batch_root_one_update_tree_bigger_flags_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();
        db.insert(vec![], b"tree", Element::empty_tree(), None, None)
            .unwrap()
            .expect("expected to insert tree");

        db.insert(
            vec![b"tree".as_slice()],
            b"key1",
            Element::new_tree_with_flags(None, Some(vec![0, 0])),
            None,
            None,
        )
        .unwrap()
        .expect("expected to insert item");

        // We are adding 1 byte to the flags
        let ops = vec![GroveDbOp::insert_op(
            vec![b"tree".to_vec()],
            b"key1".to_vec(),
            Element::new_tree_with_flags(None, Some(vec![0, 1, 1])),
        )];

        let cost = db
            .apply_batch_with_element_flags_update(
                ops,
                None,
                |cost, old_flags, new_flags| match cost.transition_type() {
                    OperationStorageTransitionType::OperationUpdateBiggerSize => {
                        if new_flags[0] == 0 {
                            new_flags[0] = 1;
                            let new_flags_epoch = new_flags[1];
                            let old_flags = old_flags.unwrap();
                            new_flags[1] = old_flags[1];
                            new_flags.push(new_flags_epoch);
                            new_flags.extend(cost.added_bytes.encode_var_vec());
                            assert_eq!(new_flags, &vec![1u8, 0, 1, 1, 1]);
                            Ok(true)
                        } else {
                            assert_eq!(new_flags[0], 1);
                            Ok(false)
                        }
                    }
                    OperationStorageTransitionType::OperationUpdateSmallerSize => {
                        new_flags.extend(vec![1, 2]);
                        Ok(true)
                    }
                    _ => Ok(false),
                },
                |_flags, _removed_key_bytes, removed_value_bytes| {
                    Ok((NoStorageRemoval, BasicStorageRemoval(removed_value_bytes)))
                },
                Some(&tx),
            )
            .cost;

        assert_eq!(
            cost,
            OperationCost {
                seek_count: 7, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 3,
                    replaced_bytes: 155, // todo: verify this
                    removed_bytes: NoStorageRemoval
                },
                storage_loaded_bytes: 224, // todo: verify this
                hash_node_calls: 12,       // todo: verify this
            }
        );
    }
}
