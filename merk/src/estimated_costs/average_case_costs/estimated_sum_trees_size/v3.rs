//! `EstimatedSumTrees::estimated_size` v3: grove version 6.
//!
//! Identical to v2 except in what a tree's aggregates are sized as. v2
//! sizes them as the feature of a node of the tree's own Merk
//! (`inner_node_type().cost()`: 8 bytes for a count or a sum). The figure is
//! added to the length of a tree element, whose cost has each count and sum
//! at the 9 bytes of its longest varint (`TreeType::cost_size`), so v2 is a
//! byte short for each. v3 sizes them as the element's cost does. A sum of
//! 16 bytes is the same in both.
//!
//! On grove versions 1 to 5 a batch's estimate has every level up to the
//! root of the grove. A write that stops at a stop point has its own level
//! alone, and a finalize counts each node once, so a node is sized in full
//! (`docs/lignindb/cost-model.md`, section 16.5).

use crate::{
    error::Error,
    estimated_costs::{average_case_costs::EstimatedSumTrees, LAYER_COST_SIZE},
    tree_type::{CostSize, TreeType},
};

/// The bytes the aggregates of a tree of `tree_type` add to the cost of its
/// element, over a tree that keeps none.
fn aggregate_bytes(tree_type: TreeType) -> u32 {
    tree_type.cost_size() - LAYER_COST_SIZE
}

impl EstimatedSumTrees {
    pub(super) fn estimated_size_v3(&self) -> Result<u32, Error> {
        match self {
            EstimatedSumTrees::NoSumTrees => Ok(0),
            EstimatedSumTrees::SomeSumTrees {
                sum_trees_weight,
                big_sum_trees_weight,
                count_trees_weight,
                count_sum_trees_weight,
                non_sum_trees_weight,
                provable_sum_trees_weight,
                provable_count_trees_weight,
                provable_count_sum_trees_weight,
                provable_count_provable_sum_trees_weight,
                provable_count_provable_big_sum_trees_weight,
                count_big_sum_trees_weight,
                provable_big_sum_trees_weight,
            } => {
                let total_weight = (*sum_trees_weight as u32)
                    .checked_add(*big_sum_trees_weight as u32)
                    .and_then(|w| w.checked_add(*count_trees_weight as u32))
                    .and_then(|w| w.checked_add(*count_sum_trees_weight as u32))
                    .and_then(|w| w.checked_add(*non_sum_trees_weight as u32))
                    .and_then(|w| w.checked_add(*provable_sum_trees_weight as u32))
                    .and_then(|w| w.checked_add(*provable_count_trees_weight as u32))
                    .and_then(|w| w.checked_add(*provable_count_sum_trees_weight as u32))
                    .and_then(|w| w.checked_add(*provable_count_provable_sum_trees_weight as u32))
                    .and_then(|w| {
                        w.checked_add(*provable_count_provable_big_sum_trees_weight as u32)
                    })
                    .and_then(|w| w.checked_add(*count_big_sum_trees_weight as u32))
                    .and_then(|w| w.checked_add(*provable_big_sum_trees_weight as u32))
                    .ok_or(Error::Overflow(
                        "Estimated size total weight calculation overflowed",
                    ))?;
                if total_weight == 0 {
                    return Err(Error::DivideByZero("weights add up to 0"));
                }
                let estimated_size = (*sum_trees_weight as u32
                    * aggregate_bytes(TreeType::SumTree))
                .checked_add(*big_sum_trees_weight as u32 * aggregate_bytes(TreeType::BigSumTree))
                .and_then(|sum| {
                    sum.checked_add(
                        *count_trees_weight as u32 * aggregate_bytes(TreeType::CountTree),
                    )
                })
                .and_then(|sum| {
                    sum.checked_add(
                        *count_sum_trees_weight as u32 * aggregate_bytes(TreeType::CountSumTree),
                    )
                })
                .and_then(|sum| {
                    sum.checked_add(
                        *provable_sum_trees_weight as u32
                            * aggregate_bytes(TreeType::ProvableSumTree),
                    )
                })
                .and_then(|sum| {
                    sum.checked_add(
                        *provable_count_trees_weight as u32
                            * aggregate_bytes(TreeType::ProvableCountTree),
                    )
                })
                .and_then(|sum| {
                    sum.checked_add(
                        *provable_count_sum_trees_weight as u32
                            * aggregate_bytes(TreeType::ProvableCountSumTree),
                    )
                })
                .and_then(|sum| {
                    sum.checked_add(
                        *provable_count_provable_sum_trees_weight as u32
                            * aggregate_bytes(TreeType::ProvableCountProvableSumTree),
                    )
                })
                .and_then(|sum| {
                    sum.checked_add(
                        *provable_count_provable_big_sum_trees_weight as u32
                            * aggregate_bytes(TreeType::ProvableCountProvableBigSumTree),
                    )
                })
                .and_then(|sum| {
                    sum.checked_add(
                        *count_big_sum_trees_weight as u32
                            * aggregate_bytes(TreeType::CountBigSumTree),
                    )
                })
                .and_then(|sum| {
                    sum.checked_add(
                        *provable_big_sum_trees_weight as u32
                            * aggregate_bytes(TreeType::ProvableBigSumTree),
                    )
                })
                .ok_or(Error::Overflow("Estimated size calculation overflowed"))?;

                Ok(estimated_size / total_weight)
            }
            EstimatedSumTrees::AllSumTrees => Ok(aggregate_bytes(TreeType::SumTree)),
            EstimatedSumTrees::AllBigSumTrees => Ok(aggregate_bytes(TreeType::BigSumTree)),
            EstimatedSumTrees::AllCountTrees => Ok(aggregate_bytes(TreeType::CountTree)),
            EstimatedSumTrees::AllCountSumTrees => Ok(aggregate_bytes(TreeType::CountSumTree)),
            EstimatedSumTrees::AllProvableSumTrees => {
                Ok(aggregate_bytes(TreeType::ProvableSumTree))
            }
            EstimatedSumTrees::AllProvableCountTrees => {
                Ok(aggregate_bytes(TreeType::ProvableCountTree))
            }
            EstimatedSumTrees::AllProvableCountSumTrees => {
                Ok(aggregate_bytes(TreeType::ProvableCountSumTree))
            }
            EstimatedSumTrees::AllProvableCountProvableSumTrees => {
                Ok(aggregate_bytes(TreeType::ProvableCountProvableSumTree))
            }
            EstimatedSumTrees::AllProvableCountProvableBigSumTrees => {
                Ok(aggregate_bytes(TreeType::ProvableCountProvableBigSumTree))
            }
            EstimatedSumTrees::AllCountBigSumTrees => {
                Ok(aggregate_bytes(TreeType::CountBigSumTree))
            }
            EstimatedSumTrees::AllProvableBigSumTrees => {
                Ok(aggregate_bytes(TreeType::ProvableBigSumTree))
            }
        }
    }
}
