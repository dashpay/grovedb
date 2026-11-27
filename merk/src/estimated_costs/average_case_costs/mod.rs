//! Average case costs for Merk

// One submodule per versioned function, each laid out as
// `<function>/{mod.rs, v0.rs, v1.rs, ...}` — `mod.rs` holds the
// version-dispatching wrapper and the `v*.rs` files hold the
// per-version implementations.
mod add_average_case_merk_finalize;
mod add_average_case_merk_propagate;
mod estimated_sum_trees_size;
mod subtree_with_feature_and_flags_size;
mod value_with_feature_and_flags_size;

pub use add_average_case_merk_finalize::add_average_case_merk_finalize;
pub use add_average_case_merk_propagate::{
    add_average_case_merk_propagate, average_case_merk_propagate,
};

use grovedb_costs::OperationCost;
use integer_encoding::VarInt;

use crate::{
    error::Error,
    tree::{kv::KV, Link, TreeNode},
    HASH_BLOCK_SIZE, HASH_BLOCK_SIZE_U32, HASH_LENGTH, HASH_LENGTH_U32,
};
use crate::{merk::NodeType, tree_type::TreeType};

/// Average key size
pub type AverageKeySize = u8;
/// Average value size
pub type AverageValueSize = u32;
/// Average flags size
pub type AverageFlagsSize = u32;
/// Weight
pub type Weight = u8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
/// Estimated number of sum trees
#[derive(Default)]
pub enum EstimatedSumTrees {
    /// No sum trees
    #[default]
    NoSumTrees,
    /// Some sum trees
    SomeSumTrees {
        /// Sum trees weight
        sum_trees_weight: Weight,
        /// Big Sum trees weight
        big_sum_trees_weight: Weight,
        /// Count trees weight
        count_trees_weight: Weight,
        /// Count Sum trees weight
        count_sum_trees_weight: Weight,
        /// Non sum trees weight
        non_sum_trees_weight: Weight,
        /// `ProvableSumTree` leaves. `ProvableSumNode` carries an i64 sum
        /// that is baked into every node's hash; cost matches
        /// `SumTree`'s `inner_node_type().cost()` (8 bytes).
        provable_sum_trees_weight: Weight,
        /// `ProvableCountTree` leaves. Same per-node cost as `CountTree`
        /// (8 bytes), but the count is hash-committed.
        provable_count_trees_weight: Weight,
        /// `ProvableCountSumTree` leaves. Same per-node cost as
        /// `CountSumTree` (16 bytes), but the count is hash-committed.
        provable_count_sum_trees_weight: Weight,
        /// `ProvableCountProvableSumTree` (PCPS) leaves — the v12 dual-axis
        /// per-node variant. Same per-node cost as `CountSumTree`
        /// (16 bytes), but BOTH the count and the sum are
        /// hash-committed.
        provable_count_provable_sum_trees_weight: Weight,
        /// `ProvableCountProvableBigSumTree` leaves. Their nodes carry a
        /// count and a 16-byte sum (24 bytes), both hash-committed. Read by
        /// the v2 formula only: v0 and v1 predate the tree and keep their
        /// historical results.
        provable_count_provable_big_sum_trees_weight: Weight,
        /// `CountBigSumTree` leaves. Their nodes carry a count and a 16-byte
        /// sum (24 bytes), neither hash-committed. Read by the v2 formula
        /// only, like the weight above.
        count_big_sum_trees_weight: Weight,
        /// `ProvableBigSumTree` leaves. Their nodes carry a 16-byte sum (16
        /// bytes), hash-committed. Read by the v2 formula only, like the
        /// two weights above.
        provable_big_sum_trees_weight: Weight,
    },
    /// All sum trees
    AllSumTrees,
    /// All big sum trees
    AllBigSumTrees,
    /// All count trees
    AllCountTrees,
    /// All count sum trees
    AllCountSumTrees,
    /// All `ProvableSumTree` leaves.
    AllProvableSumTrees,
    /// All `ProvableCountTree` leaves.
    AllProvableCountTrees,
    /// All `ProvableCountSumTree` leaves.
    AllProvableCountSumTrees,
    /// All `ProvableCountProvableSumTree` leaves.
    AllProvableCountProvableSumTrees,
    /// All `ProvableCountProvableBigSumTree` leaves.
    AllProvableCountProvableBigSumTrees,
    /// All `CountBigSumTree` leaves.
    AllCountBigSumTrees,
    /// All `ProvableBigSumTree` leaves.
    AllProvableBigSumTrees,
}

// `EstimatedSumTrees::estimated_size` and its v0/v1/v2 implementations
// live in the `estimated_sum_trees_size` submodule (one v*.rs per
// version).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
/// Estimated layer sizes
pub enum EstimatedLayerSizes {
    /// All subtrees
    AllSubtrees(AverageKeySize, EstimatedSumTrees, Option<AverageFlagsSize>),
    /// All items
    AllItems(AverageKeySize, AverageValueSize, Option<AverageFlagsSize>),
    /// References
    AllReference(AverageKeySize, AverageValueSize, Option<AverageFlagsSize>),
    /// Layer where every leaf is `Element::ItemWithSumItem`. Sizing adds
    /// the i64 sum_value's worst-case varint encoding (`+10`) on top of
    /// the plain-item layout — same constant the per-element helper
    /// `Element::required_item_with_sum_item_space` uses.
    AllItemsWithSumItem(AverageKeySize, AverageValueSize, Option<AverageFlagsSize>),
    /// Layer where every leaf is `Element::ReferenceWithSumItem`. Sizing
    /// adds the same `+10` worst-case sum_value adjustment on top of the
    /// plain-reference layout.
    AllReferencesWithSumItem(AverageKeySize, AverageValueSize, Option<AverageFlagsSize>),
    /// Mix
    Mix {
        /// Subtrees size
        subtrees_size: Option<(
            AverageKeySize,
            EstimatedSumTrees,
            Option<AverageFlagsSize>,
            Weight,
        )>,
        /// Items size
        items_size: Option<(
            AverageKeySize,
            AverageValueSize,
            Option<AverageFlagsSize>,
            Weight,
        )>,
        /// References size
        references_size: Option<(
            AverageKeySize,
            AverageValueSize,
            Option<AverageFlagsSize>,
            Weight,
        )>,
        /// Weight of `ItemWithSumItem` leaves in the mix. Same sizing
        /// formula as `items_size` plus `+10` for the worst-case i64
        /// sum_value varint.
        items_with_sum_item_size: Option<(
            AverageKeySize,
            AverageValueSize,
            Option<AverageFlagsSize>,
            Weight,
        )>,
        /// Weight of `ReferenceWithSumItem` leaves in the mix. Same
        /// sizing formula as `references_size` plus `+10`.
        references_with_sum_item_size: Option<(
            AverageKeySize,
            AverageValueSize,
            Option<AverageFlagsSize>,
            Weight,
        )>,
    },
}

