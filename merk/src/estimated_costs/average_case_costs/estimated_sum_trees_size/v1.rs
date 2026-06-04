//! `EstimatedSumTrees::estimated_size` v1 — grove v2 formula.
//!
//! Weighted average across the four legacy aggregate tree types
//! (`SumTree`, `BigSumTree`, `CountTree`, `CountSumTree`), using each
//! type's `inner_node_type().cost()` as the per-element contribution.
//! `non_sum_trees_weight` enters only via the divisor (it contributes 0
//! to the numerator since `NormalNode.cost() == 0`). The four
//! `provable_*` weight fields didn't exist when this formula shipped
//! and are ignored.

use crate::{
    error::Error, estimated_costs::average_case_costs::EstimatedSumTrees, tree_type::TreeType,
};

impl EstimatedSumTrees {
    pub(super) fn estimated_size_v1(&self) -> Result<u32, Error> {
        match self {
            EstimatedSumTrees::NoSumTrees => Ok(0),
            EstimatedSumTrees::SomeSumTrees {
                sum_trees_weight,
                big_sum_trees_weight,
                count_trees_weight,
                count_sum_trees_weight,
                non_sum_trees_weight,
                ..
            } => {
                let total_weight = *sum_trees_weight as u32
                    + *big_sum_trees_weight as u32
                    + *count_trees_weight as u32
                    + *count_sum_trees_weight as u32
                    + *non_sum_trees_weight as u32;
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
