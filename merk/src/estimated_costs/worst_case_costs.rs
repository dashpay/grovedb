use costs::{CostResult, CostsExt, OperationCost};

use crate::{
    error::Error,
    tree::{kv::KV, Link, Tree},
    HASH_BLOCK_SIZE, HASH_BLOCK_SIZE_U32, HASH_LENGTH, HASH_LENGTH_U32,
};

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WorstCaseLayerInformation {
    MaxElementsNumber(u32),
    NumberOfLevels(u32),
}

impl Tree {
    pub fn worst_case_encoded_tree_size(not_prefixed_key_len: u32, max_element_size: u32) -> u32 {
        // two option values for the left and right link
        // the actual left and right link encoding size
        // the encoded kv node size
        2 + (2 * Link::encoded_link_size(not_prefixed_key_len))
            + KV::worst_case_encoded_kv_node_size(max_element_size)
    }
}

impl KV {
    fn worst_case_encoded_kv_node_size(max_element_size: u32) -> u32 {
        // KV holds the state of a node
        // 32 bytes to encode the hash of the node
        // 32 bytes to encode the value hash
        // max_element_size to encode the worst case value size
        HASH_LENGTH_U32 + HASH_LENGTH_U32 + max_element_size
    }
}

/// Add worst case for getting a merk node
pub fn add_worst_case_get_merk_node(
    cost: &mut OperationCost,
    not_prefixed_key_len: u32,
    max_element_size: u32,
) {
    // Worst case scenario, the element is not already in memory.
    // One direct seek has to be performed to read the node from storage.
    cost.seek_count += 1;

    // To write a node to disk, the left link, right link and kv nodes are encoded.
    // worst case, the node has both the left and right link present.
    cost.storage_loaded_bytes +=
        Tree::worst_case_encoded_tree_size(not_prefixed_key_len, max_element_size);
}

/// Add worst case for getting a merk tree
pub fn add_worst_case_merk_has_value(
    cost: &mut OperationCost,
    not_prefixed_key_len: u32,
    max_element_size: u32,
) {
    cost.seek_count += 1;
    cost.storage_loaded_bytes += not_prefixed_key_len + max_element_size;
}

/// Add worst case for insertion into merk
pub fn add_worst_case_merk_insert(cost: &mut OperationCost, key_len: u32, value_len: u32) {
    cost.storage_cost.added_bytes +=
        KV::node_byte_cost_size_for_key_and_value_lengths(key_len, value_len);
    // .. and hash computation for the inserted element itself
    // todo: verify this
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32) as u16;
}

/// Add worst case for insertion into merk
pub fn add_worst_case_merk_replace_layered(cost: &mut OperationCost, key_len: u32, value_len: u32) {
    // todo: verify this
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32) as u16;
    cost.storage_cost.replaced_bytes =
        KV::layered_value_byte_cost_size_for_key_and_value_lengths(key_len, value_len);
    // 37 + 35 + key_len
}

/// Add average case for deletion from merk
pub fn add_worst_case_merk_delete_layered(cost: &mut OperationCost, _key_len: u32, value_len: u32) {
    // todo: verify this
    cost.seek_count += 1;
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32) as u16;
}

/// Add average case for deletion from merk
pub fn add_worst_case_merk_delete(cost: &mut OperationCost, _key_len: u32, value_len: u32) {
    // todo: verify this
    cost.seek_count += 1;
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
pub fn add_worst_case_merk_root_hash(cost: &mut OperationCost) {
    cost.hash_node_calls += node_hash_update_count();
}

pub const MERK_BIGGEST_VALUE_SIZE: u32 = u16::MAX as u32;
pub const MERK_BIGGEST_KEY_SIZE: u32 = 256;

pub fn worst_case_merk_propagate(input: &WorstCaseLayerInformation) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();
    add_worst_case_merk_propagate(&mut cost, input).wrap_with_cost(cost)
}

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

    if levels == 2 {
        // we can get about 1 rotation, if there are more than 2 levels
        nodes_updated += 1;
    } else if levels > 2 {
        // In AVL tree two rotation may happen at most on insertion, some of them may
        // update one more node except one we already have on our path to the
        // root, thus two more updates.
        nodes_updated += 2;
    }

    // todo: verify these numbers
    cost.storage_cost.replaced_bytes += nodes_updated * MERK_BIGGEST_VALUE_SIZE;
    cost.storage_loaded_bytes += nodes_updated * (MERK_BIGGEST_VALUE_SIZE + MERK_BIGGEST_KEY_SIZE);
    cost.seek_count += nodes_updated as u16;
    cost.hash_node_calls += (nodes_updated as u16) * 2;
    Ok(())
}
