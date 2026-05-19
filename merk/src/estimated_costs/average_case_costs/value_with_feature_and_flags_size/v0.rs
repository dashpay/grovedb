//! `value_with_feature_and_flags_size` v0 — legacy unweighted Mix
//! average. Pre-existing behavior used by grove v1/v2; bug-for-bug
//! compatible with shipped fee outputs.
//!
//! On a 3:1:2 item/ref/subtree mix this returns
//! `(item + ref + subtree) / 6`, ignoring weights in the numerator.

#[cfg(feature = "minimal")]
use grovedb_version::version::GroveVersion;

#[cfg(feature = "minimal")]
use crate::{error::Error, estimated_costs::average_case_costs::EstimatedLayerSizes};

#[cfg(feature = "minimal")]
impl EstimatedLayerSizes {
    pub(super) fn value_with_feature_and_flags_size_v0(
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

                let nonzero_kinds = (item_weight > 0) as u32
                    + (ref_weight > 0) as u32
                    + (subtree_weight > 0) as u32
                    + (item_sum_weight > 0) as u32
                    + (ref_sum_weight > 0) as u32;
                if nonzero_kinds == 0 {
                    return Err(Error::WrongEstimatedCostsElementTypeForLevel(
                        "this layer is a mix and does not have items, refs or trees",
                    ));
                }
                if nonzero_kinds == 1 {
                    if item_weight > 0 {
                        return Ok(item_size);
                    }
                    if ref_weight > 0 {
                        return Ok(ref_size);
                    }
                    if subtree_weight > 0 {
                        return Ok(subtree_size);
                    }
                    if item_sum_weight > 0 {
                        return Ok(item_sum_size);
                    }
                    if ref_sum_weight > 0 {
                        return Ok(ref_sum_size);
                    }
                }

                let combined_weight = item_weight
                    .checked_add(ref_weight)
                    .and_then(|a| a.checked_add(subtree_weight))
                    .and_then(|a| a.checked_add(item_sum_weight))
                    .and_then(|a| a.checked_add(ref_sum_weight))
                    .ok_or(Error::Overflow("overflow for value size combining weights"))?;

                let combined_size = item_size
                    .checked_add(ref_size)
                    .and_then(|a| a.checked_add(subtree_size))
                    .and_then(|a| a.checked_add(item_sum_size))
                    .and_then(|a| a.checked_add(ref_sum_size))
                    .ok_or(Error::Overflow("overflow for value size"))?;

                // `checked_div` only fails on divide-by-zero, which the
                // `nonzero_kinds == 0` guard above already rules out;
                // surface that case clearly anyway so the unreachable
                // path produces an accurate error rather than the
                // misleading "overflow" used previously.
                combined_size
                    .checked_div(combined_weight)
                    .ok_or(Error::DivideByZero("value size divisor was zero"))
            }
        }
    }
}
