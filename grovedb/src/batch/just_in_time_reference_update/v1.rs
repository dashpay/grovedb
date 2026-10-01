use grovedb_costs::{
    cost_return_on_error_into_no_add, cost_return_on_error_no_add,
    storage_cost::{removal::StorageRemovedBytes, transition::ElementFlagsUpdate, StorageCost},
    CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{
    element::{costs::ElementCostExtensions, tree_type::ElementTreeTypeExtensions},
    tree::{kv::ValueDefinedCostType::SpecializedValueDefinedCost, value_hash},
    tree_type::TreeType,
    CryptoHash,
};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::just_in_time_value_update::predict_put_final_bytes, Element, ElementFlags, Error,
};

/// Version 1 of the same-batch reference target prediction, selected by
/// `apply_batch.same_batch_reference_target_prediction == 1` (grove v4+).
///
/// It differs from version 0 in how it gets the stored bytes: it runs
/// Merk's own just-in-time value update on the stored and new bytes, with
/// the put the apply performs (a sum item at its specialized cost, any other
/// item as an ordinary put), so the reference commits to exactly what the
/// apply stores whatever the flags update answers.
#[allow(clippy::too_many_arguments)]
pub(super) fn process_old_element_flags_v1<G, SR>(
    key: &[u8],
    serialized: &[u8],
    new_element: &Element,
    old_element: &Element,
    old_serialized_element: &[u8],
    in_tree_type: TreeType,
    flags_update: &mut G,
    split_removal_bytes: &mut SR,
    grove_version: &GroveVersion,
) -> CostResult<CryptoHash, Error>
where
    G: FnMut(
        &StorageCost,
        Option<ElementFlags>,
        &mut ElementFlags,
    ) -> Result<ElementFlagsUpdate, Error>,
    SR: FnMut(
        &mut ElementFlags,
        u32,
        u32,
    ) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
{
    let mut cost = OperationCost::default();
    match (old_element.is_sum_item(), new_element.is_sum_item()) {
        (true, false) => {
            return Err(Error::NotSupported(
                "going from a sum item to a not sum item is not supported".to_string(),
            ))
            .wrap_with_cost(cost)
        }
        (false, true) => {
            return Err(Error::NotSupported(
                "going from an item to a sum item is not supported".to_string(),
            ))
            .wrap_with_cost(cost)
        }
        _ => {}
    }
    // The put the apply performs (`insert_into_batch_operations`).
    let value_defined_cost = if new_element.is_sum_item() {
        Some(SpecializedValueDefinedCost(cost_return_on_error_no_add!(
            cost,
            new_element
                .specialized_value_defined_cost(grove_version)
                .ok_or(Error::CorruptedCodeExecution(
                    "sum items should always have a value defined cost"
                ))
        )))
    } else {
        None
    };
    let feature_type =
        cost_return_on_error_into_no_add!(cost, new_element.get_feature_type(in_tree_type));
    let stored_bytes = cost_return_on_error_no_add!(
        cost,
        predict_put_final_bytes(
            key,
            old_serialized_element,
            serialized.to_vec(),
            feature_type,
            value_defined_cost,
            in_tree_type,
            flags_update,
            split_removal_bytes,
            grove_version,
        )
        .map_err(|e| Error::CorruptedData(format!(
            "predicting the just-in-time value update of a reference target: {e}"
        )))
    );
    let val_hash = value_hash(&stored_bytes).unwrap_add_cost(&mut cost);
    Ok(val_hash).wrap_with_cost(cost)
}
