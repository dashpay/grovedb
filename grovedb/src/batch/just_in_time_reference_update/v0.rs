use std::borrow::Cow;

use grovedb_costs::{
    cost_return_on_error_into_no_add, cost_return_on_error_no_add,
    storage_cost::{
        removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
        transition::ElementFlagsUpdate,
        StorageCost,
    },
    CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{
    element::costs::ElementCostExtensions,
    tree::{kv::KV, value_hash, TreeNode},
    tree_type::TreeType,
    CryptoHash,
};
use grovedb_version::version::GroveVersion;

use crate::{batch::MerkError, Element, ElementFlags, Error};

/// Version 0 of the same-batch reference target prediction, selected by
/// `apply_batch.same_batch_reference_target_prediction == 0` (grove
/// v1..v3): a hand-written copy of Merk's just-in-time value update.
///
/// It differs from version 1 in how it measures and asks the flags update.
/// A sum item is taken to keep its stored flags without consulting the
/// flags update (except, with `settle_owner_changes`, for an answer that
/// settles an owner change), while the apply stores the flags the flags
/// update leaves it, so a reference to a sum item whose flags change
/// commits to bytes that are never stored.
#[allow(clippy::too_many_arguments)]
pub(super) fn process_old_element_flags_v0<G, SR>(
    key: &[u8],
    serialized: &[u8],
    new_element: &mut Element,
    old_element: Element,
    old_serialized_element: &[u8],
    in_tree_type: TreeType,
    settle_owner_changes: bool,
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
    let client_error = |e: Error| -> Error {
        match e {
            Error::JustInTimeElementFlagsClientError(_) => {
                MerkError::ClientCorruptionError(e.to_string()).into()
            }
            _ => MerkError::ClientCorruptionError("non client error".to_string()).into(),
        }
    };
    if old_element.is_sum_item() {
        return if new_element.is_sum_item() {
            let maybe_old_flags = old_element.get_flags_owned();
            if settle_owner_changes
                && maybe_old_flags.is_some()
                && new_element.get_flags().is_some()
            {
                // Merk measures a sum item by its own specialized cost,
                // new flags included, against the stored one's.
                let node_type = in_tree_type.inner_node_type();
                let old_storage_cost = cost_return_on_error_no_add!(
                    cost,
                    Element::specialized_costs_for_key_value(
                        key,
                        old_serialized_element,
                        node_type,
                        grove_version,
                    )
                    .map_err(Error::MerkError)
                );
                let new_storage_cost = cost_return_on_error_no_add!(
                    cost,
                    Element::specialized_costs_for_key_value(
                        key,
                        serialized,
                        node_type,
                        grove_version,
                    )
                    .map_err(Error::MerkError)
                );
                let mut storage_costs =
                    TreeNode::storage_cost_for_update(new_storage_cost, old_storage_cost);
                if let BasicStorageRemoval(removed_bytes) = storage_costs.removed_bytes {
                    let mut old_flags = maybe_old_flags.clone().unwrap_or_default();
                    let (_, value_removed_bytes) = cost_return_on_error_no_add!(
                        cost,
                        split_removal_bytes(&mut old_flags, 0, removed_bytes)
                    );
                    storage_costs.removed_bytes = value_removed_bytes;
                }
                let mut settled_element = new_element.clone();
                if let Some(new_flags) = settled_element.get_flags_mut().as_mut() {
                    let update = cost_return_on_error_no_add!(
                        cost,
                        (flags_update)(&storage_costs, maybe_old_flags.clone(), new_flags)
                            .map_err(client_error)
                    );
                    if update == ElementFlagsUpdate::SettleOwnerChange {
                        let settled_bytes = cost_return_on_error_into_no_add!(
                            cost,
                            settled_element.serialize(grove_version)
                        );
                        let val_hash = value_hash(&settled_bytes).unwrap_add_cost(&mut cost);
                        return Ok(val_hash).wrap_with_cost(cost);
                    }
                }
            }
            if maybe_old_flags.is_some() {
                let mut updated_new_element_with_old_flags = new_element.clone();
                updated_new_element_with_old_flags.set_flags(maybe_old_flags.clone());
                // There are no storage flags, we can just hash new element
                let new_serialized_bytes = cost_return_on_error_into_no_add!(
                    cost,
                    updated_new_element_with_old_flags.serialize(grove_version)
                );
                let val_hash = value_hash(&new_serialized_bytes).unwrap_add_cost(&mut cost);
                Ok(val_hash).wrap_with_cost(cost)
            } else {
                let val_hash = value_hash(serialized).unwrap_add_cost(&mut cost);
                Ok(val_hash).wrap_with_cost(cost)
            }
        } else {
            Err(Error::NotSupported(
                "going from a sum item to a not sum item is not supported".to_string(),
            ))
            .wrap_with_cost(cost)
        };
    } else if new_element.is_sum_item() {
        return Err(Error::NotSupported(
            "going from an item to a sum item is not supported".to_string(),
        ))
        .wrap_with_cost(cost);
    }
    let mut maybe_old_flags = old_element.get_flags_owned();

    let old_storage_cost = KV::node_value_byte_cost_size(
        key.len() as u32,
        old_serialized_element.len() as u32,
        in_tree_type.inner_node_type(),
    );

    let original_new_element = new_element.clone();

    let mut serialization_to_use = Cow::Borrowed(serialized);

    let mut new_storage_cost = if maybe_old_flags.is_some() {
        // we need to get the new storage_cost as if it had the same storage flags as
        // before
        let mut updated_new_element_with_old_flags = original_new_element.clone();
        updated_new_element_with_old_flags.set_flags(maybe_old_flags.clone());

        let serialized_with_old_flags = cost_return_on_error_into_no_add!(
            cost,
            updated_new_element_with_old_flags.serialize(grove_version)
        );
        KV::node_value_byte_cost_size(
            key.len() as u32,
            serialized_with_old_flags.len() as u32,
            in_tree_type.inner_node_type(),
        )
    } else {
        KV::node_value_byte_cost_size(
            key.len() as u32,
            serialized.len() as u32,
            in_tree_type.inner_node_type(),
        )
    };

    let mut i = 0;

    loop {
        // Calculate storage costs
        let mut storage_costs =
            TreeNode::storage_cost_for_update(new_storage_cost, old_storage_cost);

        if let Some(old_element_flags) = maybe_old_flags.as_mut()
            && let BasicStorageRemoval(removed_bytes) = storage_costs.removed_bytes
        {
            let (_, value_removed_bytes) = cost_return_on_error_no_add!(
                cost,
                split_removal_bytes(old_element_flags, 0, removed_bytes)
            );
            storage_costs.removed_bytes = value_removed_bytes;
        }

        let mut new_element_cloned = original_new_element.clone();

        let new_flags = cost_return_on_error_no_add!(
            cost,
            new_element_cloned
                .get_flags_mut()
                .as_mut()
                .ok_or(Error::CorruptedCodeExecution(
                    "element has no flags for just-in-time update",
                ))
        );
        let update = cost_return_on_error_no_add!(
            cost,
            (flags_update)(&storage_costs, maybe_old_flags.clone(), new_flags)
                .map_err(client_error)
        );
        if update == ElementFlagsUpdate::Unchanged {
            // There are no storage flags, we can just hash new element

            let val_hash = value_hash(&serialization_to_use).unwrap_add_cost(&mut cost);
            return Ok(val_hash).wrap_with_cost(cost);
        } else {
            // There are no storage flags, we can just hash new element
            let new_serialized_bytes = cost_return_on_error_into_no_add!(
                cost,
                new_element_cloned.serialize(grove_version)
            );

            if update == ElementFlagsUpdate::SettleOwnerChange {
                // The apply stores the new element with the flags the
                // callback left it, whatever its size.
                let val_hash = value_hash(&new_serialized_bytes).unwrap_add_cost(&mut cost);
                return Ok(val_hash).wrap_with_cost(cost);
            }

            new_storage_cost = KV::node_value_byte_cost_size(
                key.len() as u32,
                new_serialized_bytes.len() as u32,
                in_tree_type.inner_node_type(),
            );

            if serialization_to_use == new_serialized_bytes {
                // it hasn't actually changed, let's do the value hash of it
                let val_hash = value_hash(&serialization_to_use).unwrap_add_cost(&mut cost);
                return Ok(val_hash).wrap_with_cost(cost);
            }

            serialization_to_use = Cow::Owned(new_serialized_bytes);
        }

        // Prevent potential infinite loop
        if i > 8 {
            return Err(Error::CyclicError(
                "updated value based on costs too many times in reference",
            ))
            .wrap_with_cost(cost);
        }
        i += 1;
    }
}
