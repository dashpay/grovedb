use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
};

use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use itertools::Itertools;
use merk::{
    estimated_costs::worst_case_costs::{worst_case_merk_propagate, WorstCaseLayerInformation},
    CryptoHash,
};
use storage::rocksdb_storage::RocksDbStorage;

use crate::{
    batch::{
        key_info::KeyInfo,
        mode::{BatchRunMode, BatchRunMode::WorstCaseMode},
        BatchApplyOptions, GroveDbOp, KeyInfoPath, Op, TreeCache,
    },
    Error, GroveDb,
};

impl Op {
    fn worst_case_cost(
        &self,
        key: &KeyInfo,
        is_in_parent_sum_tree: bool,
        worst_case_layer_element_estimates: &WorstCaseLayerInformation,
        propagate: bool,
    ) -> CostResult<(), Error> {
        let propagate_if_input = || {
            if propagate {
                Some(worst_case_layer_element_estimates)
            } else {
                None
            }
        };
        match self {
            Op::ReplaceTreeRootKey { sum, .. } => GroveDb::worst_case_merk_replace_tree(
                key,
                sum.is_some(),
                is_in_parent_sum_tree,
                worst_case_layer_element_estimates,
                propagate,
            ),
            Op::InsertTreeWithRootHash { flags, sum, .. } => GroveDb::worst_case_merk_insert_tree(
                key,
                flags,
                sum.is_some(),
                is_in_parent_sum_tree,
                propagate_if_input(),
            ),
            Op::Insert { element } => GroveDb::worst_case_merk_insert_element(
                key,
                &element,
                is_in_parent_sum_tree,
                propagate_if_input(),
            ),
            Op::Replace { element } => GroveDb::worst_case_merk_replace_element(
                key,
                &element,
                is_in_parent_sum_tree,
                propagate_if_input(),
            ),
            Op::Delete => GroveDb::worst_case_merk_delete_element(
                key,
                worst_case_layer_element_estimates,
                propagate,
            ),
            Op::DeleteTree => GroveDb::worst_case_merk_delete_tree(
                key,
                false,
                worst_case_layer_element_estimates,
                propagate,
            ),
            Op::DeleteSumTree => GroveDb::worst_case_merk_delete_tree(
                key,
                true,
                worst_case_layer_element_estimates,
                propagate,
            ),
        }
    }
}

/// Cache for subtree paths for worst case scenario costs.
#[derive(Default)]
pub(in crate::batch) struct WorstCaseTreeCacheKnownPaths {
    paths: HashMap<KeyInfoPath, WorstCaseLayerInformation>,
    cached_merks: HashSet<KeyInfoPath>,
}

impl WorstCaseTreeCacheKnownPaths {
    pub(in crate::batch) fn new_with_worst_case_layer_information(
        paths: HashMap<KeyInfoPath, WorstCaseLayerInformation>,
    ) -> Self {
        WorstCaseTreeCacheKnownPaths {
            paths,
            cached_merks: HashSet::default(),
        }
    }
}

impl fmt::Debug for WorstCaseTreeCacheKnownPaths {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeCacheKnownPaths").finish()
    }
}

impl<G, SR> TreeCache<G, SR> for WorstCaseTreeCacheKnownPaths {
    fn insert(&mut self, op: &GroveDbOp, _is_sum_tree: bool) -> CostResult<(), Error> {
        let mut worst_case_cost = OperationCost::default();
        let mut inserted_path = op.path.clone();
        inserted_path.push(op.key.clone());
        // There is no need to pay for getting a merk, because we know the merk to be
        // empty at this point.
        // There is however a hash call that creates the prefix
        worst_case_cost.hash_node_calls += 1;
        self.cached_merks.insert(inserted_path);
        Ok(()).wrap_with_cost(worst_case_cost)
    }

    fn get_batch_run_mode(&self) -> BatchRunMode {
        WorstCaseMode(self.paths.clone())
    }

