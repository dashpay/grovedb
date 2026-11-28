//! Estimated costs for Merk

#[cfg(feature = "minimal")]
use grovedb_costs::OperationCost;
#[cfg(feature = "minimal")]
use integer_encoding::VarInt;

#[cfg(feature = "minimal")]
use crate::merk::NodeType;
#[cfg(feature = "minimal")]
use crate::tree_type::TreeType;
#[cfg(feature = "minimal")]
use crate::{tree::kv::KV, HASH_BLOCK_SIZE_U32, HASH_LENGTH_U32};

#[cfg(feature = "minimal")]
pub mod average_case_costs;

#[cfg(feature = "minimal")]
pub mod worst_case_costs;

#[cfg(feature = "minimal")]
/// The cost of a subtree layer
/// It is 3 because we have:
/// 1 byte for the element type
/// 1 byte for the root key option
/// 1 byte for the flag option
pub const LAYER_COST_SIZE: u32 = 3;

#[cfg(any(feature = "minimal", feature = "verify"))]
/// The cost of a sum value
pub const SUM_VALUE_EXTRA_COST: u32 = 9;

#[cfg(any(feature = "minimal", feature = "verify"))]
/// The cost of a count value
pub const COUNT_VALUE_EXTRA_COST: u32 = 9;

#[cfg(any(feature = "minimal", feature = "verify"))]
/// The cost of a big sum value
pub const BIG_SUM_VALUE_EXTRA_COST: u32 = 16;

#[cfg(feature = "minimal")]
/// The cost of a summed subtree layer
/// This is the layer size + 9 for the encoded value
pub const SUM_LAYER_COST_SIZE: u32 = LAYER_COST_SIZE + SUM_VALUE_EXTRA_COST;

#[cfg(feature = "minimal")]
/// The cost of a summed subtree layer
/// This is the layer size + 9 for the encoded value
pub const SUM_AND_COUNT_LAYER_COST_SIZE: u32 =
    LAYER_COST_SIZE + SUM_VALUE_EXTRA_COST + COUNT_VALUE_EXTRA_COST;

#[cfg(feature = "minimal")]
/// The cost of a summed subtree layer
/// This is the layer size + 16 for the encoded value
pub const BIG_SUM_LAYER_COST_SIZE: u32 = LAYER_COST_SIZE + BIG_SUM_VALUE_EXTRA_COST;

#[cfg(feature = "minimal")]
/// The cost of a counted and big summed subtree layer
/// This is the layer size + 9 for the encoded count + 16 for the encoded sum
pub const BIG_SUM_AND_COUNT_LAYER_COST_SIZE: u32 =
    LAYER_COST_SIZE + BIG_SUM_VALUE_EXTRA_COST + COUNT_VALUE_EXTRA_COST;

#[cfg(feature = "minimal")]
/// The most nodes a Merk of `levels` levels has above `marked` of its
/// nodes: every node that is on the way from the root to one of them and
/// is not one of the deepest of them
/// (`docs/lignindb/cost-model.md`, ES3).
///
/// At depth `i` a tree has at most `2^i` nodes, and no more than `marked`
/// of them lead to a marked node. The marked nodes are taken to be at the
/// last level, where their ways are longest, so the levels above are
/// `0..levels - 1`.
pub fn nodes_above_marked(levels: u32, marked: u32) -> u32 {
    let above = levels.saturating_sub(1);
    // The levels that hold fewer nodes than there are marked ones are full:
    // 1, 2, 4, ... nodes. There are at most 32 of them, whatever `levels`
    // is. Every level below those has `marked` nodes on the ways.
    let mut nodes = 0u32;
    let mut depth = 0;
    while depth < above && depth < u32::BITS && (1u32 << depth) < marked {
        nodes = nodes.saturating_add(1 << depth);
        depth += 1;
    }
    nodes.saturating_add((above - depth).saturating_mul(marked))
}

#[cfg(feature = "minimal")]
/// The most levels a Merk can have: an AVL tree of more levels has more
/// nodes than a `u64` counts.
pub const MOST_LEVELS_OF_A_MERK: u32 = 91;

