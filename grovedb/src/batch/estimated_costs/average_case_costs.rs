//! Average case costs

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
use grovedb_merk::estimated_costs::average_case_costs::{
    add_average_case_merk_has_value, average_case_merk_propagate, EstimatedLayerInformation,
};
use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions, tree::AggregateData, tree_type::TreeType,
    RootHashKeyAndAggregateData,
};
#[cfg(feature = "minimal")]
use grovedb_storage::rocksdb_storage::RocksDbStorage;
#[cfg(feature = "minimal")]
use grovedb_storage::worst_case_costs::WorstKeyLength;
use grovedb_version::{error::GroveVersionError, version::GroveVersion};
#[cfg(feature = "minimal")]
use integer_encoding::VarInt;
#[cfg(feature = "minimal")]
use itertools::Itertools;

use crate::Element;
#[cfg(feature = "minimal")]
use crate::{
    batch::{
        key_info::KeyInfo, mode::BatchRunMode, BatchApplyOptions, GroveOp, KeyInfoPath,
        RefreshReferenceMode, TreeCache,
    },
    Error, GroveDb,
};

#[cfg(feature = "minimal")]
impl GroveOp {
    /// Get the estimated average case cost of the op. Calls a lower level
    /// function to calculate the estimate based on the type of op. Returns
    /// CostResult.
    fn average_case_cost(
        &self,
        key: &KeyInfo,
        layer_element_estimates: &EstimatedLayerInformation,
        // The declared chunk power of the commitment tree a
        // `CommitmentTreeInsert` op targets (from the tree's own layer in
        // the estimation paths), or `None` to charge the constructor-
        // enforced cap. Ignored by every other op type.
        ct_chunk_power: Option<u8>,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let in_tree_type = layer_element_estimates.tree_type;
        let propagate_if_input = || {
            if propagate {
                Some(layer_element_estimates)
            } else {
                None
            }
        };
        match self {
            GroveOp::ReplaceTreeRootKey { aggregate_data, .. } => {
                GroveDb::average_case_merk_replace_tree(
                    key,
                    layer_element_estimates,
                    aggregate_data.parent_tree_type(),
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
            } => GroveDb::average_case_merk_insert_tree(
                key,
                flags,
                aggregate_data.parent_tree_type(),
                in_tree_type,
                // Account for the wrapper byte if the op rebuilds the
                // tree as `NonCounted(...)`, `NotSummed(...)`, or
                // `NotCountedOrSummed(...)`. They share the same +1
                // discriminant overhead and are mutually exclusive on
                // the rebuilt element.
                super::wrapper_overhead_for(*non_counted, *not_summed, *not_counted_or_summed),
                propagate_if_input(),
                grove_version,
            ),
            GroveOp::InsertOrReplace { element }
            | GroveOp::InsertWithKnownToNotAlreadyExist { element } => {
                GroveDb::average_case_merk_insert_element(
                    key,
                    element,
                    in_tree_type,
                    propagate_if_input(),
                    grove_version,
                )
            }
            GroveOp::InsertIfNotExists { element, .. } => {
                // Same insert cost as InsertWithKnownToNotAlreadyExist, plus an
                // additional seek to check whether the key already exists.
                let estimated_element_size = match element.serialized_size(grove_version) {
                    Ok(size) => size as u32,
                    Err(e) => {
                        return Err(Error::InternalError(format!(
                            "unable to estimate element size: {e}"
                        )))
                        .wrap_with_cost(OperationCost::default())
                    }
                };
                let mut has_cost = OperationCost::default();
                add_average_case_merk_has_value(
                    &mut has_cost,
                    key.max_length() as u32,
                    estimated_element_size,
                );
                GroveDb::average_case_merk_insert_element(
                    key,
                    element,
                    in_tree_type,
                    propagate_if_input(),
                    grove_version,
                )
                .add_cost(has_cost)
            }
            GroveOp::RefreshReference {
                reference_path_type,
                max_reference_hop,
                mode,
                flags,
                non_counted,
                ..
            } => {
                // Build the element shape the apply path will write,
                // so the cost estimator includes any wrapper byte.
                // Untrusted modes that preserve the on-disk sum use
                // 0 as the cost-only stand-in (actual on-disk sum is
                // not knowable here without a disk read; the wrapper
                // byte is what matters for the estimate).
                let inner = match mode {
                    RefreshReferenceMode::PlainReferenceTrusted
                    | RefreshReferenceMode::PlainReferenceUntrusted => Element::Reference(
                        reference_path_type.clone(),
                        *max_reference_hop,
                        flags.clone(),
                    ),
                    RefreshReferenceMode::SumItemReferenceTrusted(sum)
                    | RefreshReferenceMode::SumItemReferenceUntrustedValueUpdate(sum) => {
                        Element::ReferenceWithSumItem(
                            reference_path_type.clone(),
                            *max_reference_hop,
                            *sum,
                            flags.clone(),
                        )
                    }
                    RefreshReferenceMode::SumItemReferenceUntrustedNoValueUpdate => {
                        Element::ReferenceWithSumItem(
                            reference_path_type.clone(),
                            *max_reference_hop,
                            0,
                            flags.clone(),
                        )
                    }
                };
                let element = if *non_counted {
                    Element::NonCounted(Box::new(inner))
                } else {
                    inner
                };
                GroveDb::average_case_merk_replace_element(
                    key,
                    &element,
                    in_tree_type,
                    propagate_if_input(),
                    grove_version,
                )
            }
            GroveOp::Replace { element } => GroveDb::average_case_merk_replace_element(
                key,
                element,
                in_tree_type,
                propagate_if_input(),
                grove_version,
            ),
            GroveOp::Patch {
                element,
                change_in_bytes,
            } => GroveDb::average_case_merk_patch_element(
                key,
                element,
                *change_in_bytes,
                in_tree_type,
                propagate_if_input(),
                grove_version,
            ),
            GroveOp::Delete => GroveDb::average_case_merk_delete_element(
                key,
                layer_element_estimates,
                propagate,
                grove_version,
            ),
            GroveOp::DeleteTree(tree_type, _) => GroveDb::average_case_merk_delete_tree(
                key,
                *tree_type,
                layer_element_estimates,
                propagate,
                grove_version,
            ),
            GroveOp::CommitmentTreeInsert { payload, .. } => {
                Self::average_case_commitment_tree_insert(
                    payload,
                    key,
                    layer_element_estimates,
                    ct_chunk_power,
                    propagate,
                    grove_version,
                )
            }
            GroveOp::MmrTreeAppend { value } => {
                // Cost of updating parent element in the Merk
                let item_cost = GroveDb::average_case_merk_replace_tree(
                    key,
                    layer_element_estimates,
                    TreeType::MmrTree,
                    propagate,
                    grove_version,
                );
                // Additional cost: MMR node I/O in data storage.
                // push() writes 1 leaf + trailing_ones(leaf_count) internal
                // nodes. Average trailing_ones ≈ 1, so ~2 writes + 1 sibling
                // read for merging.
                use grovedb_costs::storage_cost::{removal::StorageRemovedBytes, StorageCost};
                // Internal node: 33 bytes (1 flag + 32 hash)
                const INTERNAL_NODE_SIZE: u32 = 33;
                // Leaf node: 37 + value_len (1 flag + 32 hash + 4 length + value)
                let leaf_node_size = 37 + value.len() as u32;
                // hash_count_for_push = 1 + trailing_ones. Average ≈ 2.
                const AVG_HASH_CALLS: u32 = 2;
                // Average writes: 2 (1 leaf + 1 internal merge). Reads: 1 sibling.
                const AVG_INTERNAL_WRITES: u32 = 1;
                const AVG_READS: u32 = 1;
                item_cost.add_cost(OperationCost {
                    seek_count: 1 + AVG_INTERNAL_WRITES + AVG_READS,
                    storage_cost: StorageCost {
                        added_bytes: leaf_node_size + INTERNAL_NODE_SIZE * AVG_INTERNAL_WRITES,
                        replaced_bytes: 0,
                        removed_bytes: StorageRemovedBytes::NoStorageRemoval,
                    },
                    storage_loaded_bytes: (INTERNAL_NODE_SIZE * AVG_READS) as u64,
                    hash_node_calls: AVG_HASH_CALLS,
                    sinsemilla_hash_calls: 0,
                })
            }
            GroveOp::BulkAppend { value } => {
                // Cost of updating parent element in the Merk
                let item_cost = GroveDb::average_case_merk_replace_tree(
                    key,
                    layer_element_estimates,
                    TreeType::BulkAppendTree(0),
                    propagate,
                    grove_version,
                );
                // Additional cost: buffer write + running hash.
                // Most appends only write to the buffer (O(1)). Compaction
                // happens once per epoch_size appends and is amortized.
                use grovedb_costs::storage_cost::{removal::StorageRemovedBytes, StorageCost};
                let entry_size = value.len() as u32;
                // 1 blake3 hash for running buffer hash chain
                const AVG_HASH_CALLS: u32 = 1;
                item_cost.add_cost(OperationCost {
                    seek_count: 1, // 1 buffer entry write
                    storage_cost: StorageCost {
                        added_bytes: entry_size,
                        replaced_bytes: 0,
                        removed_bytes: StorageRemovedBytes::NoStorageRemoval,
                    },
                    storage_loaded_bytes: 0,
                    hash_node_calls: AVG_HASH_CALLS,
                    sinsemilla_hash_calls: 0,
                })
            }
            GroveOp::DenseTreeInsert { value } => {
                // Cost of updating parent element in the Merk
                let item_cost = GroveDb::average_case_merk_replace_tree(
                    key,
                    layer_element_estimates,
                    TreeType::DenseAppendOnlyFixedSizeTree(0),
                    propagate,
                    grove_version,
                );
                // Additional cost: 1 value write + full root recomputation.
                // compute_root_hash visits all filled positions: each does
                // 1 storage read + 2 hash calls (value_hash + node_hash).
                // Average count ≈ 8 (half-full tree, height 4).
                use grovedb_costs::storage_cost::{removal::StorageRemovedBytes, StorageCost};
                let value_size = value.len() as u32;
                const AVG_COUNT: u32 = 8;
                // 2 hash calls per filled node (value_hash + node_hash)
                const AVG_HASH_CALLS: u32 = AVG_COUNT * 2;
                item_cost.add_cost(OperationCost {
                    seek_count: 1 + AVG_COUNT, // 1 write + AVG_COUNT reads for root hash
                    storage_cost: StorageCost {
                        added_bytes: value_size,
                        replaced_bytes: 0,
                        removed_bytes: StorageRemovedBytes::NoStorageRemoval,
                    },
                    storage_loaded_bytes: (value_size as u64) * (AVG_COUNT as u64),
                    hash_node_calls: AVG_HASH_CALLS,
                    sinsemilla_hash_calls: 0,
                })
            }
            GroveOp::ReplaceNonMerkTreeRoot { meta, .. } => {
                GroveDb::average_case_merk_replace_tree(
                    key,
                    layer_element_estimates,
                    meta.to_tree_type(),
                    propagate,
                    grove_version,
                )
            }
            GroveOp::InsertNonMerkTree {
                flags,
                meta,
                non_counted,
                ..
            } => GroveDb::average_case_merk_insert_tree(
                key,
                flags,
                meta.to_tree_type(),
                in_tree_type,
                // `InsertNonMerkTree` only carries `non_counted` (the
                // four non-Merk tree types are never sum-bearing, so
                // `NotSummed` / `NotCountedOrSummed` can't apply).
                if *non_counted { 1 } else { 0 },
                propagate_if_input(),
                grove_version,
            ),
            GroveOp::InsertAggregateIndexedTreeRootKeys { element, .. } => {
                // Insert-side counterpart of the replace arm below. The tree
                // type comes from the CARRIED element, not the aggregate: the
                // estimated execute returns `NoAggregateData`, whose
                // aggregate-derived fallback is NormalTree (cost size 3) —
                // 25 bytes under a PCPSIT's 28, which the fresh-path
                // ≥-actual contract test caught. Flags come off the element;
                // indexed elements cannot be wrapped, so no wrapper overhead.
                GroveDb::average_case_merk_insert_tree(
                    key,
                    element.get_flags(),
                    element.tree_type().unwrap_or(TreeType::NormalTree),
                    in_tree_type,
                    0,
                    propagate_if_input(),
                    grove_version,
                )
            }
            GroveOp::ReplaceAggregateIndexedTreeRootKeys {
                primary_aggregate_data,
                ..
            } => {
                // The parent-merk node update that recomputes the indexed
                // element's value_hash. The per-axis secondary Merk work is
                // charged at the PRIMARY's own level in `execute_ops_on_path`,
                // where the primary's layer information is in scope.
                //
                // The node must be sized as the INDEXED type: an indexed
                // element serializes its per-axis secondary state, so the
                // non-indexed type `parent_tree_type()` yields is smaller than
                // the element's own minimum payload. The fallback covers only
                // hand-built ops carrying a non-indexed aggregate, which
                // `execute_ops_on_path` never emits.
                GroveDb::average_case_merk_replace_tree(
                    key,
                    layer_element_estimates,
                    primary_aggregate_data
                        .indexed_parent_tree_type()
                        .unwrap_or_else(|| primary_aggregate_data.parent_tree_type()),
                    propagate,
                    grove_version,
                )
            }
        }
    }

    /// Versioned cost of a `CommitmentTreeInsert` op in the average-case
    /// estimator. Downstream the estimate is an admission bound, so
    /// historical blocks admitted under the legacy numbers must re-validate
    /// identically on replay — the model is dispatched on
    /// `average_case_commitment_tree_insert`.
    fn average_case_commitment_tree_insert(
        payload: &[u8],
        key: &KeyInfo,
        layer_element_estimates: &EstimatedLayerInformation,
        ct_chunk_power: Option<u8>,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        match grove_version
            .grovedb_versions
            .operations
            .average_case
            .average_case_commitment_tree_insert
        {
            0 => Self::average_case_commitment_tree_insert_v0(
                payload,
                key,
                layer_element_estimates,
                propagate,
                grove_version,
            ),
            1 => Self::average_case_commitment_tree_insert_v1(
                payload,
                key,
                layer_element_estimates,
                ct_chunk_power,
                propagate,
                grove_version,
            ),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "average_case_commitment_tree_insert".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            ))
            .wrap_with_cost(OperationCost::default()),
        }
    }

    /// Legacy (V1..V3) model: averages, NOT upper bounds. Kept byte-for-byte
    /// for replay of historical admission decisions; unreachable from the
    /// batch estimation path on those versions (keyless ops are skipped
    /// there) but reachable through direct dispatch.
    fn average_case_commitment_tree_insert_v0(
        payload: &[u8],
        key: &KeyInfo,
        layer_element_estimates: &EstimatedLayerInformation,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let item_cost = GroveDb::average_case_merk_replace_tree(
            key,
            layer_element_estimates,
            TreeType::CommitmentTree(0),
            propagate,
            grove_version,
        );
        use grovedb_costs::storage_cost::{removal::StorageRemovedBytes, StorageCost};
        // Average frontier size with ~16 ommers:
        // 1 (flag) + 8 (position) + 32 (leaf) + 1 (count) + 16*32
        const AVG_FRONTIER_SIZE: u32 = 554;
        // Buffer entry: cmx (32) + rho (32) + cv_net (32) + payload
        let buffer_entry_size = 96 + payload.len() as u32;
        // 32 (root computation) + 1 (avg ommer updates) = 33
        const AVG_SINSEMILLA_HASHES: u32 = 33;
        // 1 blake3 for the running buffer hash
        const AVG_BLAKE3_HASHES: u32 = 1;
        item_cost.add_cost(OperationCost {
            seek_count: 3, // frontier load + frontier save + buffer write
            storage_cost: StorageCost {
                added_bytes: buffer_entry_size,
                replaced_bytes: AVG_FRONTIER_SIZE,
                removed_bytes: StorageRemovedBytes::NoStorageRemoval,
            },
            storage_loaded_bytes: AVG_FRONTIER_SIZE as u64,
            hash_node_calls: AVG_BLAKE3_HASHES,
            sinsemilla_hash_calls: AVG_SINSEMILLA_HASHES,
        })
    }

    /// V4+ model: in the apply path, preprocessing rewrites the op into
    /// ReplaceNonMerkTreeRoot. The base cost is a tree root key replacement
    /// in the parent Merk; the append work itself (frontier I/O, Sinsemilla
    /// hashing, note write, epoch compaction) is charged by the shared
    /// upper-bound model — deliberately NOT an average, since the append
    /// cost is position-dependent and the position is adversary-chosen. See
    /// `commitment_tree_insert_op_cost`.
    fn average_case_commitment_tree_insert_v1(
        payload: &[u8],
        key: &KeyInfo,
        layer_element_estimates: &EstimatedLayerInformation,
        ct_chunk_power: Option<u8>,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        // The dense-recompute and compaction terms scale with 2^chunk_power,
        // which the op does not carry, so the tree's own layer MUST be
        // declared with `TreeType::CommitmentTree(chunk_power)` in the
        // estimation paths — the same declare-your-layers contract every
        // other estimated op follows. A silent fallback here would either
        // under-bound (too small) or grotesquely over-reserve (the physical
        // ceiling), both worse than a loud error at integration time.
        let Some(chunk_power) = ct_chunk_power else {
            return Err(Error::PathNotFoundInCacheForEstimatedCosts(
                "CommitmentTreeInsert estimation requires the commitment tree's own layer \
                 declared with TreeType::CommitmentTree(chunk_power) in the estimated layer \
                 information"
                    .to_string(),
            ))
            .wrap_with_cost(OperationCost::default());
        };
        // The preprocessing read of the stored element loads its
        // caller-supplied flags too; bound them with the parent layer's
        // declared flags size — the same metadata the parent-node replace
        // below uses, so an undeclared flag size undercounts both
        // consistently.
        let element_flags_load_bound = match layer_element_estimates
            .estimated_layer_sizes
            .layered_flags_size()
        {
            Ok(flags_size) => flags_size
                .map(|f| f + f.required_space() as u32)
                .unwrap_or_default(),
            Err(e) => return Err(Error::MerkError(e)).wrap_with_cost(OperationCost::default()),
        };
        GroveDb::average_case_merk_replace_tree(
            key,
            layer_element_estimates,
            TreeType::CommitmentTree(chunk_power),
            propagate,
            grove_version,
        )
        .add_cost(super::commitment_tree_insert_op_cost(
            payload.len() as u32,
            chunk_power,
            element_flags_load_bound,
        ))
    }
}

