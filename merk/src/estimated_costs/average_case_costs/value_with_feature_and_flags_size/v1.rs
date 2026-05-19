//! `value_with_feature_and_flags_size` v1 — proper weighted Mix
//! average: `Σ (size_i · weight_i) / Σ weight_i`. Non-Mix arms behave
//! identically to v0.

use grovedb_version::version::GroveVersion;

use crate::{error::Error, estimated_costs::average_case_costs::EstimatedLayerSizes};

impl EstimatedLayerSizes {
    pub(super) fn value_with_feature_and_flags_size_v1(
        &self,
        grove_version: &GroveVersion,
    ) -> Result<u32, Error> {
        match self {
            EstimatedLayerSizes::AllItems(_, average_value_size, flags_size) => {
                Ok(*average_value_size + flags_size.unwrap_or_default() + 3)
            }
            EstimatedLayerSizes::AllReference(_, average_value_size, flags_size) => {
                Ok(*average_value_size + flags_size.unwrap_or_default() + 5)
            }
            EstimatedLayerSizes::AllItemsWithSumItem(_, average_value_size, flags_size) => {
                Ok(*average_value_size + flags_size.unwrap_or_default() + 13)
            }
            EstimatedLayerSizes::AllReferencesWithSumItem(_, average_value_size, flags_size) => {
                Ok(*average_value_size + flags_size.unwrap_or_default() + 15)
            }
            EstimatedLayerSizes::AllSubtrees(_, estimated_sum_trees, flags_size) => {
                Ok(estimated_sum_trees.estimated_size(grove_version)?
                    + flags_size.unwrap_or_default()
                    + 3)
            }
            EstimatedLayerSizes::Mix {
                subtrees_size,
                items_size,
                references_size,
                items_with_sum_item_size,
                references_with_sum_item_size,
            } => {
                let (item_size, item_weight) = items_size
                    .as_ref()
                    .map(|(_, vs, fs, weight)| (vs + fs.unwrap_or_default() + 3, *weight as u32))
                    .unwrap_or_default();

                let (ref_size, ref_weight) = references_size
                    .as_ref()
                    .map(|(_, vs, fs, weight)| (vs + fs.unwrap_or_default() + 5, *weight as u32))
                    .unwrap_or_default();

                let (item_sum_size, item_sum_weight) = items_with_sum_item_size
                    .as_ref()
                    .map(|(_, vs, fs, weight)| (vs + fs.unwrap_or_default() + 13, *weight as u32))
                    .unwrap_or_default();

                let (ref_sum_size, ref_sum_weight) = references_with_sum_item_size
                    .as_ref()
                    .map(|(_, vs, fs, weight)| (vs + fs.unwrap_or_default() + 15, *weight as u32))
                    .unwrap_or_default();

                let (subtree_size, subtree_weight) = match subtrees_size {
                    None => None,
                    Some((_, est, fs, weight)) => Some((
                        est.estimated_size(grove_version)? + fs.unwrap_or_default() + 3,
                        *weight as u32,
                    )),
                }
                .unwrap_or_default();

                let combined_weight = item_weight
                    .checked_add(ref_weight)
                    .and_then(|a| a.checked_add(subtree_weight))
                    .and_then(|a| a.checked_add(item_sum_weight))
                    .and_then(|a| a.checked_add(ref_sum_weight))
                    .ok_or(Error::Overflow("overflow for value size combining weights"))?;

                if combined_weight == 0 {
                    return Err(Error::WrongEstimatedCostsElementTypeForLevel(
                        "this layer is a mix and does not have items, refs or trees",
                    ));
                }

                // Proper weighted average. Numerator stays in u64 to
                // tolerate large per-element costs × weights without
                // overflowing the u32 size domain prematurely.
                let weighted_sum = (item_size as u64)
                    .checked_mul(item_weight as u64)
                    .and_then(|a| {
                        (ref_size as u64)
                            .checked_mul(ref_weight as u64)
                            .and_then(|b| a.checked_add(b))
                    })
                    .and_then(|a| {
                        (subtree_size as u64)
                            .checked_mul(subtree_weight as u64)
                            .and_then(|b| a.checked_add(b))
                    })
                    .and_then(|a| {
                        (item_sum_size as u64)
                            .checked_mul(item_sum_weight as u64)
                            .and_then(|b| a.checked_add(b))
                    })
                    .and_then(|a| {
                        (ref_sum_size as u64)
                            .checked_mul(ref_sum_weight as u64)
                            .and_then(|b| a.checked_add(b))
                    })
                    .ok_or(Error::Overflow("overflow for weighted value size"))?;

                let result = weighted_sum / combined_weight as u64;
                if result > u32::MAX as u64 {
                    return Err(Error::Overflow("weighted value size overflows u32"));
                }
                Ok(result as u32)
            }
        }
    }
}
