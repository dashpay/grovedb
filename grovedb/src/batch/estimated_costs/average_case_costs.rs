// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Average case costs

#[cfg(feature = "full")]
use std::{
    collections::{BTreeMap, HashMap},
    fmt,
};

#[cfg(feature = "full")]
use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
#[cfg(feature = "full")]
use itertools::Itertools;
use merk::RootHashKeyAndSum;
#[cfg(feature = "full")]
use merk::{
    estimated_costs::average_case_costs::{average_case_merk_propagate, EstimatedLayerInformation},
    IsSumTree,
};
#[cfg(feature = "full")]
use storage::rocksdb_storage::RocksDbStorage;

#[cfg(feature = "full")]
use crate::{
    batch::{
        key_info::KeyInfo, mode::BatchRunMode, BatchApplyOptions, GroveDbOp, KeyInfoPath, Op,
        TreeCache,
    },
    Error, GroveDb,
};

#[cfg(feature = "full")]
impl Op {
    /// Get the estimated average case cost of the op. Calls a lower level
    /// function to calculate the estimate based on the type of op. Returns
    /// CostResult.
    fn average_case_cost(
        &self,
        key: &KeyInfo,
        layer_element_estimates: &EstimatedLayerInformation,
        propagate: bool,
    ) -> CostResult<(), Error> {
        let in_tree_using_sums = layer_element_estimates.is_sum_tree;
        let propagate_if_input = || {
            if propagate {
                Some(layer_element_estimates)
            } else {
                None
            }
        };
        match self {
            Op::ReplaceTreeRootKey { sum, .. } => GroveDb::average_case_merk_replace_tree(
                key,
                layer_element_estimates,
                sum.is_some(),
                propagate,
            ),
            Op::InsertTreeWithRootHash { flags, sum, .. } => {
                GroveDb::average_case_merk_insert_tree(
                    key,
                    flags,
                    sum.is_some(),
                    in_tree_using_sums,
                    propagate_if_input(),
                )
            }
            Op::Insert { element } => GroveDb::average_case_merk_insert_element(
                key,
                element,
                in_tree_using_sums,
                propagate_if_input(),
            ),
            Op::Replace { element } => GroveDb::average_case_merk_replace_element(
                key,
                element,
                in_tree_using_sums,
                propagate_if_input(),
            ),
            Op::Delete => {
                GroveDb::average_case_merk_delete_element(key, layer_element_estimates, propagate)
            }
            Op::DeleteTree => GroveDb::average_case_merk_delete_tree(
                key,
                false,
                layer_element_estimates,
                propagate,
            ),
            Op::DeleteSumTree => GroveDb::average_case_merk_delete_tree(
                key,
                true,
                layer_element_estimates,
                propagate,
            ),
        }
    }
}

#[cfg(feature = "full")]
/// Cache for subtree paths for average case scenario costs.
#[derive(Default)]
pub(in crate::batch) struct AverageCaseTreeCacheKnownPaths {
    paths: HashMap<KeyInfoPath, EstimatedLayerInformation>,
    cached_merks: HashMap<KeyInfoPath, IsSumTree>,
}

#[cfg(feature = "full")]
impl AverageCaseTreeCacheKnownPaths {
    /// Updates the cache to the default setting with the given subtree paths
    pub(in crate::batch) fn new_with_estimated_layer_information(
        paths: HashMap<KeyInfoPath, EstimatedLayerInformation>,
    ) -> Self {
        AverageCaseTreeCacheKnownPaths {
            paths,
            cached_merks: HashMap::default(),
        }
    }
}

#[cfg(feature = "full")]
impl fmt::Debug for AverageCaseTreeCacheKnownPaths {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeCacheKnownPaths").finish()
    }
}

#[cfg(feature = "full")]
impl<G, SR> TreeCache<G, SR> for AverageCaseTreeCacheKnownPaths {
    fn insert(&mut self, op: &GroveDbOp, is_sum_tree: bool) -> CostResult<(), Error> {
        let mut average_case_cost = OperationCost::default();
        let mut inserted_path = op.path.clone();
        inserted_path.push(op.key.clone());
        // There is no need to pay for getting a merk, because we know the merk to be
        // empty at this point.
        // There is however a hash call that creates the prefix
        average_case_cost.hash_node_calls += 1;
        self.cached_merks.insert(inserted_path, is_sum_tree);
        Ok(()).wrap_with_cost(average_case_cost)
    }

    fn get_batch_run_mode(&self) -> BatchRunMode {
        BatchRunMode::AverageCase(self.paths.clone())
    }

