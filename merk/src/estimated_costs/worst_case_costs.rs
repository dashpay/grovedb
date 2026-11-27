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

//! Worst case costs for Merk

#[cfg(feature = "minimal")]
mod add_worst_case_merk_finalize;
#[cfg(feature = "minimal")]
mod add_worst_case_merk_propagate;

use std::cmp::Ordering;

#[cfg(feature = "minimal")]
pub use add_worst_case_merk_finalize::add_worst_case_merk_finalize;
#[cfg(feature = "minimal")]
pub use add_worst_case_merk_propagate::add_worst_case_merk_propagate_versioned;

#[cfg(feature = "minimal")]
use grovedb_costs::{CostResult, CostsExt, OperationCost};

#[cfg(feature = "minimal")]
use crate::merk::NodeType;
#[cfg(feature = "minimal")]
use crate::{
    error::Error,
    merk::defaults::MAX_PREFIXED_KEY_SIZE,
    tree::{kv::KV, Link, TreeNode},
    HASH_BLOCK_SIZE, HASH_BLOCK_SIZE_U32, HASH_LENGTH,
};

#[cfg(feature = "minimal")]
#[derive(Clone, PartialEq, Eq, Debug)]
/// Worst case layer info
pub enum WorstCaseLayerInformation {
    /// Max elements number
    MaxElementsNumber(u32),
    /// Number of levels
    NumberOfLevels(u32),
}

#[cfg(feature = "minimal")]
impl TreeNode {
    /// Return worst case size of encoded tree
    pub fn worst_case_encoded_tree_size(
        not_prefixed_key_len: u32,
        max_element_size: u32,
        node_type: NodeType,
    ) -> u32 {
        // two option values for the left and right link
        // the actual left and right link encoding size
        // the encoded kv node size
        2 + (2 * Link::encoded_link_size(not_prefixed_key_len, node_type))
            + KV::encoded_kv_node_size(max_element_size, node_type)
    }
}

#[cfg(feature = "minimal")]
/// Add worst case for getting a merk node
pub fn add_worst_case_get_merk_node(
    cost: &mut OperationCost,
    not_prefixed_key_len: u32,
    max_element_size: u32,
    node_type: NodeType,
) -> Result<(), Error> {
    // Worst case scenario, the element is not already in memory.
    // One direct seek has to be performed to read the node from storage.
    cost.seek_count += 1;

    // To write a node to disk, the left link, right link and kv nodes are encoded.
    // worst case, the node has both the left and right link present.
    cost.storage_loaded_bytes +=
        TreeNode::worst_case_encoded_tree_size(not_prefixed_key_len, max_element_size, node_type)
            as u64;
    Ok(())
}

#[cfg(feature = "minimal")]
/// Add worst case for getting a merk tree
pub fn add_worst_case_merk_has_value(
    cost: &mut OperationCost,
    not_prefixed_key_len: u32,
    max_element_size: u32,
) {
    cost.seek_count += 1;
    cost.storage_loaded_bytes += not_prefixed_key_len as u64 + max_element_size as u64;
}

#[cfg(feature = "minimal")]
/// Add worst case for insertion into merk
pub fn add_worst_case_merk_insert(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
    node_type: NodeType,
) {
    cost.storage_cost.added_bytes +=
        KV::node_byte_cost_size_for_key_and_raw_value_lengths(key_len, value_len, node_type);
    // .. and hash computation for the inserted element itself
    // todo: verify this
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32);
}

#[cfg(feature = "minimal")]
/// Add worst case for insertion into merk
pub fn add_worst_case_merk_replace_layered(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
    node_type: NodeType,
) {
    // todo: verify this
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32);
    cost.storage_cost.replaced_bytes =
        KV::layered_value_byte_cost_size_for_key_and_value_lengths(key_len, value_len, node_type);
    // 37 + 35 + key_len
}

