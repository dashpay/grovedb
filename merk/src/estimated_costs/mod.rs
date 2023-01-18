// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Estimated costs for Merk

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
    fn encoded_kv_node_size(element_size: u32, is_sum_node: bool) -> u32 {
        let sum_node_feature_size = if is_sum_node { 9 } else { 1 };
        // KV holds the state of a node
        // 32 bytes to encode the hash of the node
        // 32 bytes to encode the value hash
        // max_element_size to encode the worst case value size
        HASH_LENGTH_U32 + HASH_LENGTH_U32 + element_size + sum_node_feature_size
    }
}

/// Add cost case for insertion into merk

pub fn add_cost_case_merk_insert(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
    in_tree_using_sums: bool,
) {
    cost.seek_count += 1;
    cost.storage_cost.added_bytes += KV::node_byte_cost_size_for_key_and_raw_value_lengths(
        key_len,
        value_len,
        in_tree_using_sums,
    );
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

pub fn add_cost_case_merk_insert_layered(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
    in_tree_using_sums: bool,
) {
    cost.seek_count += 1;
    cost.storage_cost.added_bytes += KV::layered_node_byte_cost_size_for_key_and_value_lengths(
        key_len,
        value_len,
        in_tree_using_sums,
    );
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

/// Add cost case for insertion into merk
pub fn add_cost_case_merk_replace(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
    in_tree_using_sums: bool,
) {
    cost.seek_count += 1;
    cost.storage_cost.added_bytes +=
        KV::node_value_byte_cost_size(key_len, value_len, in_tree_using_sums);
    cost.storage_cost.replaced_bytes += KV::node_key_byte_cost_size(key_len);
    // .. and hash computation for the inserted element itself
    // first lets add the value hash
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32) as u16;
    // then let's add the kv_digest_to_kv_hash hash call
    let hashed_size = key_len.encode_var_vec().len() as u32 + key_len + HASH_LENGTH_U32;
    cost.hash_node_calls += 1 + ((hashed_size - 1) / HASH_BLOCK_SIZE_U32) as u16;
    // then let's add the two block hashes for the node hash call
    cost.hash_node_calls += 2;
}

/// Add cost case for replacement in merk when the value size is known to not
/// change
pub fn add_cost_case_merk_replace_same_size(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
    in_tree_using_sums: bool,
) {
    cost.seek_count += 1;
    cost.storage_cost.replaced_bytes += KV::node_byte_cost_size_for_key_and_raw_value_lengths(
        key_len,
        value_len,
        in_tree_using_sums,
    );
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
pub fn add_cost_case_merk_replace_layered(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
    in_tree_using_sums: bool,
) {
    cost.seek_count += 1;
    cost.storage_cost.replaced_bytes += KV::layered_node_byte_cost_size_for_key_and_value_lengths(
        key_len,
        value_len,
        in_tree_using_sums,
    );
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

/// Add cost case for replacement in merk when the value size is known to not
/// change
pub fn add_cost_case_merk_patch(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
    change_in_bytes: i32,
    in_tree_using_sums: bool,
) {
    cost.seek_count += 1;
    if change_in_bytes >= 0 {
        cost.storage_cost.replaced_bytes += KV::node_byte_cost_size_for_key_and_raw_value_lengths(
            key_len,
            value_len - change_in_bytes as u32,
            in_tree_using_sums,
        );

        cost.storage_cost.added_bytes += change_in_bytes as u32
    } else {
        cost.storage_cost.replaced_bytes += KV::node_byte_cost_size_for_key_and_raw_value_lengths(
            key_len,
            value_len,
            in_tree_using_sums,
        );
    }

    // .. and hash computation for the inserted element itself
    // first lets add the value hash
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32) as u16;
    // then let's add the kv_digest_to_kv_hash hash call
    let hashed_size = key_len.encode_var_vec().len() as u32 + key_len + HASH_LENGTH_U32;
    cost.hash_node_calls += 1 + ((hashed_size - 1) / HASH_BLOCK_SIZE_U32) as u16;
    // then let's add the two block hashes for the node hash call
    cost.hash_node_calls += 2;
}
