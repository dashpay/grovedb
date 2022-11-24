use costs::{CostResult, CostsExt, OperationCost};
use integer_encoding::VarInt;

use crate::{
    error::Error,
    estimated_costs::LAYER_COST_SIZE,
    tree::{kv::KV, Link, Tree},
    HASH_BLOCK_SIZE, HASH_BLOCK_SIZE_U32, HASH_LENGTH, HASH_LENGTH_U32, HASH_LENGTH_U32_X2,
};

pub type AverageKeySize = u8;
pub type AverageValueSize = u32;
pub type AverageFlagsSize = u32;
pub type Weight = u8;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EstimatedLayerSizes {
    AllSubtrees(AverageKeySize, Option<AverageFlagsSize>),
    AllItems(AverageKeySize, AverageValueSize, Option<AverageFlagsSize>),
    AllReference(AverageKeySize, AverageValueSize, Option<AverageFlagsSize>),
    Mix {
        subtree_size: Option<(AverageKeySize, Option<AverageFlagsSize>, Weight)>,
        items_size: Option<(
            AverageKeySize,
            AverageValueSize,
            Option<AverageFlagsSize>,
            Weight,
        )>,
        references_size: Option<(
            AverageKeySize,
            AverageValueSize,
            Option<AverageFlagsSize>,
            Weight,
        )>,
    },
}

impl EstimatedLayerSizes {
    pub fn layered_flags_size(&self) -> Result<&Option<AverageFlagsSize>, Error> {
        match self {
            EstimatedLayerSizes::AllSubtrees(_, flags_size) => Ok(flags_size),
            EstimatedLayerSizes::Mix {
                subtree_size,
                items_size,
                references_size,
            } => {
                if let Some((_, flags_size, _)) = subtree_size {
                    Ok(flags_size)
                } else {
                    Err(Error::WrongEstimatedCostsElementTypeForLevel(
                        "this mixed layer does not have costs for trees",
                    ))
                }
            }
            _ => Err(Error::WrongEstimatedCostsElementTypeForLevel(
                "this layer does not have costs for trees",
            )),
        }
    }