#[cfg(feature = "minimal")]
/// Add average case for deletion from merk
pub fn add_worst_case_merk_delete_layered(cost: &mut OperationCost, _key_len: u32, value_len: u32) {
    // todo: verify this
    cost.seek_count += 1;
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32);
}

#[cfg(feature = "minimal")]
/// Add average case for deletion from merk
pub fn add_worst_case_merk_delete(cost: &mut OperationCost, _key_len: u32, value_len: u32) {
    // todo: verify this
    cost.seek_count += 1;
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32);
}

#[cfg(feature = "minimal")]
const fn node_hash_update_count() -> u32 {
    // It's a hash of node hash, left and right
    let bytes = HASH_LENGTH * 3;
    // todo: verify this

    1 + ((bytes - 1) / HASH_BLOCK_SIZE) as u32
}

#[cfg(feature = "minimal")]
/// Add worst case for getting a merk tree root hash
pub fn add_worst_case_merk_root_hash(cost: &mut OperationCost) {
    cost.hash_node_calls += node_hash_update_count();
}

#[cfg(feature = "minimal")]
/// Merk biggest value size
pub const MERK_BIGGEST_VALUE_SIZE: u32 = u16::MAX as u32;
#[cfg(feature = "minimal")]
/// Merk biggest key size
pub const MERK_BIGGEST_KEY_SIZE: u32 = 256;
#[cfg(feature = "minimal")]
/// The node type whose feature and whose link cost the most bytes: a count
/// and a sum of 128 bits.
pub const MERK_BIGGEST_NODE_TYPE: NodeType = NodeType::ProvableCountProvableBigSumNode;

#[cfg(feature = "minimal")]
/// Worst case cost of a merk propagation
pub fn worst_case_merk_propagate(input: &WorstCaseLayerInformation) -> CostResult<(), Error> {
    let mut cost = OperationCost::default();
    add_worst_case_merk_propagate(&mut cost, input).wrap_with_cost(cost)
}

#[cfg(feature = "minimal")]
/// Add worst case cost of a merk propagation
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
    cost.storage_loaded_bytes +=
        nodes_updated as u64 * (MERK_BIGGEST_VALUE_SIZE + MERK_BIGGEST_KEY_SIZE) as u64;
    cost.seek_count += nodes_updated;
    cost.hash_node_calls += nodes_updated * 2;
    Ok(())
}

#[cfg(feature = "minimal")]
/// Add worst case cost for is_empty_tree_except
pub fn add_worst_case_cost_for_is_empty_tree_except(
    cost: &mut OperationCost,
    except_keys_count: u16,
) {
    cost.seek_count += except_keys_count as u32 + 1;
    cost.storage_loaded_bytes += MAX_PREFIXED_KEY_SIZE * (except_keys_count as u64 + 1);
}

/// Add average case cost for is_empty_tree_except
#[cfg(feature = "minimal")]
pub fn add_average_case_cost_for_is_empty_tree_except(
    cost: &mut OperationCost,
    except_keys_count: u16,
    estimated_prefixed_key_size: u32,
) {
    cost.seek_count += except_keys_count as u32 + 1;
    cost.storage_loaded_bytes +=
        estimated_prefixed_key_size as u64 * (except_keys_count as u64 + 1);
}

#[cfg(test)]
mod tests {
    use grovedb_costs::OperationCost;
    use grovedb_version::version::v6::GROVE_V6;

    use super::*;

    #[test]
    fn test_add_worst_case_merk_propagate_level_branches() {
        let mut two_levels = OperationCost::default();
        add_worst_case_merk_propagate(
            &mut two_levels,
            &WorstCaseLayerInformation::NumberOfLevels(2),
        )
        .unwrap();

        let mut three_levels = OperationCost::default();
        add_worst_case_merk_propagate(
            &mut three_levels,
            &WorstCaseLayerInformation::NumberOfLevels(3),
        )
        .unwrap();

        assert_eq!(two_levels.seek_count, 3);
        assert_eq!(three_levels.seek_count, 5);
        assert!(three_levels.storage_cost.replaced_bytes > two_levels.storage_cost.replaced_bytes);
    }

