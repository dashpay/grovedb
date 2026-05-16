//! Worst case costs

#[cfg(feature = "minimal")]
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
};

#[cfg(feature = "minimal")]
use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
#[cfg(feature = "minimal")]
use grovedb_merk::estimated_costs::worst_case_costs::{
    add_worst_case_merk_has_value, worst_case_merk_propagate, WorstCaseLayerInformation,
    MERK_BIGGEST_VALUE_SIZE,
};
use grovedb_merk::{tree::AggregateData, tree_type::TreeType, RootHashKeyAndAggregateData};
#[cfg(feature = "minimal")]
use grovedb_storage::rocksdb_storage::RocksDbStorage;
#[cfg(feature = "minimal")]
use grovedb_storage::worst_case_costs::WorstKeyLength;
use grovedb_version::version::GroveVersion;
#[cfg(feature = "minimal")]
use itertools::Itertools;

use crate::Element;
#[cfg(feature = "minimal")]
use crate::{
    batch::{
        key_info::KeyInfo, mode::BatchRunMode, BatchApplyOptions, GroveOp, KeyInfoPath, TreeCache,
    },
    Error, GroveDb,
};

#[cfg(feature = "minimal")]
impl GroveOp {
    fn worst_case_cost(
        &self,
        key: &KeyInfo,
        in_parent_tree_type: TreeType,
        worst_case_layer_element_estimates: &WorstCaseLayerInformation,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let propagate_if_input = || {
            if propagate {
                Some(worst_case_layer_element_estimates)
            } else {
                None
            }
        };
        match self {
            GroveOp::ReplaceTreeRootKey { aggregate_data, .. } => {
                GroveDb::worst_case_merk_replace_tree(
                    key,
                    aggregate_data.parent_tree_type(),
                    in_parent_tree_type,
                    worst_case_layer_element_estimates,
                    propagate,
                    grove_version,
                )
            }
            GroveOp::InsertTreeWithRootHash {
                flags,
                aggregate_data,
                non_counted,
                not_summed,
                not_counted_or_summed,
                ..
            } => GroveDb::worst_case_merk_insert_tree(
                key,
                flags,
                aggregate_data.parent_tree_type(),
                in_parent_tree_type,
                // See the comment in the corresponding average-case arm.
                super::wrapper_overhead_for(*non_counted, *not_summed, *not_counted_or_summed),
                propagate_if_input(),
                grove_version,
            ),
            GroveOp::InsertOrReplace { element }
            | GroveOp::InsertWithKnownToNotAlreadyExist { element } => {
                GroveDb::worst_case_merk_insert_element(
                    key,
                    element,
                    in_parent_tree_type,
                    propagate_if_input(),
                    grove_version,
                )
            }
            GroveOp::InsertIfNotExists { element, .. } => {
                // Same insert cost as InsertWithKnownToNotAlreadyExist, plus an
                // additional seek to check whether the key already exists.
                // Use MERK_BIGGEST_VALUE_SIZE for the existing value size since
                // the value already stored could be larger than the element
                // being inserted.
                let mut has_cost = OperationCost::default();
                add_worst_case_merk_has_value(
                    &mut has_cost,
                    key.max_length() as u32,
                    MERK_BIGGEST_VALUE_SIZE,
                );
                GroveDb::worst_case_merk_insert_element(
                    key,
                    element,
                    in_parent_tree_type,
                    propagate_if_input(),
                    grove_version,
                )
                .add_cost(has_cost)
            }
            GroveOp::RefreshReference {
                reference_path_type,
                max_reference_hop,
                sum_value,
                flags,
                non_counted,
                ..
            } => {
                // Build the element shape the apply path will write —
                // see the corresponding comment in the average-case
                // estimator.
                let inner = match sum_value {
                    None => Element::Reference(
                        reference_path_type.clone(),
                        *max_reference_hop,
                        flags.clone(),
                    ),
                    Some(sum) => Element::ReferenceWithSumItem(
                        reference_path_type.clone(),
                        *max_reference_hop,
                        *sum,
                        flags.clone(),
                    ),
                };
                let element = if *non_counted {
                    Element::NonCounted(Box::new(inner))
                } else {
                    inner
                };
                GroveDb::worst_case_merk_replace_element(
                    key,
                    &element,
                    in_parent_tree_type,
                    propagate_if_input(),
                    grove_version,
                )
            }
            GroveOp::Replace { element } => GroveDb::worst_case_merk_replace_element(
                key,
                element,
                in_parent_tree_type,
                propagate_if_input(),
                grove_version,
            ),
            GroveOp::Patch {
                element,
                change_in_bytes: _,
            } => GroveDb::worst_case_merk_replace_element(
                key,
                element,
                in_parent_tree_type,
                propagate_if_input(),
                grove_version,
            ),
            GroveOp::Delete => GroveDb::worst_case_merk_delete_element(
                key,
                worst_case_layer_element_estimates,
                propagate,
                grove_version,
            ),
            GroveOp::DeleteTree(tree_type, _) => GroveDb::worst_case_merk_delete_tree(
                key,
                *tree_type,
                worst_case_layer_element_estimates,
                propagate,
                grove_version,
            ),
            GroveOp::CommitmentTreeInsert { payload, .. } => {
                // After preprocessing, CommitmentTreeInsert becomes
                // ReplaceNonMerkTreeRoot. The base cost is a tree root key
                // replacement in the parent Merk.
                let item_cost = GroveDb::worst_case_merk_replace_tree(
                    key,
                    TreeType::CommitmentTree(0),
                    in_parent_tree_type,
                    worst_case_layer_element_estimates,
                    propagate,
                    grove_version,
                );
                use grovedb_costs::storage_cost::{removal::StorageRemovedBytes, StorageCost};
                // Worst-case frontier size with 32 ommers (max depth):
                // 1 (flag) + 8 (position) + 32 (leaf) + 1 (count) + 32*32 = 1066
                const MAX_FRONTIER_SIZE: u32 = 1066;
                // Buffer entry: cmx (32 bytes) + payload
                let buffer_entry_size = 32 + payload.len() as u32;
                // Worst-case Sinsemilla hashes per append:
                // 32 (root computation) + 32 (all ommers cascade) = 64
                const MAX_SINSEMILLA_HASHES: u32 = 64;
                // 1 blake3 hash for running buffer hash
                const MAX_BLAKE3_HASHES: u32 = 1;
                item_cost.add_cost(OperationCost {
                    seek_count: 3, // frontier load + frontier save + buffer write
                    storage_cost: StorageCost {
                        added_bytes: buffer_entry_size,
                        replaced_bytes: MAX_FRONTIER_SIZE,
                        removed_bytes: StorageRemovedBytes::NoStorageRemoval,
                    },
                    storage_loaded_bytes: MAX_FRONTIER_SIZE as u64,
                    hash_node_calls: MAX_BLAKE3_HASHES,
                    sinsemilla_hash_calls: MAX_SINSEMILLA_HASHES,
                })
            }
            GroveOp::MmrTreeAppend { value } => {
                // Cost of updating parent element in the Merk
                let item_cost = GroveDb::worst_case_merk_replace_tree(
                    key,
                    TreeType::MmrTree,
                    in_parent_tree_type,
                    worst_case_layer_element_estimates,
                    propagate,
                    grove_version,
                );
                // Worst-case data I/O: push writes 1 + trailing_ones(leaf_count)
                // nodes. Maximum trailing_ones for u64 is 64 (at 2^64-1 leaves).
                // Each merge reads 1 sibling.
                use grovedb_costs::storage_cost::{removal::StorageRemovedBytes, StorageCost};
                // Internal node: 33 bytes (1 flag + 32 hash)
                const INTERNAL_NODE_SIZE: u32 = 33;
                // Leaf node: 37 + value_len (1 flag + 32 hash + 4 length + value)
                let leaf_node_size = 37 + value.len() as u32;
                // hash_count_for_push = 1 + trailing_ones. Max = 65.
                const MAX_HASH_CALLS: u32 = 65;
                // Max writes: 1 leaf + 64 internal = 65
                const MAX_INTERNAL_WRITES: u32 = 64;
                // Max reads: 64 sibling reads for merges
                const MAX_NODE_READS: u32 = 64;
                item_cost.add_cost(OperationCost {
                    seek_count: 1 + MAX_INTERNAL_WRITES + MAX_NODE_READS,
                    storage_cost: StorageCost {
                        added_bytes: leaf_node_size + INTERNAL_NODE_SIZE * MAX_INTERNAL_WRITES,
                        replaced_bytes: 0,
                        removed_bytes: StorageRemovedBytes::NoStorageRemoval,
                    },
                    storage_loaded_bytes: (INTERNAL_NODE_SIZE * MAX_NODE_READS) as u64,
                    hash_node_calls: MAX_HASH_CALLS,
                    sinsemilla_hash_calls: 0,
                })
            }
            GroveOp::BulkAppend { value } => {
                // Cost of updating parent element in the Merk
                let item_cost = GroveDb::worst_case_merk_replace_tree(
                    key,
                    TreeType::BulkAppendTree(0),
                    in_parent_tree_type,
                    worst_case_layer_element_estimates,
                    propagate,
                    grove_version,
                );
                // Worst case: compaction trigger. Buffer fills → serialize
                // chunk blob → compute dense Merkle root → push to MMR.
                use grovedb_costs::storage_cost::{removal::StorageRemovedBytes, StorageCost};
                // Chunk blob worst case depends on epoch_size. For a single
                // append the value itself is always written. If compaction
                // triggers, the chunk blob is epoch_size * avg_value_size.
                // We use value.len() for the per-append write and a capped
                // compaction overhead.
                let value_size = value.len() as u32;
                // Max compaction overhead: 64KB safe bound for chunk blob
                const MAX_COMPACTION_BLOB: u32 = 65536;
                // Dense Merkle root: epoch_size hashes. Buffer hash: 1.
                // MMR push: up to 64 merges.
                // epoch hashes + buffer + MMR
                const MAX_HASH_CALLS: u32 = 1024 + 1 + 65;
                // Writes: buffer entry + chunk blob + MMR nodes
                const MAX_WRITES: u32 = 1 + 1 + 65;
                const MAX_READS: u32 = 64; // MMR sibling reads
                item_cost.add_cost(OperationCost {
                    seek_count: MAX_WRITES + MAX_READS,
                    storage_cost: StorageCost {
                        added_bytes: value_size + MAX_COMPACTION_BLOB,
                        replaced_bytes: 0,
                        removed_bytes: StorageRemovedBytes::NoStorageRemoval,
                    },
                    storage_loaded_bytes: (33 * MAX_READS) as u64,
                    hash_node_calls: MAX_HASH_CALLS,
                    sinsemilla_hash_calls: 0,
                })
            }
            GroveOp::DenseTreeInsert { value } => {
                // Cost of updating parent element in the Merk
                let item_cost = GroveDb::worst_case_merk_replace_tree(
                    key,
                    TreeType::DenseAppendOnlyFixedSizeTree(0),
                    in_parent_tree_type,
                    worst_case_layer_element_estimates,
                    propagate,
                    grove_version,
                );
                // Worst-case: 1 value write + full root hash recomputation.
                // compute_root_hash visits ALL filled positions: each does
                // 1 read + 2 hashes (value_hash + node_hash).
                // Max height = 15 (u16 count), so max positions = 2^15-1 = 32767.
                // Using practical max: height 8 → 255 positions.
                use grovedb_costs::storage_cost::{removal::StorageRemovedBytes, StorageCost};
                let value_size = value.len() as u32;
                const MAX_COUNT: u32 = 255; // practical worst case (height 8)
                                            // 2 hash calls per node (value_hash + node_hash)
                const MAX_HASH_CALLS: u32 = MAX_COUNT * 2;
                item_cost.add_cost(OperationCost {
                    seek_count: 1 + MAX_COUNT, // 1 write + MAX_COUNT reads
                    storage_cost: StorageCost {
                        added_bytes: value_size,
                        replaced_bytes: 0,
                        removed_bytes: StorageRemovedBytes::NoStorageRemoval,
                    },
                    storage_loaded_bytes: (value_size as u64) * (MAX_COUNT as u64),
                    hash_node_calls: MAX_HASH_CALLS,
                    sinsemilla_hash_calls: 0,
                })
            }
            GroveOp::ReplaceNonMerkTreeRoot { meta, .. } => GroveDb::worst_case_merk_replace_tree(
                key,
                meta.to_tree_type(),
                in_parent_tree_type,
                worst_case_layer_element_estimates,
                propagate,
                grove_version,
            ),
            GroveOp::InsertNonMerkTree {
                flags,
                meta,
                non_counted,
                ..
            } => GroveDb::worst_case_merk_insert_tree(
                key,
                flags,
                meta.to_tree_type(),
                in_parent_tree_type,
                // Non-Merk trees are never sum-bearing, so only the
                // NonCounted wrapper applies.
                if *non_counted { 1 } else { 0 },
                propagate_if_input(),
                grove_version,
            ),
        }
    }
}