    pub fn non_layered_value_with_flags_size(&self) -> Result<u32, Error> {
        match self {
            EstimatedLayerSizes::AllItems(_, average_value_size, flags_size)
            | EstimatedLayerSizes::AllReference(_, average_value_size, flags_size) => {
                Ok(*average_value_size + flags_size.unwrap_or_default())
            }
            EstimatedLayerSizes::Mix {
                subtree_size,
                items_size,
                references_size,
            } => {
                let (item_size, item_weight) = items_size
                    .map(|(ks, vs, fs, weight)| {
                        (ks as u32 + vs + fs.unwrap_or_default(), weight as u32)
                    })
                    .unwrap_or_default();

                let (ref_size, ref_weight) = references_size
                    .map(|(ks, vs, fs, weight)| {
                        (ks as u32 + vs + fs.unwrap_or_default(), weight as u32)
                    })
                    .unwrap_or_default();

                if item_weight == 0 && ref_weight == 0 {
                    return Err(Error::WrongEstimatedCostsElementTypeForLevel(
                        "this layer does not have costs for trees",
                    ));
                }
                if item_weight == 0 {
                    return Ok(ref_size);
                }
                if ref_weight == 0 {
                    return Ok(item_size);
                }
                let combined_weight = item_weight.checked_add(ref_weight).ok_or(
                    Error::Overflow("overflow for non layered value size combining weights"),
                )?;
                item_size
                    .checked_add(ref_size)
                    .and_then(|a| a.checked_div(combined_weight))
                    .ok_or(Error::Overflow("overflow for non layered value size"))
            }
            _ => Err(Error::WrongEstimatedCostsElementTypeForLevel(
                "this layer does not have costs for trees",
            )),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EstimatedLayerInformation {
    ApproximateMaxElements(u32, EstimatedLayerSizes),
    EstimatedLevel(u32, EstimatedLayerSizes),
}

impl EstimatedLayerInformation {
    pub fn sizes(&self) -> &EstimatedLayerSizes {
        match self {
            EstimatedLayerInformation::ApproximateMaxElements(_, estimated_layer_info) => {
                estimated_layer_info
            }
            EstimatedLayerInformation::EstimatedLevel(_, estimated_layer_info) => {
                estimated_layer_info
            }
        }
    }
}

impl Tree {
    pub fn average_case_encoded_tree_size(
        not_prefixed_key_len: u32,
        estimated_element_size: u32,
    ) -> u32 {
        // two option values for the left and right link
        // the actual left and right link encoding size
        // the encoded kv node size
        2 + (2 * Link::encoded_link_size(not_prefixed_key_len))
            + KV::encoded_kv_node_size(estimated_element_size)
    }
}

/// Add worst case for getting a merk node
pub fn add_average_case_get_merk_node(
    cost: &mut OperationCost,
    not_prefixed_key_len: u32,
    approximate_element_size: u32,
) {
    // Worst case scenario, the element is not already in memory.
    // One direct seek has to be performed to read the node from storage.
    cost.seek_count += 1;

    // To write a node to disk, the left link, right link and kv nodes are encoded.
    // worst case, the node has both the left and right link present.
    cost.storage_loaded_bytes +=
        Tree::average_case_encoded_tree_size(not_prefixed_key_len, approximate_element_size);
}

/// Add worst case for getting a merk tree
pub fn add_average_case_merk_has_value(
    cost: &mut OperationCost,
    not_prefixed_key_len: u32,
    max_element_size: u32,
) {
    cost.seek_count += 1;
    cost.storage_loaded_bytes += not_prefixed_key_len + max_element_size;
}

/// Add worst case for insertion into merk
pub fn add_average_case_merk_insert(cost: &mut OperationCost, key_len: u32, value_len: u32) {
    cost.storage_cost.added_bytes +=
        KV::node_byte_cost_size_for_key_and_value_lengths(key_len, value_len);
    // .. and hash computation for the inserted element itself
    // first lets add the value hash
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32) as u16;
    // then let's add the combine hash
    cost.hash_node_calls += 1;
    // then let's add the kv_digest_to_kv_hash hash call
    let hashed_size = key_len.encode_var_vec().len() as u32 + key_len + HASH_LENGTH_U32;
    cost.hash_node_calls += 1 + ((hashed_size - 1) / HASH_BLOCK_SIZE_U32) as u16;
    // then let's add the two block hashes for the node hash call
    cost.hash_node_calls += 2;
}

/// Add worst case for insertion into merk
pub fn add_average_case_merk_replace_layered(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
) {
    cost.storage_cost.replaced_bytes =
        KV::layered_value_byte_cost_size_for_key_and_value_lengths(key_len, value_len);

    // first lets add the value hash
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32) as u16;
    // then let's add the combine hash
    cost.hash_node_calls += 1;
    // then let's add the kv_digest_to_kv_hash hash call
    let hashed_size = key_len.encode_var_vec().len() as u32 + key_len + HASH_LENGTH_U32;
    cost.hash_node_calls += 1 + ((hashed_size - 1) / HASH_BLOCK_SIZE_U32) as u16;
    // then let's add the two block hashes for the node hash call
    cost.hash_node_calls += 2;
}

/// Add worst case for insertion into merk
pub fn add_average_case_merk_insert_layered(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
) {
    cost.storage_cost.added_bytes +=
        KV::layered_node_byte_cost_size_for_key_and_value_lengths(key_len, value_len);
    // .. and hash computation for the inserted element itself
    // first lets add the value hash
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32) as u16;
    // then let's add the combine hash
    cost.hash_node_calls += 1;
    // then let's add the kv_digest_to_kv_hash hash call
    let hashed_size = key_len.encode_var_vec().len() as u32 + key_len + HASH_LENGTH_U32;
    cost.hash_node_calls += 1 + ((hashed_size - 1) / HASH_BLOCK_SIZE_U32) as u16;
    // then let's add the two block hashes for the node hash call
    cost.hash_node_calls += 2;
}

/// Add average case for deletion from merk
pub fn add_average_case_merk_delete_layered(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
) {
    // todo: verify this
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32) as u16;
}

/// Add average case for deletion from merk
pub fn add_average_case_merk_delete(cost: &mut OperationCost, key_len: u32, value_len: u32) {
    // todo: verify this
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32) as u16;
}

const fn node_hash_update_count() -> u16 {
    // It's a hash of node hash, left and right
    let bytes = HASH_LENGTH * 3;
    // todo: verify this
    let blocks = 1 + ((bytes - 1) / HASH_BLOCK_SIZE) as u16;

    blocks as u16
}

/// Add worst case for getting a merk tree root hash
pub fn add_average_case_merk_root_hash(cost: &mut OperationCost) {
    cost.hash_node_calls += node_hash_update_count();
}

