//! `average_case_indexed_secondary_mirror` — **v0** (`GROVE_V1`..`GROVE_V5`).
//!
//! The estimate as it was before it had a dispatch, moved here as it was.
//! The keys of the primary are sized by `average_case_layer_key_size`.

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::estimated_costs::average_case_costs::{
    EstimatedLayerInformation, EstimatedLayerSizes,
};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{key_info::KeyInfo, KeyInfoPath},
    Error, GroveDb,
};

impl GroveDb {
    /// `average_case_indexed_secondary_mirror` v0 — see the module
    /// documentation.
    pub(super) fn average_case_indexed_secondary_mirror_v0(
        primary_path: &KeyInfoPath,
        primary_layer_information: &EstimatedLayerInformation,
        axes: &[grovedb_element::indexed::IndexAxis],
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error> {
        use crate::operations::indexed_tree::{axis_secondary_tree_type, axis_sort_key_len};

        let mut cost = OperationCost::default();
        let primary_key_size =
            Self::average_case_layer_key_size(&primary_layer_information.estimated_layer_sizes);
        let secondary_should_be_empty = primary_layer_information
            .estimated_layer_count
            .estimated_to_be_empty();

        for axis in axes {
            let secondary_key_size =
                (primary_key_size + axis_sort_key_len(*axis) as u32).min(u8::MAX as u32) as u8;
            // The worst-case canonical row for this axis, sized against a
            // primary key of the estimated width. Built through THE row
            // function the mirror writes with, so the estimate cannot drift
            // from the write path: sum values are charged at their fixed
            // worst-case varint width, so `i64::MAX` is an upper bound rather
            // than an average (and is inside `count_value_as_sum`'s domain,
            // so the conversion cannot fail here).
            let worst_case_row = cost_return_on_error_no_add!(
                cost,
                crate::operations::indexed_tree::axis_row_reference(
                    *axis,
                    &vec![0u8; primary_key_size as usize],
                    i64::MAX as u64,
                    i64::MAX,
                )
            );
            let row_value_size = cost_return_on_error_no_add!(
                cost,
                worst_case_row
                    .serialized_size(grove_version)
                    .map_err(|e| Error::CorruptedData(format!(
                        "sizing the worst-case indexed secondary row: {e}"
                    )))
            )
            .min(u32::MAX as usize) as u32;
            let secondary_layer = EstimatedLayerInformation {
                tree_type: axis_secondary_tree_type(*axis),
                // 1:1 with the primary.
                estimated_layer_count: primary_layer_information.estimated_layer_count,
                // The row shape's OWN variant, not `AllItems`: the two
                // carry different element overheads (+15 vs +3), and a
                // secondary row is a `ReferenceWithSumItem`. Describing it
                // as an item under-charged every row by 12 bytes — and
                // `added_bytes` is the one dimension a storage-fee
                // reservation must never come in under.
                estimated_layer_sizes: EstimatedLayerSizes::AllReferencesWithSumItem(
                    secondary_key_size,
                    row_value_size,
                    None,
                ),
            };
            // Opening the secondary Merk.
            cost_return_on_error_no_add!(
                cost,
                Self::add_average_case_get_merk_at_path_engine_free(
                    &mut cost,
                    primary_path,
                    secondary_should_be_empty,
                    secondary_layer.tree_type,
                    grove_version,
                )
            );
            // Deriving the secondary prefix costs one extra hash beyond the
            // primary prefix the open above accounted for.
            cost.hash_node_calls += 1;

            let secondary_key = KeyInfo::MaxKeySize {
                unique_id: primary_path.to_path().concat(),
                max_size: secondary_key_size,
            };
            // The mirror is delete-old-row then insert-new-row, each of which
            // rebalances and re-roots the secondary. (The ACTUAL commit
            // accounting rebills a moved pair's churn out of added/removed
            // into replaced; this estimate deliberately keeps the raw
            // delete+insert shape, over-reserving `added_bytes` by up to
            // the churn rather than ever coming in under.)
            //
            // The inserted row must be sized with the AXIS's real row shape:
            // `average_case_merk_insert_element` charges non-tree elements by
            // their own serialized size, and under-sizing put the PCPSIT
            // estimate ~25 bytes per key UNDER actual `added_bytes` — the one
            // dimension a storage-fee reservation cannot come in under.
            //
            // Writing the row is one extra hash beyond a plain put: a
            // combined reference commits
            // `combine_hash(H(row bytes), target hash)`, so the value hash is
            // hashed twice rather than once.
            cost.hash_node_calls += 1;
            cost_return_on_error!(
                &mut cost,
                Self::average_case_merk_delete_element(
                    &secondary_key,
                    &secondary_layer,
                    true,
                    grove_version,
                )
            );
            cost_return_on_error!(
                &mut cost,
                Self::average_case_merk_insert_element(
                    &secondary_key,
                    &worst_case_row,
                    secondary_layer.tree_type,
                    Some(&secondary_layer),
                    grove_version,
                )
            );
        }
        Ok(()).wrap_with_cost(cost)
    }
}
