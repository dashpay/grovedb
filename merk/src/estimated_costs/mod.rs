use costs::OperationCost;
use integer_encoding::VarInt;

use crate::{tree::kv::KV, HASH_BLOCK_SIZE_U32, HASH_LENGTH_U32};

pub mod average_case_costs;
pub mod worst_case_costs;

/// The cost of a subtree layer
pub const LAYER_COST_SIZE: u32 = 3;

/// The cost of a summed subtree layer
pub const SUM_LAYER_COST_SIZE: u32 = 11;

impl KV {
    fn encoded_kv_node_size(element_size: u32) -> u32 {
        // KV holds the state of a node
        // 32 bytes to encode the hash of the node
        // 32 bytes to encode the value hash
        // max_element_size to encode the worst case value size
        HASH_LENGTH_U32 + HASH_LENGTH_U32 + element_size
    }
}

/// Add cost case for insertion into merk
pub fn add_cost_case_merk_insert(cost: &mut OperationCost, key_len: u32, value_len: u32) {
    cost.seek_count += 1;
    cost.storage_cost.added_bytes +=
        KV::node_byte_cost_size_for_key_and_value_lengths(key_len, value_len);
    // .. and hash computation for the inserted element itself
    // first lets add the value hash
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32) as u16;
    // then let's add the kv_digest_to_kv_hash hash call
    let hashed_size = key_len.encode_var_vec().len() as u32 + key_len + HASH_LENGTH_U32;
    cost.hash_node_calls += 1 + ((hashed_size - 1) / HASH_BLOCK_SIZE_U32) as u16;
    // then let's add the two block hashes for the node hash call
    cost.hash_node_calls += 2;
}

/// Add cost case for insertion into merk
pub fn add_cost_case_merk_insert_layered(cost: &mut OperationCost, key_len: u32, value_len: u32) {
    cost.seek_count += 1;
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