#[cfg(feature = "minimal")]
/// The levels of a Merk as an estimate takes them from its caller, or an
/// error for a height no Merk can have. An estimate that multiplies by a
/// height it did not check overflows on one.
pub fn checked_levels(levels: u32) -> Result<u32, crate::Error> {
    if levels > MOST_LEVELS_OF_A_MERK {
        return Err(crate::Error::Overflow(
            "a Merk is estimated with more levels than a Merk can have",
        ));
    }
    Ok(levels)
}

#[cfg(feature = "minimal")]
/// The most levels an AVL tree of `elements` nodes can have. A tree of `h`
/// levels has at least `fewest(h) = fewest(h - 1) + fewest(h - 2) + 1`
/// nodes.
pub fn most_levels(elements: u32) -> u32 {
    let (mut fewest_below, mut fewest) = (0u64, 1u64);
    let mut levels = 0;
    while fewest <= u64::from(elements) {
        levels += 1;
        (fewest_below, fewest) = (fewest, fewest + fewest_below + 1);
    }
    levels
}

#[cfg(feature = "minimal")]
impl KV {
    fn encoded_kv_node_size(element_size: u32, node_type: NodeType) -> u32 {
        // We always charge 8 bytes for the sum node (even though
        // it could theoretically be 9 bytes
        let sum_node_feature_size = node_type.feature_len();
        // KV holds the state of a node
        // 32 bytes to encode the hash of the node
        // 32 bytes to encode the value hash
        // max_element_size to encode the worst case value size
        HASH_LENGTH_U32 + HASH_LENGTH_U32 + element_size + sum_node_feature_size
    }
}

#[cfg(feature = "minimal")]
/// Add cost case for insertion into merk
pub fn add_cost_case_merk_insert(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
    in_tree_type: TreeType,
) {
    cost.seek_count += 1;
    cost.storage_cost.added_bytes += KV::node_byte_cost_size_for_key_and_raw_value_lengths(
        key_len,
        value_len,
        in_tree_type.inner_node_type(),
    );
    // .. and hash computation for the inserted element itself
    // first lets add the value hash
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32);
    // then let's add the kv_digest_to_kv_hash hash call
    let hashed_size = key_len.encode_var_vec().len() as u32 + key_len + HASH_LENGTH_U32;
    cost.hash_node_calls += 1 + ((hashed_size - 1) / HASH_BLOCK_SIZE_U32);
    // then let's add the two block hashes for the node hash call
    cost.hash_node_calls += 2;
}

#[cfg(feature = "minimal")]
/// Add cost case for insertion into merk
pub fn add_cost_case_merk_insert_layered(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
    in_tree_type: TreeType,
) {
    cost.seek_count += 1;
    cost.storage_cost.added_bytes += KV::layered_node_byte_cost_size_for_key_and_value_lengths(
        key_len,
        value_len,
        in_tree_type.inner_node_type(),
    );
    // .. and hash computation for the inserted element itself
    // first lets add the value hash
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32);
    // then let's add the combine hash
    cost.hash_node_calls += 1;
    // then let's add the kv_digest_to_kv_hash hash call
    let hashed_size = key_len.encode_var_vec().len() as u32 + key_len + HASH_LENGTH_U32;
    cost.hash_node_calls += 1 + ((hashed_size - 1) / HASH_BLOCK_SIZE_U32);
    // then let's add the two block hashes for the node hash call
    cost.hash_node_calls += 2;
}

#[cfg(feature = "minimal")]
/// Add cost case for insertion into merk
pub fn add_cost_case_merk_replace(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
    in_tree_type: TreeType,
) {
    cost.seek_count += 1;
    cost.storage_cost.added_bytes +=
        KV::node_value_byte_cost_size(key_len, value_len, in_tree_type.inner_node_type());
    cost.storage_cost.replaced_bytes += KV::node_key_byte_cost_size(key_len);
    // .. and hash computation for the inserted element itself
    // first lets add the value hash
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32);
    // then let's add the kv_digest_to_kv_hash hash call
    let hashed_size = key_len.encode_var_vec().len() as u32 + key_len + HASH_LENGTH_U32;
    cost.hash_node_calls += 1 + ((hashed_size - 1) / HASH_BLOCK_SIZE_U32);
    // then let's add the two block hashes for the node hash call
    cost.hash_node_calls += 2;
}