#[cfg(feature = "minimal")]
/// Cache for subtree paths for worst case scenario costs.
#[derive(Default)]
pub(in crate::batch) struct WorstCaseTreeCacheKnownPaths {
    paths: HashMap<KeyInfoPath, WorstCaseLayerInformation>,
    cached_merks: HashSet<KeyInfoPath>,
}

#[cfg(feature = "minimal")]
impl WorstCaseTreeCacheKnownPaths {
    /// Updates the cache with the default settings and the given paths
    pub(in crate::batch) fn new_with_worst_case_layer_information(
        paths: HashMap<KeyInfoPath, WorstCaseLayerInformation>,
    ) -> Self {
        WorstCaseTreeCacheKnownPaths {
            paths,
            cached_merks: HashSet::default(),
        }
    }
}

#[cfg(feature = "minimal")]
impl fmt::Debug for WorstCaseTreeCacheKnownPaths {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeCacheKnownPaths").finish()
    }
}

#[cfg(feature = "minimal")]
impl<G, SR> TreeCache<G, SR> for WorstCaseTreeCacheKnownPaths {
    fn insert(
        &mut self,
        path: &KeyInfoPath,
        key: &KeyInfo,
        _tree_type: TreeType,
    ) -> CostResult<(), Error> {
        let mut worst_case_cost = OperationCost::default();
        let mut inserted_path = path.clone();
        inserted_path.push(key.clone());
        // There is no need to pay for getting a merk, because we know the merk to be
        // empty at this point.
        // There is however a hash call that creates the prefix
        worst_case_cost.hash_node_calls += 1;
        self.cached_merks.insert(inserted_path);
        Ok(()).wrap_with_cost(worst_case_cost)
    }

