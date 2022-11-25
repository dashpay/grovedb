use std::{
    collections::{BTreeMap, HashMap, HashSet},
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
    cached_merks: HashSet<KeyInfoPath>,
}

impl AverageCaseTreeCacheKnownPaths {
    pub(in crate::batch) fn new_with_estimated_layer_information(
        paths: HashMap<KeyInfoPath, EstimatedLayerInformation>,
    ) -> Self {
        AverageCaseTreeCacheKnownPaths {
            paths,
            cached_merks: HashSet::default(),
        }
    }
}

impl fmt::Debug for AverageCaseTreeCacheKnownPaths {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeCacheKnownPaths").finish()
    }
}

impl<G, SR> TreeCache<G, SR> for AverageCaseTreeCacheKnownPaths {
    fn insert(&mut self, op: &GroveDbOp) -> CostResult<(), Error> {
        let mut average_case_cost = OperationCost::default();
        let mut inserted_path = op.path.clone();
        inserted_path.push(op.key.clone());
        // There is no need to pay for getting a merk, because we know the merk to be
        // empty at this point.
        // There is however a hash call that creates the prefix
        average_case_cost.hash_node_calls += 1;
        self.cached_merks.insert(inserted_path);
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

        let layer_should_be_empty = layer_element_estimates.estimated_to_be_empty();

        // Then we have to get the tree
        if self.cached_merks.get(&path).is_none() {
            GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(
                &mut cost,
                path,
                layer_should_be_empty,
            );
            self.cached_merks.insert(path.clone());
        }

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
        cost.seek_count += 1;
        let base_path = KeyInfoPath(vec![]);
        if let Some(estimated_layer_info) = self.paths.get(&base_path) {
            // Then we have to get the tree
            if self.cached_merks.get(&base_path).is_none() {
                GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(
                    &mut cost,
                    &base_path,
                    estimated_layer_info.estimated_to_be_empty(),
                );
                self.cached_merks.insert(base_path);
            }
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
        EstimatedLayerInformation::{ApproximateElements, EstimatedLevel},
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
        paths.insert(
            KeyInfoPath(vec![]),
            ApproximateElements(0, AllSubtrees(4, None)),
        );
        // paths.insert(
        //     KeyInfoPath(vec![KeyInfo::KnownKey(b"key1".to_vec())]),
        //     ApproximateElements(0, AllSubtrees(4, None)),
        // );
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
            average_case_cost.eq(&cost),
            "average cost not eq {:?} \n to cost {:?}",
            average_case_cost,
            cost
        );
        // because we know the object we are inserting we can know the average
        // case cost if it doesn't already exist
        assert_eq!(
            cost.storage_cost.added_bytes,
            average_case_cost.storage_cost.added_bytes
        );

        // Hash node calls
        // 1 for the tree insert
        // 2 for the node hash
        // 1 for the value hash
        // 1 for the combine hash
        // 1 kv_digest_to_kv_hash

        assert_eq!(
            average_case_cost,
            OperationCost {
                seek_count: 3,
                storage_cost: StorageCost {
                    added_bytes: 113,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 6,
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
            EstimatedLevel(0, true, AllSubtrees(4, Some(3))),
        );
        paths.insert(
            KeyInfoPath(vec![KeyInfo::KnownKey(b"key1".to_vec())]),
            EstimatedLevel(0, true, AllSubtrees(4, None)),
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
        assert_eq!(cost, average_case_cost);

        assert_eq!(
            average_case_cost,
            OperationCost {
                seek_count: 3,
                storage_cost: StorageCost {
                    added_bytes: 117,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 6,
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
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLevel(0, true, AllItems(4, 3, None)),
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
        // because we know the object we are inserting we can know the average
        // case cost if it doesn't already exist
        assert_eq!(
            cost, average_case_cost,
            "cost not same {:?} \n as average case {:?}",
            cost, average_case_cost
        );

        // 4 Hash calls
        // 1 value hash
        // 1 kv_digest_to_kv_hash
        // 2 node hash

        assert_eq!(
            average_case_cost,
            OperationCost {
                seek_count: 3,
                storage_cost: StorageCost {
                    added_bytes: 147,
                    replaced_bytes: 0,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 0,
                hash_node_calls: 4,
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
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLevel(1, false, AllSubtrees(1, None)),
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
        // because we know the object we are inserting we can know the average
        // case cost if it doesn't already exist
        assert_eq!(cost.storage_cost, average_case_cost.storage_cost);
        assert_eq!(cost.hash_node_calls, average_case_cost.hash_node_calls);
        assert_eq!(cost.seek_count, average_case_cost.seek_count);

        // Seek Count explanation (this isn't 100% sure - needs to be verified)
        // 1 to get root merk
        // 1 to load root tree
        // 1 to get previous element
        // 1 to insert
        // 1 to insert node above

        // Replaced parent Value -> 76
        //   1 for the flag option (but no flags)
        //   1 for the enum type
        //   1 for an empty option
        // 32 for node hash
        // 32 for value hash
        // 1 byte for the value_size (required space for 75)

        // Loaded
        // For root key 1 byte
        // For root tree item 69 bytes

        assert_eq!(
            average_case_cost,
            OperationCost {
                seek_count: 5, // todo: why is this 5
                storage_cost: StorageCost {
                    added_bytes: 113,
                    replaced_bytes: 104,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 107,
                hash_node_calls: 8,
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
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLevel(0, false, AllSubtrees(1, None)),
        );
        paths.insert(
            KeyInfoPath(vec![KeyInfo::KnownKey(b"0".to_vec())]),
            EstimatedLevel(0, true, AllSubtrees(4, None)),
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
        assert_eq!(
            average_case_cost.storage_cost, cost.storage_cost,
            "average case storage not eq {:?} \n to cost {:?}",
            average_case_cost.storage_cost, cost.storage_cost
        );
        assert_eq!(average_case_cost.hash_node_calls, cost.hash_node_calls);
        assert_eq!(average_case_cost.seek_count, cost.seek_count);

        //// Seek Count explanation

        // 1 to insert new item
        // 1 to get merk at lower level
        // 1 to get root merk
        // 1 to load root tree
        // 1 to replace parent tree
        // 1 to update root

        assert_eq!(
            average_case_cost,
            OperationCost {
                seek_count: 6,
                storage_cost: StorageCost {
                    added_bytes: 113,
                    replaced_bytes: 73,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 170,
                hash_node_calls: 12,
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
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLevel(1, false, AllSubtrees(4, None)),
        );
        paths.insert(
            KeyInfoPath(vec![KeyInfo::KnownKey(b"key1".to_vec())]),
            EstimatedLevel(0, true, AllSubtrees(4, None)),
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
        .expect("expected to estimate costs");
        let cost = db.apply_batch(ops, None, Some(&tx)).cost;
        // at the moment we just check the added bytes are the same
        assert_eq!(
            average_case_cost.storage_cost.added_bytes,
            cost.storage_cost.added_bytes
        );
    }
}