    #[test]
    fn test_add_worst_case_merk_propagate_max_elements_u32_max() {
        let mut cost = OperationCost::default();
        add_worst_case_merk_propagate(
            &mut cost,
            &WorstCaseLayerInformation::MaxElementsNumber(u32::MAX),
        )
        .unwrap();

        assert_eq!(cost.seek_count, 34);
        assert_eq!(cost.hash_node_calls, 68);
    }

    #[test]
    fn test_is_empty_tree_except_cost_helpers() {
        let mut worst_cost = OperationCost::default();
        add_worst_case_cost_for_is_empty_tree_except(&mut worst_cost, 2);
        assert_eq!(worst_cost.seek_count, 3);
        assert!(worst_cost.storage_loaded_bytes > 0);

        let mut average_cost = OperationCost::default();
        add_average_case_cost_for_is_empty_tree_except(&mut average_cost, 2, 11);
        assert_eq!(average_cost.seek_count, 3);
        assert_eq!(average_cost.storage_loaded_bytes, 33);
    }

    #[test]
    fn es2_a_worst_case_level_is_what_it_was_on_every_grove_version_in_force() {
        for version in grovedb_version::version::GROVE_VERSIONS {
            for input in [
                WorstCaseLayerInformation::MaxElementsNumber(0),
                WorstCaseLayerInformation::MaxElementsNumber(30),
                WorstCaseLayerInformation::MaxElementsNumber(u32::MAX),
                WorstCaseLayerInformation::NumberOfLevels(2),
                WorstCaseLayerInformation::NumberOfLevels(9),
            ] {
                let mut versioned = OperationCost::default();
                add_worst_case_merk_propagate_versioned(&mut versioned, &input, version).unwrap();
                let mut released = OperationCost::default();
                add_worst_case_merk_propagate(&mut released, &input).unwrap();
                assert_eq!(versioned, released, "{input:?}");
            }
        }
    }

    #[test]
    fn es2_a_worst_case_level_has_the_most_levels_and_two_seeks_a_node_on_grove_version_6() {
        let worst = |input| {
            let mut cost = OperationCost::default();
            add_worst_case_merk_propagate_versioned(&mut cost, &input, &GROVE_V6).unwrap();
            cost
        };
        let node = |nodes: u32| OperationCost {
            seek_count: 2 * nodes,
            storage_cost: grovedb_costs::storage_cost::StorageCost {
                replaced_bytes: nodes * MERK_BIGGEST_VALUE_SIZE,
                ..Default::default()
            },
            storage_loaded_bytes: u64::from(nodes)
                * u64::from(MERK_BIGGEST_VALUE_SIZE + MERK_BIGGEST_KEY_SIZE),
            hash_node_calls: 2 * nodes,
            ..Default::default()
        };
        // For each level the node on the way and four beside it that a
        // double rotation at that level changes, and one for the other
        // child of a removed node. A Merk with no node has none.
        assert_eq!(worst(WorstCaseLayerInformation::NumberOfLevels(0)), node(0));
        assert_eq!(worst(WorstCaseLayerInformation::NumberOfLevels(1)), node(6));
        assert_eq!(
            worst(WorstCaseLayerInformation::NumberOfLevels(2)),
            node(11)
        );
        assert_eq!(
            worst(WorstCaseLayerInformation::NumberOfLevels(9)),
            node(46)
        );
        // Thirty elements fit in five levels at the fewest and take six at
        // the most.
        assert_eq!(
            worst(WorstCaseLayerInformation::MaxElementsNumber(30)),
            node(5 * 6 + 1)
        );
        // A height no Merk can have.
        for unreal in [92, u32::MAX] {
            assert!(matches!(
                add_worst_case_merk_propagate_versioned(
                    &mut OperationCost::default(),
                    &WorstCaseLayerInformation::NumberOfLevels(unreal),
                    &GROVE_V6,
                ),
                Err(Error::Overflow(_))
            ));
        }
        let mut released = OperationCost::default();
        add_worst_case_merk_propagate(
            &mut released,
            &WorstCaseLayerInformation::MaxElementsNumber(30),
        )
        .unwrap();
        assert_eq!(released.seek_count, 5 + 2);

        let mut unknown = GROVE_V6.clone();
        unknown
            .merk_versions
            .worst_case_costs
            .add_worst_case_merk_propagate = 2;
        assert!(matches!(
            add_worst_case_merk_propagate_versioned(
                &mut OperationCost::default(),
                &WorstCaseLayerInformation::NumberOfLevels(1),
                &unknown,
            ),
            Err(Error::VersionError(_))
        ));
    }