impl EstimatedLayerSizes {
    /// Return average flags size for layer
    pub fn layered_flags_size(&self) -> Result<&Option<AverageFlagsSize>, Error> {
        match self {
            EstimatedLayerSizes::AllSubtrees(_, _, flags_size) => Ok(flags_size),
            EstimatedLayerSizes::Mix {
                subtrees_size: subtree_size,
                ..
            } => {
                if let Some((_, _, flags_size, _)) = subtree_size {
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

    /// Refuses sizes that an estimate cannot count: the value and the flags
    /// of one element together within 65,536 bytes of what 32 bits hold.
    /// An estimate adds the key, the hashes and the lengths of a node to
    /// them, well under that many bytes, in 32 bits.
    pub fn check_sizes(&self) -> Result<(), Error> {
        const MOST: u64 = u32::MAX as u64 - 65_536;
        let fits = |value: u32, flags: &Option<AverageFlagsSize>| {
            u64::from(value) + u64::from(flags.unwrap_or(0)) <= MOST
        };
        let all_fit = match self {
            EstimatedLayerSizes::AllSubtrees(_, _, flags) => fits(0, flags),
            EstimatedLayerSizes::AllItems(_, value, flags)
            | EstimatedLayerSizes::AllReference(_, value, flags)
            | EstimatedLayerSizes::AllItemsWithSumItem(_, value, flags)
            | EstimatedLayerSizes::AllReferencesWithSumItem(_, value, flags) => fits(*value, flags),
            EstimatedLayerSizes::Mix {
                subtrees_size,
                items_size,
                references_size,
                items_with_sum_item_size,
                references_with_sum_item_size,
            } => {
                subtrees_size
                    .as_ref()
                    .is_none_or(|(_, _, flags, _)| fits(0, flags))
                    && [
                        items_size,
                        references_size,
                        items_with_sum_item_size,
                        references_with_sum_item_size,
                    ]
                    .into_iter()
                    .flatten()
                    .all(|(_, value, flags, _)| fits(*value, flags))
            }
        };
        if all_fit {
            Ok(())
        } else {
            Err(Error::Overflow(
                "a layer is estimated with elements longer than an estimate can count",
            ))
        }
    }

    // `subtree_with_feature_and_flags_size` is dispatched by version in the
    // submodule of that name.

    // `value_with_feature_and_flags_size` and its version-dispatched
    // implementations now live in the
    // `value_with_feature_and_flags_size` submodule.
}

/// Approximate element count
pub type ApproximateElementCount = u32;
/// Estimated level number
pub type EstimatedLevelNumber = u32;
/// Estimated to be empty
pub type EstimatedToBeEmpty = bool;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
/// Information on an estimated layer
pub struct EstimatedLayerInformation {
    /// The kind of tree we are in
    pub tree_type: TreeType,
    /// Estimated layer count
    pub estimated_layer_count: EstimatedLayerCount,
    /// Estimated layer sizes
    pub estimated_layer_sizes: EstimatedLayerSizes,
}

impl EstimatedLayerInformation {}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
/// Estimated elements and level number of a layer
pub enum EstimatedLayerCount {
    /// Potentially at max elements
    PotentiallyAtMaxElements,
    /// Approximate elements
    ApproximateElements(ApproximateElementCount),
    /// Estimated level
    EstimatedLevel(EstimatedLevelNumber, EstimatedToBeEmpty),
}

impl EstimatedLayerCount {
    /// Returns true if the tree is estimated to be empty.
    pub fn estimated_to_be_empty(&self) -> bool {
        match self {
            EstimatedLayerCount::ApproximateElements(count) => *count == 0,
            EstimatedLayerCount::PotentiallyAtMaxElements => false,
            EstimatedLayerCount::EstimatedLevel(_, empty) => *empty,
        }
    }

    /// Estimate the number of levels based on the size of the tree, for big
    /// trees this is very inaccurate.
    pub fn estimate_levels(&self) -> u32 {
        match self {
            EstimatedLayerCount::ApproximateElements(n) => {
                if *n == u32::MAX {
                    32
                } else {
                    ((n + 1) as f32).log2().ceil() as u32
                }
            }
            EstimatedLayerCount::PotentiallyAtMaxElements => 32,
            EstimatedLayerCount::EstimatedLevel(n, _) => *n,
        }
    }

    /// The fewest levels a Merk of this layer can have, counted in whole
    /// numbers: a tree of `l` levels holds at most `2^l - 1` nodes.
    ///
    /// [`estimate_levels`](Self::estimate_levels) takes a logarithm in 32-bit
    /// floating point, and gives a level too few for a count that is a power
    /// of two from 2^21 on. It is what the estimates of grove versions 1 to
    /// 5 count levels by.
    pub fn fewest_levels(&self) -> u32 {
        match self {
            EstimatedLayerCount::ApproximateElements(n) => u32::BITS - n.leading_zeros(),
            EstimatedLayerCount::PotentiallyAtMaxElements => 32,
            EstimatedLayerCount::EstimatedLevel(n, _) => *n,
        }
    }
}

impl TreeNode {
    /// Return estimate of average encoded tree size
    pub fn average_case_encoded_tree_size(
        not_prefixed_key_len: u32,
        estimated_element_size: u32,
        node_type: NodeType,
    ) -> u32 {
        // two option values for the left and right link
        // the actual left and right link encoding size
        // the encoded kv node size
        2 + (2 * Link::encoded_link_size(not_prefixed_key_len, node_type))
            + KV::encoded_kv_node_size(estimated_element_size, node_type)
    }
}

/// Add worst case for getting a merk node
pub fn add_average_case_get_merk_node(
    cost: &mut OperationCost,
    not_prefixed_key_len: u32,
    approximate_element_size: u32,
    node_type: NodeType,
) -> Result<(), Error> {
    // Worst case scenario, the element is not already in memory.
    // One direct seek has to be performed to read the node from storage.
    cost.seek_count += 1;

    // To write a node to disk, the left link, right link and kv nodes are encoded.
    // worst case, the node has both the left and right link present.
    cost.storage_loaded_bytes += TreeNode::average_case_encoded_tree_size(
        not_prefixed_key_len,
        approximate_element_size,
        node_type,
    ) as u64;
    Ok(())
}

/// Add worst case for getting a merk tree
pub fn add_average_case_merk_has_value(
    cost: &mut OperationCost,
    not_prefixed_key_len: u32,
    estimated_element_size: u32,
) {
    cost.seek_count += 1;
    cost.storage_loaded_bytes += (not_prefixed_key_len + estimated_element_size) as u64;
}

/// Add worst case for insertion into merk
pub fn add_average_case_merk_replace_layered(
    cost: &mut OperationCost,
    key_len: u32,
    value_len: u32,
    node_type: NodeType,
) {
    cost.seek_count += 1;
    cost.storage_cost.replaced_bytes =
        KV::layered_value_byte_cost_size_for_key_and_value_lengths(key_len, value_len, node_type);

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

/// Add average case for deletion from merk
pub fn add_average_case_merk_delete_layered(
    cost: &mut OperationCost,
    _key_len: u32,
    value_len: u32,
) {
    // todo: verify this
    cost.seek_count += 1;
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32);
}

/// Add average case for deletion from merk
pub fn add_average_case_merk_delete(cost: &mut OperationCost, _key_len: u32, value_len: u32) {
    // todo: verify this
    cost.seek_count += 1;
    cost.hash_node_calls += 1 + ((value_len - 1) / HASH_BLOCK_SIZE_U32);
}

const fn node_hash_update_count() -> u32 {
    // It's a hash of node hash, left and right
    let bytes = HASH_LENGTH * 3;
    // todo: verify this

    1 + ((bytes - 1) / HASH_BLOCK_SIZE) as u32
}

/// Add worst case for getting a merk tree root hash
pub fn add_average_case_merk_root_hash(cost: &mut OperationCost) {
    cost.hash_node_calls += node_hash_update_count();
}

#[cfg(test)]
mod tests {
    use grovedb_costs::OperationCost;
    use grovedb_version::version::{v5::GROVE_V5, v6::GROVE_V6, GroveVersion};

    use super::*;

    #[test]
    fn test_estimated_sum_trees_divide_by_zero() {
        let estimated = EstimatedSumTrees::SomeSumTrees {
            sum_trees_weight: 0,
            big_sum_trees_weight: 0,
            count_trees_weight: 0,
            count_sum_trees_weight: 0,
            non_sum_trees_weight: 0,
            provable_sum_trees_weight: 0,
            provable_count_trees_weight: 0,
            provable_count_sum_trees_weight: 0,
            provable_count_provable_sum_trees_weight: 0,
            provable_count_provable_big_sum_trees_weight: 0,
            count_big_sum_trees_weight: 0,
            provable_big_sum_trees_weight: 0,
        };
        let err = estimated
            .estimated_size(GroveVersion::latest())
            .unwrap_err();
        assert!(matches!(err, Error::DivideByZero("weights add up to 0")));
    }

    #[test]
    fn test_estimated_sum_trees_v0_formula_path() {
        let estimated = EstimatedSumTrees::SomeSumTrees {
            sum_trees_weight: 1,
            big_sum_trees_weight: 0,
            count_trees_weight: 0,
            count_sum_trees_weight: 0,
            non_sum_trees_weight: 3,
            provable_sum_trees_weight: 0,
            provable_count_trees_weight: 0,
            provable_count_sum_trees_weight: 0,
            provable_count_provable_sum_trees_weight: 0,
            provable_count_provable_big_sum_trees_weight: 0,
            count_big_sum_trees_weight: 0,
            provable_big_sum_trees_weight: 0,
        };
        let size = estimated.estimated_size(GroveVersion::first()).unwrap();
        assert_eq!(size, 6);
    }

    #[test]
    fn test_layered_flags_size_errors() {
        let layer = EstimatedLayerSizes::AllItems(4, 10, Some(1));
        let err = layer.layered_flags_size().unwrap_err();
        assert!(matches!(
            err,
            Error::WrongEstimatedCostsElementTypeForLevel(
                "this layer does not have costs for trees"
            )
        ));
    }

    #[test]
    fn test_subtree_with_feature_and_flags_size_mix_missing_subtrees() {
        let layer = EstimatedLayerSizes::Mix {
            subtrees_size: None,
            items_size: Some((4, 10, Some(1), 1)),
            references_size: None,
            items_with_sum_item_size: None,
            references_with_sum_item_size: None,
        };
        let err = layer
            .subtree_with_feature_and_flags_size(GroveVersion::latest())
            .unwrap_err();
        assert!(matches!(
            err,
            Error::WrongEstimatedCostsElementTypeForLevel(
                "this layer is a mix but doesn't have subtrees"
            )
        ));
    }

    #[test]
    fn test_value_with_feature_and_flags_size_mix_without_weights() {
        let layer = EstimatedLayerSizes::Mix {
            subtrees_size: None,
            items_size: None,
            references_size: None,
            items_with_sum_item_size: None,
            references_with_sum_item_size: None,
        };
        let err = layer
            .value_with_feature_and_flags_size(GroveVersion::latest())
            .unwrap_err();
        assert!(matches!(
            err,
            Error::WrongEstimatedCostsElementTypeForLevel(
                "this layer is a mix and does not have items, refs or trees"
            )
        ));
    }

    #[test]
    fn test_estimated_layer_count_helpers() {
        assert!(EstimatedLayerCount::ApproximateElements(0).estimated_to_be_empty());
        assert_eq!(
            EstimatedLayerCount::ApproximateElements(7).estimate_levels(),
            3
        );
        assert_eq!(
            EstimatedLayerCount::PotentiallyAtMaxElements.estimate_levels(),
            32
        );
        assert_eq!(
            EstimatedLayerCount::EstimatedLevel(5, false).estimate_levels(),
            5
        );
    }

    #[test]
    fn test_add_average_case_merk_propagate_version_mismatch_error() {
        let mut custom_version = GroveVersion::latest().clone();
        custom_version
            .merk_versions
            .average_case_costs
            .add_average_case_merk_propagate = 99;

        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(1, false),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(5, 20, None),
        };

        let mut cost = OperationCost::default();
        let err =
            add_average_case_merk_propagate(&mut cost, &layer_info, &custom_version).unwrap_err();
        assert!(matches!(
            err,
            Error::VersionError(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch { .. }
            )
        ));
    }

    #[test]
    fn test_add_average_case_merk_propagate_all_items_updates_cost() {
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(3, false),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(8, 32, Some(2)),
        };

        let mut cost = OperationCost::default();
        add_average_case_merk_propagate(&mut cost, &layer_info, GroveVersion::latest()).unwrap();

        assert!(cost.seek_count > 0);
        assert!(cost.hash_node_calls > 0);
        assert!(cost.storage_cost.replaced_bytes > 0);
        assert!(cost.storage_loaded_bytes > 0);
    }

    // =========================================================================
    // v0 propagation (covers add_average_case_merk_propagate_v0 entirely)
    // =========================================================================

    #[test]
    fn test_propagate_v0_all_subtrees() {
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(3, false),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                8,
                EstimatedSumTrees::NoSumTrees,
                Some(4),
            ),
        };
        let mut cost = OperationCost::default();
        add_average_case_merk_propagate(&mut cost, &layer_info, GroveVersion::first()).unwrap();
        assert!(cost.seek_count > 0);
        assert!(cost.storage_cost.replaced_bytes > 0);
        assert!(cost.storage_loaded_bytes > 0);
    }

