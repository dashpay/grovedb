use std::{
    collections::{BTreeMap, HashMap},
    fmt,
};

use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use itertools::Itertools;
use merk::{
    estimated_costs::average_case_costs::{
        add_average_case_merk_propagate, average_case_merk_propagate, EstimatedLayerInformation,
    },
    CryptoHash,
};
use storage::rocksdb_storage::RocksDbStorage;

use crate::{
    batch::{
        key_info::KeyInfo,
        mode::{BatchRunMode, BatchRunMode::AverageCaseMode},
        BatchApplyOptions, GroveDbOp, KeyInfoPath, Op, TreeCache,
    },
    Error, GroveDb, MAX_ELEMENTS_NUMBER,
};

/// Cache for subtree paths for average case scenario costs.
#[derive(Default)]
pub(in crate::batch) struct AverageCaseTreeCacheKnownPaths {
    paths: HashMap<KeyInfoPath, EstimatedLayerInformation>,
}

impl AverageCaseTreeCacheKnownPaths {
    pub(in crate::batch) fn new_with_estimated_layer_information(
        paths: HashMap<KeyInfoPath, EstimatedLayerInformation>,
    ) -> Self {
        AverageCaseTreeCacheKnownPaths { paths }
    }
}

impl fmt::Debug for AverageCaseTreeCacheKnownPaths {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeCacheKnownPaths").finish()
    }
}

impl<G, SR> TreeCache<G, SR> for AverageCaseTreeCacheKnownPaths {
    fn insert(&mut self, op: &GroveDbOp) -> CostResult<(), Error> {
        let mut inserted_path = op.path.clone();
        inserted_path.push(op.key.clone());
        if !self.paths.contains_key(&inserted_path) {
            return Err(Error::PathNotFoundInCacheForEstimatedCosts(format!(
                "inserting into average case costs path: {}",
                inserted_path
                    .0
                    .iter()
                    .map(|k| hex::encode(k.as_slice()))
                    .join("/")
            )))
            .wrap_with_cost(OperationCost::default());
        }
        let mut average_case_cost = OperationCost::default();
        GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(
            &mut average_case_cost,
            &op.path,
        );
        Ok(()).wrap_with_cost(average_case_cost)
    }

    fn get_batch_run_mode(&self) -> BatchRunMode {
        AverageCaseMode(self.paths.clone())
    }

    fn execute_ops_on_path(
        &mut self,
        path: &KeyInfoPath,
        ops_at_path_by_key: BTreeMap<KeyInfo, Op>,
        ops_by_qualified_paths: &BTreeMap<Vec<Vec<u8>>, Op>,
        batch_apply_options: &BatchApplyOptions,
        flags_update: &mut G,
        split_removal_bytes: &mut SR,
    ) -> CostResult<(CryptoHash, Option<Vec<u8>>), Error> {
        let mut cost = OperationCost::default();

        let layer_element_estimates = cost_return_on_error_no_add!(
            &cost,
            self.paths
                .get(path)
                .ok_or(Error::PathNotFoundInCacheForEstimatedCosts(format!(
                    "inserting into average case costs path: {}",
                    path.0.iter().map(|k| hex::encode(k.as_slice())).join("/")
                )))
        );

        // Then we have to get the tree
        GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(&mut cost, path);
        for (key, op) in ops_at_path_by_key.into_iter() {
            cost_return_on_error!(
                &mut cost,
                op.average_case_cost(&key, layer_element_estimates, false)
            );
        }

        cost_return_on_error!(
            &mut cost,
            average_case_merk_propagate(layer_element_estimates).map_err(Error::MerkError)
        );
        Ok(([0u8; 32], None)).wrap_with_cost(cost)
    }