#[cfg(feature = "minimal")]
/// Add cost case for replacement in merk when the value size is known to not
/// change
pub fn add_cost_case_merk_replace_same_size(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
    in_tree_type: TreeType,
) {
    cost.seek_count += 1;
    cost.storage_cost.replaced_bytes += KV::node_byte_cost_size_for_key_and_raw_value_lengths(
        key_len,
        value_len,
        in_tree_type.inner_node_type(),
    );
    // .. and hash computation for the inserted element itself
    // first lets add the value hash
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32);
    // then let's add the kv_digest_to_kv_hash hash call
    let hashed_size = key_len.encode_var_vec().len() as u32 + key_len + HASH_LENGTH_U32;
    cost.hash_node_calls += 1 + ((hashed_size - 1) / HASH_BLOCK_SIZE_U32);
    // then let's add the two block hashes for the node hash call
    cost.hash_node_calls += 2;
}

#[cfg(feature = "minimal")]
/// Add cost case for insertion into merk
pub fn add_cost_case_merk_replace_layered(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
    in_tree_type: TreeType,
) {
    cost.seek_count += 1;
    cost.storage_cost.replaced_bytes += KV::layered_node_byte_cost_size_for_key_and_value_lengths(
        key_len,
        value_len,
        in_tree_type.inner_node_type(),
    );
    // .. and hash computation for the inserted element itself
    // first lets add the value hash
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32);
    // then let's add the combine hash
    cost.hash_node_calls += 1;
    // then let's add the kv_digest_to_kv_hash hash call
    let hashed_size = key_len.encode_var_vec().len() as u32 + key_len + HASH_LENGTH_U32;
    cost.hash_node_calls += 1 + ((hashed_size - 1) / HASH_BLOCK_SIZE_U32);
    // then let's add the two block hashes for the node hash call
    cost.hash_node_calls += 2;
}

#[cfg(feature = "minimal")]
/// Add cost case for replacement in merk when the value size is known to not
/// change
pub fn add_cost_case_merk_patch(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
    change_in_bytes: i32,
    in_tree_type: TreeType,
) {
    cost.seek_count += 1;
    if change_in_bytes >= 0 {
        // it's possible that the required space has also changed which would cause a +1
        // to happen
        let old_byte_size = KV::node_byte_cost_size_for_key_and_raw_value_lengths(
            key_len,
            value_len - change_in_bytes as u32,
            in_tree_type.inner_node_type(),
        );
        let new_byte_size = KV::node_byte_cost_size_for_key_and_raw_value_lengths(
            key_len,
            value_len,
            in_tree_type.inner_node_type(),
        );
        cost.storage_cost.replaced_bytes += old_byte_size;

        cost.storage_cost.added_bytes += new_byte_size - old_byte_size;
    } else {
        cost.storage_cost.replaced_bytes += KV::node_byte_cost_size_for_key_and_raw_value_lengths(
            key_len,
            value_len,
            in_tree_type.inner_node_type(),
        );
    }

    // .. and hash computation for the inserted element itself
    // first lets add the value hash
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32);
    // then let's add the kv_digest_to_kv_hash hash call
    let hashed_size = key_len.encode_var_vec().len() as u32 + key_len + HASH_LENGTH_U32;
    cost.hash_node_calls += 1 + ((hashed_size - 1) / HASH_BLOCK_SIZE_U32);
    // then let's add the two block hashes for the node hash call
    cost.hash_node_calls += 2;
}

#[cfg(test)]
mod tests {
    use grovedb_costs::OperationCost;

    use super::*;

    #[test]
    fn es3_the_levels_of_a_worst_case_merk_are_the_most_an_avl_tree_can_have() {
        // The fewest nodes of a tree of 1, 2, 3, ... levels: 1, 2, 4, 7, 12.
        let expected = [
            (0, 0),
            (1, 1),
            (2, 2),
            (3, 2),
            (4, 3),
            (6, 3),
            (7, 4),
            (11, 4),
            (12, 5),
        ];
        for (elements, levels) in expected {
            assert_eq!(most_levels(elements), levels, "{elements} elements");
        }
        assert_eq!(most_levels(u32::MAX), 45);
    }