#[cfg(feature = "minimal")]
/// Axes whose secondary Merks an indexed primary maintains.
///
/// A PCPSIT's configured axes are a 1..=3 subset carried by the element, which
/// the estimator cannot see, so the worst case (all three) is charged — the
/// estimate stays an upper bound.
fn indexed_axes_for_tree_type(tree_type: TreeType) -> Vec<grovedb_element::indexed::IndexAxis> {
    use grovedb_element::indexed::IndexAxis;
    match tree_type {
        TreeType::ProvableSumIndexedTree => vec![IndexAxis::Sum],
        TreeType::ProvableCountProvableSumIndexedTree => {
            vec![IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg]
        }
        _ => vec![IndexAxis::Count],
    }
}

#[cfg(feature = "minimal")]
/// Cache for subtree paths for average case scenario costs.
#[derive(Default)]
pub(in crate::batch) struct AverageCaseTreeCacheKnownPaths {
    paths: HashMap<KeyInfoPath, EstimatedLayerInformation>,
    cached_merks: HashMap<KeyInfoPath, TreeType>,
    /// Paths whose layer is an indexed primary, so the bubble-up emits the
    /// same `ReplaceAggregateIndexedTreeRootKeys` op a real apply produces.
    pending_cidx_secondary: HashSet<Vec<Vec<u8>>>,
    /// The axes each pending path maintains, so the emitted op carries one
    /// entry per axis exactly as a real apply would.
    pending_cidx_axes: HashMap<Vec<Vec<u8>>, Vec<grovedb_element::indexed::IndexAxis>>,
}

