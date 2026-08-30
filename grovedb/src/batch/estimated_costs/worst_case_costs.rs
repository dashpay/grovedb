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
    add_worst_case_get_merk_node, add_worst_case_merk_has_value, worst_case_merk_propagate,
    WorstCaseLayerInformation, MERK_BIGGEST_KEY_SIZE, MERK_BIGGEST_VALUE_SIZE,
};
#[cfg(feature = "minimal")]
use grovedb_merk::estimated_costs::{
    add_cost_case_merk_replace_layered, add_cost_case_merk_replace_same_size,
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
    fn worst_case_cost(
        &self,
        // The op's own path: sizes the inverted-registration growth bound
        // (every `invert()` output is built from the origin's qualified
        // path).
        path: &KeyInfoPath,
        key: &KeyInfo,
        in_parent_tree_type: TreeType,
        worst_case_layer_element_estimates: &WorstCaseLayerInformation,
        // Whether the batch opts into backward-references bookkeeping
        // (`BatchApplyOptions::propagate_backward_references`): family ops
        // and deletes then charge the derived fan-out on GROVE_V4+.
        backward_references_enabled: bool,
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
        let fan_out_version = grove_version
            .grovedb_versions
            .operations
            .worst_case
            .worst_case_backward_references_fan_out;
        // The derived fan-out charged on top of an op's own model, per the
        // documented worst-case bounds (see the model in `super`).
        let backward_references_fan_out = |element: Option<&Element>| {
            if !backward_references_enabled || fan_out_version == 0 {
                return None;
            }
            match element {
                Some(Element::BidirectionalReference(..)) => {
                    // The registration entry appended to the target: an
                    // inverted path built from the referrer's qualified
                    // origin (this op's path segments plus its key — an
                    // absolute inversion serializes them all), the cascade
                    // flag, and framing.
                    let origin_bytes: u32 = path
                        .0
                        .iter()
                        .map(|segment| 4 + segment.max_length() as u32)
                        .sum::<u32>()
                        .saturating_add(4 + key.max_length() as u32);
                    let entry_bound = origin_bytes.saturating_add(16);
                    Some(super::BackwardReferencesFanOut::worst_reference(
                        entry_bound,
                    ))
                }
                // The estimator cannot see the STORED element the op
                // displaces (or deletes): any write can land on a
                // registered family element whose propagation/cascade work
                // is the full item bound.
                Some(_) | None => Some(super::BackwardReferencesFanOut::worst_item()),
            }
        };
        let with_fan_out = |base: CostResult<(), Error>,
                            fan_out: Option<super::BackwardReferencesFanOut>|
         -> CostResult<(), Error> {
            let Some(fan_out) = fan_out else { return base };
            let mut extra = OperationCost::default();
            match add_worst_case_backward_references_fan_out(
                &mut extra,
                fan_out,
                in_parent_tree_type,
                worst_case_layer_element_estimates,
            ) {
                Ok(()) => base.add_cost(extra),
                Err(e) => Err(e).wrap_with_cost(extra),
            }
        };
        match self {
            // The internal derived rewrite: a same-size element replace
            // whose node hash is provided precombined — the standard
            // replace model plus the two combine calls.
            GroveOp::ReplaceBackwardReferenceFamilyMember { element, .. } => {
                if fan_out_version == 0 {
                    return Err(Error::NotSupported(
                        "estimated costs for backward-references batch operations require \
                         GROVE_V4+"
                            .to_owned(),
                    ))
                    .wrap_with_cost(OperationCost::default());
                }
                let combine_cost = OperationCost {
                    hash_node_calls: 2,
                    ..Default::default()
                };
                GroveDb::worst_case_merk_replace_element(
                    key,
                    element,
                    in_parent_tree_type,
                    propagate_if_input(),
                    grove_version,
                )
                .add_cost(combine_cost)
            }
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
            | GroveOp::InsertWithKnownToNotAlreadyExist { element } => with_fan_out(
                GroveDb::worst_case_merk_insert_element(
                    key,
                    element,
                    in_parent_tree_type,
                    propagate_if_input(),
                    grove_version,
                ),
                backward_references_fan_out(Some(element)),
            ),
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
                with_fan_out(
                    GroveDb::worst_case_merk_insert_element(
                        key,
                        element,
                        in_parent_tree_type,
                        propagate_if_input(),
                        grove_version,
                    ),
                    backward_references_fan_out(Some(element)),
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
                // Build the element shape the apply path will write —
                // see the corresponding comment in the average-case
                // estimator.
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
                GroveDb::worst_case_merk_replace_element(
                    key,
                    &element,
                    in_parent_tree_type,
                    propagate_if_input(),
                    grove_version,
                )
            }
            GroveOp::Replace { element } => with_fan_out(
                GroveDb::worst_case_merk_replace_element(
                    key,
                    element,
                    in_parent_tree_type,
                    propagate_if_input(),
                    grove_version,
                ),
                backward_references_fan_out(Some(element)),
            ),
            GroveOp::Patch {
                element,
                change_in_bytes: _,
            } => with_fan_out(
                GroveDb::worst_case_merk_replace_element(
                    key,
                    element,
                    in_parent_tree_type,
                    propagate_if_input(),
                    grove_version,
                ),
                backward_references_fan_out(Some(element)),
            ),
            GroveOp::Delete => with_fan_out(
                GroveDb::worst_case_merk_delete_element(
                    key,
                    worst_case_layer_element_estimates,
                    propagate,
                    grove_version,
                ),
                backward_references_fan_out(None),
            ),
            GroveOp::DeleteTree(tree_type, _) => GroveDb::worst_case_merk_delete_tree(
                key,
                *tree_type,
                worst_case_layer_element_estimates,
                propagate,
                grove_version,
            ),
            GroveOp::CommitmentTreeInsert { payload, .. } => {
                Self::worst_case_commitment_tree_insert(
                    payload,
                    key,
                    in_parent_tree_type,
                    worst_case_layer_element_estimates,
                    propagate,
                    grove_version,
                )
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
                // The fixed per-append model at the physical ceiling (the op
                // carries only the value): the buffer's root-maintenance
                // model, the amortized compaction, the value's chunk-blob
                // share and its churn — plus the compacting append's puts as
                // a bound, the one position-dependent residual.
                use grovedb_costs::storage_cost::{removal::StorageRemovedBytes, StorageCost};
                let value_size = value.len() as u32;
                let buffer = super::dense_buffer_model(super::PHYSICAL_MAX_CHUNK_POWER);
                // The compaction share shrinks with the epoch: the smallest
                // epoch is the bound.
                let amortized_compaction_added = super::max_amortized_compaction_added_bytes();
                const PER_PUT_KEY_AND_LENGTHS: u32 = 50;
                let paid_value = value_size.saturating_add(5); // + the value-length varint
                item_cost.add_cost(OperationCost {
                    // The stored element read, the slot and record writes, the
                    // model's record reads, the compaction's commit-time puts
                    // amortized at the smallest epoch.
                    seek_count: 3u32
                        .saturating_add(buffer.cost.seek_count)
                        .saturating_add(grovedb_bulk_append_tree::max_amortized_compaction_seeks()),
                    storage_cost: StorageCost {
                        // Value + the variable format's per-entry prefix +
                        // the compaction share.
                        added_bytes: value_size
                            .saturating_add(grovedb_bulk_append_tree::VARIABLE_ENTRY_FRAMING_BYTES)
                            .saturating_add(amortized_compaction_added),
                        // Slot (key included as a bound), record, and the
                        // value's part of the blob rewrite.
                        replaced_bytes: paid_value
                            .saturating_add(PER_PUT_KEY_AND_LENGTHS)
                            .saturating_add(buffer.record_len)
                            .saturating_add(PER_PUT_KEY_AND_LENGTHS)
                            .saturating_add(value_size),
                        removed_bytes: StorageRemovedBytes::NoStorageRemoval,
                    },
                    // The stored element with the largest flags a Merk node
                    // can hold, and the model's records.
                    storage_loaded_bytes: (super::CT_ELEMENT_LOAD_BASE + MERK_BIGGEST_VALUE_SIZE)
                        as u64
                        + buffer.cost.storage_loaded_bytes,
                    hash_node_calls: buffer.cost.hash_node_calls
                        + grovedb_bulk_append_tree::max_amortized_compaction_hashes()
                        + 1,
                    sinsemilla_hash_calls: 0,
                })
            }
            GroveOp::PrivateDocumentStoreInsert { entry } => {
                // Cost of updating parent element in the Merk.
                let item_cost = GroveDb::worst_case_merk_replace_tree(
                    key,
                    TreeType::PrivateDocumentStore(0),
                    in_parent_tree_type,
                    worst_case_layer_element_estimates,
                    propagate,
                    grove_version,
                );
                // A genuine UPPER BOUND over every configuration the type
                // permits, not a typical-case figure: `chunk_power` is
                // validated to 1..=16, so the model is charged at the
                // physical ceiling. This deliberately OVER-estimates smaller
                // configurations — a `chunk_power = 4` store pays the
                // `chunk_power = 16` bound — because the op carries only the
                // entry, not the store's committed config, and the config is
                // not knowable here. Over-estimating is the safe direction
                // for a fee admission bound (an under-estimate makes a
                // legitimate block fail replay). Threading the committed
                // `{entry_size, chunk_power}` into the estimate is the way
                // to tighten this; it needs the op or the layer estimate to
                // carry the config. Caller-supplied element flags have no
                // declared bound in the worst-case paths — charge the
                // largest value a Merk node can store.
                item_cost.add_cost(super::private_document_store_insert_op_cost(
                    entry.len() as u32,
                    super::PHYSICAL_MAX_CHUNK_POWER,
                    MERK_BIGGEST_VALUE_SIZE,
                    super::max_amortized_compaction_added_bytes(),
                ))
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
                // The op carries only the value, not the tree's height, so
                // the bound is taken at the physical ceiling: a full-buffer
                // walk (the GROVE_V1..V3 root recompute, and the one-time
                // catch-up a buffer filled under those versions pays at its
                // first GROVE_V4 insert) plus the GROVE_V4 hash-record
                // maintenance. Over-estimating smaller trees is the safe
                // direction for an admission bound.
                item_cost.add_cost(super::dense_tree_insert_op_cost(
                    value.len() as u32,
                    super::PHYSICAL_MAX_CHUNK_POWER,
                ))
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
            // KNOWN GAP: this covers only the parent-merk node update that
            // recomputes the indexed element's value_hash; the per-axis
            // secondary Merk work is not charged.
            //
            // The average-case estimator DOES charge it (see
            // `average_case_indexed_secondary_mirror`), deriving the
            // secondary's shape from the primary's `EstimatedLayerInformation`.
            // That is not possible here: `WorstCaseLayerInformation` carries
            // only `MaxElementsNumber` / `NumberOfLevels` — no tree type (so an
            // indexed primary cannot even be identified) and no key/value sizes
            // (so a secondary row cannot be sized). Closing this needs that
            // public type extended, which is a breaking change for callers
            // that construct it. Worst-case fee reservation for indexed-tree
            // ops must not rely on this number until then.
            GroveOp::InsertAggregateIndexedTreeRootKeys { element, .. } => {
                // Insert-side counterpart of the replace arm below. Sized
                // from the CARRIED element's own tree type — the aggregate a
                // worst-case estimate carries is `NoAggregateData`, whose
                // fallback under-sizes an indexed parent. Flags come off the
                // element; indexed elements cannot be wrapped.
                GroveDb::worst_case_merk_insert_tree(
                    key,
                    element.get_flags(),
                    element.tree_type().unwrap_or(TreeType::NormalTree),
                    in_parent_tree_type,
                    0,
                    propagate_if_input(),
                    grove_version,
                )
            }
            GroveOp::ReplaceAggregateIndexedTreeRootKeys {
                primary_aggregate_data,
                ..
            } => GroveDb::worst_case_merk_replace_tree(
                key,
                // Sized as the INDEXED type — see the note on
                // `indexed_parent_tree_type`; the non-indexed type is smaller
                // than the indexed element's own minimum payload.
                primary_aggregate_data
                    .indexed_parent_tree_type()
                    .unwrap_or_else(|| primary_aggregate_data.parent_tree_type()),
                in_parent_tree_type,
                worst_case_layer_element_estimates,
                propagate,
                grove_version,
            ),
        }
    }

    /// Versioned cost of a `CommitmentTreeInsert` op in the worst-case
    /// estimator. Downstream the estimate is an admission bound, so
    /// historical blocks admitted under the legacy numbers must re-validate
    /// identically on replay — the model is dispatched on
    /// `worst_case_commitment_tree_insert`.
    fn worst_case_commitment_tree_insert(
        payload: &[u8],
        key: &KeyInfo,
        in_parent_tree_type: TreeType,
        worst_case_layer_element_estimates: &WorstCaseLayerInformation,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        match grove_version
            .grovedb_versions
            .operations
            .worst_case
            .worst_case_commitment_tree_insert
        {
            0 => Self::worst_case_commitment_tree_insert_v0(
                payload,
                key,
                in_parent_tree_type,
                worst_case_layer_element_estimates,
                propagate,
                grove_version,
            ),
            1 => Self::worst_case_commitment_tree_insert_v1(
                payload,
                key,
                in_parent_tree_type,
                worst_case_layer_element_estimates,
                propagate,
                grove_version,
            ),
            version => Err(Error::VersionError(
                GroveVersionError::UnknownVersionMismatch {
                    method: "worst_case_commitment_tree_insert".to_string(),
                    known_versions: vec![0, 1],
                    received: version,
                },
            ))
            .wrap_with_cost(OperationCost::default()),
        }
    }

    /// Legacy (V1..V3) model: depth-correct Sinsemilla and frontier bounds
    /// but no dense-buffer recompute or epoch compaction. Kept byte-for-byte
    /// for replay of historical admission decisions; unreachable from the
    /// batch estimation path on those versions (keyless ops are skipped
    /// there) but reachable through direct dispatch.
    fn worst_case_commitment_tree_insert_v0(
        payload: &[u8],
        key: &KeyInfo,
        in_parent_tree_type: TreeType,
        worst_case_layer_element_estimates: &WorstCaseLayerInformation,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        let item_cost = GroveDb::worst_case_merk_replace_tree(
            key,
            TreeType::CommitmentTree(0),
            in_parent_tree_type,
            worst_case_layer_element_estimates,
            propagate,
            grove_version,
        );
        use grovedb_costs::storage_cost::{removal::StorageRemovedBytes, StorageCost};
        // 1 (flag) + 8 (position) + 32 (leaf) + 1 (count) + 32*32
        const MAX_FRONTIER_SIZE: u32 = 1066;
        // Buffer entry: cmx (32) + rho (32) + cv_net (32) + payload
        let buffer_entry_size = 96 + payload.len() as u32;
        // 32 (root computation) + 32 (all ommers cascade) = 64
        const MAX_SINSEMILLA_HASHES: u32 = 64;
        // 1 blake3 for the running buffer hash
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

    /// V4+ model: in the apply path, preprocessing rewrites the op into
    /// ReplaceNonMerkTreeRoot. The base cost is a tree root key replacement
    /// in the parent Merk; the append work itself (frontier I/O, Sinsemilla
    /// hashing, note write, epoch compaction) is charged by the shared
    /// upper-bound model with constants derived from the frontier depth. The
    /// epoch scale is the PHYSICAL ceiling (2^16, the dense buffer's u16
    /// count limit — no tree beyond it can function): unlike the
    /// average-case paths, `WorstCaseLayerInformation` carries no tree type,
    /// so the tree's actual chunk power cannot be declared here. See
    /// `commitment_tree_insert_op_cost`.
    fn worst_case_commitment_tree_insert_v1(
        payload: &[u8],
        key: &KeyInfo,
        in_parent_tree_type: TreeType,
        worst_case_layer_element_estimates: &WorstCaseLayerInformation,
        propagate: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        GroveDb::worst_case_merk_replace_tree(
            key,
            TreeType::CommitmentTree(0),
            in_parent_tree_type,
            worst_case_layer_element_estimates,
            propagate,
            grove_version,
        )
        .add_cost(super::commitment_tree_insert_op_cost(
            payload.len() as u32,
            super::PHYSICAL_MAX_CHUNK_POWER,
            // Caller-supplied element flags have no declared bound in the
            // worst-case paths — charge the largest value a Merk node can
            // store, consistent with the rest of the worst-case machinery.
            MERK_BIGGEST_VALUE_SIZE,
            // The buffer model grows with the height, the compaction share
            // shrinks with the epoch: each at its own worst.
            super::max_amortized_compaction_added_bytes(),
        ))
    }
}

#[cfg(feature = "minimal")]
/// Charge the derived backward-references fan-out at its worst (see the
/// model in `super`): each rewrite is a biggest-node load plus a same-size
/// biggest-node rewrite with the family's hash calls, each resolution a
/// biggest-node load, and each propagation a replay of the layer's merk
/// propagation.
fn add_worst_case_backward_references_fan_out(
    cost: &mut OperationCost,
    fan_out: super::BackwardReferencesFanOut,
    in_parent_tree_type: TreeType,
    worst_case_layer_element_estimates: &WorstCaseLayerInformation,
) -> Result<(), Error> {
    let node_type = in_parent_tree_type.inner_node_type();
    for _ in 0..fan_out.rewrites {
        add_worst_case_get_merk_node(
            cost,
            MERK_BIGGEST_KEY_SIZE,
            MERK_BIGGEST_VALUE_SIZE,
            node_type,
        )
        .map_err(Error::MerkError)?;
        add_cost_case_merk_replace_same_size(
            cost,
            MERK_BIGGEST_KEY_SIZE,
            MERK_BIGGEST_VALUE_SIZE,
            in_parent_tree_type,
        );
        cost.hash_node_calls = cost
            .hash_node_calls
            .saturating_add(super::BACKWARD_REFERENCES_REWRITE_HASH_CALLS);
    }
    for _ in 0..fan_out.resolution_loads {
        add_worst_case_get_merk_node(
            cost,
            MERK_BIGGEST_KEY_SIZE,
            MERK_BIGGEST_VALUE_SIZE,
            node_type,
        )
        .map_err(Error::MerkError)?;
    }
    cost.storage_cost.added_bytes = cost
        .storage_cost
        .added_bytes
        .saturating_add(fan_out.registration_added_bytes);
    for _ in 0..fan_out.propagations {
        worst_case_merk_propagate(worst_case_layer_element_estimates)
            .unwrap_add_cost(cost)
            .map_err(Error::MerkError)?;
        // A derived write in a FOREIGN subtree also propagates up the
        // Grove: one parent tree-element rewrite per ancestor level. The
        // registration rule bounds every bidirectional-edge position to
        // `MAX_BACKWARD_REFERENCES_GROVE_DEPTH` levels, so that many
        // biggest-node ancestor updates is a true ceiling.
        for _ in 0..crate::bidirectional_references::MAX_BACKWARD_REFERENCES_GROVE_DEPTH {
            add_worst_case_get_merk_node(
                cost,
                MERK_BIGGEST_KEY_SIZE,
                MERK_BIGGEST_VALUE_SIZE,
                node_type,
            )
            .map_err(Error::MerkError)?;
            add_cost_case_merk_replace_layered(
                cost,
                MERK_BIGGEST_KEY_SIZE,
                MERK_BIGGEST_VALUE_SIZE,
                in_parent_tree_type,
            );
        }
    }
    Ok(())
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
        batch_apply_options: &BatchApplyOptions,
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
                    path,
                    &key,
                    TreeType::NormalTree,
                    worst_case_layer_element_estimates,
                    batch_apply_options.propagate_backward_references,
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
            /* non_counted = */ false,
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
            cv_net: [3u8; 32],
            payload: vec![0u8; 100],
        };
        let key = KeyInfo::KnownKey(b"tree_key".to_vec());
        let cost = op
            .worst_case_cost(
                &KeyInfoPath(vec![]),
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
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
            cv_net: [3u8; 32],
            payload: vec![0u8; 50],
        };
        let key = KeyInfo::KnownKey(b"tree_key".to_vec());
        let cost = op
            .worst_case_cost(
                &KeyInfoPath(vec![]),
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
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
                &KeyInfoPath(vec![]),
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
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
                &KeyInfoPath(vec![]),
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
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
    fn test_private_document_store_insert_worst_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        let op = GroveOp::PrivateDocumentStoreInsert {
            entry: vec![0u8; 128],
        };
        let key = KeyInfo::KnownKey(b"pds_key".to_vec());
        let cost = op
            .worst_case_cost(
                &KeyInfoPath(vec![]),
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
                false,
                grove_version,
            )
            .cost_as_result()
            .expect("expected worst case cost for private document store insert");
        assert!(cost.seek_count > 0);
        assert!(cost.hash_node_calls > 0);
        // The worst case includes the entry-size-parametrized compaction
        // blob at the physical ceiling — reported as a replacement of the
        // epoch's prepaid entry bytes (GROVE_V4 accounting, issue #822) —
        // plus the per-append slot write and chunk-blob share as added.
        // The fixed model: the entry's slot and blob-rewrite part replaced,
        // its share added; nothing scales with the epoch any more.
        assert!(cost.storage_cost.replaced_bytes >= 2 * 128);
        assert!(cost.storage_cost.replaced_bytes < 128 * 65536);
        assert!(cost.storage_cost.added_bytes >= 128 + 1);
        // The compaction's read-back is amortized into the model: no
        // epoch-sized seek count any more.
        assert!(cost.seek_count < 65535);
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
                &KeyInfoPath(vec![]),
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
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
                &KeyInfoPath(vec![]),
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
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
                &KeyInfoPath(vec![]),
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(50),
                false,
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
                &KeyInfoPath(vec![]),
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
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
                &KeyInfoPath(vec![]),
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
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
                &KeyInfoPath(vec![]),
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
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
                &KeyInfoPath(vec![]),
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
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

    /// Covers the `GroveOp::ReplaceAggregateIndexedTreeRootKeys` arm in
    /// `worst_case_cost`. The op is emitted by `execute_ops_on_path` when
    /// a level's path resolves to a Count/ProvableCount-indexed primary
    /// — the worst-case path here just delegates to
    /// `worst_case_merk_replace_tree` with the indexed tree type derived
    /// from the carried `primary_aggregate_data`.
    #[test]
    fn test_replace_aggregate_indexed_tree_root_keys_worst_case_cost_direct() {
        let grove_version = GroveVersion::latest();
        use grovedb_merk::tree::AggregateData;
        let key = KeyInfo::KnownKey(b"agg_idx".to_vec());

        let op_count = GroveOp::ReplaceAggregateIndexedTreeRootKeys {
            primary_hash: [1u8; 32],
            primary_root_key: Some(b"prk".to_vec()),
            primary_aggregate_data: AggregateData::ProvableCount(42),
            axes: vec![(0u8, [2u8; 32], Some(b"srk".to_vec()))],
        };
        let cost_count = op_count
            .worst_case_cost(
                &KeyInfoPath(vec![]),
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
                false,
                grove_version,
            )
            .cost_as_result()
            .expect("expected worst case cost for ReplaceAggregateIndexedTreeRootKeys (Count)");
        assert!(
            cost_count.seek_count > 0
                || cost_count.storage_loaded_bytes > 0
                || cost_count.hash_node_calls > 0,
            "expected non-trivial cost; got {:?}",
            cost_count
        );

        // Also exercise the propagate=true path.
        let op_pcount = GroveOp::ReplaceAggregateIndexedTreeRootKeys {
            primary_hash: [3u8; 32],
            primary_root_key: None,
            primary_aggregate_data: AggregateData::ProvableCount(7),
            axes: vec![(0u8, [4u8; 32], None)],
        };
        let cost_pcount = op_pcount
            .worst_case_cost(
                &KeyInfoPath(vec![]),
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
                true,
                grove_version,
            )
            .cost_as_result()
            .expect(
                "expected worst case cost for ReplaceAggregateIndexedTreeRootKeys (ProvableCount)",
            );
        assert!(
            cost_pcount.seek_count >= cost_count.seek_count
                || cost_pcount.hash_node_calls >= cost_count.hash_node_calls,
            "propagate should not reduce cost below non-propagate baseline; pcount={:?}, \
             count={:?}",
            cost_pcount,
            cost_count,
        );
    }

    /// Replay guarantee: the V1..V3 worst-case CommitmentTreeInsert arm
    /// must keep producing the LEGACY numbers byte-for-byte — 64 Sinsemilla
    /// hashes, a 1066-byte frontier charged as replaced and loaded bytes,
    /// 1 blake3, 3 seeks on top of the parent-node replace, no epoch
    /// compaction — because historical admission bounds were computed with
    /// them. The upper-bound model is gated to V4+
    /// (`worst_case_commitment_tree_insert`).
    #[test]
    fn test_commitment_tree_insert_worst_case_cost_pinned_before_v4() {
        use grovedb_costs::{
            storage_cost::{removal::StorageRemovedBytes::NoStorageRemoval, StorageCost},
            OperationCost,
        };
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
        let layer_info = MaxElementsNumber(100);

        let arm_cost = op
            .worst_case_cost(
                &KeyInfoPath(vec![]),
                &key,
                TreeType::NormalTree,
                &layer_info,
                false,
                false,
                grove_version,
            )
            .cost_as_result()
            .expect("expected V3 worst case cost");

        let replace_part = GroveDb::worst_case_merk_replace_tree(
            &key,
            grovedb_merk::tree_type::TreeType::CommitmentTree(0),
            TreeType::NormalTree,
            &layer_info,
            false,
            grove_version,
        )
        .cost_as_result()
        .expect("expected replace-tree part");
        let legacy_flat = OperationCost {
            seek_count: 3,
            storage_cost: StorageCost {
                added_bytes: 96 + payload_len,
                replaced_bytes: 1066,
                removed_bytes: NoStorageRemoval,
            },
            storage_loaded_bytes: 1066,
            hash_node_calls: 1,
            sinsemilla_hash_calls: 64,
        };
        assert_eq!(
            arm_cost,
            replace_part + legacy_flat,
            "V3 worst-case CommitmentTreeInsert output changed — this breaks replay of \
             historical admission bounds",
        );
    }

    /// Boundary test for the saturating epoch arithmetic: a hand-built op
    /// with an oversized payload (the op type is public; the apply path only
    /// rejects wrong-sized payloads later) drives the physical-ceiling epoch
    /// term past u32. The estimate must saturate at u32::MAX — never panic
    /// in debug builds nor wrap in release builds, since a wrapped figure
    /// would silently UNDER-estimate. The epoch term is the compaction
    /// blob, which the V4 accounting reports as replaced (issue #822).
    #[test]
    fn test_commitment_tree_insert_worst_case_cost_oversized_payload_saturates() {
        let grove_version = GroveVersion::latest();
        let op = GroveOp::CommitmentTreeInsert {
            cmx: [1u8; 32],
            rho: [2u8; 32],
            cv_net: [3u8; 32],
            // 2^16 epoch x (96 + 70_000 + 16) bytes ≈ 4.6e9 > u32::MAX.
            payload: vec![0u8; 70_000],
        };
        let key = KeyInfo::KnownKey(b"tree_key".to_vec());
        let cost = op
            .worst_case_cost(
                &KeyInfoPath(vec![]),
                &key,
                TreeType::NormalTree,
                &MaxElementsNumber(100),
                false,
                false,
                grove_version,
            )
            .cost_as_result()
            .expect("expected worst case cost for oversized payload");
        // The epoch no longer multiplies anything: the compaction is
        // amortized into the per-note model, so the figure is finite and
        // per-note — the u64 sums exist for hand-built payloads only.
        assert!(
            cost.storage_cost.replaced_bytes < u32::MAX,
            "the per-note estimate does not scale with the epoch: {cost:?}",
        );
        assert!(cost.storage_cost.replaced_bytes >= 2 * (96 + 70_000));
        // The per-note added term (slot + blob share + frontier + framing)
        // is epoch-independent and stays far from saturation.
        assert!(
            cost.storage_cost.added_bytes < 1_000_000,
            "added_bytes is per-note, not epoch-scaled: {}",
            cost.storage_cost.added_bytes
        );
    }
}
