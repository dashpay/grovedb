//! `EstimatedSumTrees::estimated_size` v0 — original grove v1 formula.
//!
//! v0 only consults `sum_trees_weight` and `non_sum_trees_weight`; the
//! other weight fields (`big_sum`, `count*`, `provable_*`) didn't exist
//! when this formula shipped and are ignored. The divisor is
//! `sum_trees + non_sum_trees`, guarded against division by zero — a
//! layer with only `big_sum_trees_weight` (or any count weight) set
//! would have a nonzero legacy total but a zero v0 denominator.

#[cfg(feature = "minimal")]
use crate::{
    error::Error, estimated_costs::average_case_costs::EstimatedSumTrees, tree_type::TreeType,
};

#[cfg(feature = "minimal")]
impl EstimatedSumTrees {
    pub(super) fn estimated_size_v0(&self) -> Result<u32, Error> {
        match self {
            EstimatedSumTrees::NoSumTrees => Ok(0),
            EstimatedSumTrees::SomeSumTrees {
                sum_trees_weight,
                non_sum_trees_weight,
                ..
            } => {
                let denominator = *sum_trees_weight as u32 + *non_sum_trees_weight as u32;
                if denominator == 0 {
                    return Err(Error::DivideByZero("weights add up to 0"));
                }
                Ok((*non_sum_trees_weight as u32 * 9) / denominator)
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