    #[test]
    fn es3_a_worst_case_finalize_in_one_merk_bounds_every_changed_node() {
        let worst = |input, marked, biggest_value| {
            let mut cost = OperationCost::default();
            add_worst_case_merk_finalize(&mut cost, &input, marked, biggest_value, &GROVE_V6)
                .unwrap();
            cost
        };
        // A node whose value is 100 bytes, of the kind that costs the most.
        // What it replaces: the value with its two hashes and a feature of
        // 25 bytes, 189, the two bytes of that length, and a link of the
        // key's 256 bytes, a hash, four and 24 for the aggregates. What a
        // read of it loads: the value, two hashes, a feature of a tag, a
        // count of ten bytes and a sum of 16, and two links, each a byte
        // that says it is there, the length of the key, its 255 bytes, a
        // hash, two heights and aggregates as long as the feature.
        let replaced = 100 + 64 + 25 + 2 + (256 + 32 + 4 + 24);
        let record = 100 + 64 + 27 + 2 * (1 + 1 + 255 + 32 + 2 + 27);
        // Four levels and three marked elements: six nodes above them, nine
        // changed. Each is fetched and put, and each marked one is read by
        // its key as well.
        let several = worst(WorstCaseLayerInformation::NumberOfLevels(4), 3, 100);
        assert_eq!(several.seek_count, 9 + 9 + 3);
        assert_eq!(several.storage_loaded_bytes, (9 + 3) * record);
        assert_eq!(several.storage_cost.replaced_bytes, 9 * replaced);
        assert_eq!(several.storage_cost.added_bytes, 0);
        assert!(!several.storage_cost.removed_bytes.has_removal());
        assert_eq!(several.hash_node_calls, 2 * 9);
        // Seven elements fit in three levels and take four at the most.
        assert_eq!(
            worst(WorstCaseLayerInformation::MaxElementsNumber(7), 3, 100),
            several
        );
        assert_eq!(
            worst(WorstCaseLayerInformation::NumberOfLevels(4), 0, 100),
            OperationCost::default()
        );
        // A byte more of value is a byte more of every read and every put,
        // at any length: the bound follows what its caller gives, past the
        // 65,535 bytes the worst case of a level takes a value to be.
        for value in [0, 100, MERK_BIGGEST_VALUE_SIZE, 1_000_000] {
            let (at, longer) = (
                worst(WorstCaseLayerInformation::NumberOfLevels(1), 1, value),
                worst(WorstCaseLayerInformation::NumberOfLevels(1), 1, value + 1),
            );
            assert!(at.storage_cost.replaced_bytes >= value);
            assert!(longer.storage_cost.replaced_bytes > at.storage_cost.replaced_bytes);
            assert_eq!(longer.storage_loaded_bytes, at.storage_loaded_bytes + 2);
        }
        for version in grovedb_version::version::GROVE_VERSIONS {
            assert!(matches!(
                add_worst_case_merk_finalize(
                    &mut OperationCost::default(),
                    &WorstCaseLayerInformation::NumberOfLevels(4),
                    3,
                    100,
                    version
                ),
                Err(Error::NotSupported(_))
            ));
        }
    }

