//! `add_average_case_merk_propagate` v2 — grove v3 dispatch path.
//!
//! Fixes the pre-existing Mix divisor bug. v1 computed
//! `total_updates_cost / (nodes_updated · total_weight)`, which gave
//! the per-node weighted-average cost divided by `nodes_updated`
//! instead of the per-layer weighted total — equivalent to
//! `cost / nodes_updated²` for an items-only Mix vs the same
//! `AllItems` layer.
//!
//! v2 computes `nodes_updated · total_updates_cost / total_weight`,
//! matching what the `AllItems` / `AllSubtrees` arms already do
//! (`nodes_updated · cost_per_node`).
//!
//! Uses `tree_type.inner_node_type()` for the Mix reference-cost arms
//! throughout (same as v0; v1's `storage_loaded_bytes` arm used
//! `TreeType::NormalTree.inner_node_type()` — see v1's doc comment).

#![cfg(feature = "minimal")]

use grovedb_costs::OperationCost;
use grovedb_version::version::GroveVersion;
use integer_encoding::VarInt;

use crate::{
    error::Error,
    estimated_costs::{
        average_case_costs::{EstimatedLayerInformation, EstimatedLayerSizes},
        LAYER_COST_SIZE,
    },
    tree::kv::KV,
};

pub(super) fn add_average_case_merk_propagate_v2(
    cost: &mut OperationCost,
    input: &EstimatedLayerInformation,
    grove_version: &GroveVersion,
) -> Result<(), Error> {
    let mut nodes_updated = 0;
    let EstimatedLayerInformation {
        tree_type,
        estimated_layer_count,
        estimated_layer_sizes,
    } = input;
    let levels = estimated_layer_count.estimate_levels();
    nodes_updated += levels;

    if levels > 1 {
        nodes_updated += 1;
    }
    cost.seek_count += nodes_updated;

    cost.hash_node_calls += nodes_updated * 2;

    cost.storage_cost.replaced_bytes += match estimated_layer_sizes {
        EstimatedLayerSizes::AllSubtrees(
            average_key_size,
            estimated_sum_trees,
            average_flags_size,
        ) => {
            let value_len = LAYER_COST_SIZE
                + average_flags_size
                    .map_or(0, |flags_len| flags_len + flags_len.required_space() as u32);
            let sum_tree_addition = estimated_sum_trees.estimated_size(grove_version)?;
            nodes_updated
                * (KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                    *average_key_size as u32,
                    value_len,
                    tree_type.inner_node_type(),
                ) + sum_tree_addition)
        }
        EstimatedLayerSizes::AllItems(average_key_size, average_item_size, average_flags_size)
        | EstimatedLayerSizes::AllReference(
            average_key_size,
            average_item_size,
            average_flags_size,
        ) => {
            let flags_len = average_flags_size.unwrap_or(0);
            let average_value_len = average_item_size + flags_len;
            nodes_updated
                * KV::value_byte_cost_size_for_key_and_raw_value_lengths(
                    *average_key_size as u32,
                    average_value_len,
                    tree_type.inner_node_type(),
                )
        }
        EstimatedLayerSizes::AllItemsWithSumItem(
            average_key_size,
            average_item_size,
            average_flags_size,
        )
        | EstimatedLayerSizes::AllReferencesWithSumItem(
            average_key_size,
            average_item_size,
            average_flags_size,
        ) => {
            let flags_len = average_flags_size.unwrap_or(0);
            let average_value_len = average_item_size + flags_len + 10;
            nodes_updated
                * KV::value_byte_cost_size_for_key_and_raw_value_lengths(
                    *average_key_size as u32,
                    average_value_len,
                    tree_type.inner_node_type(),
                )
        }
        EstimatedLayerSizes::Mix {
            subtrees_size,
            items_size,
            references_size,
            items_with_sum_item_size,
            references_with_sum_item_size,
        } => {
            let total_weight = subtrees_size
                .as_ref()
                .map(|(_, _, _, weight)| *weight as u32)
                .unwrap_or_default()
                + items_size
                    .as_ref()
                    .map(|(_, _, _, weight)| *weight as u32)
                    .unwrap_or_default()
                + references_size
                    .as_ref()
                    .map(|(_, _, _, weight)| *weight as u32)
                    .unwrap_or_default()
                + items_with_sum_item_size
                    .as_ref()
                    .map(|(_, _, _, weight)| *weight as u32)
                    .unwrap_or_default()
                + references_with_sum_item_size
                    .as_ref()
                    .map(|(_, _, _, weight)| *weight as u32)
                    .unwrap_or_default();
            if total_weight == 0 {
                0
            } else {
                let tree_node_updates_cost = match subtrees_size {
                    None => 0,
                    Some((average_key_size, estimated_sum_trees, average_flags_size, weight)) => {
                        let flags_len = average_flags_size.unwrap_or(0);
                        let value_len = LAYER_COST_SIZE + flags_len;
                        let sum_tree_addition =
                            estimated_sum_trees.estimated_size(grove_version)?;
                        let cost = KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                            *average_key_size as u32,
                            value_len,
                            tree_type.inner_node_type(),
                        ) + sum_tree_addition;
                        (*weight as u64)
                            .checked_mul(cost as u64)
                            .ok_or(Error::Overflow("overflow for mixed tree nodes updates"))?
                    }
                };
                let item_node_updates_cost = match items_size {
                    None => 0,
                    Some((average_key_size, average_value_size, average_flags_size, weight)) => {
                        let flags_len = average_flags_size.unwrap_or(0);
                        let value_len = average_value_size + flags_len;
                        let cost = KV::value_byte_cost_size_for_key_and_raw_value_lengths(
                            *average_key_size as u32,
                            value_len,
                            tree_type.inner_node_type(),
                        );
                        (*weight as u64)
                            .checked_mul(cost as u64)
                            .ok_or(Error::Overflow("overflow for mixed item nodes updates"))?
                    }
                };
                let reference_node_updates_cost = match references_size {
                    None => 0,
                    Some((average_key_size, average_value_size, average_flags_size, weight)) => {
                        let flags_len = average_flags_size.unwrap_or(0);
                        let value_len = average_value_size + flags_len;
                        let cost = KV::value_byte_cost_size_for_key_and_raw_value_lengths(
                            *average_key_size as u32,
                            value_len,
                            tree_type.inner_node_type(),
                        );
                        (*weight as u64)
                            .checked_mul(cost as u64)
                            .ok_or(Error::Overflow("overflow for mixed item nodes updates"))?
                    }
                };
                let item_with_sum_node_updates_cost = match items_with_sum_item_size {
                    None => 0,
                    Some((average_key_size, average_value_size, average_flags_size, weight)) => {
                        let flags_len = average_flags_size.unwrap_or(0);
                        let value_len = average_value_size + flags_len + 10;
                        let cost = KV::value_byte_cost_size_for_key_and_raw_value_lengths(
                            *average_key_size as u32,
                            value_len,
                            tree_type.inner_node_type(),
                        );
                        (*weight as u64)
                            .checked_mul(cost as u64)
                            .ok_or(Error::Overflow(
                                "overflow for mixed item-with-sum nodes updates",
                            ))?
                    }
                };
                let reference_with_sum_node_updates_cost = match references_with_sum_item_size {
                    None => 0,
                    Some((average_key_size, average_value_size, average_flags_size, weight)) => {
                        let flags_len = average_flags_size.unwrap_or(0);
                        let value_len = average_value_size + flags_len + 10;
                        let cost = KV::value_byte_cost_size_for_key_and_raw_value_lengths(
                            *average_key_size as u32,
                            value_len,
                            tree_type.inner_node_type(),
                        );
                        (*weight as u64)
                            .checked_mul(cost as u64)
                            .ok_or(Error::Overflow(
                                "overflow for mixed reference-with-sum nodes updates",
                            ))?
                    }
                };

                let total_updates_cost = tree_node_updates_cost
                    .checked_add(item_node_updates_cost)
                    .and_then(|c| c.checked_add(reference_node_updates_cost))
                    .and_then(|c| c.checked_add(item_with_sum_node_updates_cost))
                    .and_then(|c| c.checked_add(reference_with_sum_node_updates_cost))
                    .ok_or(Error::Overflow("overflow for mixed item adding parts"))?;
                // v2 fix: scale by nodes_updated, divide by total_weight
                // (the per-node weighted average × nodes_updated).
                // Multiply first to preserve precision in integer
                // arithmetic.
                let scaled = (nodes_updated as u64)
                    .checked_mul(total_updates_cost)
                    .ok_or(Error::Overflow("overflow scaling mixed propagate cost"))?;
                let total_replaced_bytes = scaled / total_weight as u64;
                if total_replaced_bytes > u32::MAX as u64 {
                    return Err(Error::Overflow(
                        "overflow for total replaced bytes more than u32 max",
                    ));
                }
                total_replaced_bytes as u32
            }
        }
    };
    cost.storage_loaded_bytes += match estimated_layer_sizes {
        EstimatedLayerSizes::AllSubtrees(
            average_key_size,
            estimated_sum_trees,
            average_flags_size,
        ) => {
            let flags_len = average_flags_size.unwrap_or(0);
            let value_len = LAYER_COST_SIZE + flags_len;
            let sum_tree_addition = estimated_sum_trees.estimated_size(grove_version)?;
            nodes_updated
                * KV::layered_node_byte_cost_size_for_key_and_value_lengths(
                    *average_key_size as u32,
                    value_len + sum_tree_addition,
                    tree_type.inner_node_type(),
                )
        }
        EstimatedLayerSizes::AllItems(average_key_size, average_item_size, average_flags_size)
        | EstimatedLayerSizes::AllReference(
            average_key_size,
            average_item_size,
            average_flags_size,
        ) => {
            let flags_len = average_flags_size.unwrap_or(0);
            let average_value_len = average_item_size + flags_len;
            nodes_updated
                * KV::node_byte_cost_size_for_key_and_raw_value_lengths(
                    *average_key_size as u32,
                    average_value_len,
                    tree_type.inner_node_type(),
                )
        }
        EstimatedLayerSizes::AllItemsWithSumItem(
            average_key_size,
            average_item_size,
            average_flags_size,
        )
        | EstimatedLayerSizes::AllReferencesWithSumItem(
            average_key_size,
            average_item_size,
            average_flags_size,
        ) => {
            let flags_len = average_flags_size.unwrap_or(0);
            let average_value_len = average_item_size + flags_len + 10;
            nodes_updated
                * KV::node_byte_cost_size_for_key_and_raw_value_lengths(
                    *average_key_size as u32,
                    average_value_len,
                    tree_type.inner_node_type(),
                )
        }
        EstimatedLayerSizes::Mix {
            subtrees_size,
            items_size,
            references_size,
            items_with_sum_item_size,
            references_with_sum_item_size,
        } => {
            let total_weight = subtrees_size
                .as_ref()
                .map(|(_, _, _, weight)| *weight as u32)
                .unwrap_or_default()
                + items_size
                    .as_ref()
                    .map(|(_, _, _, weight)| *weight as u32)
                    .unwrap_or_default()
                + references_size
                    .as_ref()
                    .map(|(_, _, _, weight)| *weight as u32)
                    .unwrap_or_default()
                + items_with_sum_item_size
                    .as_ref()
                    .map(|(_, _, _, weight)| *weight as u32)
                    .unwrap_or_default()
                + references_with_sum_item_size
                    .as_ref()
                    .map(|(_, _, _, weight)| *weight as u32)
                    .unwrap_or_default();
            if total_weight == 0 {
                0
            } else {
                let tree_node_updates_cost = subtrees_size
                    .as_ref()
                    .map(
                        |(average_key_size, estimated_sum_trees, average_flags_size, weight)| {
                            let flags_len = average_flags_size.unwrap_or(0);
                            let value_len = LAYER_COST_SIZE + flags_len;
                            let sum_tree_addition =
                                estimated_sum_trees.estimated_size(grove_version)?;
                            let cost = KV::layered_node_byte_cost_size_for_key_and_value_lengths(
                                *average_key_size as u32,
                                value_len + sum_tree_addition,
                                tree_type.inner_node_type(),
                            );
                            (*weight as u64)
                                .checked_mul(cost as u64)
                                .ok_or(Error::Overflow("overflow for mixed tree nodes updates"))
                        },
                    )
                    .unwrap_or(Ok(0))?;
                let item_node_updates_cost = items_size
                    .as_ref()
                    .map(
                        |(average_key_size, average_value_size, average_flags_size, weight)| {
                            let flags_len = average_flags_size.unwrap_or(0);
                            let value_len = average_value_size + flags_len;
                            let cost = KV::node_byte_cost_size_for_key_and_raw_value_lengths(
                                *average_key_size as u32,
                                value_len,
                                tree_type.inner_node_type(),
                            );
                            (*weight as u64)
                                .checked_mul(cost as u64)
                                .ok_or(Error::Overflow("overflow for mixed item nodes updates"))
                        },
                    )
                    .unwrap_or(Ok(0))?;
                let reference_node_updates_cost = references_size
                    .as_ref()
                    .map(
                        |(average_key_size, average_value_size, average_flags_size, weight)| {
                            let flags_len = average_flags_size.unwrap_or(0);
                            let value_len = average_value_size + flags_len;
                            let cost = KV::node_byte_cost_size_for_key_and_raw_value_lengths(
                                *average_key_size as u32,
                                value_len,
                                tree_type.inner_node_type(),
                            );
                            (*weight as u64)
                                .checked_mul(cost as u64)
                                .ok_or(Error::Overflow("overflow for mixed item nodes updates"))
                        },
                    )
                    .unwrap_or(Ok(0))?;
                let item_with_sum_node_updates_cost = items_with_sum_item_size
                    .as_ref()
                    .map(
                        |(average_key_size, average_value_size, average_flags_size, weight)| {
                            let flags_len = average_flags_size.unwrap_or(0);
                            let value_len = average_value_size + flags_len + 10;
                            let cost = KV::node_byte_cost_size_for_key_and_raw_value_lengths(
                                *average_key_size as u32,
                                value_len,
                                tree_type.inner_node_type(),
                            );
                            (*weight as u64)
                                .checked_mul(cost as u64)
                                .ok_or(Error::Overflow(
                                    "overflow for mixed item-with-sum nodes updates",
                                ))
                        },
                    )
                    .unwrap_or(Ok(0))?;
                let reference_with_sum_node_updates_cost = references_with_sum_item_size
                    .as_ref()
                    .map(
                        |(average_key_size, average_value_size, average_flags_size, weight)| {
                            let flags_len = average_flags_size.unwrap_or(0);
                            let value_len = average_value_size + flags_len + 10;
                            let cost = KV::node_byte_cost_size_for_key_and_raw_value_lengths(
                                *average_key_size as u32,
                                value_len,
                                tree_type.inner_node_type(),
                            );
                            (*weight as u64)
                                .checked_mul(cost as u64)
                                .ok_or(Error::Overflow(
                                    "overflow for mixed reference-with-sum nodes updates",
                                ))
                        },
                    )
                    .unwrap_or(Ok(0))?;

                let total_updates_cost = tree_node_updates_cost
                    .checked_add(item_node_updates_cost)
                    .and_then(|c| c.checked_add(reference_node_updates_cost))
                    .and_then(|c| c.checked_add(item_with_sum_node_updates_cost))
                    .and_then(|c| c.checked_add(reference_with_sum_node_updates_cost))
                    .ok_or(Error::Overflow("overflow for mixed item adding parts"))?;
                let scaled = (nodes_updated as u64)
                    .checked_mul(total_updates_cost)
                    .ok_or(Error::Overflow("overflow scaling mixed propagate cost"))?;
                let total_loaded_bytes = scaled / total_weight as u64;
                if total_loaded_bytes > u32::MAX as u64 {
                    return Err(Error::Overflow(
                        "overflow for total replaced bytes more than u32 max",
                    ));
                }
                total_loaded_bytes as u32
            }
        }
    } as u64;
    Ok(())
}
