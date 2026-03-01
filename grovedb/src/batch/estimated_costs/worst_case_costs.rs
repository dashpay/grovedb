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
    worst_case_merk_propagate, WorstCaseLayerInformation,
};
use grovedb_merk::{tree::AggregateData, tree_type::TreeType, RootHashKeyAndAggregateData};
#[cfg(feature = "minimal")]
use grovedb_storage::rocksdb_storage::RocksDbStorage;
use grovedb_version::version::GroveVersion;
#[cfg(feature = "minimal")]
use itertools::Itertools;

use crate::Element;
#[cfg(feature = "minimal")]
use crate::{
    batch::{
        key_info::KeyInfo, mode::BatchRunMode, BatchApplyOptions, GroveOp, KeyInfoPath,
        QualifiedGroveDbOp, TreeCache,
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
                ..
            } => GroveDb::worst_case_merk_insert_tree(
                key,
                flags,
                aggregate_data.parent_tree_type(),
                in_parent_tree_type,
                propagate_if_input(),
                grove_version,
            ),
            GroveOp::InsertOrReplace { element } | GroveOp::InsertOnly { element } => {
                GroveDb::worst_case_merk_insert_element(
                    key,
                    element,
                    in_parent_tree_type,
                    propagate_if_input(),
                    grove_version,
                )
            }
            GroveOp::RefreshReference {
                reference_path_type,
                max_reference_hop,
                flags,
                ..
            } => GroveDb::worst_case_merk_replace_element(
                key,
                &Element::Reference(
                    reference_path_type.clone(),
                    *max_reference_hop,
                    flags.clone(),
                ),
                in_parent_tree_type,
                propagate_if_input(),
                grove_version,
            ),
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
            GroveOp::DeleteTree(tree_type) => GroveDb::worst_case_merk_delete_tree(
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
            GroveOp::InsertNonMerkTree { flags, meta, .. } => GroveDb::worst_case_merk_insert_tree(
                key,
                flags,
                meta.to_tree_type(),
                in_parent_tree_type,
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
    fn insert(&mut self, op: &QualifiedGroveDbOp, _tree_type: TreeType) -> CostResult<(), Error> {
        let mut worst_case_cost = OperationCost::default();
        let mut inserted_path = op.path.clone();
        let key = cost_return_on_error_no_add!(
            worst_case_cost,
            op.key
                .clone()
                .ok_or(Error::InvalidBatchOperation("insert op is missing a key"))
        );
        inserted_path.push(key);
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
        if let Some(_estimated_layer_info) = self.paths.get(&base_path) {
            // Then we have to get the tree
            if !self.cached_merks.contains(&base_path) {
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
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::{
            estimated_costs::EstimatedCostsType::WorstCaseCostsType, key_info::KeyInfo,
            KeyInfoPath, QualifiedGroveDbOp,
        },
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
}
