use costs::OperationCost;

use crate::{
    tree::{kv::KV, Link, Tree},
    HASH_BLOCK_SIZE, HASH_BLOCK_SIZE_U32, HASH_LENGTH, HASH_LENGTH_U32,
};

pub enum MerkWorstCaseInput {
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
    // One direct seek has to be performed to read the node from storage_cost.
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
    cost.hash_node_calls += ((value_len + 1) / HASH_BLOCK_SIZE_U32) as u16;
}

/// Add worst case for insertion into merk
pub fn add_worst_case_merk_replace_layered(cost: &mut OperationCost, key_len: u32, value_len: u32) {
    // todo: verify this
    cost.hash_node_calls += ((value_len + 1) / HASH_BLOCK_SIZE_U32) as u16;
    cost.storage_cost.replaced_bytes =
        KV::layered_value_byte_cost_size_for_key_and_value_lengths(key_len, value_len);
    // 37 + 35 + key_len
}

/// Add worst case for insertion into merk
pub fn add_worst_case_merk_insert_layered(cost: &mut OperationCost, key_len: u32, value_len: u32) {
    cost.storage_cost.added_bytes +=
        KV::layered_node_byte_cost_size_for_key_and_value_lengths(key_len, value_len);
    // .. and hash computation for the inserted element itself
    // todo: verify this
    cost.hash_node_calls += ((value_len + 1) / HASH_BLOCK_SIZE_U32) as u16;
}

const fn node_hash_update_count() -> u16 {
    // It's a hash of node hash, left and right
    let bytes = HASH_LENGTH * 3;
    // todo: verify this
    let blocks = (bytes + 1) / HASH_BLOCK_SIZE;

    blocks as u16
}

/// Add worst case for getting a merk tree root hash
pub fn add_worst_case_merk_root_hash(cost: &mut OperationCost) {
    cost.hash_node_calls += node_hash_update_count();
}

pub const MERK_BIGGEST_VALUE_SIZE: u32 = 1024;
pub const MERK_BIGGEST_KEY_SIZE: u32 = 256;

pub fn add_worst_case_merk_propagate(cost: &mut OperationCost, input: MerkWorstCaseInput) {
    let mut nodes_updated = 0;
    // Propagation requires to recompute and write hashes up to the root
    let levels = match input {
        MerkWorstCaseInput::MaxElementsNumber(n) => ((n + 1) as f32).log2().ceil() as u32,
        MerkWorstCaseInput::NumberOfLevels(n) => n,
    };
    nodes_updated += levels;
    // In AVL tree two rotation may happen at most on insertion, some of them may
    // update one more node except one we already have on our path to the
    // root, thus two more updates.
    nodes_updated += 2;

    // todo: verify these numbers
    cost.storage_cost.replaced_bytes += nodes_updated * MERK_BIGGEST_VALUE_SIZE;
    cost.storage_loaded_bytes += nodes_updated * (MERK_BIGGEST_VALUE_SIZE + MERK_BIGGEST_KEY_SIZE);
    // Same number of hash recomputations for propagation
    cost.hash_node_calls += (nodes_updated as u16) * node_hash_update_count();
}