    fn get_batch_run_mode(&self) -> BatchRunMode {
        BatchRunMode::WorstCase(self.paths.clone())
    }

    fn execute_ops_on_path(
        &mut self,
        path: &KeyInfoPath,
        ops_at_path_by_key: BTreeMap<KeyInfo, GroveOp>,
        _ops_by_qualified_paths: &BTreeMap<Vec<Vec<u8>>, GroveOp>,
        _batch_apply_options: &BatchApplyOptions,
        _flags_update: &mut G,
        _split_removal_bytes: &mut SR,
        grove_version: &GroveVersion,
    ) -> CostResult<RootHashKeyAndAggregateData, Error> {
        let mut cost = OperationCost::default();

        let worst_case_layer_element_estimates = cost_return_on_error_no_add!(
            cost,
            self.paths
                .get(path)
                .ok_or_else(|| Error::PathNotFoundInCacheForEstimatedCosts(format!(
                    "inserting into worst case costs path: {}",
                    path.0.iter().map(|k| hex::encode(k.as_slice())).join("/")
                )))
        );

        // Then we have to get the tree
        if !self.cached_merks.contains(path) {
            cost_return_on_error_no_add!(
                cost,
                GroveDb::add_worst_case_get_merk_at_path::<RocksDbStorage>(
                    &mut cost,
                    path,
                    TreeType::NormalTree,
                    grove_version,
                )
            );
            self.cached_merks.insert(path.clone());
        }

        for (key, op) in ops_at_path_by_key.into_iter() {
            cost_return_on_error!(
                &mut cost,
                op.worst_case_cost(
                    &key,
                    TreeType::NormalTree,
                    worst_case_layer_element_estimates,
                    false,
                    grove_version
                )
            );
        }

        cost_return_on_error!(
            &mut cost,
            worst_case_merk_propagate(worst_case_layer_element_estimates).map_err(Error::MerkError)
        );
        Ok(([0u8; 32], None, AggregateData::NoAggregateData)).wrap_with_cost(cost)
    }

