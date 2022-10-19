#[cfg(test)]
mod tests {
    use std::option::Option::None;

    use costs::{
        storage_cost::{
            removal::StorageRemovedBytes::NoStorageRemoval,
            transition::OperationStorageTransitionType, StorageCost,
        },
        OperationCost,
    };
    use integer_encoding::VarInt;
    use merk::proofs::Query;

    use super::*;
    use crate::{
        batch::GroveDbOp,
        reference_path::ReferencePathType,
        tests::{make_empty_grovedb, make_test_grovedb, ANOTHER_TEST_LEAF, TEST_LEAF},
        Element, GroveDb, PathQuery,
    };

    #[test]
    fn test_batch_root_one_op_worst_case_costs() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

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
        assert!(
            worst_case_cost_result.cost.worse_or_eq_than(&cost),
            "not worse {:?} \n than {:?}",
            worst_case_cost_result.cost,
            cost
        );

        assert_eq!(
            worst_case_cost_result.cost,
            OperationCost {
                seek_count: 6, // todo: why is this 6
                storage_cost: StorageCost {
                    added_bytes: 177,
                    replaced_bytes: 640, // log(max_elements) * 32 = 640 // todo: verify
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 22, // todo: verify why
            }
        );
    }

    #[test]
    fn test_batch_root_one_op_under_element_worst_case_costs() {
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
        let worst_case_cost_result = GroveDb::worst_case_operations_for_batch(
            worst_case_ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_bytes| Ok(NoStorageRemoval),
        );
        assert!(worst_case_cost_result.value.is_ok());
        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert_eq!(worst_case_cost_result.cost, cost);
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
        assert_eq!(worst_case_cost_result.cost, cost);
    }
}