    fn execute_ops_on_path(
        &mut self,
        path: &KeyInfoPath,
        ops_at_path_by_key: BTreeMap<KeyInfo, Op>,
        _ops_by_qualified_paths: &BTreeMap<Vec<Vec<u8>>, Op>,
        _batch_apply_options: &BatchApplyOptions,
        _flags_update: &mut G,
        _split_removal_bytes: &mut SR,
    ) -> CostResult<RootHashKeyAndSum, Error> {
        let mut cost = OperationCost::default();

        let layer_element_estimates = cost_return_on_error_no_add!(
            &cost,
            self.paths.get(path).ok_or_else(|| {
                let paths = self
                    .paths
                    .iter()
                    .map(|(k, _v)| k.0.iter().map(|k| hex::encode(k.as_slice())).join("/"))
                    .join(" | ");
                Error::PathNotFoundInCacheForEstimatedCosts(format!(
                    "required path {} not found in paths {}",
                    path.0.iter().map(|k| hex::encode(k.as_slice())).join("/"),
                    paths
                ))
            })
        );

        let layer_should_be_empty = layer_element_estimates
            .estimated_layer_count
            .estimated_to_be_empty();

        // Then we have to get the tree
        if self.cached_merks.get(path).is_none() {
            let layer_info = cost_return_on_error_no_add!(
                &cost,
                self.paths.get(path).ok_or_else(|| {
                    let paths = self
                        .paths
                        .iter()
                        .map(|(k, _v)| k.0.iter().map(|k| hex::encode(k.as_slice())).join("/"))
                        .join(" | ");
                    Error::PathNotFoundInCacheForEstimatedCosts(format!(
                        "required path for estimated merk caching {} not found in paths {}",
                        path.0.iter().map(|k| hex::encode(k.as_slice())).join("/"),
                        paths
                    ))
                })
            );
            GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(
                &mut cost,
                path,
                layer_should_be_empty,
                layer_info.is_sum_tree,
            );
            self.cached_merks
                .insert(path.clone(), layer_info.is_sum_tree);
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
        Ok(([0u8; 32], None, None)).wrap_with_cost(cost)
    }

    fn update_base_merk_root_key(&mut self, _root_key: Option<Vec<u8>>) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        cost.seek_count += 1;
        let base_path = KeyInfoPath(vec![]);
        if let Some(estimated_layer_info) = self.paths.get(&base_path) {
            // Then we have to get the tree
            if self.cached_merks.get(&base_path).is_none() {
                GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(
                    &mut cost,
                    &base_path,
                    estimated_layer_info
                        .estimated_layer_count
                        .estimated_to_be_empty(),
                    estimated_layer_info.is_sum_tree,
                );
                self.cached_merks
                    .insert(base_path, estimated_layer_info.is_sum_tree);
            }
        }
        Ok(()).wrap_with_cost(cost)
    }
}

#[cfg(feature = "full")]
#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use costs::{
        storage_cost::{removal::StorageRemovedBytes::NoStorageRemoval, StorageCost},
        OperationCost,
    };
    use merk::estimated_costs::average_case_costs::{
        EstimatedLayerCount::{ApproximateElements, EstimatedLevel, PotentiallyAtMaxElements},
        EstimatedLayerInformation,
        EstimatedLayerSizes::{AllItems, AllSubtrees},
        EstimatedSumTrees::{NoSumTrees, SomeSumTrees},
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
            EstimatedLayerInformation {
                is_sum_tree: false,
                estimated_layer_count: ApproximateElements(0),
                estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
            },
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
                    added_bytes: 115,
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
            EstimatedLayerInformation {
                is_sum_tree: false,
                estimated_layer_count: EstimatedLevel(0, true),
                estimated_layer_sizes: AllSubtrees(4, NoSumTrees, Some(3)),
            },
        );
        paths.insert(
            KeyInfoPath(vec![KeyInfo::KnownKey(b"key1".to_vec())]),
            EstimatedLayerInformation {
                is_sum_tree: false,
                estimated_layer_count: EstimatedLevel(0, true),
                estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
            },
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
            EstimatedLayerInformation {
                is_sum_tree: false,
                estimated_layer_count: EstimatedLevel(0, true),
                estimated_layer_sizes: AllItems(4, 3, None),
            },
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
            EstimatedLayerInformation {
                is_sum_tree: false,
                estimated_layer_count: EstimatedLevel(1, false),
                estimated_layer_sizes: AllSubtrees(1, NoSumTrees, None),
            },
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
                    added_bytes: 115,
                    replaced_bytes: 106,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 109,
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
            EstimatedLayerInformation {
                is_sum_tree: false,
                estimated_layer_count: EstimatedLevel(0, false),
                estimated_layer_sizes: AllSubtrees(1, NoSumTrees, None),
            },
        );

        paths.insert(
            KeyInfoPath(vec![KeyInfo::KnownKey(b"0".to_vec())]),
            EstimatedLayerInformation {
                is_sum_tree: false,
                estimated_layer_count: EstimatedLevel(0, true),
                estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
            },
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
                    added_bytes: 115,
                    replaced_bytes: 75,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 173,
                hash_node_calls: 12,
            }
        );
    }

    #[test]
    fn test_batch_root_one_sum_item_replace_op_average_case_costs() {
        let ops = vec![GroveDbOp::replace_op(
            vec![vec![7]],
            hex::decode("46447a3b4c8939fd4cf8b610ba7da3d3f6b52b39ab2549bf91503b9b07814055")
                .unwrap(),
            Element::new_sum_item(500),
        )];
        let mut paths = HashMap::new();
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                is_sum_tree: false,
                estimated_layer_count: EstimatedLevel(1, false),
                estimated_layer_sizes: AllSubtrees(
                    1,
                    SomeSumTrees {
                        sum_trees_weight: 1,
                        non_sum_trees_weight: 1,
                    },
                    None,
                ),
            },
        );
        paths.insert(
            KeyInfoPath::from_known_owned_path(vec![vec![7]]),
            EstimatedLayerInformation {
                is_sum_tree: true,
                estimated_layer_count: PotentiallyAtMaxElements,
                estimated_layer_sizes: AllItems(32, 8, None),
            },
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

        // because we know the object we are inserting we can know the average
        // case cost if it doesn't already exist
        assert_eq!(average_case_cost.storage_cost.added_bytes, 0);

        assert_eq!(
            average_case_cost,
            OperationCost {
                seek_count: 41,
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 5624,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 7669,
                hash_node_calls: 79,
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
            EstimatedLayerInformation {
                is_sum_tree: false,
                estimated_layer_count: EstimatedLevel(1, false),
                estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
            },
        );

        paths.insert(
            KeyInfoPath(vec![KeyInfo::KnownKey(b"0".to_vec())]),
            EstimatedLayerInformation {
                is_sum_tree: false,
                estimated_layer_count: EstimatedLevel(0, true),
                estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
            },
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