    fn update_base_merk_root_key(
        &mut self,
        _root_key: Option<Vec<u8>>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        cost.seek_count += 1;
        let base_path = KeyInfoPath(vec![]);
        if let Some(_estimated_layer_info) = self.paths.get(&base_path)
            && !self.cached_merks.contains(&base_path)
        {
            // Then we have to get the tree
            cost_return_on_error_no_add!(
                cost,
                GroveDb::add_worst_case_get_merk_at_path::<RocksDbStorage>(
                    &mut cost,
                    &base_path,
                    TreeType::NormalTree,
                    grove_version,
                )
            );
            self.cached_merks.insert(base_path);
        }
        Ok(()).wrap_with_cost(cost)
    }
}

#[cfg(feature = "minimal")]
#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use grovedb_costs::{
        storage_cost::{removal::StorageRemovedBytes::NoStorageRemoval, StorageCost},
        OperationCost,
    };
    #[rustfmt::skip]
    use grovedb_merk::estimated_costs::worst_case_costs::WorstCaseLayerInformation::MaxElementsNumber;
    use grovedb_merk::tree_type::TreeType;
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::{
            estimated_costs::EstimatedCostsType::WorstCaseCostsType, key_info::KeyInfo, GroveOp,
            KeyInfoPath, NonMerkTreeMeta, QualifiedGroveDbOp, SubelementsDeletionBehavior,
        },
        reference_path::ReferencePathType,
        tests::{common::EMPTY_PATH, make_empty_grovedb},
        Element, GroveDb,
    };

    #[test]
    fn test_batch_root_one_tree_insert_op_worst_case_costs() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
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
            grove_version,
        )
        .cost_as_result()
        .expect("expected to get worst case costs");

        let cost = db.apply_batch(ops, None, Some(&tx), grove_version).cost;
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
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_batch_root_one_tree_with_flags_insert_op_worst_case_costs() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
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
            grove_version,
        )
        .cost_as_result()
        .expect("expected to get worst case costs");

        let cost = db.apply_batch(ops, None, Some(&tx), grove_version).cost;
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
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_batch_root_one_item_insert_op_worst_case_costs() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
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
            grove_version,
        )
        .cost_as_result()
        .expect("expected to get worst case costs");

        let cost = db.apply_batch(ops, None, Some(&tx), grove_version).cost;
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
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_batch_root_one_tree_insert_op_under_element_worst_case_costs() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"0",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("successful root tree leaf insert");

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
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
            grove_version,
        )
        .cost_as_result()
        .expect("expected to get worst case costs");

        let cost = db.apply_batch(ops, None, Some(&tx), grove_version).cost;
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
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_batch_root_one_tree_insert_op_in_sub_tree_worst_case_costs() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"0",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("successful root tree leaf insert");

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
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
            grove_version,
        )
        .cost_as_result()
        .expect("expected to get worst case costs");

        let cost = db.apply_batch(ops, None, Some(&tx), grove_version).cost;
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
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_batch_worst_case_costs() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            EMPTY_PATH,
            b"keyb",
            Element::empty_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("successful root tree leaf insert");

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
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
            grove_version,
        );
        assert!(worst_case_cost_result.value.is_ok());
        let cost = db.apply_batch(ops, None, Some(&tx), grove_version).cost;
        // at the moment we just check the added bytes are the same
        assert_eq!(
            worst_case_cost_result.cost.storage_cost.added_bytes,
            cost.storage_cost.added_bytes
        );
    }

    // ---------------------------------------------------------------
    // Tests for previously uncovered GroveOp match arms in worst_case_cost
    // ---------------------------------------------------------------

    // Approach 1: Tests via estimated_case_operations_for_batch (keyed ops)

    #[test]
    fn test_refresh_reference_worst_case_cost() {
        let grove_version = GroveVersion::latest();
        let ops = vec![QualifiedGroveDbOp::refresh_reference_op(
            vec![vec![7]],
            b"ref_key".to_vec(),
            ReferencePathType::AbsolutePathReference(vec![b"target".to_vec()]),
            Some(5),
            None,
            true,
        )];
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(1));
        paths.insert(
            KeyInfoPath::from_known_owned_path(vec![vec![7]]),
            MaxElementsNumber(100),
        );
        let cost = GroveDb::estimated_case_operations_for_batch(
            WorstCaseCostsType(paths),
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("expected worst case costs for refresh reference");
        assert!(cost.seek_count > 0);
        assert!(cost.hash_node_calls > 0);
    }

    #[test]
    fn test_refresh_reference_with_sum_item_worst_case_cost() {
        let grove_version = GroveVersion::latest();
        let ops = vec![QualifiedGroveDbOp::refresh_reference_with_sum_item_op(
            vec![vec![7]],
            b"ref_key".to_vec(),
            ReferencePathType::AbsolutePathReference(vec![b"target".to_vec()]),
            Some(5),
            42,    // sum_value
            None,  // flags
            false, // non_counted
            true,  // trust_refresh_reference
        )];
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(1));
        paths.insert(
            KeyInfoPath::from_known_owned_path(vec![vec![7]]),
            MaxElementsNumber(100),
        );
        let cost = GroveDb::estimated_case_operations_for_batch(
            WorstCaseCostsType(paths),
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("expected worst case costs for refresh reference with sum item");
        assert!(cost.seek_count > 0);
        assert!(cost.hash_node_calls > 0);
    }

    #[test]
    fn test_refresh_reference_with_sum_item_non_counted_worst_case_cost() {
        // Symmetric to the average-case test: verify the non_counted=true
        // variant's worst-case estimate is at least as large as the
        // bare variant. Before the fix the estimator dropped the
        // NonCounted wrapper byte from the cost model.
        let grove_version = GroveVersion::latest();
        let nc_ops = vec![QualifiedGroveDbOp::refresh_reference_with_sum_item_op(
            vec![vec![7]],
            b"ref_key".to_vec(),
            ReferencePathType::AbsolutePathReference(vec![b"target".to_vec()]),
            Some(5),
            42,
            None,
            /* non_counted = */ true,
            /* trust_refresh_reference = */ true,
        )];
        let bare_ops = vec![QualifiedGroveDbOp::refresh_reference_with_sum_item_op(
            vec![vec![7]],
            b"ref_key".to_vec(),
            ReferencePathType::AbsolutePathReference(vec![b"target".to_vec()]),
            Some(5),
            42,
            None,
            /* non_counted = */ false,
            /* trust_refresh_reference = */ true,
        )];
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(1));
        paths.insert(
            KeyInfoPath::from_known_owned_path(vec![vec![7]]),
            MaxElementsNumber(100),
        );
        let nc_cost = GroveDb::estimated_case_operations_for_batch(
            WorstCaseCostsType(paths.clone()),
            nc_ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("expected worst case costs for non-counted refresh");
        let bare_cost = GroveDb::estimated_case_operations_for_batch(
            WorstCaseCostsType(paths),
            bare_ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("expected worst case costs for bare refresh");

        // Strict `>`: NonCounted-wrapped element must be at least one
        // wrapper-discriminant byte larger than the bare variant. The
        // bug we're pinning is an undercount that produces an
        // *identical* estimate, so equality must fail this check.
        assert!(
            nc_cost.storage_cost.added_bytes + nc_cost.storage_cost.replaced_bytes
                > bare_cost.storage_cost.added_bytes + bare_cost.storage_cost.replaced_bytes,
            "non_counted=true cost must be strictly greater than bare cost; nc={:?}, bare={:?}",
            nc_cost,
            bare_cost,
        );
        assert!(nc_cost.seek_count > 0);
        assert!(nc_cost.hash_node_calls > 0);
    }

    #[test]
    fn test_patch_worst_case_cost() {
        let grove_version = GroveVersion::latest();
        let ops = vec![QualifiedGroveDbOp::patch_op(
            vec![vec![7]],
            b"patch_key".to_vec(),
            Element::new_item(b"patched_value".to_vec()),
            5,
        )];
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(1));
        paths.insert(
            KeyInfoPath::from_known_owned_path(vec![vec![7]]),
            MaxElementsNumber(100),
        );
        let cost = GroveDb::estimated_case_operations_for_batch(
            WorstCaseCostsType(paths),
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("expected worst case costs for patch");
        assert!(cost.seek_count > 0);
        assert!(cost.hash_node_calls > 0);
    }

    #[test]
    fn test_delete_worst_case_cost() {
        let grove_version = GroveVersion::latest();
        let ops = vec![QualifiedGroveDbOp::delete_op(
            vec![vec![7]],
            b"del_key".to_vec(),
        )];
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(1));
        paths.insert(
            KeyInfoPath::from_known_owned_path(vec![vec![7]]),
            MaxElementsNumber(100),
        );
        let cost = GroveDb::estimated_case_operations_for_batch(
            WorstCaseCostsType(paths),
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("expected worst case costs for delete");
        assert!(cost.seek_count > 0);
    }

    #[test]
    fn test_delete_tree_worst_case_cost() {
        let grove_version = GroveVersion::latest();
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![vec![7]],
            b"tree_key".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Error,
        )];
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(1));
        paths.insert(
            KeyInfoPath::from_known_owned_path(vec![vec![7]]),
            MaxElementsNumber(100),
        );
        let cost = GroveDb::estimated_case_operations_for_batch(
            WorstCaseCostsType(paths),
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("expected worst case costs for delete tree");
        assert!(cost.seek_count > 0);
    }

    // Approach 2: Direct worst_case_cost() tests (keyless/internal ops)

    #[test]
    fn test_commitment_tree_insert_worst_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        let op = GroveOp::CommitmentTreeInsert {
            cmx: [1u8; 32],
            rho: [2u8; 32],
            payload: vec![0u8; 100],
        };
        let key = KeyInfo::KnownKey(b"tree_key".to_vec());
        let cost = op
            .worst_case_cost(
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
                grove_version,
            )
            .cost_as_result()
            .expect("expected worst case cost for commitment tree insert");
        assert!(cost.seek_count > 0);
        assert!(cost.sinsemilla_hash_calls > 0);
    }

    #[test]
    fn test_commitment_tree_insert_worst_case_cost_with_propagate() {
        let grove_version = GroveVersion::latest();
        let op = GroveOp::CommitmentTreeInsert {
            cmx: [1u8; 32],
            rho: [2u8; 32],
            payload: vec![0u8; 50],
        };
        let key = KeyInfo::KnownKey(b"tree_key".to_vec());
        let cost = op
            .worst_case_cost(
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                true,
                grove_version,
            )
            .cost_as_result()
            .expect("expected worst case cost for commitment tree insert with propagate");
        assert!(cost.seek_count > 0);
        assert!(cost.sinsemilla_hash_calls > 0);
        // propagate adds additional hash calls for merk propagation
        assert!(cost.hash_node_calls > 0);
    }

    #[test]
    fn test_mmr_tree_append_worst_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        let op = GroveOp::MmrTreeAppend {
            value: vec![0u8; 64],
        };
        let key = KeyInfo::KnownKey(b"mmr_key".to_vec());
        let cost = op
            .worst_case_cost(
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
                grove_version,
            )
            .cost_as_result()
            .expect("expected worst case cost for mmr tree append");
        assert!(cost.seek_count > 0);
        assert!(cost.hash_node_calls > 0);
        // MMR append uses blake3, not sinsemilla
        assert_eq!(cost.sinsemilla_hash_calls, 0);
    }

    #[test]
    fn test_bulk_append_worst_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        let op = GroveOp::BulkAppend {
            value: vec![0u8; 128],
        };
        let key = KeyInfo::KnownKey(b"bulk_key".to_vec());
        let cost = op
            .worst_case_cost(
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
                grove_version,
            )
            .cost_as_result()
            .expect("expected worst case cost for bulk append");
        assert!(cost.seek_count > 0);
        assert!(cost.hash_node_calls > 0);
        assert_eq!(cost.sinsemilla_hash_calls, 0);
    }

    #[test]
    fn test_dense_tree_insert_worst_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        let op = GroveOp::DenseTreeInsert {
            value: vec![0u8; 32],
        };
        let key = KeyInfo::KnownKey(b"dense_key".to_vec());
        let cost = op
            .worst_case_cost(
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
                grove_version,
            )
            .cost_as_result()
            .expect("expected worst case cost for dense tree insert");
        assert!(cost.seek_count > 0);
        assert!(cost.hash_node_calls > 0);
        assert_eq!(cost.sinsemilla_hash_calls, 0);
    }

    #[test]
    fn test_replace_non_merk_tree_root_worst_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        let op = GroveOp::ReplaceNonMerkTreeRoot {
            hash: [3u8; 32],
            meta: NonMerkTreeMeta::CommitmentTree {
                total_count: 10,
                chunk_power: 4,
            },
        };
        let key = KeyInfo::KnownKey(b"nmerk_key".to_vec());
        let cost = op
            .worst_case_cost(
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                true,
                grove_version,
            )
            .cost_as_result()
            .expect("expected worst case cost for replace non-merk tree root");
        // With propagation the merk replace tree operation produces seeks
        assert!(cost.seek_count > 0 || cost.hash_node_calls > 0);
    }

    #[test]
    fn test_replace_non_merk_tree_root_mmr_worst_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        let op = GroveOp::ReplaceNonMerkTreeRoot {
            hash: [4u8; 32],
            meta: NonMerkTreeMeta::MmrTree { mmr_size: 100 },
        };
        let key = KeyInfo::KnownKey(b"nmerk_mmr".to_vec());
        let cost = op
            .worst_case_cost(
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(50),
                true,
                grove_version,
            )
            .cost_as_result()
            .expect("expected worst case cost for replace non-merk mmr tree root");
        assert!(cost.seek_count > 0);
    }

    #[test]
    fn test_insert_non_merk_tree_worst_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        use grovedb_merk::tree::AggregateData;
        let op = GroveOp::InsertNonMerkTree {
            hash: [5u8; 32],
            root_key: None,
            flags: None,
            aggregate_data: AggregateData::NoAggregateData,
            meta: NonMerkTreeMeta::DenseTree {
                count: 0,
                height: 8,
            },

            non_counted: false,
        };
        let key = KeyInfo::KnownKey(b"new_dense".to_vec());
        let cost = op
            .worst_case_cost(
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
                grove_version,
            )
            .cost_as_result()
            .expect("expected worst case cost for insert non-merk tree");
        assert!(cost.seek_count > 0);
    }

    #[test]
    fn test_insert_non_merk_tree_with_flags_worst_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        use grovedb_merk::tree::AggregateData;
        let op = GroveOp::InsertNonMerkTree {
            hash: [6u8; 32],
            root_key: Some(b"rk".to_vec()),
            flags: Some(b"flag_data".to_vec()),
            aggregate_data: AggregateData::NoAggregateData,
            meta: NonMerkTreeMeta::BulkAppendTree {
                total_count: 0,
                chunk_power: 3,
            },

            non_counted: false,
        };
        let key = KeyInfo::KnownKey(b"new_bulk".to_vec());
        let cost = op
            .worst_case_cost(
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                true,
                grove_version,
            )
            .cost_as_result()
            .expect("expected worst case cost for insert non-merk tree with flags");
        assert!(cost.seek_count > 0);
        assert!(cost.hash_node_calls > 0);
    }

    /// Covers the wrapper-byte accounting in
    /// `GroveOp::InsertTreeWithRootHash::worst_case_cost`. The three
    /// wrapper bits (`non_counted` / `not_summed` /
    /// `not_counted_or_summed`) each prepend one bincode discriminant
    /// byte to the rebuilt tree element; the cost estimator must
    /// include it in `value_len`. Mirror of the average-case test.
    #[test]
    fn test_insert_tree_with_root_hash_wrapper_bits_worst_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        use grovedb_merk::tree::AggregateData;
        let key = KeyInfo::KnownKey(b"merk_key".to_vec());
        let cost_for = |non_counted: bool, not_summed: bool, not_counted_or_summed: bool| {
            let op = GroveOp::InsertTreeWithRootHash {
                hash: [0xAAu8; 32],
                root_key: None,
                flags: None,
                aggregate_data: AggregateData::NoAggregateData,
                non_counted,
                not_summed,
                not_counted_or_summed,
            };
            op.worst_case_cost(
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
                grove_version,
            )
            .cost_as_result()
            .expect("expected worst case cost for InsertTreeWithRootHash")
        };
        let bare = cost_for(false, false, false);
        let nc = cost_for(true, false, false);
        let ns = cost_for(false, true, false);
        let ncs = cost_for(false, false, true);
        assert!(
            nc.storage_cost.added_bytes > bare.storage_cost.added_bytes,
            "non_counted should add wrapper byte; nc={:?}, bare={:?}",
            nc,
            bare,
        );
        assert!(
            ns.storage_cost.added_bytes > bare.storage_cost.added_bytes,
            "not_summed should add wrapper byte; ns={:?}, bare={:?}",
            ns,
            bare,
        );
        assert!(
            ncs.storage_cost.added_bytes > bare.storage_cost.added_bytes,
            "not_counted_or_summed should add wrapper byte; ncs={:?}, bare={:?}",
            ncs,
            bare,
        );
    }

    /// Covers the wrapper-byte accounting in
    /// `GroveOp::InsertNonMerkTree::worst_case_cost`. Non-Merk trees
    /// only carry `non_counted`.
    #[test]
    fn test_insert_non_merk_tree_non_counted_worst_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        use grovedb_merk::tree::AggregateData;
        let key = KeyInfo::KnownKey(b"new_dense".to_vec());
        let cost_for = |non_counted: bool| {
            let op = GroveOp::InsertNonMerkTree {
                hash: [5u8; 32],
                root_key: None,
                flags: None,
                aggregate_data: AggregateData::NoAggregateData,
                meta: NonMerkTreeMeta::DenseTree {
                    count: 0,
                    height: 8,
                },
                non_counted,
            };
            op.worst_case_cost(
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
                grove_version,
            )
            .cost_as_result()
            .expect("expected worst case cost for InsertNonMerkTree")
        };
        let bare = cost_for(false);
        let nc = cost_for(true);
        assert!(
            nc.storage_cost.added_bytes > bare.storage_cost.added_bytes,
            "InsertNonMerkTree non_counted should add wrapper byte; nc={:?}, bare={:?}",
            nc,
            bare,
        );
    }

    #[test]
    fn test_replace_worst_case_cost() {
        let grove_version = GroveVersion::latest();
        let ops = vec![QualifiedGroveDbOp::replace_op(
            vec![vec![7]],
            b"key1".to_vec(),
            Element::new_item(b"val".to_vec()),
        )];
        let mut paths = HashMap::new();
        paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(1));
        paths.insert(
            KeyInfoPath::from_known_owned_path(vec![vec![7]]),
            MaxElementsNumber(100),
        );
        let cost = GroveDb::estimated_case_operations_for_batch(
            WorstCaseCostsType(paths),
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("expected worst case costs for replace");
        assert!(cost.seek_count > 0);
        assert!(cost.hash_node_calls > 0);
    }
}