    #[test]
    fn test_propagate_v0_all_items() {
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(3, false),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(8, 32, Some(2)),
        };
        let mut cost = OperationCost::default();
        add_average_case_merk_propagate(&mut cost, &layer_info, GroveVersion::first()).unwrap();
        assert!(cost.seek_count > 0);
        assert!(cost.storage_cost.replaced_bytes > 0);
        assert!(cost.storage_loaded_bytes > 0);
    }

    #[test]
    fn test_propagate_v0_all_reference() {
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(3, false),
            estimated_layer_sizes: EstimatedLayerSizes::AllReference(8, 24, Some(2)),
        };
        let mut cost = OperationCost::default();
        add_average_case_merk_propagate(&mut cost, &layer_info, GroveVersion::first()).unwrap();
        assert!(cost.seek_count > 0);
        assert!(cost.storage_cost.replaced_bytes > 0);
        assert!(cost.storage_loaded_bytes > 0);
    }

    #[test]
    fn test_propagate_v0_mix() {
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(3, false),
            estimated_layer_sizes: EstimatedLayerSizes::Mix {
                subtrees_size: Some((8, EstimatedSumTrees::NoSumTrees, Some(4), 2)),
                items_size: Some((8, 32, Some(2), 3)),
                references_size: Some((8, 24, Some(1), 1)),
                items_with_sum_item_size: None,
                references_with_sum_item_size: None,
            },
        };
        let mut cost = OperationCost::default();
        add_average_case_merk_propagate(&mut cost, &layer_info, GroveVersion::first()).unwrap();
        assert!(cost.seek_count > 0);
        assert!(cost.storage_cost.replaced_bytes > 0);
        assert!(cost.storage_loaded_bytes > 0);
    }

    // =========================================================================
    // v1 propagation branches not yet covered (Mix, AllReference)
    // =========================================================================

    #[test]
    fn test_propagate_v1_all_reference() {
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(3, false),
            estimated_layer_sizes: EstimatedLayerSizes::AllReference(8, 24, Some(2)),
        };
        let mut cost = OperationCost::default();
        add_average_case_merk_propagate(&mut cost, &layer_info, GroveVersion::latest()).unwrap();
        assert!(cost.seek_count > 0);
        assert!(cost.storage_cost.replaced_bytes > 0);
        assert!(cost.storage_loaded_bytes > 0);
    }

    #[test]
    fn test_propagate_v1_mix() {
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(3, false),
            estimated_layer_sizes: EstimatedLayerSizes::Mix {
                subtrees_size: Some((8, EstimatedSumTrees::NoSumTrees, Some(4), 2)),
                items_size: Some((8, 32, Some(2), 3)),
                references_size: Some((8, 24, Some(1), 1)),
                items_with_sum_item_size: None,
                references_with_sum_item_size: None,
            },
        };
        let mut cost = OperationCost::default();
        add_average_case_merk_propagate(&mut cost, &layer_info, GroveVersion::latest()).unwrap();
        assert!(cost.seek_count > 0);
        assert!(cost.storage_cost.replaced_bytes > 0);
        assert!(cost.storage_loaded_bytes > 0);
    }

    // =========================================================================
    // Utility functions with zero coverage
    // =========================================================================

    #[test]
    fn test_add_average_case_merk_has_value() {
        let mut cost = OperationCost::default();
        add_average_case_merk_has_value(&mut cost, 32, 100);
        assert_eq!(cost.seek_count, 1);
        assert_eq!(cost.storage_loaded_bytes, 132);
    }

    #[test]
    fn test_node_hash_update_count() {
        let count = node_hash_update_count();
        // hash of 3 * HASH_LENGTH bytes: 1 + (bytes - 1) / HASH_BLOCK_SIZE
        assert!(count > 0);
    }

    #[test]
    fn test_add_average_case_merk_root_hash() {
        let mut cost = OperationCost::default();
        add_average_case_merk_root_hash(&mut cost);
        assert!(cost.hash_node_calls > 0);
    }

    // =========================================================================
    // EstimatedSumTrees variant arms not yet covered
    // =========================================================================

    #[test]
    fn test_estimated_sum_trees_all_variant_arms() {
        let v = GroveVersion::latest();
        let sum = EstimatedSumTrees::AllSumTrees.estimated_size(v).unwrap();
        assert!(sum > 0);
        let big = EstimatedSumTrees::AllBigSumTrees.estimated_size(v).unwrap();
        assert!(big > 0);
        let count = EstimatedSumTrees::AllCountTrees.estimated_size(v).unwrap();
        assert!(count > 0);
        let count_sum = EstimatedSumTrees::AllCountSumTrees
            .estimated_size(v)
            .unwrap();
        assert!(count_sum > 0);
    }

    // =========================================================================
    // EstimatedLayerCount edge case
    // =========================================================================

    #[test]
    fn test_estimate_levels_u32_max() {
        assert_eq!(
            EstimatedLayerCount::ApproximateElements(u32::MAX).estimate_levels(),
            32
        );
    }

    // =========================================================================
    // EstimatedLayerSizes method branches
    // =========================================================================

    #[test]
    fn test_layered_flags_size_mix_with_subtrees() {
        let layer = EstimatedLayerSizes::Mix {
            subtrees_size: Some((8, EstimatedSumTrees::NoSumTrees, Some(4), 1)),
            items_size: Some((8, 32, Some(2), 1)),
            references_size: None,
            items_with_sum_item_size: None,
            references_with_sum_item_size: None,
        };
        let flags = layer.layered_flags_size().unwrap();
        assert_eq!(*flags, Some(4));
    }

    #[test]
    fn test_layered_flags_size_mix_without_subtrees() {
        let layer = EstimatedLayerSizes::Mix {
            subtrees_size: None,
            items_size: Some((8, 32, Some(2), 1)),
            references_size: None,
            items_with_sum_item_size: None,
            references_with_sum_item_size: None,
        };
        assert!(layer.layered_flags_size().is_err());
    }

    #[test]
    fn test_subtree_size_all_subtrees() {
        let layer = EstimatedLayerSizes::AllSubtrees(8, EstimatedSumTrees::NoSumTrees, Some(4));
        let size = layer
            .subtree_with_feature_and_flags_size(GroveVersion::latest())
            .unwrap();
        assert_eq!(size, 7); // NoSumTrees=0, flags=4, base=3
    }

    #[test]
    fn test_subtree_size_mix_with_subtrees() {
        let layer = EstimatedLayerSizes::Mix {
            subtrees_size: Some((8, EstimatedSumTrees::NoSumTrees, Some(4), 1)),
            items_size: None,
            references_size: None,
            items_with_sum_item_size: None,
            references_with_sum_item_size: None,
        };
        let size = layer
            .subtree_with_feature_and_flags_size(GroveVersion::latest())
            .unwrap();
        assert_eq!(size, 7);
    }

    #[test]
    fn test_subtree_size_non_subtree_errors() {
        let layer = EstimatedLayerSizes::AllReference(8, 24, Some(2));
        assert!(layer
            .subtree_with_feature_and_flags_size(GroveVersion::latest())
            .is_err());
    }

    #[test]
    fn test_value_size_all_reference() {
        let layer = EstimatedLayerSizes::AllReference(8, 24, Some(2));
        let size = layer
            .value_with_feature_and_flags_size(GroveVersion::latest())
            .unwrap();
        assert_eq!(size, 24 + 2 + 5); // value + flags + 5 for refs
    }

    #[test]
    fn test_value_size_all_subtrees() {
        let layer = EstimatedLayerSizes::AllSubtrees(8, EstimatedSumTrees::NoSumTrees, Some(4));
        let size = layer
            .value_with_feature_and_flags_size(GroveVersion::latest())
            .unwrap();
        assert_eq!(size, 7);
    }

    /// v1 (grove v3) returns the proper weighted average.
    /// item_size=37 weight=3, ref_size=30 weight=1, subtree_size=7 weight=2.
    /// Σ(size_i · weight_i) = 111 + 30 + 14 = 155; combined_weight = 6;
    /// 155 / 6 = 25.
    #[test]
    fn test_value_size_mix_weighted_combination_v1() {
        let layer = EstimatedLayerSizes::Mix {
            subtrees_size: Some((8, EstimatedSumTrees::NoSumTrees, Some(4), 2)),
            items_size: Some((8, 32, Some(2), 3)),
            references_size: Some((8, 24, Some(1), 1)),
            items_with_sum_item_size: None,
            references_with_sum_item_size: None,
        };
        let size = layer
            .value_with_feature_and_flags_size(GroveVersion::latest())
            .unwrap();
        assert_eq!(size, 25);
    }

    /// v0 (grove v1/v2) keeps the legacy unweighted formula:
    /// (item + ref + subtree) / combined_weight = (37+30+7)/6 = 12.
    /// Regression pin so the consensus-locked grove v1/v2 dispatch
    /// can't accidentally pick up the fixed formula.
    #[test]
    fn test_value_size_mix_weighted_combination_v0_legacy() {
        let layer = EstimatedLayerSizes::Mix {
            subtrees_size: Some((8, EstimatedSumTrees::NoSumTrees, Some(4), 2)),
            items_size: Some((8, 32, Some(2), 3)),
            references_size: Some((8, 24, Some(1), 1)),
            items_with_sum_item_size: None,
            references_with_sum_item_size: None,
        };
        let size = layer
            .value_with_feature_and_flags_size(GroveVersion::first())
            .unwrap();
        assert_eq!(size, 12);
    }

    #[test]
    fn test_value_size_mix_only_subtrees() {
        let layer = EstimatedLayerSizes::Mix {
            subtrees_size: Some((8, EstimatedSumTrees::NoSumTrees, Some(4), 2)),
            items_size: None,
            references_size: None,
            items_with_sum_item_size: None,
            references_with_sum_item_size: None,
        };
        let size = layer
            .value_with_feature_and_flags_size(GroveVersion::latest())
            .unwrap();
        assert_eq!(size, 7);
    }

    #[test]
    fn test_value_size_mix_only_refs() {
        let layer = EstimatedLayerSizes::Mix {
            subtrees_size: None,
            items_size: None,
            references_size: Some((8, 24, Some(1), 1)),
            items_with_sum_item_size: None,
            references_with_sum_item_size: None,
        };
        let size = layer
            .value_with_feature_and_flags_size(GroveVersion::latest())
            .unwrap();
        assert_eq!(size, 24 + 1 + 5);
    }

    #[test]
    fn test_value_size_mix_only_items() {
        let layer = EstimatedLayerSizes::Mix {
            subtrees_size: None,
            items_size: Some((8, 32, Some(2), 3)),
            references_size: None,
            items_with_sum_item_size: None,
            references_with_sum_item_size: None,
        };
        let size = layer
            .value_with_feature_and_flags_size(GroveVersion::latest())
            .unwrap();
        assert_eq!(size, 32 + 2 + 3);
    }

    // =========================================================================
    // Sum-bearing layer / tree variants
    // =========================================================================

    /// `AllItemsWithSumItem` adds +10 over the plain-item layout for the
    /// worst-case i64 sum_value varint, matching the per-element helper
    /// `Element::required_item_with_sum_item_space`.
    #[test]
    fn test_value_size_all_items_with_sum_item_is_all_items_plus_ten() {
        let key = 8;
        let value = 32;
        let flags = Some(2);

        let plain = EstimatedLayerSizes::AllItems(key, value, flags);
        let plain_size = plain
            .value_with_feature_and_flags_size(GroveVersion::latest())
            .unwrap();

        let with_sum = EstimatedLayerSizes::AllItemsWithSumItem(key, value, flags);
        let with_sum_size = with_sum
            .value_with_feature_and_flags_size(GroveVersion::latest())
            .unwrap();

        assert_eq!(with_sum_size, plain_size + 10);
    }

    /// `AllReferencesWithSumItem` adds the same +10 over the plain
    /// `AllReference` formula.
    #[test]
    fn test_value_size_all_references_with_sum_item_is_all_reference_plus_ten() {
        let key = 8;
        let value = 24;
        let flags = Some(1);

        let plain = EstimatedLayerSizes::AllReference(key, value, flags);
        let plain_size = plain
            .value_with_feature_and_flags_size(GroveVersion::latest())
            .unwrap();

        let with_sum = EstimatedLayerSizes::AllReferencesWithSumItem(key, value, flags);
        let with_sum_size = with_sum
            .value_with_feature_and_flags_size(GroveVersion::latest())
            .unwrap();

        assert_eq!(with_sum_size, plain_size + 10);
    }

    /// `AllItemsWithSumItem` propagation cost strictly exceeds the plain
    /// `AllItems` cost for the same key/value/flags. The +10 per-element
    /// sum-value adjustment flows through both `storage_cost.replaced_bytes`
    /// and `storage_loaded_bytes`.
    #[test]
    fn test_propagate_all_items_with_sum_item_exceeds_all_items() {
        let layer_count = EstimatedLayerCount::EstimatedLevel(3, false);
        let plain_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: layer_count,
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(8, 32, Some(2)),
        };
        let sum_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: layer_count,
            estimated_layer_sizes: EstimatedLayerSizes::AllItemsWithSumItem(8, 32, Some(2)),
        };

        let mut plain_cost = OperationCost::default();
        add_average_case_merk_propagate(&mut plain_cost, &plain_info, GroveVersion::latest())
            .unwrap();
        let mut sum_cost = OperationCost::default();
        add_average_case_merk_propagate(&mut sum_cost, &sum_info, GroveVersion::latest()).unwrap();

        assert!(
            sum_cost.storage_cost.replaced_bytes > plain_cost.storage_cost.replaced_bytes,
            "sum-item replaced_bytes ({}) must exceed plain ({})",
            sum_cost.storage_cost.replaced_bytes,
            plain_cost.storage_cost.replaced_bytes,
        );
        assert!(
            sum_cost.storage_loaded_bytes > plain_cost.storage_loaded_bytes,
            "sum-item storage_loaded_bytes ({}) must exceed plain ({})",
            sum_cost.storage_loaded_bytes,
            plain_cost.storage_loaded_bytes,
        );
    }

    /// `AllReferencesWithSumItem` propagation cost strictly exceeds the
    /// plain `AllReference` cost.
    #[test]
    fn test_propagate_all_references_with_sum_item_exceeds_all_reference() {
        let layer_count = EstimatedLayerCount::EstimatedLevel(3, false);
        let plain_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: layer_count,
            estimated_layer_sizes: EstimatedLayerSizes::AllReference(8, 24, Some(1)),
        };
        let sum_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: layer_count,
            estimated_layer_sizes: EstimatedLayerSizes::AllReferencesWithSumItem(8, 24, Some(1)),
        };

        let mut plain_cost = OperationCost::default();
        add_average_case_merk_propagate(&mut plain_cost, &plain_info, GroveVersion::latest())
            .unwrap();
        let mut sum_cost = OperationCost::default();
        add_average_case_merk_propagate(&mut sum_cost, &sum_info, GroveVersion::latest()).unwrap();

        assert!(sum_cost.storage_cost.replaced_bytes > plain_cost.storage_cost.replaced_bytes);
        assert!(sum_cost.storage_loaded_bytes > plain_cost.storage_loaded_bytes);
    }

    /// Mix with the new sum-bearing fields propagates without panicking
    /// and produces a strictly larger cost than the same Mix with
    /// `items_with_sum_item_size: None`.
    #[test]
    fn test_propagate_mix_with_items_with_sum_item_increases_cost() {
        let layer_count = EstimatedLayerCount::EstimatedLevel(3, false);
        let base = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: layer_count,
            estimated_layer_sizes: EstimatedLayerSizes::Mix {
                subtrees_size: None,
                items_size: Some((8, 32, Some(2), 1)),
                references_size: None,
                items_with_sum_item_size: None,
                references_with_sum_item_size: None,
            },
        };
        let with_sum = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: layer_count,
            estimated_layer_sizes: EstimatedLayerSizes::Mix {
                subtrees_size: None,
                items_size: Some((8, 32, Some(2), 1)),
                references_size: None,
                items_with_sum_item_size: Some((8, 32, Some(2), 1)),
                references_with_sum_item_size: None,
            },
        };

        let mut base_cost = OperationCost::default();
        add_average_case_merk_propagate(&mut base_cost, &base, GroveVersion::latest()).unwrap();
        let mut sum_cost = OperationCost::default();
        add_average_case_merk_propagate(&mut sum_cost, &with_sum, GroveVersion::latest()).unwrap();

        // Adding a sum-item-bearing kind to the mix must push the
        // averaged cost upward (each sum-item leaf is +10 over a plain
        // item leaf).
        assert!(sum_cost.storage_cost.replaced_bytes > base_cost.storage_cost.replaced_bytes);
    }

    /// `Mix` layered_flags_size still picks up the subtrees' flags when
    /// only the new sum-item fields are also present — sanity for the
    /// `..` destructure.
    #[test]
    fn test_layered_flags_size_mix_with_subtrees_and_sum_item_fields() {
        let layer = EstimatedLayerSizes::Mix {
            subtrees_size: Some((8, EstimatedSumTrees::NoSumTrees, Some(7), 1)),
            items_size: None,
            references_size: None,
            items_with_sum_item_size: Some((8, 32, Some(2), 1)),
            references_with_sum_item_size: None,
        };
        let flags = layer.layered_flags_size().unwrap();
        assert_eq!(*flags, Some(7));
    }

    /// Regression: `Mix` with weights only on `items_with_sum_item_size`
    /// short-circuits to the per-element sum-item size instead of
    /// dividing by zero across the other empty kinds.
    #[test]
    fn test_value_size_mix_only_items_with_sum_item() {
        let layer = EstimatedLayerSizes::Mix {
            subtrees_size: None,
            items_size: None,
            references_size: None,
            items_with_sum_item_size: Some((8, 32, Some(2), 3)),
            references_with_sum_item_size: None,
        };
        let size = layer
            .value_with_feature_and_flags_size(GroveVersion::latest())
            .unwrap();
        // 32 (value) + 2 (flags) + 3 (item base) + 10 (sum varint worst case)
        assert_eq!(size, 32 + 2 + 13);
    }

    /// Regression: `Mix` with weights only on `references_with_sum_item_size`.
    #[test]
    fn test_value_size_mix_only_references_with_sum_item() {
        let layer = EstimatedLayerSizes::Mix {
            subtrees_size: None,
            items_size: None,
            references_size: None,
            items_with_sum_item_size: None,
            references_with_sum_item_size: Some((8, 24, Some(1), 2)),
        };
        let size = layer
            .value_with_feature_and_flags_size(GroveVersion::latest())
            .unwrap();
        // 24 + 1 + 5 (ref base) + 10 (sum varint)
        assert_eq!(size, 24 + 1 + 15);
    }

    // =========================================================================
    // EstimatedSumTrees v0/v1/v2 formula stability + new weights
    // =========================================================================

    /// New homogeneous variants resolve to their tree-type's
    /// `inner_node_type().cost()`, mirroring the existing
    /// `AllSumTrees`/`AllCountTrees` etc.
    #[test]
    fn test_estimated_sum_trees_all_provable_variant_arms() {
        let v = GroveVersion::latest();
        assert_eq!(
            EstimatedSumTrees::AllProvableSumTrees
                .estimated_size(v)
                .unwrap(),
            TreeType::ProvableSumTree.inner_node_type().cost(),
        );
        assert_eq!(
            EstimatedSumTrees::AllProvableCountTrees
                .estimated_size(v)
                .unwrap(),
            TreeType::ProvableCountTree.inner_node_type().cost(),
        );
        assert_eq!(
            EstimatedSumTrees::AllProvableCountSumTrees
                .estimated_size(v)
                .unwrap(),
            TreeType::ProvableCountSumTree.inner_node_type().cost(),
        );
        assert_eq!(
            EstimatedSumTrees::AllProvableCountProvableSumTrees
                .estimated_size(v)
                .unwrap(),
            TreeType::ProvableCountProvableSumTree
                .inner_node_type()
                .cost(),
        );
        // The wide tree's nodes carry a count and a 16-byte sum, under
        // every version of the estimate.
        for version in grovedb_version::version::GROVE_VERSIONS {
            assert_eq!(
                EstimatedSumTrees::AllProvableCountProvableBigSumTrees
                    .estimated_size(version)
                    .unwrap(),
                24,
            );
        }
    }

    /// The wide tree's weight in a mixed layer is read by the v2 formula,
    /// which averages its 24-byte feature with the other trees', and ignored
    /// by v0 and v1, which predate the tree and keep their results.
    #[test]
    fn test_estimated_sum_trees_mixed_layer_weighs_the_wide_tree() {
        use grovedb_version::version::{v1::GROVE_V1, v2::GROVE_V2};

        let mixed = |wide_weight| EstimatedSumTrees::SomeSumTrees {
            sum_trees_weight: 1,
            big_sum_trees_weight: 0,
            count_trees_weight: 0,
            count_sum_trees_weight: 0,
            non_sum_trees_weight: 0,
            provable_sum_trees_weight: 0,
            provable_count_trees_weight: 0,
            provable_count_sum_trees_weight: 0,
            provable_count_provable_sum_trees_weight: 0,
            provable_count_provable_big_sum_trees_weight: wide_weight,
            count_big_sum_trees_weight: 0,
            provable_big_sum_trees_weight: 0,
        };

        let latest = GroveVersion::latest();
        // One sum tree (8 bytes) alone, then with three wide trees (24).
        assert_eq!(mixed(0).estimated_size(latest).unwrap(), 8);
        assert_eq!(mixed(3).estimated_size(latest).unwrap(), (8 + 3 * 24) / 4);

        for older in [&GROVE_V1, &GROVE_V2] {
            assert_eq!(
                mixed(3).estimated_size(older).unwrap(),
                mixed(0).estimated_size(older).unwrap(),
                "protocol version {}",
                older.protocol_version
            );
        }
    }

    /// v0 formula output is pinned to its historical value so the v2
    /// expansion can't silently shift fees on already-shipped paths
    /// (grove v1 uses v0 here). Same input as
    /// `test_estimated_sum_trees_v0_formula_path` but explicit.
    #[test]
    fn test_estimated_sum_trees_v0_output_pinned_for_provable_weights() {
        // v0 ignores ALL `provable_*` weights — populating them must
        // produce the same output as leaving them at zero.
        let baseline = EstimatedSumTrees::SomeSumTrees {
            sum_trees_weight: 1,
            big_sum_trees_weight: 0,
            count_trees_weight: 0,
            count_sum_trees_weight: 0,
            non_sum_trees_weight: 3,
            provable_sum_trees_weight: 0,
            provable_count_trees_weight: 0,
            provable_count_sum_trees_weight: 0,
            provable_count_provable_sum_trees_weight: 0,
            provable_count_provable_big_sum_trees_weight: 0,
            count_big_sum_trees_weight: 0,
            provable_big_sum_trees_weight: 0,
        };
        let with_provable = EstimatedSumTrees::SomeSumTrees {
            sum_trees_weight: 1,
            big_sum_trees_weight: 0,
            count_trees_weight: 0,
            count_sum_trees_weight: 0,
            non_sum_trees_weight: 3,
            provable_sum_trees_weight: 5,
            provable_count_trees_weight: 7,
            provable_count_sum_trees_weight: 9,
            provable_count_provable_sum_trees_weight: 11,
            provable_count_provable_big_sum_trees_weight: 0,
            count_big_sum_trees_weight: 0,
            provable_big_sum_trees_weight: 0,
        };

        let v0 = GroveVersion::first();
        assert_eq!(
            baseline.estimated_size(v0).unwrap(),
            with_provable.estimated_size(v0).unwrap(),
            "v0 must ignore new provable_* weights",
        );
    }

    /// Same stability invariant for v1: production grove v2 uses v1 of
    /// this method, so its output must not shift when callers fill new
    /// `provable_*` fields.
    #[test]
    fn test_estimated_sum_trees_v1_output_pinned_for_provable_weights() {
        let mut v1_version = GroveVersion::latest().clone();
        v1_version
            .merk_versions
            .average_case_costs
            .sum_tree_estimated_size = 1;

        let baseline = EstimatedSumTrees::SomeSumTrees {
            sum_trees_weight: 2,
            big_sum_trees_weight: 1,
            count_trees_weight: 1,
            count_sum_trees_weight: 1,
            non_sum_trees_weight: 3,
            provable_sum_trees_weight: 0,
            provable_count_trees_weight: 0,
            provable_count_sum_trees_weight: 0,
            provable_count_provable_sum_trees_weight: 0,
            provable_count_provable_big_sum_trees_weight: 0,
            count_big_sum_trees_weight: 0,
            provable_big_sum_trees_weight: 0,
        };
        let with_provable = EstimatedSumTrees::SomeSumTrees {
            sum_trees_weight: 2,
            big_sum_trees_weight: 1,
            count_trees_weight: 1,
            count_sum_trees_weight: 1,
            non_sum_trees_weight: 3,
            provable_sum_trees_weight: 4,
            provable_count_trees_weight: 6,
            provable_count_sum_trees_weight: 8,
            provable_count_provable_sum_trees_weight: 10,
            provable_count_provable_big_sum_trees_weight: 0,
            count_big_sum_trees_weight: 0,
            provable_big_sum_trees_weight: 0,
        };

        assert_eq!(
            baseline.estimated_size(&v1_version).unwrap(),
            with_provable.estimated_size(&v1_version).unwrap(),
            "v1 must ignore new provable_* weights so production grove v2 stays stable",
        );
    }

    /// v2 must actually use the new `provable_*` weights — toggling them
    /// changes the output, unlike v0/v1.
    #[test]
    fn test_estimated_sum_trees_v2_includes_provable_weights() {
        let v = GroveVersion::latest(); // v3 → v2 of this method

        let baseline = EstimatedSumTrees::SomeSumTrees {
            sum_trees_weight: 1,
            big_sum_trees_weight: 0,
            count_trees_weight: 0,
            count_sum_trees_weight: 0,
            non_sum_trees_weight: 0,
            provable_sum_trees_weight: 0,
            provable_count_trees_weight: 0,
            provable_count_sum_trees_weight: 0,
            provable_count_provable_sum_trees_weight: 0,
            provable_count_provable_big_sum_trees_weight: 0,
            count_big_sum_trees_weight: 0,
            provable_big_sum_trees_weight: 0,
        };
        let with_pcps = EstimatedSumTrees::SomeSumTrees {
            sum_trees_weight: 1,
            big_sum_trees_weight: 0,
            count_trees_weight: 0,
            count_sum_trees_weight: 0,
            non_sum_trees_weight: 0,
            provable_sum_trees_weight: 0,
            provable_count_trees_weight: 0,
            provable_count_sum_trees_weight: 0,
            provable_count_provable_sum_trees_weight: 3, // PCPS dominates
            provable_count_provable_big_sum_trees_weight: 0,
            count_big_sum_trees_weight: 0,
            provable_big_sum_trees_weight: 0,
        };

        let baseline_size = baseline.estimated_size(v).unwrap();
        let with_pcps_size = with_pcps.estimated_size(v).unwrap();
        assert_ne!(
            baseline_size, with_pcps_size,
            "v2 formula must respond to provable_count_provable_sum_trees_weight",
        );
    }

    /// v2 weighted average: a homogeneous SomeSumTrees with only one
    /// `provable_*` weight populated equals that tree-type's cost,
    /// matching the `AllProvable*` shortcuts.
    #[test]
    fn test_estimated_sum_trees_v2_homogeneous_provable_matches_all_shortcut() {
        let v = GroveVersion::latest();

        let cases = [
            (
                EstimatedSumTrees::SomeSumTrees {
                    sum_trees_weight: 0,
                    big_sum_trees_weight: 0,
                    count_trees_weight: 0,
                    count_sum_trees_weight: 0,
                    non_sum_trees_weight: 0,
                    provable_sum_trees_weight: 4,
                    provable_count_trees_weight: 0,
                    provable_count_sum_trees_weight: 0,
                    provable_count_provable_sum_trees_weight: 0,
                    provable_count_provable_big_sum_trees_weight: 0,
                    count_big_sum_trees_weight: 0,
                    provable_big_sum_trees_weight: 0,
                },
                EstimatedSumTrees::AllProvableSumTrees,
            ),
            (
                EstimatedSumTrees::SomeSumTrees {
                    sum_trees_weight: 0,
                    big_sum_trees_weight: 0,
                    count_trees_weight: 0,
                    count_sum_trees_weight: 0,
                    non_sum_trees_weight: 0,
                    provable_sum_trees_weight: 0,
                    provable_count_trees_weight: 5,
                    provable_count_sum_trees_weight: 0,
                    provable_count_provable_sum_trees_weight: 0,
                    provable_count_provable_big_sum_trees_weight: 0,
                    count_big_sum_trees_weight: 0,
                    provable_big_sum_trees_weight: 0,
                },
                EstimatedSumTrees::AllProvableCountTrees,
            ),
            (
                EstimatedSumTrees::SomeSumTrees {
                    sum_trees_weight: 0,
                    big_sum_trees_weight: 0,
                    count_trees_weight: 0,
                    count_sum_trees_weight: 0,
                    non_sum_trees_weight: 0,
                    provable_sum_trees_weight: 0,
                    provable_count_trees_weight: 0,
                    provable_count_sum_trees_weight: 6,
                    provable_count_provable_sum_trees_weight: 0,
                    provable_count_provable_big_sum_trees_weight: 0,
                    count_big_sum_trees_weight: 0,
                    provable_big_sum_trees_weight: 0,
                },
                EstimatedSumTrees::AllProvableCountSumTrees,
            ),
            (
                EstimatedSumTrees::SomeSumTrees {
                    sum_trees_weight: 0,
                    big_sum_trees_weight: 0,
                    count_trees_weight: 0,
                    count_sum_trees_weight: 0,
                    non_sum_trees_weight: 0,
                    provable_sum_trees_weight: 0,
                    provable_count_trees_weight: 0,
                    provable_count_sum_trees_weight: 0,
                    provable_count_provable_sum_trees_weight: 7,
                    provable_count_provable_big_sum_trees_weight: 0,
                    count_big_sum_trees_weight: 0,
                    provable_big_sum_trees_weight: 0,
                },
                EstimatedSumTrees::AllProvableCountProvableSumTrees,
            ),
        ];

        for (homogeneous, shortcut) in cases {
            assert_eq!(
                homogeneous.estimated_size(v).unwrap(),
                shortcut.estimated_size(v).unwrap(),
                "homogeneous SomeSumTrees must match the All* shortcut for the same tree type",
            );
        }
    }

    /// v2 divide-by-zero guard kicks in even when only `provable_*`
    /// weights are zero (legacy weights were zero too).
    #[test]
    fn test_estimated_sum_trees_v2_divide_by_zero() {
        let v = GroveVersion::latest();
        let all_zero = EstimatedSumTrees::SomeSumTrees {
            sum_trees_weight: 0,
            big_sum_trees_weight: 0,
            count_trees_weight: 0,
            count_sum_trees_weight: 0,
            non_sum_trees_weight: 0,
            provable_sum_trees_weight: 0,
            provable_count_trees_weight: 0,
            provable_count_sum_trees_weight: 0,
            provable_count_provable_sum_trees_weight: 0,
            provable_count_provable_big_sum_trees_weight: 0,
            count_big_sum_trees_weight: 0,
            provable_big_sum_trees_weight: 0,
        };
        let err = all_zero.estimated_size(v).unwrap_err();
        assert!(matches!(err, Error::DivideByZero("weights add up to 0")));
    }

    /// v0's divisor is `sum_trees + non_sum_trees`, not the full legacy
    /// weight sum. If only `big_sum_trees_weight` (or any count weight)
    /// is populated, the legacy sum is nonzero but the v0 denominator
    /// is zero — the guard must surface `DivideByZero` rather than
    /// panicking on `/ 0`. Regression for the v0 dispatch.
    #[test]
    fn test_estimated_sum_trees_v0_only_big_sum_weight_returns_divide_by_zero() {
        let big_only = EstimatedSumTrees::SomeSumTrees {
            sum_trees_weight: 0,
            big_sum_trees_weight: 5,
            count_trees_weight: 0,
            count_sum_trees_weight: 0,
            non_sum_trees_weight: 0,
            provable_sum_trees_weight: 0,
            provable_count_trees_weight: 0,
            provable_count_sum_trees_weight: 0,
            provable_count_provable_sum_trees_weight: 0,
            provable_count_provable_big_sum_trees_weight: 0,
            count_big_sum_trees_weight: 0,
            provable_big_sum_trees_weight: 0,
        };
        let err = big_only.estimated_size(GroveVersion::first()).unwrap_err();
        assert!(matches!(err, Error::DivideByZero("weights add up to 0")));
    }

    /// `check_grovedb_v0_v1_or_v2!` rejects any version outside {0, 1, 2}.
    /// Exercises the version-gate error path on `estimated_size` so we
    /// don't ship the macro without a regression test.
    #[test]
    fn test_estimated_sum_trees_unknown_version_error() {
        let mut bad_version = GroveVersion::latest().clone();
        bad_version
            .merk_versions
            .average_case_costs
            .sum_tree_estimated_size = 99;
        let any = EstimatedSumTrees::SomeSumTrees {
            sum_trees_weight: 1,
            big_sum_trees_weight: 0,
            count_trees_weight: 0,
            count_sum_trees_weight: 0,
            non_sum_trees_weight: 1,
            provable_sum_trees_weight: 0,
            provable_count_trees_weight: 0,
            provable_count_sum_trees_weight: 0,
            provable_count_provable_sum_trees_weight: 0,
            provable_count_provable_big_sum_trees_weight: 0,
            count_big_sum_trees_weight: 0,
            provable_big_sum_trees_weight: 0,
        };
        let err = any.estimated_size(&bad_version).unwrap_err();
        assert!(matches!(
            err,
            Error::VersionError(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch { .. }
            )
        ));
    }

    // =========================================================================
    // v0 propagation (grove v1 path) with the new sum-bearing layer variants
    // — the variants are new at the type level, so even on the v0 propagate
    // dispatch they must compile and produce sensible costs.
    // =========================================================================

    #[test]
    fn test_propagate_v0_all_items_with_sum_item() {
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(3, false),
            estimated_layer_sizes: EstimatedLayerSizes::AllItemsWithSumItem(8, 32, Some(2)),
        };
        let mut sum_cost = OperationCost::default();
        add_average_case_merk_propagate(&mut sum_cost, &layer_info, GroveVersion::first()).unwrap();

        // The +10 must flow through v0 propagate the same way as v1: the
        // sum-bearing cost strictly exceeds the plain-AllItems cost.
        let plain_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(3, false),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(8, 32, Some(2)),
        };
        let mut plain_cost = OperationCost::default();
        add_average_case_merk_propagate(&mut plain_cost, &plain_info, GroveVersion::first())
            .unwrap();

        assert!(sum_cost.storage_cost.replaced_bytes > plain_cost.storage_cost.replaced_bytes);
        assert!(sum_cost.storage_loaded_bytes > plain_cost.storage_loaded_bytes);
    }

    #[test]
    fn test_propagate_v0_all_references_with_sum_item() {
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(3, false),
            estimated_layer_sizes: EstimatedLayerSizes::AllReferencesWithSumItem(8, 24, Some(1)),
        };
        let mut sum_cost = OperationCost::default();
        add_average_case_merk_propagate(&mut sum_cost, &layer_info, GroveVersion::first()).unwrap();

        let plain_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(3, false),
            estimated_layer_sizes: EstimatedLayerSizes::AllReference(8, 24, Some(1)),
        };
        let mut plain_cost = OperationCost::default();
        add_average_case_merk_propagate(&mut plain_cost, &plain_info, GroveVersion::first())
            .unwrap();

        assert!(sum_cost.storage_cost.replaced_bytes > plain_cost.storage_cost.replaced_bytes);
        assert!(sum_cost.storage_loaded_bytes > plain_cost.storage_loaded_bytes);
    }

    /// v0 propagate with `Mix` containing both sum-item fields populated
    /// must produce a strictly larger cost than the same Mix with those
    /// fields cleared. Exercises both the replaced_bytes and the
    /// storage_loaded_bytes Mix arms in v0.
    #[test]
    fn test_propagate_v0_mix_with_both_sum_item_fields() {
        let layer_count = EstimatedLayerCount::EstimatedLevel(3, false);
        let base = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: layer_count,
            estimated_layer_sizes: EstimatedLayerSizes::Mix {
                subtrees_size: Some((8, EstimatedSumTrees::NoSumTrees, Some(4), 1)),
                items_size: Some((8, 32, Some(2), 1)),
                references_size: Some((8, 24, Some(1), 1)),
                items_with_sum_item_size: None,
                references_with_sum_item_size: None,
            },
        };
        let with_sum = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: layer_count,
            estimated_layer_sizes: EstimatedLayerSizes::Mix {
                subtrees_size: Some((8, EstimatedSumTrees::NoSumTrees, Some(4), 1)),
                items_size: Some((8, 32, Some(2), 1)),
                references_size: Some((8, 24, Some(1), 1)),
                items_with_sum_item_size: Some((8, 32, Some(2), 1)),
                references_with_sum_item_size: Some((8, 24, Some(1), 1)),
            },
        };

        let mut base_cost = OperationCost::default();
        add_average_case_merk_propagate(&mut base_cost, &base, GroveVersion::first()).unwrap();
        let mut sum_cost = OperationCost::default();
        add_average_case_merk_propagate(&mut sum_cost, &with_sum, GroveVersion::first()).unwrap();

        assert!(sum_cost.storage_cost.replaced_bytes > base_cost.storage_cost.replaced_bytes);
        assert!(sum_cost.storage_loaded_bytes > base_cost.storage_loaded_bytes);
    }

    // =========================================================================
    // Propagate v1 vs v2 Mix-arm divergence (CodeRabbit's outside-diff finding).
    // v1 (grove v2 dispatch): total_updates_cost / (nodes_updated · total_weight)
    //   — equivalent per-node avg / nodes_updated. Pre-existing buggy formula.
    // v2 (grove v3 dispatch): nodes_updated · total_updates_cost / total_weight
    //   — proper layer total, matching the AllItems/AllSubtrees arms.
    // =========================================================================

    /// Build a v2-grove version (dispatches propagate v1, the legacy
    /// formula) for regression pinning. Used only in tests so we don't
    /// need to expose intermediate grove versions to consumers.
    fn grove_version_dispatching_propagate_v1() -> GroveVersion {
        let mut v = GroveVersion::latest().clone();
        v.merk_versions
            .average_case_costs
            .add_average_case_merk_propagate = 1;
        v
    }

    /// A Mix layer with only `items_size` populated is dimensionally
    /// equivalent to `AllItems`. v2 brings the Mix cost up to match
    /// `AllItems` (matching the documented invariant that the Mix
    /// formula should reduce to the homogeneous case). v1's legacy
    /// output is much smaller — pinned here so grove v2 fee outputs
    /// don't drift.
    #[test]
    fn test_propagate_v2_mix_items_only_matches_all_items() {
        let layer_count = EstimatedLayerCount::EstimatedLevel(3, false);
        let mix = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: layer_count,
            estimated_layer_sizes: EstimatedLayerSizes::Mix {
                subtrees_size: None,
                items_size: Some((8, 32, Some(2), 1)),
                references_size: None,
                items_with_sum_item_size: None,
                references_with_sum_item_size: None,
            },
        };
        let all_items = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: layer_count,
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(8, 32, Some(2)),
        };

        let mut mix_cost = OperationCost::default();
        add_average_case_merk_propagate(&mut mix_cost, &mix, GroveVersion::latest()).unwrap();
        let mut all_items_cost = OperationCost::default();
        add_average_case_merk_propagate(&mut all_items_cost, &all_items, GroveVersion::latest())
            .unwrap();

        assert_eq!(
            mix_cost.storage_cost.replaced_bytes, all_items_cost.storage_cost.replaced_bytes,
            "v2 Mix items-only should match AllItems exactly",
        );
        assert_eq!(
            mix_cost.storage_loaded_bytes, all_items_cost.storage_loaded_bytes,
            "v2 Mix items-only should match AllItems exactly",
        );
    }

    /// v2 Mix output is strictly larger than v1 Mix output for the same
    /// input (because v1 divides by `nodes_updated²` worth of denominator).
    #[test]
    fn test_propagate_v2_mix_exceeds_v1_mix() {
        let layer_count = EstimatedLayerCount::EstimatedLevel(3, false);
        let layer_info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: layer_count,
            estimated_layer_sizes: EstimatedLayerSizes::Mix {
                subtrees_size: Some((8, EstimatedSumTrees::NoSumTrees, Some(4), 2)),
                items_size: Some((8, 32, Some(2), 3)),
                references_size: Some((8, 24, Some(1), 1)),
                items_with_sum_item_size: None,
                references_with_sum_item_size: None,
            },
        };

        let mut v1_cost = OperationCost::default();
        add_average_case_merk_propagate(
            &mut v1_cost,
            &layer_info,
            &grove_version_dispatching_propagate_v1(),
        )
        .unwrap();
        let mut v2_cost = OperationCost::default();
        add_average_case_merk_propagate(&mut v2_cost, &layer_info, GroveVersion::latest()).unwrap();

        assert!(
            v2_cost.storage_cost.replaced_bytes > v1_cost.storage_cost.replaced_bytes,
            "v2 ({}) must exceed v1 ({}) for Mix replaced_bytes",
            v2_cost.storage_cost.replaced_bytes,
            v1_cost.storage_cost.replaced_bytes,
        );
        assert!(
            v2_cost.storage_loaded_bytes > v1_cost.storage_loaded_bytes,
            "v2 ({}) must exceed v1 ({}) for Mix storage_loaded_bytes",
            v2_cost.storage_loaded_bytes,
            v1_cost.storage_loaded_bytes,
        );
    }

    /// Non-Mix variants must produce identical output under v1 and v2
    /// (only the Mix arm changed). Sanity-check the dispatch isn't
    /// changing anything outside the Mix path.
    #[test]
    fn test_propagate_v1_v2_agree_on_non_mix_variants() {
        let layer_count = EstimatedLayerCount::EstimatedLevel(3, false);
        let cases = [
            EstimatedLayerSizes::AllItems(8, 32, Some(2)),
            EstimatedLayerSizes::AllReference(8, 24, Some(1)),
            EstimatedLayerSizes::AllItemsWithSumItem(8, 32, Some(2)),
            EstimatedLayerSizes::AllReferencesWithSumItem(8, 24, Some(1)),
            EstimatedLayerSizes::AllSubtrees(8, EstimatedSumTrees::NoSumTrees, Some(4)),
        ];

        let v1_version = grove_version_dispatching_propagate_v1();
        for layer in cases {
            let info = EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: layer_count,
                estimated_layer_sizes: layer,
            };
            let mut v1 = OperationCost::default();
            add_average_case_merk_propagate(&mut v1, &info, &v1_version).unwrap();
            let mut v2 = OperationCost::default();
            add_average_case_merk_propagate(&mut v2, &info, GroveVersion::latest()).unwrap();
            assert_eq!(
                v1.storage_cost.replaced_bytes, v2.storage_cost.replaced_bytes,
                "non-Mix replaced_bytes must agree across propagate v1/v2 for {:?}",
                info.estimated_layer_sizes,
            );
            assert_eq!(
                v1.storage_loaded_bytes, v2.storage_loaded_bytes,
                "non-Mix storage_loaded_bytes must agree across propagate v1/v2 for {:?}",
                info.estimated_layer_sizes,
            );
        }
    }

    /// `add_average_case_merk_propagate` rejects unknown versions, and
    /// names every version it has.
    #[test]
    fn test_propagate_unknown_version_error_lists_every_version() {
        let mut bad = GroveVersion::latest().clone();
        bad.merk_versions
            .average_case_costs
            .add_average_case_merk_propagate = 99;
        let info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(2, false),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(8, 32, None),
        };
        let err = add_average_case_merk_propagate(&mut OperationCost::default(), &info, &bad)
            .unwrap_err();
        match err {
            Error::VersionError(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                    known_versions,
                    ..
                },
            ) => assert_eq!(known_versions, vec![0, 1, 2, 3]),
            other => panic!("expected UnknownVersionMismatch, got {:?}", other),
        }
    }

    /// Every tree that keeps its elements in one Merk, with the word a
    /// layer has for a layer of such trees.
    fn merk_trees() -> [(TreeType, EstimatedSumTrees); 12] {
        use EstimatedSumTrees::*;
        [
            (TreeType::NormalTree, NoSumTrees),
            (TreeType::SumTree, AllSumTrees),
            (TreeType::BigSumTree, AllBigSumTrees),
            (TreeType::CountTree, AllCountTrees),
            (TreeType::CountSumTree, AllCountSumTrees),
            (TreeType::ProvableCountTree, AllProvableCountTrees),
            (TreeType::ProvableCountSumTree, AllProvableCountSumTrees),
            (TreeType::ProvableSumTree, AllProvableSumTrees),
            (TreeType::ProvableBigSumTree, AllProvableBigSumTrees),
            (
                TreeType::ProvableCountProvableSumTree,
                AllProvableCountProvableSumTrees,
            ),
            (
                TreeType::ProvableCountProvableBigSumTree,
                AllProvableCountProvableBigSumTrees,
            ),
            (TreeType::CountBigSumTree, AllCountBigSumTrees),
        ]
    }

    #[test]
    fn es2_a_layer_of_trees_is_sized_as_the_cost_of_their_elements_on_grove_version_6() {
        use crate::tree_type::CostSize;
        for (tree_type, trees) in merk_trees() {
            // What the element of such a tree costs over a tree with no
            // aggregate: 9 bytes for each count and sum, 16 for a wide sum.
            let aggregates = tree_type.cost_size() - TreeType::NormalTree.cost_size();
            assert_eq!(
                trees.estimated_size(&GROVE_V6).unwrap(),
                aggregates,
                "{tree_type}"
            );
            // Grove version 5 sizes them as the feature of a node: a byte
            // less for each count and sum.
            let as_a_node = tree_type.inner_node_type().cost();
            assert_eq!(
                trees.estimated_size(&GROVE_V5).unwrap(),
                as_a_node,
                "{tree_type}"
            );
            let counts_and_sums = match aggregates {
                0 | 16 => 0,
                9 | 25 => 1,
                18 => 2,
                other => panic!("{tree_type}: {other} bytes of aggregates"),
            };
            assert_eq!(aggregates - as_a_node, counts_and_sums, "{tree_type}");
        }
        // A layer of two kinds: the average of the two, by weight.
        let mixed = EstimatedSumTrees::SomeSumTrees {
            sum_trees_weight: 1,
            big_sum_trees_weight: 0,
            count_trees_weight: 0,
            count_sum_trees_weight: 2,
            non_sum_trees_weight: 1,
            provable_sum_trees_weight: 0,
            provable_count_trees_weight: 0,
            provable_count_sum_trees_weight: 0,
            provable_count_provable_sum_trees_weight: 0,
            provable_count_provable_big_sum_trees_weight: 0,
            count_big_sum_trees_weight: 0,
            provable_big_sum_trees_weight: 0,
        };
        assert_eq!(mixed.estimated_size(&GROVE_V6).unwrap(), (9 + 2 * 18) / 4);
        assert_eq!(mixed.estimated_size(&GROVE_V5).unwrap(), (8 + 2 * 16) / 4);
    }

    #[test]
    fn es2_a_level_has_the_fetch_of_each_node_below_its_root_on_grove_version_6() {
        for levels in 0..=6u32 {
            for sizes in [
                EstimatedLayerSizes::AllItems(8, 32, None),
                EstimatedLayerSizes::AllSubtrees(8, EstimatedSumTrees::NoSumTrees, None),
            ] {
                let info = EstimatedLayerInformation {
                    tree_type: TreeType::NormalTree,
                    estimated_layer_count: EstimatedLayerCount::EstimatedLevel(levels, false),
                    estimated_layer_sizes: sizes,
                };
                let mut released = OperationCost::default();
                add_average_case_merk_propagate(&mut released, &info, &GROVE_V5).unwrap();
                let mut stopped = OperationCost::default();
                add_average_case_merk_propagate(&mut stopped, &info, &GROVE_V6).unwrap();
                // One seek more for each node it rewrites but the root,
                // and nothing else: the sizes are those of grove version 5.
                // It rewrites a node for each level, and beyond one level
                // a node beside the way.
                let rewritten = levels + u32::from(levels > 1);
                assert_eq!(released.seek_count, rewritten, "{levels} levels");
                assert_eq!(
                    stopped.seek_count,
                    released.seek_count + rewritten.saturating_sub(1),
                    "{levels} levels"
                );
                stopped.seek_count = released.seek_count;
                assert_eq!(stopped, released, "{levels} levels");
            }
        }
        // A height no Merk can have is refused, and nothing is added.
        for unreal in [92, u32::MAX] {
            let info = EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: EstimatedLayerCount::EstimatedLevel(unreal, false),
                estimated_layer_sizes: EstimatedLayerSizes::AllItems(8, 32, None),
            };
            let mut cost = OperationCost::default();
            assert!(matches!(
                add_average_case_merk_propagate(&mut cost, &info, &GROVE_V6),
                Err(Error::Overflow(_))
            ));
            assert_eq!(cost, OperationCost::default());
        }
    }

    #[test]
    fn es2_the_levels_of_a_count_are_the_fewest_a_tree_of_that_count_has() {
        use EstimatedLayerCount::{ApproximateElements, EstimatedLevel, PotentiallyAtMaxElements};
        // A tree of `l` levels holds `2^l - 1` nodes at the most.
        assert_eq!(ApproximateElements(0).fewest_levels(), 0);
        for levels in 1..=31u32 {
            let full = (1u32 << levels) - 1;
            assert_eq!(ApproximateElements(full).fewest_levels(), levels);
            assert_eq!(ApproximateElements(full + 1).fewest_levels(), levels + 1);
            if levels > 1 {
                assert_eq!(ApproximateElements(full - 1).fewest_levels(), levels);
            }
        }
        assert_eq!(ApproximateElements(u32::MAX).fewest_levels(), 32);
        assert_eq!(PotentiallyAtMaxElements.fewest_levels(), 32);
        assert_eq!(EstimatedLevel(17, false).fewest_levels(), 17);
        // Up to a million elements the two counts of levels are one.
        for count in (0..=1_048_576u32).step_by(7).chain(1_048_570..=1_048_580) {
            assert_eq!(
                ApproximateElements(count).fewest_levels(),
                ApproximateElements(count).estimate_levels(),
                "{count}"
            );
        }
        // 2,097,152 elements do not fit 21 levels, which is what the
        // logarithm gives. Grove version 6 estimates a level of them by 22:
        // 23 nodes and 22 fetches.
        let count = ApproximateElements(2_097_152);
        assert_eq!((count.estimate_levels(), count.fewest_levels()), (21, 22));
        let info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: count,
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(8, 32, None),
        };
        let mut cost = OperationCost::default();
        add_average_case_merk_propagate(&mut cost, &info, &GROVE_V6).unwrap();
        assert_eq!((cost.seek_count, cost.hash_node_calls), (23 + 22, 2 * 23));
        let mut earlier = OperationCost::default();
        add_average_case_merk_propagate(&mut earlier, &info, &GROVE_V5).unwrap();
        assert_eq!((earlier.seek_count, earlier.hash_node_calls), (22, 2 * 22));
    }

    #[test]
    fn es2_the_size_of_a_value_too_long_to_count_is_refused_on_grove_version_6() {
        // Version 1 of the size adds the aggregates of a tree, its flags
        // and 3 in 32 bits. A count is 8 bytes there on grove version 5 and
        // 9 on grove version 6, so these flags are the longest version 5
        // counts.
        let flags = u32::MAX - 11;
        let trees =
            EstimatedLayerSizes::AllSubtrees(1, EstimatedSumTrees::AllCountTrees, Some(flags));
        assert_eq!(
            trees.value_with_feature_and_flags_size(&GROVE_V5).unwrap(),
            u32::MAX
        );
        assert!(matches!(
            trees.value_with_feature_and_flags_size(&GROVE_V6),
            Err(Error::Overflow(_))
        ));
        // The size of a subtree of the layer is the same sum.
        assert_eq!(
            trees
                .subtree_with_feature_and_flags_size(&GROVE_V5)
                .unwrap(),
            u32::MAX
        );
        assert!(matches!(
            trees.subtree_with_feature_and_flags_size(&GROVE_V6),
            Err(Error::Overflow(_))
        ));
        let mixed = EstimatedLayerSizes::Mix {
            subtrees_size: Some((1, EstimatedSumTrees::AllCountTrees, Some(flags), 1)),
            items_size: None,
            references_size: None,
            items_with_sum_item_size: None,
            references_with_sum_item_size: None,
        };
        assert_eq!(
            mixed
                .subtree_with_feature_and_flags_size(&GROVE_V5)
                .unwrap(),
            u32::MAX
        );
        assert!(matches!(
            mixed.subtree_with_feature_and_flags_size(&GROVE_V6),
            Err(Error::Overflow(_))
        ));
        // Below what an estimate refuses the two differ by the byte.
        for sizes in [
            EstimatedLayerSizes::AllSubtrees(1, EstimatedSumTrees::AllCountTrees, Some(77)),
            EstimatedLayerSizes::AllSubtrees(1, EstimatedSumTrees::NoSumTrees, None),
            EstimatedLayerSizes::AllItems(8, 300, Some(5)),
            EstimatedLayerSizes::AllReference(8, 300, None),
        ] {
            let aggregates_longer = u32::from(matches!(
                sizes,
                EstimatedLayerSizes::AllSubtrees(_, EstimatedSumTrees::AllCountTrees, _)
            ));
            assert_eq!(
                sizes.value_with_feature_and_flags_size(&GROVE_V6).unwrap(),
                sizes.value_with_feature_and_flags_size(&GROVE_V5).unwrap() + aggregates_longer
            );
        }
    }

    #[test]
    fn es2_no_size_of_a_layer_overflows_an_estimate_of_grove_version_6() {
        // Sizes at the end of 32 bits, on both sides of what an estimate
        // refuses, for every kind of layer: each estimate of grove version
        // 6 counts them or refuses them. A panic fails the test.
        let edges: Vec<u32> = [0u32, 1, 2, 3, 5, 9, 11, 12, 13, 15, 16, 17, 60]
            .into_iter()
            .map(|below| u32::MAX - below)
            .chain([u32::MAX - 65_535, u32::MAX - 65_536, u32::MAX - 65_537])
            .chain([0, 1, 65_535, 1 << 31])
            .collect();
        let trees = [
            EstimatedSumTrees::NoSumTrees,
            EstimatedSumTrees::AllSumTrees,
            EstimatedSumTrees::AllBigSumTrees,
            EstimatedSumTrees::AllCountTrees,
            EstimatedSumTrees::AllCountSumTrees,
        ];
        let mut counted = 0;
        let mut refused = 0;
        for value in &edges {
            for flags in [
                None,
                Some(0),
                Some(1),
                Some(65_536),
                Some(u32::MAX - 11),
                Some(u32::MAX),
            ] {
                let mut layers = vec![
                    EstimatedLayerSizes::AllItems(255, *value, flags),
                    EstimatedLayerSizes::AllReference(255, *value, flags),
                    EstimatedLayerSizes::AllItemsWithSumItem(255, *value, flags),
                    EstimatedLayerSizes::AllReferencesWithSumItem(255, *value, flags),
                    EstimatedLayerSizes::Mix {
                        subtrees_size: Some((
                            255,
                            EstimatedSumTrees::AllCountTrees,
                            Some(*value),
                            1,
                        )),
                        items_size: Some((255, *value, flags, 1)),
                        references_size: Some((255, *value, flags, 0)),
                        items_with_sum_item_size: Some((255, *value, flags, 255)),
                        references_with_sum_item_size: None,
                    },
                ];
                layers.extend(
                    trees
                        .iter()
                        .map(|kind| EstimatedLayerSizes::AllSubtrees(255, *kind, Some(*value))),
                );
                for sizes in layers {
                    for count in [
                        EstimatedLayerCount::ApproximateElements(1),
                        EstimatedLayerCount::EstimatedLevel(91, false),
                        EstimatedLayerCount::PotentiallyAtMaxElements,
                    ] {
                        let info = EstimatedLayerInformation {
                            tree_type: TreeType::CountSumTree,
                            estimated_layer_count: count,
                            estimated_layer_sizes: sizes,
                        };
                        let size = sizes.value_with_feature_and_flags_size(&GROVE_V6);
                        // Of a layer with no tree this is another error.
                        let _ = sizes.subtree_with_feature_and_flags_size(&GROVE_V6);
                        let mut cost = OperationCost::default();
                        let level = add_average_case_merk_propagate(&mut cost, &info, &GROVE_V6);
                        let mut cost = OperationCost::default();
                        let finalize =
                            add_average_case_merk_finalize(&mut cost, &info, 3, &GROVE_V6);
                        for result in [size.map(|_| ()), level, finalize] {
                            match result {
                                Ok(()) => counted += 1,
                                Err(_) => refused += 1,
                            }
                        }
                    }
                }
            }
        }
        assert!(
            counted > 0 && refused > 0,
            "{counted} counted, {refused} refused"
        );
    }

    #[test]
    fn es2_a_level_of_long_elements_is_counted_in_full_or_refused() {
        let level_of = |tree_type, levels, sizes| {
            let info = EstimatedLayerInformation {
                tree_type,
                estimated_layer_count: EstimatedLayerCount::EstimatedLevel(levels, false),
                estimated_layer_sizes: sizes,
            };
            let mut cost = OperationCost::default();
            let result = add_average_case_merk_propagate(&mut cost, &info, &GROVE_V6);
            (result, cost)
        };
        let level = |levels, sizes| level_of(TreeType::NormalTree, levels, sizes);
        // 33 nodes of an item of 130,150,400 bytes under a key of two: the
        // replaced bytes fit 32 bits and the loaded bytes do not. The
        // estimate of grove version 5 multiplies both in 32 bits.
        let (result, cost) = level(32, EstimatedLayerSizes::AllItems(2, 130_150_400, None));
        result.unwrap();
        assert_eq!(cost.storage_cost.replaced_bytes, 4_294_966_731);
        assert_eq!(cost.storage_loaded_bytes, 4_294_967_886);
        assert_eq!(cost.seek_count, 33 + 32);
        assert_eq!(cost.hash_node_calls, 2 * 33);

        // One node more, and the replaced bytes no longer fit their field.
        // Nothing is added.
        let (result, cost) = level(33, EstimatedLayerSizes::AllItems(2, 130_150_400, None));
        assert!(matches!(result, Err(Error::Overflow(_))));
        assert_eq!(cost, OperationCost::default());

        // One node of an item of a gigabyte is counted.
        let (result, cost) = level(1, EstimatedLayerSizes::AllItems(1, 1 << 30, None));
        result.unwrap();
        assert_eq!(cost.storage_cost.replaced_bytes, 1_073_741_931);
        assert_eq!(cost.storage_loaded_bytes, 1_073_741_965);

        // The longest element an estimate counts is 65,536 bytes short of
        // what 32 bits hold, its flags among them. One node of it is
        // counted in every kind of layer and of tree, and one byte more is
        // refused, with nothing added.
        let most = u32::MAX - 65_536;
        let layers = |value: u32, flags: Option<u32>| {
            let kind = |weight| Some((255, value, flags, weight));
            let none = None;
            vec![
                EstimatedLayerSizes::AllItems(255, value, flags),
                EstimatedLayerSizes::AllReference(255, value, flags),
                EstimatedLayerSizes::AllItemsWithSumItem(255, value, flags),
                EstimatedLayerSizes::AllReferencesWithSumItem(255, value, flags),
                EstimatedLayerSizes::Mix {
                    subtrees_size: none,
                    items_size: kind(255),
                    references_size: kind(255),
                    items_with_sum_item_size: kind(255),
                    references_with_sum_item_size: kind(255),
                },
            ]
        };
        let trees_with = |flags: u32| {
            let trees = EstimatedSumTrees::AllBigSumTrees;
            vec![
                EstimatedLayerSizes::AllSubtrees(255, trees, Some(flags)),
                EstimatedLayerSizes::Mix {
                    subtrees_size: Some((255, trees, Some(flags), 255)),
                    items_size: None,
                    references_size: None,
                    items_with_sum_item_size: None,
                    references_with_sum_item_size: None,
                },
            ]
        };
        for tree_type in [
            TreeType::NormalTree,
            TreeType::SumTree,
            TreeType::CountSumTree,
            TreeType::ProvableCountProvableBigSumTree,
            TreeType::CountBigSumTree,
        ] {
            let counted = layers(most, None)
                .into_iter()
                .chain(layers(most / 2, Some(most - most / 2)))
                .chain(trees_with(most));
            for sizes in counted {
                let (result, cost) = level_of(tree_type, 1, sizes);
                result.unwrap_or_else(|e| panic!("{tree_type} {sizes:?}: {e}"));
                assert!(cost.storage_cost.replaced_bytes > most);
            }
            let refused = layers(most + 1, None)
                .into_iter()
                .chain(layers(most / 2, Some(most - most / 2 + 1)))
                .chain(layers(u32::MAX, Some(u32::MAX)))
                .chain(trees_with(most + 1))
                .chain(trees_with(u32::MAX));
            for sizes in refused {
                for levels in [1, 2, 91] {
                    let (result, cost) = level_of(tree_type, levels, sizes);
                    assert!(
                        matches!(result, Err(Error::Overflow(_))),
                        "{tree_type} {sizes:?} at {levels} levels"
                    );
                    assert_eq!(cost, OperationCost::default());
                }
            }
            // The longest at the most levels: refused for the product.
            for sizes in layers(most, None).into_iter().chain(trees_with(most)) {
                let (result, cost) = level_of(tree_type, 91, sizes);
                assert!(matches!(result, Err(Error::Overflow(_))), "{sizes:?}");
                assert_eq!(cost, OperationCost::default());
            }
        }
    }

    #[test]
    fn es2_a_level_of_several_kinds_of_element_is_sized_as_the_version_before_sizes_it() {
        // A layer of several kinds is a weighted average, which the version
        // before rounds once, after the nodes have multiplied it. Grove
        // version 6 has the same bytes, and the fetches. The version before
        // is taken with the rest of grove version 6, which sizes the
        // aggregates of a tree by a version of its own.
        let mut before = GROVE_V6.clone();
        before
            .merk_versions
            .average_case_costs
            .add_average_case_merk_propagate = GROVE_V5
            .merk_versions
            .average_case_costs
            .add_average_case_merk_propagate;
        let sizes = EstimatedLayerSizes::Mix {
            subtrees_size: Some((8, EstimatedSumTrees::AllSumTrees, Some(3), 1)),
            items_size: Some((3, 10, None, 1)),
            references_size: Some((3, 11, None, 1)),
            items_with_sum_item_size: Some((5, 7, Some(2), 3)),
            references_with_sum_item_size: None,
        };
        let mut rounded_after = false;
        for levels in 0..=12u32 {
            let info = EstimatedLayerInformation {
                tree_type: TreeType::SumTree,
                estimated_layer_count: EstimatedLayerCount::EstimatedLevel(levels, false),
                estimated_layer_sizes: sizes,
            };
            let mut released = OperationCost::default();
            add_average_case_merk_propagate(&mut released, &info, &before).unwrap();
            let mut stopped = OperationCost::default();
            add_average_case_merk_propagate(&mut stopped, &info, &GROVE_V6).unwrap();
            assert_eq!(
                stopped.seek_count,
                released.seek_count + if levels > 1 { levels } else { 0 }
            );
            stopped.seek_count = released.seek_count;
            assert_eq!(stopped, released, "{levels} levels");
            // The bytes of one node, rounded and then multiplied, are fewer.
            let one = EstimatedLayerInformation {
                estimated_layer_count: EstimatedLayerCount::EstimatedLevel(1, false),
                ..info
            };
            let mut one_node = OperationCost::default();
            add_average_case_merk_propagate(&mut one_node, &one, &before).unwrap();
            let nodes = levels + u32::from(levels > 1);
            rounded_after |=
                nodes * one_node.storage_cost.replaced_bytes < released.storage_cost.replaced_bytes;
        }
        assert!(rounded_after);
    }

    #[test]
    fn es2_a_layer_of_one_kind_is_counted_the_same_as_a_mix_of_it() {
        // A layer of items, given as items and given as a `Mix` that names
        // items alone, is one layer. Two levels are three nodes, and items
        // of 1,431,655,658 bytes under keys of one byte replace all that a
        // field of 32 bits holds and load 102 bytes more, which the 64 bits
        // of that field hold. Grove version 6 counts both the same. The
        // version before refuses the `Mix`, whose loaded bytes it holds to
        // 32 bits.
        let size = 1_431_655_658;
        let level = |sizes, version: &GroveVersion| {
            let info = EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: EstimatedLayerCount::EstimatedLevel(2, false),
                estimated_layer_sizes: sizes,
            };
            let mut cost = OperationCost::default();
            add_average_case_merk_propagate(&mut cost, &info, version).map(|()| cost)
        };
        let of_items_alone = |size| EstimatedLayerSizes::Mix {
            subtrees_size: None,
            items_size: Some((1, size, None, 1)),
            references_size: None,
            items_with_sum_item_size: None,
            references_with_sum_item_size: None,
        };
        let as_items = level(EstimatedLayerSizes::AllItems(1, size, None), &GROVE_V6)
            .expect("the level is counted");
        assert_eq!(
            (
                as_items.storage_cost.replaced_bytes,
                as_items.storage_loaded_bytes
            ),
            (4_294_967_295, 4_294_967_397)
        );
        assert_eq!(
            level(of_items_alone(size), &GROVE_V6).expect("the level is counted"),
            as_items
        );
        assert!(matches!(
            level(of_items_alone(size), &GROVE_V5),
            Err(Error::Overflow(_))
        ));
        // A byte more is more replaced bytes than their field holds: both
        // are refused.
        for sizes in [
            EstimatedLayerSizes::AllItems(1, size + 1, None),
            of_items_alone(size + 1),
        ] {
            assert!(matches!(level(sizes, &GROVE_V6), Err(Error::Overflow(_))));
        }
        // And a kind of each: the weights, the order and the rounding of
        // the version before, at sizes whose loaded bytes fit 32 bits.
        for weight in [1u8, 2, 7, 255] {
            assert_eq!(
                level(
                    EstimatedLayerSizes::Mix {
                        subtrees_size: None,
                        items_size: Some((1, 1_000, None, weight)),
                        references_size: None,
                        items_with_sum_item_size: None,
                        references_with_sum_item_size: None,
                    },
                    &GROVE_V6
                )
                .expect("the level is counted")
                .storage_loaded_bytes,
                level(EstimatedLayerSizes::AllItems(1, 1_000, None), &GROVE_V6)
                    .expect("the level is counted")
                    .storage_loaded_bytes
            );
        }
    }

    #[test]
    fn es2_a_kind_of_no_weight_in_a_layer_of_several_is_not_sized() {
        // A `Mix` can name a kind and give it no weight: it adds nothing to
        // the average. Trees whose kinds all have no weight cannot be sized
        // at all (their aggregates are an average over those weights), and
        // a layer that names them so, with no weight, is the layer without
        // them: one level is a seek and two hash calls, and no bytes, as
        // on the version before.
        let trees_of_no_kind = EstimatedSumTrees::SomeSumTrees {
            sum_trees_weight: 0,
            big_sum_trees_weight: 0,
            count_trees_weight: 0,
            count_sum_trees_weight: 0,
            non_sum_trees_weight: 0,
            provable_sum_trees_weight: 0,
            provable_count_trees_weight: 0,
            provable_count_sum_trees_weight: 0,
            provable_count_provable_sum_trees_weight: 0,
            provable_count_provable_big_sum_trees_weight: 0,
            count_big_sum_trees_weight: 0,
            provable_big_sum_trees_weight: 0,
        };
        assert!(matches!(
            trees_of_no_kind.estimated_size(&GROVE_V6),
            Err(Error::DivideByZero(_))
        ));
        let level = |sizes, version: &GroveVersion| {
            let info = EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: EstimatedLayerCount::EstimatedLevel(1, false),
                estimated_layer_sizes: sizes,
            };
            let mut cost = OperationCost::default();
            add_average_case_merk_propagate(&mut cost, &info, version).map(|()| cost)
        };
        let mix = |subtrees_size, items_size| EstimatedLayerSizes::Mix {
            subtrees_size,
            items_size,
            references_size: None,
            items_with_sum_item_size: None,
            references_with_sum_item_size: None,
        };
        let unweighed = Some((8, trees_of_no_kind, None, 0));
        let nothing = OperationCost {
            seek_count: 1,
            hash_node_calls: 2,
            ..Default::default()
        };
        for version in [&GROVE_V5, &GROVE_V6] {
            assert_eq!(level(mix(unweighed, None), version).unwrap(), nothing);
            assert_eq!(level(mix(None, None), version).unwrap(), nothing);
        }
        // Beside a kind that has a weight, grove version 6 leaves the one
        // without out, and the version before sizes it and fails.
        let items = Some((3, 10, None, 2));
        assert_eq!(
            level(mix(unweighed, items), &GROVE_V6).unwrap(),
            level(mix(None, items), &GROVE_V6).unwrap()
        );
        assert!(matches!(
            level(mix(unweighed, items), &GROVE_V5),
            Err(Error::DivideByZero(_))
        ));
    }

    fn finalize_of(levels: u32, marked: u32, version: &GroveVersion) -> OperationCost {
        let info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(levels, false),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                8,
                EstimatedSumTrees::NoSumTrees,
                None,
            ),
        };
        let mut cost = OperationCost::default();
        add_average_case_merk_finalize(&mut cost, &info, marked, version).unwrap();
        cost
    }

    #[test]
    fn es3_a_finalize_in_one_merk_is_the_nodes_above_its_marked_elements() {
        // A node of this layer is a tree element with no flags under a key
        // of eight bytes, in a tree whose nodes have no feature.
        //
        // Its value costs 82 bytes: 3 for a tree, 32 for a hash and 2, and
        // the 45 of the link to it (the key and its length byte, a hash,
        // two heights and two tags).
        let value_cost = 3 + 32 + 2 + 45;
        // It loads 123 bytes: the 37 of the value, 41 for the key under its
        // prefix with its length, and the link.
        let bytes_of_a_node = 37 + (32 + 8 + 1) + 45;

        // Two levels and one marked element: the root above it is put and
        // hashed. The element is fetched on the way and read by its key;
        // the root's fetch is the open's. The bytes are those of the root,
        // of the element, and of the element again.
        assert_eq!(
            finalize_of(2, 1, &GROVE_V6),
            OperationCost {
                seek_count: 1 + 1 + 1,
                storage_cost: grovedb_costs::storage_cost::StorageCost {
                    replaced_bytes: value_cost,
                    ..Default::default()
                },
                storage_loaded_bytes: 3 * bytes_of_a_node,
                hash_node_calls: 2,
                ..Default::default()
            }
        );

        // A Merk of one level: its only node is the marked element, which
        // the open fetched. Nothing is above it and nothing is read again.
        assert_eq!(
            finalize_of(1, 1, &GROVE_V6),
            OperationCost {
                storage_loaded_bytes: bytes_of_a_node,
                ..Default::default()
            }
        );
        // A layer that is estimated to be empty hosts a marked element all
        // the same: it is taken as a Merk of one level.
        assert_eq!(finalize_of(0, 1, &GROVE_V6), finalize_of(1, 1, &GROVE_V6));
        // Nothing marked, nothing charged.
        assert_eq!(finalize_of(5, 0, &GROVE_V6), OperationCost::default());

        // Four levels and three marked elements: six nodes above them, nine
        // changed. The puts of the six, the fetches of all nine but the
        // root, and the three reads by key.
        assert_eq!(crate::estimated_costs::nodes_above_marked(4, 3), 6);
        assert_eq!(
            finalize_of(4, 3, &GROVE_V6),
            OperationCost {
                seek_count: 6 + (9 - 1) + 3,
                storage_cost: grovedb_costs::storage_cost::StorageCost {
                    replaced_bytes: 6 * value_cost,
                    ..Default::default()
                },
                storage_loaded_bytes: (9 + 3) * bytes_of_a_node,
                hash_node_calls: 2 * 6,
                ..Default::default()
            }
        );

        // A height no Merk can have is refused, and nothing is added.
        let unreal = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(u32::MAX, false),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(8, 32, None),
        };
        let mut cost = OperationCost::default();
        assert!(matches!(
            add_average_case_merk_finalize(&mut cost, &unreal, 1, &GROVE_V6),
            Err(Error::Overflow(_))
        ));
        assert!(matches!(
            add_average_case_merk_propagate(&mut cost, &unreal, &GROVE_V6),
            Err(Error::Overflow(_))
        ));
        assert_eq!(cost, OperationCost::default());
    }

    #[test]
    fn es3_a_finalize_counts_the_levels_of_a_count_in_whole_numbers() {
        // 2,097,152 trees do not fit 21 levels, which is what the logarithm
        // gives: a finalize in a Merk of that count is one in 22.
        let count = EstimatedLayerCount::ApproximateElements(2_097_152);
        assert_eq!((count.estimate_levels(), count.fewest_levels()), (21, 22));
        let trees = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: count,
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                8,
                EstimatedSumTrees::NoSumTrees,
                None,
            ),
        };
        for marked in [1, 5] {
            let mut by_count = OperationCost::default();
            add_average_case_merk_finalize(&mut by_count, &trees, marked, &GROVE_V6).unwrap();
            assert_eq!(by_count, finalize_of(22, marked, &GROVE_V6));
        }
    }

    #[test]
    fn es3_a_refused_estimate_adds_nothing_to_the_cost_it_was_given() {
        use crate::estimated_costs::worst_case_costs::{
            add_worst_case_merk_finalize, WorstCaseLayerInformation,
        };
        // A full Merk of 17 levels has 65,536 leaves, and a finalize of all
        // of them replaces more bytes than 32 bits count: the worst case
        // by the biggest value, and the average case with flags of 65,536
        // bytes on each tree.
        let marked = 65_536;
        let trees = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(131_071),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                3,
                EstimatedSumTrees::NoSumTrees,
                Some(65_536),
            ),
        };
        let worst = WorstCaseLayerInformation::NumberOfLevels(17);
        // From a cost of nothing, and from one that is all but full in a
        // field: each refusal leaves the cost it was given.
        let mut given = vec![OperationCost::default()];
        for field in 0..4 {
            let mut cost = OperationCost {
                seek_count: 11,
                storage_loaded_bytes: 13,
                hash_node_calls: 17,
                ..Default::default()
            };
            cost.storage_cost.replaced_bytes = 19;
            match field {
                0 => cost.seek_count = u32::MAX - 1,
                1 => cost.storage_loaded_bytes = u64::MAX - 1,
                2 => cost.hash_node_calls = u32::MAX - 1,
                _ => cost.storage_cost.replaced_bytes = u32::MAX - 1,
            }
            given.push(cost);
        }
        let mut refused = 0;
        for before in given {
            let estimates: [&dyn Fn(&mut OperationCost) -> Result<(), Error>; 4] = [
                &|cost| add_average_case_merk_finalize(cost, &trees, marked, &GROVE_V6),
                &|cost| {
                    add_worst_case_merk_finalize(
                        cost,
                        &worst,
                        marked,
                        crate::estimated_costs::worst_case_costs::MERK_BIGGEST_VALUE_SIZE,
                        &GROVE_V6,
                    )
                },
                &|cost| add_average_case_merk_finalize(cost, &trees, 1, &GROVE_V6),
                &|cost| {
                    add_worst_case_merk_finalize(
                        cost,
                        &worst,
                        1,
                        crate::estimated_costs::worst_case_costs::MERK_BIGGEST_VALUE_SIZE,
                        &GROVE_V6,
                    )
                },
            ];
            for (number, estimate) in estimates.iter().enumerate() {
                let mut cost = before.clone();
                match estimate(&mut cost) {
                    Ok(()) => assert_ne!(cost, before, "{number}"),
                    Err(Error::Overflow(_)) => {
                        assert_eq!(cost, before, "{number}");
                        refused += 1;
                    }
                    Err(other) => panic!("{number}: {other}"),
                }
            }
        }
        // The two finalizes of every leaf are refused from nothing, and
        // each of the four from a cost that is all but full in some field.
        assert!(refused >= 2 + 4, "{refused}");
    }

    #[test]
    fn es2_a_refused_level_adds_nothing_to_the_cost_it_was_given() {
        use crate::estimated_costs::worst_case_costs::{
            add_worst_case_merk_propagate_versioned, WorstCaseLayerInformation,
        };
        // The same of the estimates of a level: given a cost that is all
        // but full in a field, each refuses and leaves the cost as it was.
        let trees = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::ApproximateElements(131_071),
            estimated_layer_sizes: EstimatedLayerSizes::AllSubtrees(
                3,
                EstimatedSumTrees::NoSumTrees,
                Some(65_536),
            ),
        };
        let worst = WorstCaseLayerInformation::NumberOfLevels(17);
        for case in [false, true] {
            let mut counted = OperationCost::default();
            let level = |cost: &mut OperationCost| {
                if case {
                    add_worst_case_merk_propagate_versioned(cost, &worst, &GROVE_V6)
                } else {
                    add_average_case_merk_propagate(cost, &trees, &GROVE_V6)
                }
            };
            level(&mut counted).expect("the level is estimated");
            assert_ne!(counted, OperationCost::default());
            for field in 0..4 {
                let mut before = OperationCost {
                    seek_count: 11,
                    storage_loaded_bytes: 13,
                    hash_node_calls: 17,
                    ..Default::default()
                };
                before.storage_cost.replaced_bytes = 19;
                match field {
                    0 => before.seek_count = u32::MAX - 1,
                    1 => before.storage_loaded_bytes = u64::MAX - 1,
                    2 => before.hash_node_calls = u32::MAX - 1,
                    _ => before.storage_cost.replaced_bytes = u32::MAX - 1,
                }
                let mut cost = before.clone();
                assert!(
                    matches!(level(&mut cost), Err(Error::Overflow(_))),
                    "{field}"
                );
                assert_eq!(cost, before, "{case} {field}");
            }
        }
    }

    #[test]
    fn es3_a_grove_version_with_no_finalize_has_no_estimate_of_one_in_a_merk() {
        for version in grovedb_version::version::GROVE_VERSIONS {
            let info = EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: EstimatedLayerCount::EstimatedLevel(2, false),
                estimated_layer_sizes: EstimatedLayerSizes::AllItems(8, 32, None),
            };
            let mut cost = OperationCost::default();
            assert!(matches!(
                add_average_case_merk_finalize(&mut cost, &info, 1, version),
                Err(Error::NotSupported(_))
            ));
            assert_eq!(cost, OperationCost::default());
        }
        let mut unknown = GROVE_V6.clone();
        unknown
            .merk_versions
            .average_case_costs
            .add_average_case_merk_finalize = 1;
        let info = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: EstimatedLayerCount::EstimatedLevel(2, false),
            estimated_layer_sizes: EstimatedLayerSizes::AllItems(8, 32, None),
        };
        assert!(matches!(
            add_average_case_merk_finalize(&mut OperationCost::default(), &info, 1, &unknown),
            Err(Error::VersionError(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                    ref known_versions,
                    received: 1,
                    ..
                }
            )) if known_versions == &[0]
        ));
    }

    /// `value_with_feature_and_flags_size` dispatcher rejects unknown
    /// versions — regression for the new dispatch path.
    #[test]
    fn test_value_with_feature_and_flags_size_unknown_version_error() {
        let mut bad = GroveVersion::latest().clone();
        bad.merk_versions
            .average_case_costs
            .value_with_feature_and_flags_size = 99;
        let layer = EstimatedLayerSizes::AllItems(8, 32, None);
        let err = layer.value_with_feature_and_flags_size(&bad).unwrap_err();
        assert!(matches!(
            err,
            Error::VersionError(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch { .. }
            )
        ));
    }

    /// Same as above but on the v1 propagate dispatch (grove v3 path),
    /// so we cover the `references_with_sum_item_size` branch in the v1
    /// Mix arms — the items-only version of this test already covers
    /// the items-with-sum branch.
    #[test]
    fn test_propagate_v1_mix_with_references_with_sum_item() {
        let layer_count = EstimatedLayerCount::EstimatedLevel(3, false);
        let base = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: layer_count,
            estimated_layer_sizes: EstimatedLayerSizes::Mix {
                subtrees_size: None,
                items_size: None,
                references_size: Some((8, 24, Some(1), 1)),
                items_with_sum_item_size: None,
                references_with_sum_item_size: None,
            },
        };
        let with_sum = EstimatedLayerInformation {
            tree_type: TreeType::NormalTree,
            estimated_layer_count: layer_count,
            estimated_layer_sizes: EstimatedLayerSizes::Mix {
                subtrees_size: None,
                items_size: None,
                references_size: Some((8, 24, Some(1), 1)),
                items_with_sum_item_size: None,
                references_with_sum_item_size: Some((8, 24, Some(1), 1)),
            },
        };

        let mut base_cost = OperationCost::default();
        add_average_case_merk_propagate(&mut base_cost, &base, GroveVersion::latest()).unwrap();
        let mut sum_cost = OperationCost::default();
        add_average_case_merk_propagate(&mut sum_cost, &with_sum, GroveVersion::latest()).unwrap();

        assert!(sum_cost.storage_cost.replaced_bytes > base_cost.storage_cost.replaced_bytes);
        assert!(sum_cost.storage_loaded_bytes > base_cost.storage_loaded_bytes);
    }
}
