//! `EstimatedSumTrees::estimated_size` v2 — grove v3 formula.
//!
//! Extends v1 with the four `provable_*` weight fields covering
//! `ProvableSumTree`, `ProvableCountTree`, `ProvableCountSumTree`, and
//! the dual-axis `ProvableCountProvableSumTree`. Each contributes its
//! own `inner_node_type().cost()` and is summed into both numerator
//! and denominator alongside the legacy four.

#[cfg(feature = "minimal")]
use crate::{
    error::Error, estimated_costs::average_case_costs::EstimatedSumTrees, tree_type::TreeType,
};

#[cfg(feature = "minimal")]
impl EstimatedSumTrees {
    pub(super) fn estimated_size_v2(&self) -> Result<u32, Error> {
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
                    .ok_or(Error::Overflow(
                        "Estimated size total weight calculation overflowed",
                    ))?;
                if total_weight == 0 {
                    return Err(Error::DivideByZero("weights add up to 0"));
                }
                let estimated_size = (*sum_trees_weight as u32
                    * TreeType::SumTree.inner_node_type().cost())
                .checked_add(
                    *big_sum_trees_weight as u32 * TreeType::BigSumTree.inner_node_type().cost(),
                )
                .and_then(|sum| {
                    sum.checked_add(
                        *count_trees_weight as u32 * TreeType::CountTree.inner_node_type().cost(),
                    )
                })
                .and_then(|sum| {
                    sum.checked_add(
                        *count_sum_trees_weight as u32
                            * TreeType::CountSumTree.inner_node_type().cost(),
                    )
                })
                .and_then(|sum| {
                    sum.checked_add(
                        *provable_sum_trees_weight as u32
                            * TreeType::ProvableSumTree.inner_node_type().cost(),
                    )
                })
                .and_then(|sum| {
                    sum.checked_add(
                        *provable_count_trees_weight as u32
                            * TreeType::ProvableCountTree.inner_node_type().cost(),
                    )
                })
                .and_then(|sum| {
                    sum.checked_add(
                        *provable_count_sum_trees_weight as u32
                            * TreeType::ProvableCountSumTree.inner_node_type().cost(),
                    )
                })
                .and_then(|sum| {
                    sum.checked_add(
                        *provable_count_provable_sum_trees_weight as u32
                            * TreeType::ProvableCountProvableSumTree
                                .inner_node_type()
                                .cost(),
                    )
                })
                .ok_or(Error::Overflow("Estimated size calculation overflowed"))?;

                Ok(estimated_size / total_weight)
            }
            EstimatedSumTrees::AllSumTrees => Ok(TreeType::SumTree.inner_node_type().cost()),
            EstimatedSumTrees::AllBigSumTrees => Ok(TreeType::BigSumTree.inner_node_type().cost()),
            EstimatedSumTrees::AllCountTrees => Ok(TreeType::CountTree.inner_node_type().cost()),
            EstimatedSumTrees::AllCountSumTrees => {
                Ok(TreeType::CountSumTree.inner_node_type().cost())
            }
            EstimatedSumTrees::AllProvableSumTrees => {
                Ok(TreeType::ProvableSumTree.inner_node_type().cost())
            }
            EstimatedSumTrees::AllProvableCountTrees => {
                Ok(TreeType::ProvableCountTree.inner_node_type().cost())
            }
            EstimatedSumTrees::AllProvableCountSumTrees => {
                Ok(TreeType::ProvableCountSumTree.inner_node_type().cost())
            }
            EstimatedSumTrees::AllProvableCountProvableSumTrees => {
                Ok(TreeType::ProvableCountProvableSumTree
                    .inner_node_type()
                    .cost())
            }
        }
    }
}