pub fn average_case_merk_propagate(input: &EstimatedLayerInformation) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();
    add_average_case_merk_propagate(&mut cost, input).wrap_with_cost(cost)
}

pub fn add_average_case_merk_propagate(
    cost: &mut OperationCost,
    input: &EstimatedLayerInformation,
) -> Result<(), Error> {
    let mut nodes_updated = 0;
    // Propagation requires to recompute and write hashes up to the root
    let (levels, average_typed_size) = match input {
        EstimatedLayerInformation::ApproximateMaxElements(n, s) => {
            (((n + 1) as f32).log2().ceil() as u32, s)
        }
        EstimatedLayerInformation::EstimatedLevel(n, s) => (*n, s),
    };
    nodes_updated += levels;
    // In AVL tree on average 1 rotation will happen.
    // todo: verify this statement
    nodes_updated += 1;

    cost.storage_cost.replaced_bytes += match average_typed_size {
        EstimatedLayerSizes::AllSubtrees(average_key_size, average_flags_size) => {
            let flags_len = average_flags_size.unwrap_or(0);
            let value_len = LAYER_COST_SIZE + flags_len;
            nodes_updated
                * KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                    *average_key_size as u32,
                    value_len,
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
                * KV::value_byte_cost_size_for_key_and_raw_value_lengths(
                    *average_key_size as u32,
                    average_value_len,
                )
        }
        EstimatedLayerSizes::Mix {
            subtree_size,
            items_size,
            references_size,
        } => {
            let total_weight = subtree_size.unwrap_or_default().2 as u32
                + items_size.unwrap_or_default().3 as u32
                + items_size.unwrap_or_default().3 as u32;
            if total_weight == 0 {
                0
            } else {
                let weighted_nodes_updated = (nodes_updated as u64)
                    .checked_mul(total_weight as u64)
                    .ok_or(Error::Overflow("overflow for weights average cost"))?;
                let tree_node_updates_cost = subtree_size
                    .map(|(average_key_size, average_flags_size, weight)| {
                        let flags_len = average_flags_size.unwrap_or(0);
                        let value_len = LAYER_COST_SIZE + flags_len;
                        let cost = KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                            average_key_size as u32,
                            value_len,
                        );
                        (weight as u64)
                            .checked_mul(cost as u64)
                            .ok_or(Error::Overflow("overflow for mixed tree nodes updates"))
                    })
                    .unwrap_or(Ok(0))?;
                let item_node_updates_cost = items_size
                    .map(
                        |(average_key_size, average_value_size, average_flags_size, weight)| {
                            let flags_len = average_flags_size.unwrap_or(0);
                            let value_len = average_value_size + flags_len;
                            let cost = KV::value_byte_cost_size_for_key_and_raw_value_lengths(
                                average_key_size as u32,
                                value_len,
                            );
                            (weight as u64)
                                .checked_mul(cost as u64)
                                .ok_or(Error::Overflow("overflow for mixed item nodes updates"))
                        },
                    )
                    .unwrap_or(Ok(0))?;
                let reference_node_updates_cost = references_size
                    .map(
                        |(average_key_size, average_value_size, average_flags_size, weight)| {
                            let flags_len = average_flags_size.unwrap_or(0);
                            let value_len = average_value_size + flags_len;
                            let cost = KV::value_byte_cost_size_for_key_and_raw_value_lengths(
                                average_key_size as u32,
                                value_len,
                            );
                            (weight as u64)
                                .checked_mul(cost as u64)
                                .ok_or(Error::Overflow("overflow for mixed item nodes updates"))
                        },
                    )
                    .unwrap_or(Ok(0))?;

                let total_updates_cost = tree_node_updates_cost
                    .checked_add(item_node_updates_cost)
                    .and_then(|c| c.checked_add(reference_node_updates_cost))
                    .ok_or(Error::Overflow("overflow for mixed item adding parts"))?;
                let total_replaced_bytes = total_updates_cost / weighted_nodes_updated;
                if total_replaced_bytes > u32::MAX as u64 {
                    return Err(Error::Overflow(
                        "overflow for total replaced bytes more than u32 max",
                    ));
                }
                total_replaced_bytes as u32
            }
        }
    };
    // cost.storage_loaded_bytes += nodes_updated * (MERK_BIGGEST_VALUE_SIZE +
    // MERK_BIGGEST_KEY_SIZE); // Same number of hash recomputations for
    // propagation cost.hash_node_calls += (nodes_updated as u16) *
    // node_hash_update_count();
    Ok(())
}