#[cfg(feature = "minimal")]
impl AverageCaseTreeCacheKnownPaths {
    /// Updates the cache to the default setting with the given subtree paths
    pub(in crate::batch) fn new_with_estimated_layer_information(
        paths: HashMap<KeyInfoPath, EstimatedLayerInformation>,
    ) -> Self {
        AverageCaseTreeCacheKnownPaths {
            paths,
            cached_merks: HashMap::default(),
            pending_cidx_secondary: HashSet::default(),
            pending_cidx_axes: HashMap::default(),
        }
    }
}

#[cfg(feature = "minimal")]
impl fmt::Debug for AverageCaseTreeCacheKnownPaths {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TreeCacheKnownPaths").finish()
    }
}

#[cfg(feature = "minimal")]
impl<G, SR> TreeCache<G, SR> for AverageCaseTreeCacheKnownPaths {
    /// Report a secondary mirror for layers that are indexed primaries, so the
    /// bubble-up emits `ReplaceAggregateIndexedTreeRootKeys` — the op a real
    /// `apply_batch` produces — instead of a plain `ReplaceTreeRootKey`. The
    /// hash and root key are placeholders: the estimator only needs the op
    /// SHAPE to charge the right parent-node update.
    fn take_cidx_secondary_after_apply(
        &mut self,
        path: &[Vec<u8>],
    ) -> Option<Vec<(u8, grovedb_merk::CryptoHash, Option<Vec<u8>>)>> {
        if self.pending_cidx_secondary.remove(path) {
            // Placeholder per-axis state: the estimator only needs the op SHAPE
            // to charge the right parent-node update, not real hashes. One
            // entry per axis so the shape matches what a real apply produces.
            Some(
                self.pending_cidx_axes
                    .remove(path)
                    .unwrap_or_else(|| vec![grovedb_element::indexed::IndexAxis::Count])
                    .into_iter()
                    .map(|axis| (axis.tag(), [0u8; 32], None))
                    .collect(),
            )
        } else {
            None
        }
    }

    fn insert(
        &mut self,
        path: &KeyInfoPath,
        key: &KeyInfo,
        tree_type: TreeType,
    ) -> CostResult<(), Error> {
        let mut average_case_cost = OperationCost::default();
        let mut inserted_path = path.clone();
        inserted_path.push(key.clone());
        // There is no need to pay for getting a merk, because we know the merk to be
        // empty at this point.
        // There is however a hash call that creates the prefix
        average_case_cost.hash_node_calls += 1;
        self.cached_merks.insert(inserted_path, tree_type);
        Ok(()).wrap_with_cost(average_case_cost)
    }

