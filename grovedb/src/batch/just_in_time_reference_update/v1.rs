use std::collections::HashMap;

use grovedb_costs::{
    cost_return_on_error_into_no_add, cost_return_on_error_no_add,
    storage_cost::{removal::StorageRemovedBytes, transition::ElementFlagsUpdate, StorageCost},
    CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{
    element::{insert::specialized_put_cost, tree_type::ElementTreeTypeExtensions},
    tree::{value_hash, PredictedPut},
    tree_type::TreeType,
    CryptoHash,
};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::just_in_time_value_update::predict_put_final_bytes, Element, ElementFlags, Error,
};

/// Version 1 of the same-batch reference target prediction, selected by
/// `apply_batch.same_batch_reference_target_prediction == 1` (grove v4+),
/// and on every grove version for a batch with `settle_owner_changes`.
///
/// It differs from version 0 in how it gets the stored bytes. Once this pass
/// has written the target, `old_serialized_element` (the target's bytes as
/// the Merk cache holds them) are the bytes it stores. Before that, they are
/// the bytes the apply will update, and the stored bytes are predicted by
/// running Merk's own just-in-time value update on them and the new ones,
/// with the put the apply performs (`specialized_put_cost`), once per target.
/// The reference commits to exactly what the apply stores whatever the flags
/// update answers, and whether the target is a sum item or not.
#[allow(clippy::too_many_arguments)]
pub(super) fn process_old_element_flags_v1<G, SR>(
    key: &[u8],
    qualified_path: &[Vec<u8>],
    new_element: &Element,
    old_serialized_element: Vec<u8>,
    in_tree_type: TreeType,
    target_written: bool,
    predicted_targets: &mut HashMap<Vec<Vec<u8>>, Vec<u8>>,
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
    if target_written {
        let val_hash = value_hash(&old_serialized_element).unwrap_add_cost(&mut cost);
        return Ok(val_hash).wrap_with_cost(cost);
    }
    if let Some(stored_bytes) = predicted_targets.get(qualified_path) {
        let val_hash = value_hash(stored_bytes).unwrap_add_cost(&mut cost);
        return Ok(val_hash).wrap_with_cost(cost);
    }
    // Serialized only now: a written target or a cached prediction does not
    // need the incoming bytes.
    let serialized = cost_return_on_error_into_no_add!(cost, new_element.serialize(grove_version));
    let put = match cost_return_on_error_no_add!(
        cost,
        specialized_put_cost(new_element, grove_version).map_err(Error::MerkError)
    ) {
        Some(value_cost) => PredictedPut::SpecializedCost(value_cost),
        None => PredictedPut::Ordinary,
    };
    let feature_type =
        cost_return_on_error_into_no_add!(cost, new_element.get_feature_type(in_tree_type));
    let stored_bytes = cost_return_on_error_no_add!(
        cost,
        predict_put_final_bytes(
            key,
            old_serialized_element,
            serialized,
            feature_type,
            put,
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
    predicted_targets.insert(qualified_path.to_vec(), stored_bytes);
    Ok(val_hash).wrap_with_cost(cost)
}
