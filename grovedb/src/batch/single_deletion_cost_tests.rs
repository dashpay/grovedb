#[cfg(test)]
mod tests {
    use std::option::Option::None;

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
        operations::delete::DeleteOptions,
        reference_path::ReferencePathType,
        tests::{make_empty_grovedb, make_test_grovedb, ANOTHER_TEST_LEAF, TEST_LEAF},
        Element, PathQuery,
    };

    #[test]
    fn test_batch_one_deletion_costs_match_non_batch() {
        let db = make_empty_grovedb();

        db.insert(vec![], b"key1", Element::empty_tree(), None, None)
            .cost_as_result()
            .expect("expected to insert successfully");

        let tx = db.start_transaction();

        let non_batch_cost = db
            .delete(vec![], b"key1", None, Some(&tx))
            .cost_as_result()
            .expect("expected to delete successfully");

        tx.rollback().expect("expected to rollback");
        let ops = vec![GroveDbOp::delete_run_op(vec![], b"key1".to_vec())];
        let cost = db
            .apply_batch(ops, None, Some(&tx))
            .cost_as_result()
            .expect("expected to delete successfully");
        assert_eq!(non_batch_cost.storage_cost, cost.storage_cost);
    }

    #[test]
    fn test_batch_one_deletion_tree_costs() {
        let db = make_empty_grovedb();

        let added_bytes = db
            .insert(vec![], b"key1", Element::empty_tree(), None, None)
            .cost_as_result()
            .expect("expected to insert successfully")
            .storage_cost
            .added_bytes;

        let tx = db.start_transaction();

        let ops = vec![GroveDbOp::delete_run_op(vec![], b"key1".to_vec())];
        let cost = db
            .apply_batch(ops, None, Some(&tx))
            .cost_as_result()
            .expect("expected to delete successfully");
        assert_eq!(
            added_bytes,
            cost.storage_cost.removed_bytes.total_removed_bytes()
        );
    }
}