    #[test]
    fn es2_a_merk_of_more_levels_than_a_merk_can_have_is_refused() {
        // The fewest nodes of an AVL tree of 1, 2, 3, ... levels.
        let (mut fewest_below, mut fewest) = (0u128, 1u128);
        let mut levels = 1;
        while fewest <= u128::from(u64::MAX) {
            levels += 1;
            (fewest_below, fewest) = (fewest, fewest + fewest_below + 1);
        }
        // `levels` is the first height that needs more nodes than a `u64`
        // counts.
        assert_eq!(levels, MOST_LEVELS_OF_A_MERK + 1);
        assert_eq!(checked_levels(0).unwrap(), 0);
        assert_eq!(
            checked_levels(MOST_LEVELS_OF_A_MERK).unwrap(),
            MOST_LEVELS_OF_A_MERK
        );
        for unreal in [MOST_LEVELS_OF_A_MERK + 1, 1 << 20, u32::MAX] {
            assert!(matches!(
                checked_levels(unreal),
                Err(crate::Error::Overflow(_))
            ));
        }
        assert!(most_levels(u32::MAX) <= MOST_LEVELS_OF_A_MERK);
    }

    #[test]
    fn es3_the_nodes_above_marked_nodes_are_those_of_a_full_tree() {
        // A tree of one level has nothing above its root.
        assert_eq!(nodes_above_marked(0, 3), 0);
        assert_eq!(nodes_above_marked(1, 3), 0);
        // One marked node: one node at each level above the last.
        assert_eq!(nodes_above_marked(4, 1), 3);
        // Three marked nodes in four levels: the root, its two children,
        // and three of the four nodes below them.
        assert_eq!(nodes_above_marked(4, 3), 1 + 2 + 3);
        // No more than the tree has above its last level.
        assert_eq!(nodes_above_marked(4, 100), 1 + 2 + 4);
        assert_eq!(nodes_above_marked(4, 0), 0);
        // A level that holds more nodes than a count can name.
        assert_eq!(nodes_above_marked(40, 5), 1 + 2 + 4 + 5 * 36);
        assert_eq!(nodes_above_marked(u32::MAX, u32::MAX), u32::MAX);
    }

    #[test]
    fn test_add_cost_case_merk_insert_increments_cost_fields() {
        let mut cost = OperationCost::default();
        add_cost_case_merk_insert(&mut cost, 8, 64, TreeType::NormalTree);

        assert_eq!(cost.seek_count, 1);
        assert!(cost.storage_cost.added_bytes > 0);
        assert!(cost.hash_node_calls >= 4);
    }

    #[test]
    fn test_add_cost_case_merk_insert_layered_includes_combine_hash() {
        let mut cost_plain = OperationCost::default();
        add_cost_case_merk_insert(&mut cost_plain, 8, 64, TreeType::NormalTree);

        let mut cost_layered = OperationCost::default();
        add_cost_case_merk_insert_layered(&mut cost_layered, 8, 64, TreeType::NormalTree);

        assert_eq!(cost_layered.seek_count, 1);
        assert_eq!(cost_layered.hash_node_calls, cost_plain.hash_node_calls + 1);
    }

    #[test]
    fn test_add_cost_case_merk_patch_positive_change_tracks_added_bytes() {
        let mut cost = OperationCost::default();
        add_cost_case_merk_patch(&mut cost, 8, 70, 6, TreeType::NormalTree);

        assert_eq!(cost.seek_count, 1);
        assert!(cost.storage_cost.replaced_bytes > 0);
        assert!(cost.storage_cost.added_bytes > 0);
    }

    #[test]
    fn test_add_cost_case_merk_patch_negative_change_only_replaces() {
        let mut cost = OperationCost::default();
        add_cost_case_merk_patch(&mut cost, 8, 64, -3, TreeType::NormalTree);

        assert_eq!(cost.seek_count, 1);
        assert!(cost.storage_cost.replaced_bytes > 0);
        assert_eq!(cost.storage_cost.added_bytes, 0);
    }
}