    fn update_base_merk_root_key(&mut self, root_key: Option<Vec<u8>>) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let base_path = KeyInfoPath(vec![]);
        if let Some(input) = self.paths.get(&base_path) {
            // Then we have to get the tree
            GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(&mut cost, &base_path);
        }
        if let Some(_root_key) = root_key {
            // todo: add average case of updating the base root
            // GroveDb::add_average_case_insert_merk_node()
        } else {
        }
        Ok(()).wrap_with_cost(cost)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use costs::{
        storage_cost::{removal::StorageRemovedBytes::NoStorageRemoval, StorageCost},
        OperationCost,
    };
    use merk::estimated_costs::average_case_costs::{
        EstimatedLayerInformation,
        EstimatedLayerInformation::EstimatedLevel,
        EstimatedLayerSizes::{AllItems, AllSubtrees},
    };

    use crate::{
        batch::{
            estimated_costs::EstimatedCostsType::AverageCaseCostsType, key_info::KeyInfo,
            GroveDbOp, KeyInfoPath,
        },
        tests::make_empty_grovedb,
        Element, GroveDb,
    };

    #[test]
    fn test_batch_root_one_tree_insert_op_average_case_costs() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![GroveDbOp::insert_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), EstimatedLevel(0, AllSubtrees(4, None)));
        paths.insert(
            KeyInfoPath(vec![KeyInfo::KnownKey(b"key1".to_vec())]),
            EstimatedLevel(0, AllSubtrees(4, None)),
        );
        let average_case_cost = GroveDb::estimated_case_operations_for_batch(
            AverageCaseCostsType(paths),
            ops.clone(),
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
        )
        .cost_as_result()
        .expect("expected to get average case costs");

        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert!(
            average_case_cost.worse_or_eq_than(&cost),
            "not worse {:?} \n than {:?}",
            average_case_cost,
            cost
        );
        // because we know the object we are inserting we can know the average
        // case cost if it doesn't already exist
        assert_eq!(
            cost.storage_cost.added_bytes,
            average_case_cost.storage_cost.added_bytes
        );

        assert_eq!(
            average_case_cost,
            OperationCost {
                seek_count: 6, // todo: why is this 6
                storage_cost: StorageCost {
                    added_bytes: 113,
                    replaced_bytes: 76, // todo: verify
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 8, // todo: verify why
            }
        );
    }

    #[test]
    fn test_batch_root_one_tree_with_flags_insert_op_average_case_costs() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![GroveDbOp::insert_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree_with_flags(Some(b"cat".to_vec())),
        )];
        let mut paths = HashMap::new();
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLevel(0, AllSubtrees(4, Some(3))),
        );
        paths.insert(
            KeyInfoPath(vec![KeyInfo::KnownKey(b"key1".to_vec())]),
            EstimatedLevel(0, AllSubtrees(4, None)),
        );
        let average_case_cost = GroveDb::estimated_case_operations_for_batch(
            AverageCaseCostsType(paths),
            ops.clone(),
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
        )
        .cost_as_result()
        .expect("expected to get average case costs");

        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert!(
            average_case_cost.worse_or_eq_than(&cost),
            "not worse {:?} \n than {:?}",
            average_case_cost,
            cost
        );
        // because we know the object we are inserting we can know the average
        // case cost if it doesn't already exist
        assert_eq!(
            cost.storage_cost.added_bytes,
            average_case_cost.storage_cost.added_bytes
        );

        assert_eq!(
            average_case_cost,
            OperationCost {
                seek_count: 6, // todo: why is this 6
                storage_cost: StorageCost {
                    added_bytes: 117,
                    replaced_bytes: 79, // todo: verify
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 8, // todo: verify why
            }
        );
    }

    #[test]
    fn test_batch_root_one_item_insert_op_average_case_costs() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![GroveDbOp::insert_op(
            vec![],
            b"key1".to_vec(),
            Element::new_item(b"cat".to_vec()),
        )];
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), EstimatedLevel(0, AllItems(4, 3, None)));
        let average_case_cost = GroveDb::estimated_case_operations_for_batch(
            AverageCaseCostsType(paths),
            ops.clone(),
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
        )
        .cost_as_result()
        .expect("expected to get average case costs");

        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert!(
            average_case_cost.worse_or_eq_than(&cost),
            "not worse {:?} \n than {:?}",
            average_case_cost,
            cost
        );
        // because we know the object we are inserting we can know the average
        // case cost if it doesn't already exist
        assert_eq!(
            cost.storage_cost.added_bytes,
            average_case_cost.storage_cost.added_bytes
        );

        assert_eq!(
            average_case_cost,
            OperationCost {
                seek_count: 4, // todo: why is this 6
                storage_cost: StorageCost {
                    added_bytes: 147,
                    replaced_bytes: 107, // log(max_elements) * 32 = 640 // todo: verify
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 7, // todo: verify why
            }
        );
    }

    #[test]
    fn test_batch_root_one_tree_insert_op_under_element_average_case_costs() {
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
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), EstimatedLevel(1, AllSubtrees(4, None)));
        paths.insert(
            KeyInfoPath(vec![KeyInfo::KnownKey(b"key1".to_vec())]),
            EstimatedLevel(0, AllSubtrees(2, None)),
        );

        let average_case_cost = GroveDb::estimated_case_operations_for_batch(
            AverageCaseCostsType(paths),
            ops.clone(),
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
        )
        .cost_as_result()
        .expect("expected to get average case costs");

        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert!(
            average_case_cost.worse_or_eq_than(&cost),
            "not worse {:?} \n than {:?}",
            average_case_cost,
            cost
        );
        // because we know the object we are inserting we can know the average
        // case cost if it doesn't already exist
        assert_eq!(
            cost.storage_cost.added_bytes,
            average_case_cost.storage_cost.added_bytes
        );

        assert_eq!(
            average_case_cost,
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
    fn test_batch_root_one_tree_insert_op_in_sub_tree_average_case_costs() {
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
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), EstimatedLevel(0, AllSubtrees(1, None)));
        paths.insert(
            KeyInfoPath(vec![KeyInfo::KnownKey(b"0".to_vec())]),
            EstimatedLevel(0, AllSubtrees(4, None)),
        );
        paths.insert(
            KeyInfoPath(vec![
                KeyInfo::KnownKey(b"0".to_vec()),
                KeyInfo::KnownKey(b"key1".to_vec()),
            ]),
            EstimatedLevel(0, AllSubtrees(4, None)),
        );
        let average_case_cost = GroveDb::estimated_case_operations_for_batch(
            AverageCaseCostsType(paths),
            ops.clone(),
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
        )
        .cost_as_result()
        .expect("expected to get average case costs");

        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        assert!(
            average_case_cost.worse_or_eq_than(&cost),
            "not worse {:?} \n than {:?}",
            average_case_cost,
            cost
        );
        // /// because we know the object we are inserting we can know the average
        // /// case cost if it doesn't already exist
        // assert_eq!(
        //     cost.storage_cost.added_bytes,
        //     average_case_cost.storage_cost.added_bytes
        // );

        assert_eq!(
            average_case_cost,
            OperationCost {
                seek_count: 8, // todo: why is this 8
                storage_cost: StorageCost {
                    added_bytes: 113,
                    replaced_bytes: 223, // todo: verify
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 340,
                hash_node_calls: 14, // todo: verify why
            }
        );
    }

    #[test]
    fn test_batch_average_case_costs() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(vec![], b"keyb", Element::empty_tree(), None, Some(&tx))
            .unwrap()
            .expect("successful root tree leaf insert");

        let ops = vec![GroveDbOp::insert_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), EstimatedLevel(1, AllSubtrees(4, None)));
        paths.insert(KeyInfoPath(vec![KeyInfo::KnownKey(b"key1".to_vec())]), EstimatedLevel(0, AllSubtrees(4, None)));
        let average_case_cost = GroveDb::estimated_case_operations_for_batch(
            AverageCaseCostsType(paths),
            ops.clone(),
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
        ).cost_as_result().expect("expected to estimate costs");
        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        // at the moment we just check the added bytes are the same
        assert_eq!(
            average_case_cost.storage_cost.added_bytes,
            cost.storage_cost.added_bytes
        );
    }
}
