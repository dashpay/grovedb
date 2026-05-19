//! Average case costs for Merk

// One submodule per versioned function, each laid out as
// `<function>/{mod.rs, v0.rs, v1.rs, ...}` — `mod.rs` holds the
// version-dispatching wrapper and the `v*.rs` files hold the
// per-version implementations.
#[cfg(feature = "minimal")]
mod add_average_case_merk_propagate;
#[cfg(feature = "minimal")]
mod estimated_sum_trees_size;
#[cfg(feature = "minimal")]
mod value_with_feature_and_flags_size;

#[cfg(feature = "minimal")]
pub use add_average_case_merk_propagate::{
    add_average_case_merk_propagate, average_case_merk_propagate,
};

#[cfg(feature = "minimal")]
use grovedb_costs::OperationCost;
#[cfg(feature = "minimal")]
use grovedb_version::version::GroveVersion;
#[cfg(feature = "minimal")]
use integer_encoding::VarInt;

#[cfg(feature = "minimal")]
use crate::{
    error::Error,
    tree::{kv::KV, Link, TreeNode},
    HASH_BLOCK_SIZE, HASH_BLOCK_SIZE_U32, HASH_LENGTH, HASH_LENGTH_U32,
};
use crate::{merk::NodeType, tree_type::TreeType};

#[cfg(feature = "minimal")]
/// Average key size
pub type AverageKeySize = u8;
#[cfg(feature = "minimal")]
/// Average value size
pub type AverageValueSize = u32;
#[cfg(feature = "minimal")]
/// Average flags size
pub type AverageFlagsSize = u32;
#[cfg(feature = "minimal")]
/// Weight
pub type Weight = u8;

#[cfg(feature = "minimal")]
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
}

// `EstimatedSumTrees::estimated_size` and its v0/v1/v2 implementations
// live in the `estimated_sum_trees_size` submodule (one v*.rs per
// version).

#[cfg(feature = "minimal")]
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

#[cfg(feature = "minimal")]
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

    /// Returns the size of a subtree's feature and flags
    /// This only takes into account subtrees in the estimated layer info
    /// Only should be used when it is known to be a subtree
    pub fn subtree_with_feature_and_flags_size(
        &self,
        grove_version: &GroveVersion,
    ) -> Result<u32, Error> {
        match self {
            EstimatedLayerSizes::AllSubtrees(_, estimated_sum_trees, flags_size) => {
                // 1 for enum type
                // 1 for empty
                // 1 for flags size
                Ok(estimated_sum_trees.estimated_size(grove_version)?
                    + flags_size.unwrap_or_default()
                    + 3)
            }
            EstimatedLayerSizes::Mix { subtrees_size, .. } => match subtrees_size {
                None => Err(Error::WrongEstimatedCostsElementTypeForLevel(
                    "this layer is a mix but doesn't have subtrees",
                )),
                Some((_, est, fs, _)) => {
                    Ok(est.estimated_size(grove_version)? + fs.unwrap_or_default() + 3)
                }
            },
            _ => Err(Error::WrongEstimatedCostsElementTypeForLevel(
                "this layer needs to have trees",
            )),
        }
    }

    // `value_with_feature_and_flags_size` and its version-dispatched
    // implementations now live in the
    // `value_with_feature_and_flags_size` submodule.
}

#[cfg(feature = "minimal")]
/// Approximate element count
pub type ApproximateElementCount = u32;
#[cfg(feature = "minimal")]
/// Estimated level number
pub type EstimatedLevelNumber = u32;
#[cfg(feature = "minimal")]
/// Estimated to be empty
pub type EstimatedToBeEmpty = bool;

#[cfg(feature = "minimal")]
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

#[cfg(feature = "minimal")]
impl EstimatedLayerInformation {}

#[cfg(feature = "minimal")]
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

#[cfg(feature = "minimal")]
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
}

#[cfg(feature = "minimal")]
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

#[cfg(feature = "minimal")]
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

#[cfg(feature = "minimal")]
/// Add worst case for getting a merk tree
pub fn add_average_case_merk_has_value(
    cost: &mut OperationCost,
    not_prefixed_key_len: u32,
    estimated_element_size: u32,
) {
    cost.seek_count += 1;
    cost.storage_loaded_bytes += (not_prefixed_key_len + estimated_element_size) as u64;
}

#[cfg(feature = "minimal")]
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

#[cfg(feature = "minimal")]
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

#[cfg(feature = "minimal")]
/// Add average case for deletion from merk
pub fn add_average_case_merk_delete(cost: &mut OperationCost, _key_len: u32, value_len: u32) {
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
pub fn add_average_case_merk_root_hash(cost: &mut OperationCost) {
    cost.hash_node_calls += node_hash_update_count();
}

#[cfg(test)]
mod tests {
    use grovedb_costs::OperationCost;
    use grovedb_version::version::GroveVersion;

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

    /// `add_average_case_merk_propagate` rejects unknown versions
    /// (regression for the v2 dispatch row).
    #[test]
    fn test_propagate_unknown_version_error_lists_all_three() {
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
            ) => assert_eq!(known_versions, vec![0, 1, 2]),
            other => panic!("expected UnknownVersionMismatch, got {:?}", other),
        }
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
