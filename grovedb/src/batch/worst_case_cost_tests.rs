#[cfg(test)]
mod tests {
    use std::option::Option::None;

    use costs::{
        storage_cost::{removal::StorageRemovedBytes::NoStorageRemoval, StorageCost},
        OperationCost,
    };

    use crate::{batch::GroveDbOp, tests::make_empty_grovedb, Element, GroveDb};

    #[test]
    fn test_batch_root_one_tree_insert_op_worst_case_costs() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![GroveDbOp::insert_run_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let worst_case_ops = ops.iter().map(|op| op.to_worst_case_clone()).collect();
        let worst_case_cost = GroveDb::worst_case_operations_for_batch(
            worst_case_ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_bytes| Ok(NoStorageRemoval),
        )
        .cost_as_result()
        .expect("expected to get worst case costs");

        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert!(
            worst_case_cost.worse_or_eq_than(&cost),
            "not worse {:?} \n than {:?}",
            worst_case_cost,
            cost
        );
        // because we know the object we are inserting we can know the worst
        // case cost if it doesn't already exist
        assert_eq!(
            cost.storage_cost.added_bytes,
            worst_case_cost.storage_cost.added_bytes
        );

        assert_eq!(
            worst_case_cost,
            OperationCost {
                seek_count: 6, // todo: why is this 6
                storage_cost: StorageCost {
                    added_bytes: 113,
                    replaced_bytes: 18432, // todo: verify
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 23040,
                hash_node_calls: 18, // todo: verify why
            }
        );
    }

    #[test]
    fn test_batch_root_one_item_insert_op_worst_case_costs() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![GroveDbOp::insert_run_op(
            vec![],
            b"key1".to_vec(),
            Element::new_item(b"cat".to_vec()),
        )];
        let worst_case_ops = ops.iter().map(|op| op.to_worst_case_clone()).collect();
        let worst_case_cost = GroveDb::worst_case_operations_for_batch(
            worst_case_ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_bytes| Ok(NoStorageRemoval),
        )
        .cost_as_result()
        .expect("expected to get worst case costs");

        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert!(
            worst_case_cost.worse_or_eq_than(&cost),
            "not worse {:?} \n than {:?}",
            worst_case_cost,
            cost
        );
        // because we know the object we are inserting we can know the worst
        // case cost if it doesn't already exist
        assert_eq!(
            cost.storage_cost.added_bytes,
            worst_case_cost.storage_cost.added_bytes
        );

        assert_eq!(
            worst_case_cost,
            OperationCost {
                seek_count: 4, // todo: why is this 6
                storage_cost: StorageCost {
                    added_bytes: 147,
                    replaced_bytes: 18432, // log(max_elements) * 32 = 640 // todo: verify
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 23040,
                hash_node_calls: 18, // todo: verify why
            }
        );
    }

    #[test]
    fn test_batch_root_one_tree_insert_op_under_element_worst_case_costs() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"0", Element::empty_tree(), None, Some(&tx))
            .unwrap()
            .expect("successful root tree leaf insert");

        let ops = vec![GroveDbOp::insert_run_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let worst_case_ops = ops.iter().map(|op| op.to_worst_case_clone()).collect();
        let worst_case_cost = GroveDb::worst_case_operations_for_batch(
            worst_case_ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_bytes| Ok(NoStorageRemoval),
        )
        .cost_as_result()
        .expect("expected to get worst case costs");

        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert!(
            worst_case_cost.worse_or_eq_than(&cost),
            "not worse {:?} \n than {:?}",
            worst_case_cost,
            cost
        );
        // because we know the object we are inserting we can know the worst
        // case cost if it doesn't already exist
        assert_eq!(
            cost.storage_cost.added_bytes,
            worst_case_cost.storage_cost.added_bytes
        );

        assert_eq!(
            worst_case_cost,
            OperationCost {
                seek_count: 6, // todo: why is this 6
                storage_cost: StorageCost {
                    added_bytes: 113,
                    replaced_bytes: 18432, // todo: verify
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 23040,
                hash_node_calls: 18, // todo: verify why
            }
        );
    }

    #[test]
    fn test_batch_root_one_tree_insert_op_in_sub_tree_worst_case_costs() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"0", Element::empty_tree(), None, Some(&tx))
            .unwrap()
            .expect("successful root tree leaf insert");

        let ops = vec![GroveDbOp::insert_run_op(
            vec![b"0".to_vec()],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let worst_case_ops = ops.iter().map(|op| op.to_worst_case_clone()).collect();
        let worst_case_cost = GroveDb::worst_case_operations_for_batch(
            worst_case_ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_bytes| Ok(NoStorageRemoval),
        )
        .cost_as_result()
        .expect("expected to get worst case costs");

        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert!(
            worst_case_cost.worse_or_eq_than(&cost),
            "not worse {:?} \n than {:?}",
            worst_case_cost,
            cost
        );
        // /// because we know the object we are inserting we can know the worst
        // /// case cost if it doesn't already exist
        // assert_eq!(
        //     cost.storage_cost.added_bytes,
        //     worst_case_cost.storage_cost.added_bytes
        // );

        assert_eq!(
            worst_case_cost,
            OperationCost {
                seek_count: 8, // todo: why is this 8
                storage_cost: StorageCost {
                    added_bytes: 113,
                    replaced_bytes: 36937, // todo: verify
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 46420,
                hash_node_calls: 38, // todo: verify why
            }
        );
    }

    #[test]
    fn test_batch_worst_case_costs() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"keyb", Element::empty_tree(), None, Some(&tx))
            .unwrap()
            .expect("successful root tree leaf insert");

        let ops = vec![GroveDbOp::insert_run_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let worst_case_ops = ops.iter().map(|op| op.to_worst_case_clone()).collect();
        let worst_case_cost_result = GroveDb::worst_case_operations_for_batch(
            worst_case_ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_bytes| Ok(NoStorageRemoval),
        );
        assert!(worst_case_cost_result.value.is_ok());
        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        // at the moment we just check the added bytes are the same
        assert_eq!(
            worst_case_cost_result.cost.storage_cost.added_bytes,
            cost.storage_cost.added_bytes
        );
    }
}