    #[test]
    fn es3_a_worst_case_finalize_bounds_a_node_of_every_kind_at_the_value_it_is_given() {
        use crate::tree::kv::KV;
        // The most bytes a node replaces, for a value of the length the
        // estimate is given: no node of that value replaces more, as an
        // item's node or as a tree's, under any key and of any node type.
        let node_types = [
            NodeType::NormalNode,
            NodeType::SumNode,
            NodeType::BigSumNode,
            NodeType::CountNode,
            NodeType::CountSumNode,
            NodeType::ProvableCountNode,
            NodeType::ProvableCountSumNode,
            NodeType::ProvableSumNode,
            NodeType::ProvableCountProvableSumNode,
            NodeType::ProvableCountProvableBigSumNode,
            NodeType::CountBigSumNode,
            NodeType::ProvableBigSumNode,
        ];
        for node_type in node_types {
            // Every node type is listed above: one that Merk gains does not
            // compile here until it is.
            match node_type {
                NodeType::NormalNode
                | NodeType::SumNode
                | NodeType::BigSumNode
                | NodeType::CountNode
                | NodeType::CountSumNode
                | NodeType::ProvableCountNode
                | NodeType::ProvableCountSumNode
                | NodeType::ProvableSumNode
                | NodeType::ProvableCountProvableSumNode
                | NodeType::ProvableCountProvableBigSumNode
                | NodeType::CountBigSumNode
                | NodeType::ProvableBigSumNode => {}
            }
            assert!(node_type.feature_len() <= MERK_BIGGEST_NODE_TYPE.feature_len());
            assert!(node_type.cost() <= MERK_BIGGEST_NODE_TYPE.cost());
            for value in [0u32, 1, 63, 100, 65_535, 65_536, 1_000_000] {
                let mut cost = OperationCost::default();
                add_worst_case_merk_finalize(
                    &mut cost,
                    &WorstCaseLayerInformation::NumberOfLevels(1),
                    1,
                    value,
                    &GROVE_V6,
                )
                .unwrap();
                for key in [0u32, 1, 32, 255] {
                    let as_an_item = KV::value_byte_cost_size_for_key_and_raw_value_lengths(
                        key, value, node_type,
                    );
                    let as_a_tree = KV::layered_value_byte_cost_size_for_key_and_value_lengths(
                        key, value, node_type,
                    );
                    assert!(cost.storage_cost.replaced_bytes >= as_an_item);
                    assert!(cost.storage_cost.replaced_bytes >= as_a_tree);
                }
            }
        }
        // A value too long for a node to be sized by it is refused, and the
        // cost is as it was given.
        let before = OperationCost {
            seek_count: 5,
            ..Default::default()
        };
        for too_long in [u32::MAX - 65_535, u32::MAX] {
            let mut cost = before.clone();
            assert!(matches!(
                add_worst_case_merk_finalize(
                    &mut cost,
                    &WorstCaseLayerInformation::NumberOfLevels(1),
                    1,
                    too_long,
                    &GROVE_V6,
                ),
                Err(Error::Overflow(_))
            ));
            assert_eq!(cost, before);
        }
        let mut cost = before.clone();
        add_worst_case_merk_finalize(
            &mut cost,
            &WorstCaseLayerInformation::NumberOfLevels(1),
            1,
            u32::MAX - 65_536,
            &GROVE_V6,
        )
        .expect("the longest value a node is sized by");
    }

    #[test]
    fn test_add_worst_case_merk_root_hash() {
        let mut cost = OperationCost::default();
        add_worst_case_merk_root_hash(&mut cost);
        assert!(cost.hash_node_calls > 0);
    }
}