    fn get_batch_run_mode(&self) -> BatchRunMode {
        BatchRunMode::AverageCase(self.paths.clone())
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

        let layer_element_estimates = cost_return_on_error_no_add!(
            cost,
            self.paths.get(path).ok_or_else(|| {
                let paths = self
                    .paths
                    .keys()
                    .map(|k| k.0.iter().map(|k| hex::encode(k.as_slice())).join("/"))
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
        if !self.cached_merks.contains_key(path) {
            let layer_info = cost_return_on_error_no_add!(
                cost,
                self.paths.get(path).ok_or_else(|| {
                    let paths = self
                        .paths
                        .keys()
                        .map(|k| k.0.iter().map(|k| hex::encode(k.as_slice())).join("/"))
                        .join(" | ");
                    Error::PathNotFoundInCacheForEstimatedCosts(format!(
                        "required path for estimated merk caching {} not found in paths {}",
                        path.0.iter().map(|k| hex::encode(k.as_slice())).join("/"),
                        paths
                    ))
                })
            );
            cost_return_on_error_no_add!(
                cost,
                GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(
                    &mut cost,
                    path,
                    layer_should_be_empty,
                    layer_info.tree_type,
                    grove_version,
                )
            );
            self.cached_merks.insert(path.clone(), layer_info.tree_type);
        }

        // How many keys at this level the mirror will actually rewrite. The
        // real mirror (`apply_cidx_secondary_mirror_post_apply`) does one
        // delete+insert per captured key, so charging a single key's worth
        // per level undercharges in proportion to batch width.
        let mirrored_key_count = ops_at_path_by_key
            .values()
            .filter(|op| op.can_mutate_child_count())
            .count();

        for (key, op) in ops_at_path_by_key.into_iter() {
            // A CommitmentTreeInsert arrives under a synthetic key carrying
            // the real tree key (see `keyless_op_synthetic_key`). When the
            // caller declared the tree's own layer with
            // `TreeType::CommitmentTree(chunk_power)` — as Dash Platform
            // does — the estimate uses the tree's ACTUAL epoch scale
            // instead of the constructor-enforced cap.
            let ct_chunk_power = if matches!(op, GroveOp::CommitmentTreeInsert { .. }) {
                crate::batch::batch_structure::keyless_op_tree_key(&key).and_then(|tree_key| {
                    let mut tree_path = path.clone();
                    tree_path.push(KeyInfo::KnownKey(tree_key.to_vec()));
                    match self.paths.get(&tree_path).map(|layer| layer.tree_type) {
                        Some(TreeType::CommitmentTree(chunk_power)) => Some(chunk_power),
                        _ => None,
                    }
                })
            } else {
                None
            };
            cost_return_on_error!(
                &mut cost,
                op.average_case_cost(
                    &key,
                    layer_element_estimates,
                    ct_chunk_power,
                    false,
                    grove_version
                )
            );
        }

        cost_return_on_error!(
            &mut cost,
            average_case_merk_propagate(layer_element_estimates, grove_version)
                .map_err(Error::MerkError)
        );

        // An indexed primary also maintains one secondary Merk per axis. Those
        // live at derived prefixes (`Blake3(primary_prefix ‖ axis_tag)`) which
        // are not GroveDB paths, so they never appear in `self.paths` and no
        // layer charges them — but this IS the primary's level, so its layer
        // information is right here and the secondary's shape follows from it.
        //
        // Recording the path additionally makes `take_cidx_secondary_after_apply`
        // report a mirror, so the bubble-up emits
        // `ReplaceAggregateIndexedTreeRootKeys` exactly as a real apply_batch
        // does. Without it the estimator emitted a plain `ReplaceTreeRootKey`
        // and the indexed arm of `average_case_cost` was unreachable.
        if layer_element_estimates.tree_type.is_indexed_primary() {
            let axes = indexed_axes_for_tree_type(layer_element_estimates.tree_type);
            // Once per mutated key, not once per level: the mirror rewrites
            // every captured key's row on every axis.
            for _ in 0..mirrored_key_count {
                cost_return_on_error!(
                    &mut cost,
                    GroveDb::average_case_indexed_secondary_mirror(
                        path,
                        layer_element_estimates,
                        &axes,
                        grove_version,
                    )
                );
            }
            self.pending_cidx_secondary.insert(path.to_path());
            self.pending_cidx_axes.insert(path.to_path(), axes.clone());
        }

        // Return an aggregate whose KIND matches the layer's tree type, so
        // the bubble-up ops built from this return size the parent node as
        // the right element: `NoAggregateData` maps to NormalTree (cost size
        // 3), silently under-sizing an indexed parent's 13/28 bytes in the
        // Replace/Insert aggregate arms. Values are zero — only the variant
        // drives estimator sizing.
        let aggregate_kind = match layer_element_estimates.tree_type {
            TreeType::ProvableCountIndexedTree => AggregateData::ProvableCount(0),
            TreeType::ProvableSumIndexedTree => AggregateData::ProvableSum(0),
            TreeType::ProvableCountProvableSumIndexedTree => {
                AggregateData::ProvableCountAndProvableSum(0, 0)
            }
            _ => AggregateData::NoAggregateData,
        };
        Ok(([0u8; 32], None, aggregate_kind)).wrap_with_cost(cost)
    }

    // Clippy's suggestion doesn't respect ownership in this case
    #[allow(clippy::map_entry)]
    fn update_base_merk_root_key(
        &mut self,
        _root_key: Option<Vec<u8>>,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();
        cost.seek_count += 1;
        let base_path = KeyInfoPath(vec![]);
        if let Some(estimated_layer_info) = self.paths.get(&base_path)
            && !self.cached_merks.contains_key(&base_path)
        {
            // Then we have to get the tree
            cost_return_on_error_no_add!(
                cost,
                GroveDb::add_average_case_get_merk_at_path::<RocksDbStorage>(
                    &mut cost,
                    &base_path,
                    estimated_layer_info
                        .estimated_layer_count
                        .estimated_to_be_empty(),
                    estimated_layer_info.tree_type,
                    grove_version
                )
            );
            self.cached_merks
                .insert(base_path, estimated_layer_info.tree_type);
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
    use grovedb_merk::{
        estimated_costs::average_case_costs::{
            EstimatedLayerCount::{ApproximateElements, EstimatedLevel, PotentiallyAtMaxElements},
            EstimatedLayerInformation,
            EstimatedLayerSizes::{AllItems, AllSubtrees},
            EstimatedSumTrees::{NoSumTrees, SomeSumTrees},
        },
        tree_type::TreeType,
    };
    use grovedb_version::version::GroveVersion;

    use grovedb_merk::tree::AggregateData;

    use crate::{
        batch::{
            estimated_costs::EstimatedCostsType::AverageCaseCostsType, key_info::KeyInfo, GroveOp,
            KeyInfoPath, NonMerkTreeMeta, QualifiedGroveDbOp, SubelementsDeletionBehavior,
        },
        reference_path::ReferencePathType,
        tests::{common::EMPTY_PATH, make_empty_grovedb},
        Element, GroveDb,
    };

    #[test]
    fn test_batch_root_one_tree_insert_op_average_case_costs() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree(),
        )];
        let mut paths = HashMap::new();
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
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
            grove_version,
        )
        .cost_as_result()
        .expect("expected to get average case costs");

        let cost = db.apply_batch(ops, None, Some(&tx), grove_version).cost;
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
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_batch_root_one_tree_with_flags_insert_op_average_case_costs() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_tree_with_flags(Some(b"cat".to_vec())),
        )];
        let mut paths = HashMap::new();
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: EstimatedLevel(0, true),
                estimated_layer_sizes: AllSubtrees(4, NoSumTrees, Some(3)),
            },
        );
        paths.insert(
            KeyInfoPath(vec![KeyInfo::KnownKey(b"key1".to_vec())]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
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
            grove_version,
        )
        .cost_as_result()
        .expect("expected to get average case costs");

        let cost = db.apply_batch(ops, None, Some(&tx), grove_version).cost;
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
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_batch_root_one_item_insert_op_average_case_costs() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![],
            b"key1".to_vec(),
            Element::new_item(b"cat".to_vec()),
        )];
        let mut paths = HashMap::new();
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
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
            grove_version,
        )
        .cost_as_result()
        .expect("expected to get average case costs");

        let cost = db.apply_batch(ops, None, Some(&tx), grove_version).cost;
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
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_batch_root_one_tree_insert_op_under_element_average_case_costs() {
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
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
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
            grove_version,
        )
        .cost_as_result()
        .expect("expected to get average case costs");

        let cost = db.apply_batch(ops, None, Some(&tx), grove_version).cost;
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
                    replaced_bytes: 75,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 109,
                hash_node_calls: 8,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_batch_root_one_tree_insert_op_in_sub_tree_average_case_costs() {
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
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: EstimatedLevel(0, false),
                estimated_layer_sizes: AllSubtrees(1, NoSumTrees, None),
            },
        );

        paths.insert(
            KeyInfoPath(vec![KeyInfo::KnownKey(b"0".to_vec())]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
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
            grove_version,
        )
        .cost_as_result()
        .expect("expected to get average case costs");

        let cost = db.apply_batch(ops, None, Some(&tx), grove_version).cost;
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
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_batch_root_one_sum_item_replace_op_average_case_costs() {
        let grove_version = GroveVersion::latest();
        let ops = vec![QualifiedGroveDbOp::replace_op(
            vec![vec![7]],
            hex::decode("46447a3b4c8939fd4cf8b610ba7da3d3f6b52b39ab2549bf91503b9b07814055")
                .unwrap(),
            Element::new_sum_item(500),
        )];
        let mut paths = HashMap::new();
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: EstimatedLevel(1, false),
                estimated_layer_sizes: AllSubtrees(
                    1,
                    SomeSumTrees {
                        sum_trees_weight: 1,
                        big_sum_trees_weight: 0,
                        count_trees_weight: 0,
                        count_sum_trees_weight: 0,
                        non_sum_trees_weight: 1,
                        provable_sum_trees_weight: 0,
                        provable_count_trees_weight: 0,
                        provable_count_sum_trees_weight: 0,
                        provable_count_provable_sum_trees_weight: 0,
                    },
                    None,
                ),
            },
        );
        paths.insert(
            KeyInfoPath::from_known_owned_path(vec![vec![7]]),
            EstimatedLayerInformation {
                tree_type: TreeType::SumTree,
                estimated_layer_count: PotentiallyAtMaxElements,
                estimated_layer_sizes: AllItems(32, 8, None),
            },
        );
        let average_case_cost = GroveDb::estimated_case_operations_for_batch(
            AverageCaseCostsType(paths),
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
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
                    replaced_bytes: 5594,
                    removed_bytes: NoStorageRemoval,
                },
                storage_loaded_bytes: 7669,
                hash_node_calls: 79,
                sinsemilla_hash_calls: 0,
            }
        );
    }

    #[test]
    fn test_batch_average_case_costs() {
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
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: EstimatedLevel(1, false),
                estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
            },
        );

        paths.insert(
            KeyInfoPath(vec![KeyInfo::KnownKey(b"0".to_vec())]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
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
            grove_version,
        )
        .cost_as_result()
        .expect("expected to estimate costs");
        let cost = db.apply_batch(ops, None, Some(&tx), grove_version).cost;
        // at the moment we just check the added bytes are the same
        assert_eq!(
            average_case_cost.storage_cost.added_bytes,
            cost.storage_cost.added_bytes
        );
    }

    #[test]
    fn test_batch_root_one_dense_tree_insert_op_average_case_costs() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![],
            b"key1".to_vec(),
            Element::empty_dense_tree(3),
        )];
        let mut paths = HashMap::new();
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
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
            grove_version,
        )
        .cost_as_result()
        .expect("expected to get average case costs for dense tree insert");

        let cost = db.apply_batch(ops, None, Some(&tx), grove_version).cost;
        assert!(
            average_case_cost.eq(&cost),
            "average cost not eq {:?} \n to cost {:?}",
            average_case_cost,
            cost
        );
        assert_eq!(
            cost.storage_cost.added_bytes,
            average_case_cost.storage_cost.added_bytes
        );
    }

    #[test]
    fn test_refresh_reference_average_case_cost() {
        let grove_version = GroveVersion::latest();
        let ops = vec![QualifiedGroveDbOp::refresh_reference_op(
            vec![vec![7]],
            b"ref_key".to_vec(),
            ReferencePathType::AbsolutePathReference(vec![b"target".to_vec()]),
            Some(5),
            None,
            /* non_counted = */ false,
            true,
        )];
        let mut paths = HashMap::new();
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: EstimatedLevel(1, false),
                estimated_layer_sizes: AllSubtrees(1, NoSumTrees, None),
            },
        );
        paths.insert(
            KeyInfoPath::from_known_owned_path(vec![vec![7]]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: PotentiallyAtMaxElements,
                estimated_layer_sizes: AllItems(32, 64, None),
            },
        );
        let result = GroveDb::estimated_case_operations_for_batch(
            AverageCaseCostsType(paths),
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("expected to get average case costs for refresh reference");
        // RefreshReference delegates to average_case_merk_replace_element, so there
        // should be seeks for getting the merk and replacing the element plus
        // hash calls for propagation.
        assert!(
            result.seek_count > 0,
            "expected seek_count > 0, got {}",
            result.seek_count
        );
        assert!(
            result.hash_node_calls > 0,
            "expected hash_node_calls > 0, got {}",
            result.hash_node_calls
        );
    }

    #[test]
    fn test_refresh_reference_with_sum_item_average_case_cost() {
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
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: EstimatedLevel(1, false),
                estimated_layer_sizes: AllSubtrees(1, NoSumTrees, None),
            },
        );
        paths.insert(
            KeyInfoPath::from_known_owned_path(vec![vec![7]]),
            EstimatedLayerInformation {
                tree_type: TreeType::SumTree,
                estimated_layer_count: PotentiallyAtMaxElements,
                estimated_layer_sizes: AllItems(32, 64, None),
            },
        );
        let result = GroveDb::estimated_case_operations_for_batch(
            AverageCaseCostsType(paths),
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("expected average case costs for refresh reference with sum item");
        // Same dispatch as `RefreshReference` but the rebuilt element carries
        // an extra i64 sum, so the byte estimate is ~8 bytes larger.
        assert!(
            result.seek_count > 0,
            "expected seek_count > 0, got {}",
            result.seek_count
        );
        assert!(
            result.hash_node_calls > 0,
            "expected hash_node_calls > 0, got {}",
            result.hash_node_calls
        );
    }

    #[test]
    fn test_refresh_reference_with_sum_item_non_counted_average_case_cost() {
        // Coverage for the `non_counted = true` branch of the
        // RefreshReferenceWithSumItem cost arm. The estimator must
        // build the same NonCounted(...) shape that execution writes,
        // otherwise the serialized value is undercounted by the
        // wrapper byte.
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
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: EstimatedLevel(1, false),
                estimated_layer_sizes: AllSubtrees(1, NoSumTrees, None),
            },
        );
        paths.insert(
            KeyInfoPath::from_known_owned_path(vec![vec![7]]),
            EstimatedLayerInformation {
                tree_type: TreeType::CountSumTree,
                estimated_layer_count: PotentiallyAtMaxElements,
                estimated_layer_sizes: AllItems(32, 64, None),
            },
        );
        let nc_cost = GroveDb::estimated_case_operations_for_batch(
            AverageCaseCostsType(paths.clone()),
            nc_ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("expected average case costs for non-counted refresh");
        let bare_cost = GroveDb::estimated_case_operations_for_batch(
            AverageCaseCostsType(paths),
            bare_ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("expected average case costs for bare refresh");

        // The NonCounted-wrapped variant has at least one extra byte on
        // the wire (the wrapper discriminant), so its cost estimate
        // must be strictly larger than the bare variant (at least the
        // +1 wrapper-discriminant byte). Strict `>` makes the test
        // load-bearing: an undercount regression that produced an
        // identical (under-counted) estimate — the exact bug being
        // pinned — would fail this assertion.
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
    fn test_patch_average_case_cost() {
        let grove_version = GroveVersion::latest();
        let ops = vec![QualifiedGroveDbOp::patch_op(
            vec![vec![7]],
            b"patch_key".to_vec(),
            Element::new_item(b"patched_value".to_vec()),
            5, // change_in_bytes
        )];
        let mut paths = HashMap::new();
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: EstimatedLevel(1, false),
                estimated_layer_sizes: AllSubtrees(1, NoSumTrees, None),
            },
        );
        paths.insert(
            KeyInfoPath::from_known_owned_path(vec![vec![7]]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: PotentiallyAtMaxElements,
                estimated_layer_sizes: AllItems(32, 64, None),
            },
        );
        let result = GroveDb::estimated_case_operations_for_batch(
            AverageCaseCostsType(paths),
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("expected to get average case costs for patch");
        // Patch delegates to average_case_merk_patch_element which performs
        // seeks and hash calls.
        assert!(
            result.seek_count > 0,
            "expected seek_count > 0, got {}",
            result.seek_count
        );
        assert!(
            result.hash_node_calls > 0,
            "expected hash_node_calls > 0, got {}",
            result.hash_node_calls
        );
    }

    #[test]
    fn test_delete_average_case_cost() {
        let grove_version = GroveVersion::latest();
        let ops = vec![QualifiedGroveDbOp::delete_op(
            vec![vec![7]],
            b"del_key".to_vec(),
        )];
        let mut paths = HashMap::new();
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: EstimatedLevel(1, false),
                estimated_layer_sizes: AllSubtrees(1, NoSumTrees, None),
            },
        );
        paths.insert(
            KeyInfoPath::from_known_owned_path(vec![vec![7]]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: PotentiallyAtMaxElements,
                estimated_layer_sizes: AllItems(32, 64, None),
            },
        );
        let result = GroveDb::estimated_case_operations_for_batch(
            AverageCaseCostsType(paths),
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("expected to get average case costs for delete");
        // Delete delegates to average_case_merk_delete_element which requires
        // seeks to find and remove the element.
        assert!(
            result.seek_count > 0,
            "expected seek_count > 0, got {}",
            result.seek_count
        );
        assert!(
            result.hash_node_calls > 0,
            "expected hash_node_calls > 0, got {}",
            result.hash_node_calls
        );
    }

    #[test]
    fn test_delete_tree_average_case_cost() {
        let grove_version = GroveVersion::latest();
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![vec![7]],
            b"tree_key".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Error,
        )];
        let mut paths = HashMap::new();
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: EstimatedLevel(1, false),
                estimated_layer_sizes: AllSubtrees(1, NoSumTrees, None),
            },
        );
        paths.insert(
            KeyInfoPath::from_known_owned_path(vec![vec![7]]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: PotentiallyAtMaxElements,
                estimated_layer_sizes: AllSubtrees(32, NoSumTrees, None),
            },
        );
        let result = GroveDb::estimated_case_operations_for_batch(
            AverageCaseCostsType(paths),
            ops,
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("expected to get average case costs for delete tree");
        // DeleteTree delegates to average_case_merk_delete_tree which requires
        // seeks and hash calls.
        assert!(
            result.seek_count > 0,
            "expected seek_count > 0, got {}",
            result.seek_count
        );
        assert!(
            result.hash_node_calls > 0,
            "expected hash_node_calls > 0, got {}",
            result.hash_node_calls
        );
    }

    // Direct average_case_cost tests for keyless/internal GroveOp variants.
    // These ops are either skipped by from_ops (key=None) or are internal-only,
    // so we call average_case_cost() directly on the GroveOp instance.

    #[test]
    fn test_commitment_tree_insert_average_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        let op = GroveOp::CommitmentTreeInsert {
            cmx: [1u8; 32],
            rho: [2u8; 32],
            cv_net: [3u8; 32],
            payload: vec![0u8; 100],
        };
        let key = KeyInfo::KnownKey(b"tree_key".to_vec());
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: ApproximateElements(10),
            estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
        };
        // The V4+ model requires the tree's chunk power (normally read from
        // the tree's own declared layer); an undeclared dispatch errors.
        assert!(op
            .average_case_cost(&key, &layer_info, None, false, grove_version)
            .cost_as_result()
            .is_err());
        let cost = op
            .average_case_cost(&key, &layer_info, Some(10), false, grove_version)
            .cost_as_result()
            .expect("expected cost for commitment tree insert");
        // CommitmentTreeInsert includes frontier I/O and buffer writes plus
        // Sinsemilla hashing for the commitment tree anchor.
        assert!(
            cost.seek_count > 0,
            "expected seek_count > 0, got {}",
            cost.seek_count
        );
        assert!(
            cost.sinsemilla_hash_calls > 0,
            "expected sinsemilla_hash_calls > 0, got {}",
            cost.sinsemilla_hash_calls
        );
        assert!(
            cost.hash_node_calls > 0,
            "expected hash_node_calls > 0, got {}",
            cost.hash_node_calls
        );
        // Buffer entry size = 96 + payload.len() = 196
        // Frontier replaced bytes = 554
        assert!(
            cost.storage_cost.added_bytes > 0,
            "expected added_bytes > 0, got {}",
            cost.storage_cost.added_bytes
        );
        assert!(
            cost.storage_cost.replaced_bytes > 0,
            "expected replaced_bytes > 0, got {}",
            cost.storage_cost.replaced_bytes
        );
    }

    #[test]
    fn test_mmr_tree_append_average_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        let op = GroveOp::MmrTreeAppend {
            value: vec![42u8; 64],
        };
        let key = KeyInfo::KnownKey(b"mmr_key".to_vec());
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: ApproximateElements(5),
            estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
        };
        let cost = op
            .average_case_cost(&key, &layer_info, None, false, grove_version)
            .cost_as_result()
            .expect("expected cost for mmr tree append");
        // MmrTreeAppend includes parent replace cost plus MMR node I/O.
        assert!(
            cost.seek_count > 0,
            "expected seek_count > 0, got {}",
            cost.seek_count
        );
        assert!(
            cost.hash_node_calls > 0,
            "expected hash_node_calls > 0, got {}",
            cost.hash_node_calls
        );
        // Leaf node + internal node writes
        assert!(
            cost.storage_cost.added_bytes > 0,
            "expected added_bytes > 0, got {}",
            cost.storage_cost.added_bytes
        );
        // Sibling read for merging
        assert!(
            cost.storage_loaded_bytes > 0,
            "expected storage_loaded_bytes > 0, got {}",
            cost.storage_loaded_bytes
        );
    }

    #[test]
    fn test_bulk_append_average_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        let op = GroveOp::BulkAppend {
            value: vec![99u8; 50],
        };
        let key = KeyInfo::KnownKey(b"bulk_key".to_vec());
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: ApproximateElements(5),
            estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
        };
        let cost = op
            .average_case_cost(&key, &layer_info, None, false, grove_version)
            .cost_as_result()
            .expect("expected cost for bulk append");
        // BulkAppend includes parent replace cost plus buffer write + running
        // hash.
        assert!(
            cost.seek_count > 0,
            "expected seek_count > 0, got {}",
            cost.seek_count
        );
        assert!(
            cost.hash_node_calls > 0,
            "expected hash_node_calls > 0, got {}",
            cost.hash_node_calls
        );
        // Buffer entry write adds bytes equal to value length
        assert!(
            cost.storage_cost.added_bytes > 0,
            "expected added_bytes > 0, got {}",
            cost.storage_cost.added_bytes
        );
    }

    #[test]
    fn test_dense_tree_insert_average_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        let op = GroveOp::DenseTreeInsert {
            value: vec![77u8; 32],
        };
        let key = KeyInfo::KnownKey(b"dense_key".to_vec());
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: ApproximateElements(5),
            estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
        };
        let cost = op
            .average_case_cost(&key, &layer_info, None, false, grove_version)
            .cost_as_result()
            .expect("expected cost for dense tree insert");
        // DenseTreeInsert includes parent replace cost plus value write and
        // full root recomputation (AVG_COUNT reads + hashes).
        assert!(
            cost.seek_count > 0,
            "expected seek_count > 0, got {}",
            cost.seek_count
        );
        assert!(
            cost.hash_node_calls > 0,
            "expected hash_node_calls > 0, got {}",
            cost.hash_node_calls
        );
        assert!(
            cost.storage_cost.added_bytes > 0,
            "expected added_bytes > 0, got {}",
            cost.storage_cost.added_bytes
        );
        assert!(
            cost.storage_loaded_bytes > 0,
            "expected storage_loaded_bytes > 0, got {}",
            cost.storage_loaded_bytes
        );
    }

    #[test]
    fn test_replace_non_merk_tree_root_average_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        let op = GroveOp::ReplaceNonMerkTreeRoot {
            hash: [0xABu8; 32],
            meta: NonMerkTreeMeta::CommitmentTree {
                total_count: 100,
                chunk_power: 4,
            },
        };
        let key = KeyInfo::KnownKey(b"nmerk_key".to_vec());
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: ApproximateElements(5),
            estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
        };
        let cost = op
            .average_case_cost(&key, &layer_info, None, false, grove_version)
            .cost_as_result()
            .expect("expected cost for replace non-merk tree root");
        // ReplaceNonMerkTreeRoot delegates to average_case_merk_replace_tree.
        assert!(
            cost.seek_count > 0,
            "expected seek_count > 0, got {}",
            cost.seek_count
        );
        assert!(
            cost.hash_node_calls > 0,
            "expected hash_node_calls > 0, got {}",
            cost.hash_node_calls
        );
    }

    #[test]
    fn test_insert_non_merk_tree_average_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        let op = GroveOp::InsertNonMerkTree {
            hash: [0xCDu8; 32],
            root_key: None,
            flags: None,
            aggregate_data: AggregateData::NoAggregateData,
            meta: NonMerkTreeMeta::MmrTree { mmr_size: 50 },

            non_counted: false,
        };
        let key = KeyInfo::KnownKey(b"inmerk_key".to_vec());
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: ApproximateElements(0),
            estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
        };
        let cost = op
            .average_case_cost(&key, &layer_info, None, false, grove_version)
            .cost_as_result()
            .expect("expected cost for insert non-merk tree");
        // InsertNonMerkTree delegates to average_case_merk_insert_tree.
        assert!(
            cost.seek_count > 0,
            "expected seek_count > 0, got {}",
            cost.seek_count
        );
        assert!(
            cost.hash_node_calls > 0,
            "expected hash_node_calls > 0, got {}",
            cost.hash_node_calls
        );
        // Inserting a new tree adds bytes.
        assert!(
            cost.storage_cost.added_bytes > 0,
            "expected added_bytes > 0, got {}",
            cost.storage_cost.added_bytes
        );
    }

    /// Covers the wrapper-byte accounting in
    /// `GroveOp::InsertTreeWithRootHash::average_case_cost`. Each
    /// wrapper bit (`non_counted` / `not_summed` /
    /// `not_counted_or_summed`) prepends one bincode discriminant byte
    /// to the rebuilt tree element; the cost estimator must include it
    /// in `value_len`. Before the fix the arm dropped the wrapper byte
    /// entirely. The three bits are mutually exclusive on any element
    /// — we vary one at a time.
    #[test]
    fn test_insert_tree_with_root_hash_wrapper_bits_average_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        let key = KeyInfo::KnownKey(b"merk_key".to_vec());
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: ApproximateElements(0),
            estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
        };
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
            op.average_case_cost(&key, &layer_info, None, false, grove_version)
                .cost_as_result()
                .expect("expected cost for InsertTreeWithRootHash")
        };
        let bare = cost_for(false, false, false);
        let nc = cost_for(true, false, false);
        let ns = cost_for(false, true, false);
        let ncs = cost_for(false, false, true);
        // Each wrapper bit must increase the estimated value_len by at
        // least the +1 wrapper byte. We use `>=` because `add_bytes`
        // can also pick up the varint-required-space overhead at
        // payload-size boundaries.
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
    /// `GroveOp::InsertNonMerkTree::average_case_cost`. Non-Merk trees
    /// only carry the `non_counted` wrapper bit (they're never
    /// sum-bearing).
    #[test]
    fn test_insert_non_merk_tree_non_counted_average_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        let key = KeyInfo::KnownKey(b"inmerk_key".to_vec());
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: ApproximateElements(0),
            estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
        };
        let cost_for = |non_counted: bool| {
            let op = GroveOp::InsertNonMerkTree {
                hash: [0xCDu8; 32],
                root_key: None,
                flags: None,
                aggregate_data: AggregateData::NoAggregateData,
                meta: NonMerkTreeMeta::MmrTree { mmr_size: 50 },
                non_counted,
            };
            op.average_case_cost(&key, &layer_info, None, false, grove_version)
                .cost_as_result()
                .expect("expected cost for InsertNonMerkTree")
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

    /// Covers the `GroveOp::ReplaceAggregateIndexedTreeRootKeys` arm in
    /// `average_case_cost`. Cost shape is equivalent to
    /// `ReplaceTreeRootKey` at this level (the dual-Merk update on the
    /// secondary is accounted for at the cidx primary level inside
    /// `execute_ops_on_path`, not here).
    #[test]
    fn test_replace_aggregate_indexed_tree_root_keys_average_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        let key = KeyInfo::KnownKey(b"agg_idx".to_vec());
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: ApproximateElements(8),
            estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
        };

        // Both AggregateData::Count and AggregateData::ProvableCount
        // route through the same `worst_case_merk_replace_tree` shape;
        // here we just confirm the arm yields a non-trivial cost.
        let op_count = GroveOp::ReplaceAggregateIndexedTreeRootKeys {
            primary_hash: [0xCDu8; 32],
            primary_root_key: Some(b"prk".to_vec()),
            primary_aggregate_data: AggregateData::Count(123),
            axes: vec![(0u8, [0xEFu8; 32], Some(b"srk".to_vec()))],
        };
        let cost_count = op_count
            .average_case_cost(&key, &layer_info, None, false, grove_version)
            .cost_as_result()
            .expect("expected average case cost for Count cidx replace");
        assert!(cost_count.seek_count > 0 || cost_count.hash_node_calls > 0);

        let op_pcount = GroveOp::ReplaceAggregateIndexedTreeRootKeys {
            primary_hash: [9u8; 32],
            primary_root_key: None,
            primary_aggregate_data: AggregateData::ProvableCount(11),
            axes: vec![(0u8, [8u8; 32], None)],
        };
        let cost_pcount = op_pcount
            .average_case_cost(&key, &layer_info, None, true, grove_version)
            .cost_as_result()
            .expect("expected average case cost for ProvableCount cidx replace (propagate)");
        assert!(
            cost_pcount.seek_count >= cost_count.seek_count
                || cost_pcount.hash_node_calls >= cost_count.hash_node_calls,
            "propagate=true cost should be >= non-propagate baseline; pcount={:?}, count={:?}",
            cost_pcount,
            cost_count,
        );
    }

    /// The estimator must not UNDER-report an indexed-tree batch.
    ///
    /// The per-axis secondary Merk work (open + delete-old + insert-new +
    /// re-root) is charged from the primary's layer information; before that
    /// it was charged nowhere, so the estimate came in well under actual on
    /// exactly the ops a fee reservation is computed from. Estimates for
    /// indexed trees are an upper bound rather than an equality (the axis set
    /// of a PCPSIT is not visible to the estimator, so the worst case of three
    /// axes is charged), hence >= rather than the eq() other tests use.
    #[test]
    fn test_batch_indexed_tree_insert_average_case_cost_is_not_under_actual() {
        let grove_version = GroveVersion::latest();
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        // A PCIT holding one item.
        db.insert(
            EMPTY_PATH,
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("create pcit");

        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"cidx".to_vec()],
            b"k1".to_vec(),
            Element::new_item(b"v1".to_vec()),
        )];

        let mut paths = HashMap::new();
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: ApproximateElements(1),
                estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
            },
        );
        // The PRIMARY's layer — this is what the secondary's shape is derived
        // from.
        paths.insert(
            KeyInfoPath(vec![KeyInfo::KnownKey(b"cidx".to_vec())]),
            EstimatedLayerInformation {
                tree_type: TreeType::ProvableCountIndexedTree,
                estimated_layer_count: ApproximateElements(1),
                estimated_layer_sizes: AllItems(2, 2, None),
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
            grove_version,
        )
        .cost_as_result()
        .expect("expected to get average case costs");

        let cost = db
            .apply_batch(ops, None, Some(&tx), grove_version)
            .cost_as_result()
            .expect("expected to apply batch");

        assert!(
            average_case_cost.seek_count >= cost.seek_count,
            "estimated seeks {} must not be under actual {}",
            average_case_cost.seek_count,
            cost.seek_count
        );
        assert!(
            average_case_cost.storage_cost.added_bytes >= cost.storage_cost.added_bytes,
            "estimated added_bytes {} must not be under actual {}",
            average_case_cost.storage_cost.added_bytes,
            cost.storage_cost.added_bytes
        );
        assert!(
            average_case_cost.hash_node_calls >= cost.hash_node_calls,
            "estimated hash_node_calls {} must not be under actual {}",
            average_case_cost.hash_node_calls,
            cost.hash_node_calls
        );
    }

    /// The mirror charge must scale with the number of keys a batch mutates
    /// in an indexed primary, not be a flat per-level constant.
    ///
    /// The real mirror does one delete+insert per captured key on every axis,
    /// so a flat charge undercharges in proportion to batch width — the exact
    /// direction that lets an attacker buy unpaid work.
    #[test]
    fn test_batch_indexed_tree_mirror_cost_scales_with_batch_width() {
        let grove_version = GroveVersion::latest();

        let estimate_for = |n: usize| {
            let db = make_empty_grovedb();
            let tx = db.start_transaction();
            db.insert(
                EMPTY_PATH,
                b"cidx",
                Element::empty_provable_count_indexed_tree(),
                None,
                Some(&tx),
                grove_version,
            )
            .unwrap()
            .expect("create pcit");

            let ops: Vec<_> = (0..n)
                .map(|i| {
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![b"cidx".to_vec()],
                        vec![b'k', i as u8],
                        Element::new_item(b"v".to_vec()),
                    )
                })
                .collect();

            let mut paths = HashMap::new();
            paths.insert(
                KeyInfoPath(vec![]),
                EstimatedLayerInformation {
                    tree_type: TreeType::NormalTree,
                    estimated_layer_count: ApproximateElements(1),
                    estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
                },
            );
            paths.insert(
                KeyInfoPath(vec![KeyInfo::KnownKey(b"cidx".to_vec())]),
                EstimatedLayerInformation {
                    tree_type: TreeType::ProvableCountIndexedTree,
                    estimated_layer_count: ApproximateElements(n as u32),
                    estimated_layer_sizes: AllItems(2, 2, None),
                },
            );

            GroveDb::estimated_case_operations_for_batch(
                AverageCaseCostsType(paths),
                ops,
                None,
                |_cost, _old_flags, _new_flags| Ok(false),
                |_flags, _removed_key_bytes, _removed_value_bytes| {
                    Ok((NoStorageRemoval, NoStorageRemoval))
                },
                grove_version,
            )
            .cost_as_result()
            .expect("expected average case costs")
        };

        let one = estimate_for(1);
        let four = estimate_for(4);

        assert!(
            four.storage_cost.added_bytes > one.storage_cost.added_bytes,
            "a 4-key batch must be charged more mirror work than a 1-key batch \
             (1-key: {one:?}, 4-key: {four:?})"
        );
        assert!(
            four.seek_count > one.seek_count,
            "seek count must scale with the number of mirrored keys \
             (1-key: {}, 4-key: {})",
            one.seek_count,
            four.seek_count
        );
    }

    /// The ≥-actual contract must hold for the MULTI-axis variant, not just
    /// PCIT. The sum and avg axes' secondary rows (`SumItem`,
    /// `ItemWithSumItem`) are larger than the count axis's empty `Item`;
    /// sizing all three as an empty item put the PCPSIT estimate ~25 bytes
    /// per key UNDER actual `added_bytes` — and the PCIT-only test above
    /// could never see it because there the shapes coincide.
    ///
    /// The inserted elements carry worst-width sums (9-byte varint) on
    /// purpose: `average_case_merk_insert_element` sizes non-tree elements by
    /// their actual serialized size while the real charge always uses the
    /// fixed worst-case sum width, a PRE-EXISTING gap shared with live sum
    /// trees that this PR neither introduced nor may change (live estimator
    /// outputs feed fees). Worst-width sums make actual serialized size equal
    /// the charged width, so what this test pins is the indexed machinery's
    /// own contract, not that unrelated gap.
    #[test]
    fn test_batch_pcpsit_insert_average_case_cost_is_not_under_actual() {
        let grove_version = GroveVersion::latest();
        // Worst varint width, but small enough that eight of them cannot
        // overflow the primary's i64 sum aggregate.
        let worst_width_sum = i64::MAX >> 8;
        for n in [1usize, 4, 8] {
            let db = make_empty_grovedb();
            let tx = db.start_transaction();
            db.insert(
                EMPTY_PATH,
                b"idx",
                Element::empty_provable_count_provable_sum_indexed_tree(vec![
                    (0u8, None),
                    (1u8, None),
                    (2u8, None),
                ])
                .expect("canonical axes"),
                None,
                Some(&tx),
                grove_version,
            )
            .unwrap()
            .expect("create pcpsit");

            let ops: Vec<_> = (0..n)
                .map(|i| {
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![b"idx".to_vec()],
                        vec![b'k', i as u8],
                        Element::new_item_with_sum_item(b"v".to_vec(), worst_width_sum - i as i64),
                    )
                })
                .collect();

            let mut paths = HashMap::new();
            paths.insert(
                KeyInfoPath(vec![]),
                EstimatedLayerInformation {
                    tree_type: TreeType::NormalTree,
                    estimated_layer_count: ApproximateElements(1),
                    estimated_layer_sizes: AllSubtrees(3, NoSumTrees, None),
                },
            );
            paths.insert(
                KeyInfoPath(vec![KeyInfo::KnownKey(b"idx".to_vec())]),
                EstimatedLayerInformation {
                    tree_type: TreeType::ProvableCountProvableSumIndexedTree,
                    estimated_layer_count: ApproximateElements(n as u32),
                    estimated_layer_sizes:
                        grovedb_merk::estimated_costs::average_case_costs::EstimatedLayerSizes::AllItemsWithSumItem(2, 2, None),
                },
            );

            let est = GroveDb::estimated_case_operations_for_batch(
                AverageCaseCostsType(paths),
                ops.clone(),
                None,
                |_cost, _old_flags, _new_flags| Ok(false),
                |_flags, _removed_key_bytes, _removed_value_bytes| {
                    Ok((NoStorageRemoval, NoStorageRemoval))
                },
                grove_version,
            )
            .cost_as_result()
            .expect("estimate");

            let actual = db
                .apply_batch(ops, None, Some(&tx), grove_version)
                .cost_as_result()
                .expect("apply");

            assert!(
                est.storage_cost.added_bytes >= actual.storage_cost.added_bytes,
                "n={n}: estimated added_bytes {} must not be under actual {}",
                est.storage_cost.added_bytes,
                actual.storage_cost.added_bytes
            );
            assert!(
                est.seek_count >= actual.seek_count,
                "n={n}: estimated seeks {} must not be under actual {}",
                est.seek_count,
                actual.seek_count
            );
            assert!(
                est.hash_node_calls >= actual.hash_node_calls,
                "n={n}: estimated hash_node_calls {} must not be under actual {}",
                est.hash_node_calls,
                actual.hash_node_calls
            );
            assert!(
                est.storage_loaded_bytes >= actual.storage_loaded_bytes,
                "n={n}: estimated storage_loaded_bytes {} must not be under actual {}",
                est.storage_loaded_bytes,
                actual.storage_loaded_bytes
            );
        }
    }

    /// The ≥-actual contract must also hold for the FRESH path: a batch that
    /// CREATES the indexed primary and populates it in one go exercises
    /// `InsertAggregateIndexedTreeRootKeys` (the insert-side bubble-up arm)
    /// instead of the replace arm the other contract tests reach.
    ///
    /// Worst-width sums for the same reason as the PCPSIT test above.
    #[test]
    fn test_batch_fresh_pcpsit_create_and_populate_average_case_cost_is_not_under_actual() {
        let grove_version = GroveVersion::latest();
        let worst_width_sum = i64::MAX >> 8;
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        let mut ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![],
            b"idx".to_vec(),
            Element::empty_provable_count_provable_sum_indexed_tree(vec![
                (0u8, None),
                (1u8, None),
                (2u8, None),
            ])
            .expect("canonical axes"),
        )];
        ops.extend((0..4usize).map(|i| {
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"idx".to_vec()],
                vec![b'k', i as u8],
                Element::new_item_with_sum_item(b"v".to_vec(), worst_width_sum - i as i64),
            )
        }));

        let mut paths = HashMap::new();
        paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: ApproximateElements(1),
                estimated_layer_sizes: AllSubtrees(3, NoSumTrees, None),
            },
        );
        paths.insert(
            KeyInfoPath(vec![KeyInfo::KnownKey(b"idx".to_vec())]),
            EstimatedLayerInformation {
                tree_type: TreeType::ProvableCountProvableSumIndexedTree,
                estimated_layer_count: ApproximateElements(4),
                estimated_layer_sizes:
                    grovedb_merk::estimated_costs::average_case_costs::EstimatedLayerSizes::AllItemsWithSumItem(2, 2, None),
            },
        );

        let est = GroveDb::estimated_case_operations_for_batch(
            AverageCaseCostsType(paths),
            ops.clone(),
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("estimate");

        let actual = db
            .apply_batch(ops, None, Some(&tx), grove_version)
            .cost_as_result()
            .expect("apply");

        assert!(
            est.storage_cost.added_bytes >= actual.storage_cost.added_bytes,
            "estimated added_bytes {} must not be under actual {}",
            est.storage_cost.added_bytes,
            actual.storage_cost.added_bytes
        );
        assert!(
            est.seek_count >= actual.seek_count,
            "estimated seeks {} must not be under actual {}",
            est.seek_count,
            actual.seek_count
        );
        assert!(
            est.hash_node_calls >= actual.hash_node_calls,
            "estimated hash_node_calls {} must not be under actual {}",
            est.hash_node_calls,
            actual.hash_node_calls
        );
        assert!(
            est.storage_loaded_bytes >= actual.storage_loaded_bytes,
            "estimated storage_loaded_bytes {} must not be under actual {}",
            est.storage_loaded_bytes,
            actual.storage_loaded_bytes
        );
    }

    /// Replay guarantee: the V1..V3 average-case CommitmentTreeInsert arm
    /// must keep producing the LEGACY numbers byte-for-byte — 33 Sinsemilla
    /// hashes, a 554-byte frontier charged as replaced and loaded bytes,
    /// 1 blake3, 3 seeks on top of the parent-node replace — because
    /// historical admission bounds were computed with them. The upper-bound
    /// model is gated to V4+ (`average_case_commitment_tree_insert`).
    #[test]
    fn test_commitment_tree_insert_average_case_cost_pinned_before_v4() {
        use grovedb_version::version::v3::GROVE_V3;
        let grove_version = &GROVE_V3;

        let payload_len: u32 = 216;
        let op = GroveOp::CommitmentTreeInsert {
            cmx: [1u8; 32],
            rho: [2u8; 32],
            cv_net: [3u8; 32],
            payload: vec![0u8; payload_len as usize],
        };
        let key = KeyInfo::KnownKey(b"pool".to_vec());
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLevel(1, false),
            estimated_layer_sizes: AllSubtrees(4, NoSumTrees, None),
        };

        let arm_cost = op
            .average_case_cost(&key, &layer_info, None, false, grove_version)
            .cost_as_result()
            .expect("expected V3 average case cost");
        // A declared chunk power must not change the V3 output — the
        // declared-layer machinery is part of the V4+ model only.
        let arm_cost_with_declared_chunk_power = op
            .average_case_cost(&key, &layer_info, Some(4), false, grove_version)
            .cost_as_result()
            .expect("expected V3 average case cost with declared chunk power");
        assert_eq!(arm_cost, arm_cost_with_declared_chunk_power);

        let replace_part = GroveDb::average_case_merk_replace_tree(
            &key,
            &layer_info,
            grovedb_merk::tree_type::TreeType::CommitmentTree(0),
            false,
            grove_version,
        )
        .cost_as_result()
        .expect("expected replace-tree part");
        let legacy_flat = OperationCost {
            seek_count: 3,
            storage_cost: StorageCost {
                added_bytes: 96 + payload_len,
                replaced_bytes: 554,
                removed_bytes: NoStorageRemoval,
            },
            storage_loaded_bytes: 554,
            hash_node_calls: 1,
            sinsemilla_hash_calls: 33,
        };
        assert_eq!(
            arm_cost,
            replace_part + legacy_flat,
            "V3 average-case CommitmentTreeInsert output changed — this breaks replay of \
             historical admission bounds",
        );
    }
}
