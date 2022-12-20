use std::cmp::Ordering;

#[cfg(feature = "full")]
use costs::{CostResult, CostsExt, OperationCost};

#[cfg(feature = "full")]
use crate::{
    error::Error,
    merk::defaults::MAX_PREFIXED_KEY_SIZE,
    tree::{kv::KV, Link, Tree},
    HASH_BLOCK_SIZE, HASH_BLOCK_SIZE_U32, HASH_LENGTH,
};

#[cfg(feature = "full")]
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WorstCaseLayerInformation {
    MaxElementsNumber(u32),
    NumberOfLevels(u32),
}

#[cfg(feature = "full")]
impl Tree {
    pub fn worst_case_encoded_tree_size(
        not_prefixed_key_len: u32,
        max_element_size: u32,
        is_sum_node: bool,
    ) -> u32 {
        // two option values for the left and right link
        // the actual left and right link encoding size
        // the encoded kv node size
        2 + (2 * Link::encoded_link_size(not_prefixed_key_len, is_sum_node))
            + KV::encoded_kv_node_size(max_element_size, is_sum_node)
    }
}

#[cfg(feature = "full")]
/// Add worst case for getting a merk node
pub fn add_worst_case_get_merk_node(
    cost: &mut OperationCost,
    not_prefixed_key_len: u32,
    max_element_size: u32,
    is_sum_node: bool,
) {
    // Worst case scenario, the element is not already in memory.
    // One direct seek has to be performed to read the node from storage.
    cost.seek_count += 1;

    // To write a node to disk, the left link, right link and kv nodes are encoded.
    // worst case, the node has both the left and right link present.
    cost.storage_loaded_bytes +=
        Tree::worst_case_encoded_tree_size(not_prefixed_key_len, max_element_size, is_sum_node);
}

#[cfg(feature = "full")]
/// Add worst case for getting a merk tree
pub fn add_worst_case_merk_has_value(
    cost: &mut OperationCost,
    not_prefixed_key_len: u32,
    max_element_size: u32,
) {
    cost.seek_count += 1;
    cost.storage_loaded_bytes += not_prefixed_key_len + max_element_size;
}

#[cfg(feature = "full")]
/// Add worst case for insertion into merk
pub fn add_worst_case_merk_insert(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
    is_sum_node: bool,
) {
    cost.storage_cost.added_bytes +=
        KV::node_byte_cost_size_for_key_and_raw_value_lengths(key_len, value_len, is_sum_node);
    // .. and hash computation for the inserted element itself
    // todo: verify this
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32) as u16;
}

#[cfg(feature = "full")]
/// Add worst case for insertion into merk
pub fn add_worst_case_merk_replace_layered(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
    is_sum_node: bool,
) {
    // todo: verify this
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32) as u16;
    cost.storage_cost.replaced_bytes =
        KV::layered_value_byte_cost_size_for_key_and_value_lengths(key_len, value_len, is_sum_node);
    // 37 + 35 + key_len
}

#[cfg(feature = "full")]
/// Add average case for deletion from merk
pub fn add_worst_case_merk_delete_layered(cost: &mut OperationCost, _key_len: u32, value_len: u32) {
    // todo: verify this
    cost.seek_count += 1;
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32) as u16;
}

#[cfg(feature = "full")]
/// Add average case for deletion from merk
pub fn add_worst_case_merk_delete(cost: &mut OperationCost, _key_len: u32, value_len: u32) {
    // todo: verify this
    cost.seek_count += 1;
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32) as u16;
}

#[cfg(feature = "full")]
const fn node_hash_update_count() -> u16 {
    // It's a hash of node hash, left and right
    let bytes = HASH_LENGTH * 3;
    // todo: verify this

    1 + ((bytes - 1) / HASH_BLOCK_SIZE) as u16
}

#[cfg(feature = "full")]
/// Add worst case for getting a merk tree root hash
pub fn add_worst_case_merk_root_hash(cost: &mut OperationCost) {
    cost.hash_node_calls += node_hash_update_count();
}

#[cfg(feature = "full")]
pub const MERK_BIGGEST_VALUE_SIZE: u32 = u16::MAX as u32;
#[cfg(feature = "full")]
pub const MERK_BIGGEST_KEY_SIZE: u32 = 256;

#[cfg(feature = "full")]
pub fn worst_case_merk_propagate(input: &WorstCaseLayerInformation) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();
    add_worst_case_merk_propagate(&mut cost, input).wrap_with_cost(cost)
}

#[cfg(feature = "full")]
pub fn add_worst_case_merk_propagate(
    cost: &mut OperationCost,
    input: &WorstCaseLayerInformation,
) -> Result<(), Error> {
    let mut nodes_updated = 0;
    // Propagation requires to recompute and write hashes up to the root
    let levels = match input {
        WorstCaseLayerInformation::MaxElementsNumber(n) => {
            if *n == u32::MAX {
                32
            } else {
                ((*n + 1) as f32).log2().ceil() as u32
            }
        }
        WorstCaseLayerInformation::NumberOfLevels(n) => *n,
    };
    nodes_updated += levels;

    match levels.cmp(&2) {
        Ordering::Equal => {
            // we can get about 1 rotation, if there are more than 2 levels
            nodes_updated += 1;
        }
        Ordering::Greater => {
            // In AVL tree two rotation may happen at most on insertion, some of them may
            // update one more node except one we already have on our path to the
            // root, thus two more updates.
            nodes_updated += 2;
        }
        _ => {}
    }

    // todo: verify these numbers
    cost.storage_cost.replaced_bytes += nodes_updated * MERK_BIGGEST_VALUE_SIZE;
    cost.storage_loaded_bytes += nodes_updated * (MERK_BIGGEST_VALUE_SIZE + MERK_BIGGEST_KEY_SIZE);
    cost.seek_count += nodes_updated as u16;
    cost.hash_node_calls += (nodes_updated as u16) * 2;
    Ok(())
}

#[cfg(feature = "full")]
pub fn add_worst_case_cost_for_is_empty_tree_except(
    cost: &mut OperationCost,
    except_keys_count: u16,
) {
    cost.seek_count += except_keys_count + 1;
    cost.storage_loaded_bytes += MAX_PREFIXED_KEY_SIZE * (except_keys_count as u32 + 1);
}

#[cfg(feature = "full")]
pub fn add_average_case_cost_for_is_empty_tree_except(
    cost: &mut OperationCost,
    except_keys_count: u16,
    estimated_prefixed_key_size: u32,
) {
    cost.seek_count += except_keys_count + 1;
    cost.storage_loaded_bytes += estimated_prefixed_key_size * (except_keys_count as u32 + 1);
}