    fn execute_ops_on_path(
        &mut self,
        path: &KeyInfoPath,
        ops_at_path_by_key: BTreeMap<KeyInfo, Op>,
        _ops_by_qualified_paths: &BTreeMap<Vec<Vec<u8>>, Op>,
        _batch_apply_options: &BatchApplyOptions,
        _flags_update: &mut G,
        _split_removal_bytes: &mut SR,
    ) -> CostResult<(CryptoHash, Option<Vec<u8>>, Option<i64>), Error> {
        let mut cost = OperationCost::default();

        let worst_case_layer_element_estimates = cost_return_on_error_no_add!(
            &cost,
            self.paths
                .get(path)
                .ok_or(Error::PathNotFoundInCacheForEstimatedCosts(format!(
                    "inserting into worst case costs path: {}",
                    path.0.iter().map(|k| hex::encode(k.as_slice())).join("/")
                )))
        );

        // Then we have to get the tree
        if self.cached_merks.get(&path).is_none() {
            GroveDb::add_worst_case_get_merk_at_path::<RocksDbStorage>(&mut cost, path, false);
            self.cached_merks.insert(path.clone());
        }

        for (key, op) in ops_at_path_by_key.into_iter() {
            cost_return_on_error!(
                &mut cost,
                op.worst_case_cost(&key, false, worst_case_layer_element_estimates, false)
            );
        }

        cost_return_on_error!(
            &mut cost,
            worst_case_merk_propagate(worst_case_layer_element_estimates).map_err(Error::MerkError)
        );
        Ok(([0u8; 32], None, None)).wrap_with_cost(cost)
    }

    fn update_base_merk_root_key(&mut self, _root_key: Option<Vec<u8>>) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        cost.seek_count += 1;
        let base_path = KeyInfoPath(vec![]);
        if let Some(_estimated_layer_info) = self.paths.get(&base_path) {
            // Then we have to get the tree
            if self.cached_merks.get(&base_path).is_none() {
                GroveDb::add_worst_case_get_merk_at_path::<RocksDbStorage>(
                    &mut cost, &base_path, false,
                );
                self.cached_merks.insert(base_path);
            }
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
    use merk::estimated_costs::worst_case_costs::WorstCaseLayerInformation::MaxElementsNumber;

    use crate::{
        batch::{
            estimated_costs::EstimatedCostsType::WorstCaseCostsType, key_info::KeyInfo, GroveDbOp,
            KeyInfoPath,
        },
        tests::make_empty_grovedb,
        Element, GroveDb,
    };

    #[test]
    fn test_batch_root_one_tree_insert_op_worst_case_costs() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![GroveDbOp::insert_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(1));
        let worst_case_cost = GroveDb::estimated_case_operations_for_batch(
            WorstCaseCostsType(paths),
            ops.clone(),
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
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
                seek_count: 5,
                storage_cost: StorageCost {
                    added_bytes: 115,
                    replaced_bytes: 65535, // todo: verify
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 65791,
                hash_node_calls: 8, // todo: verify why
            }
        );
    }

    #[test]
    fn test_batch_root_one_tree_with_flags_insert_op_worst_case_costs() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![GroveDbOp::insert_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree_with_flags(Some(b"cat".to_vec())),
        )];
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(0));
        let worst_case_cost = GroveDb::estimated_case_operations_for_batch(
            WorstCaseCostsType(paths),
            ops.clone(),
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
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
                seek_count: 4,
                storage_cost: StorageCost {
                    added_bytes: 119,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 6,
            }
        );
    }

    #[test]
    fn test_batch_root_one_item_insert_op_worst_case_costs() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![GroveDbOp::insert_op(
            vec![],
            b"key1".to_vec(),
            Element::new_item(b"cat".to_vec()),
        )];
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(0));
        let worst_case_cost = GroveDb::estimated_case_operations_for_batch(
            WorstCaseCostsType(paths),
            ops.clone(),
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
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
                seek_count: 4,
                storage_cost: StorageCost {
                    added_bytes: 149,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 4,
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

        let ops = vec![GroveDbOp::insert_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(u32::MAX));
        let worst_case_cost = GroveDb::estimated_case_operations_for_batch(
            WorstCaseCostsType(paths),
            ops.clone(),
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
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
                seek_count: 38,
                storage_cost: StorageCost {
                    added_bytes: 115,
                    replaced_bytes: 2228190, // todo: verify
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 2236894,
                hash_node_calls: 74,
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

        let ops = vec![GroveDbOp::insert_op(
            vec![b"0".to_vec()],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(1));
        paths.insert(
            KeyInfoPath(vec![KeyInfo::KnownKey(b"0".to_vec())]),
            MaxElementsNumber(0),
        );
        let worst_case_cost = GroveDb::estimated_case_operations_for_batch(
            WorstCaseCostsType(paths),
            ops.clone(),
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
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

        assert_eq!(
            worst_case_cost,
            OperationCost {
                seek_count: 7,
                storage_cost: StorageCost {
                    added_bytes: 115,
                    replaced_bytes: 81996,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 65964,
                hash_node_calls: 266,
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

        let ops = vec![GroveDbOp::insert_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(u32::MAX));
        let worst_case_cost_result = GroveDb::estimated_case_operations_for_batch(
            WorstCaseCostsType(paths),
            ops.clone(),
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
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
