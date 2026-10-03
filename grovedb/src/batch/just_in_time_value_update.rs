//! The client-driven just-in-time value update a batch apply hands Merk for
//! every write over a stored value: carry the old storage flags into the
//! size measurement, let the caller's flags-update callback rewrite the new
//! element's flags, and let its removal-bytes callback section freed bytes.
//!
//! The apply and the backward-references preprocessor share these exact
//! functions. The preprocessor must know the bytes a family item finally
//! stores BEFORE the apply runs, because every referrer of that item
//! commits to the item's logical hash — flags included — and a flags
//! callback (Drive's epoch-based storage flags) rewrites flags on every
//! cross-epoch update. [`predict_provided_value_hash_put`] runs this update
//! through Merk's own routine to get those bytes.

use std::cell::Cell;

use grovedb_costs::storage_cost::{
    removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
    transition::ElementFlagsUpdate,
    StorageCost,
};
use grovedb_merk::{
    element::{costs::ElementCostExtensions, tree_type::ElementTreeTypeExtensions},
    tree::{
        kv::{
            ValueDefinedCostType,
            ValueDefinedCostType::{LayeredValueDefinedCost, SpecializedValueDefinedCost},
        },
        PredictedPut, TreeFeatureType, TreeNode,
    },
    tree_type::{CostSize, TreeType, SUM_ITEM_COST_SIZE},
    Error as MerkError,
};
use grovedb_version::version::GroveVersion;
use integer_encoding::VarInt;

use crate::{Element, ElementFlags, Error};

/// Why a batch whose flags update settled an owner change was refused: its
/// options do not set `settle_owner_changes`.
pub(crate) const SETTLE_OWNER_CHANGES_NOT_SET: &str =
    "the flags update settled an owner change, but the batch options do not set \
     settle_owner_changes";

/// The caller's flags-update callback as a batch runs it: its answer as an
/// [`ElementFlagsUpdate`] (a `bool` answer is `Changed` or `Unchanged`), and
/// `SettleOwnerChange` refused unless the batch's options set
/// `settle_owner_changes`. A refusal is recorded in `refused`, so the batch
/// reports it with [`settle_refusal_error`] whatever error it surfaces as.
pub(crate) fn batch_flags_update<U>(
    mut flags_update: impl FnMut(
        &StorageCost,
        Option<ElementFlags>,
        &mut ElementFlags,
    ) -> Result<U, Error>,
    settle_owner_changes: bool,
    refused: &Cell<bool>,
) -> impl FnMut(&StorageCost, Option<ElementFlags>, &mut ElementFlags) -> Result<ElementFlagsUpdate, Error>
where
    U: Into<ElementFlagsUpdate>,
{
    move |storage_cost: &StorageCost,
          old_flags: Option<ElementFlags>,
          new_flags: &mut ElementFlags| {
        let update = flags_update(storage_cost, old_flags, new_flags)?.into();
        if update == ElementFlagsUpdate::SettleOwnerChange && !settle_owner_changes {
            refused.set(true);
            return Err(Error::JustInTimeElementFlagsClientError(
                SETTLE_OWNER_CHANGES_NOT_SET.to_owned(),
            ));
        }
        Ok(update)
    }
}

/// The error a batch run with [`batch_flags_update`] reports: the refusal of
/// a settled owner change, as the invalid batch it is, when `refused` says
/// one happened (the Merk and prediction layers it surfaced through report
/// it as client or data corruption), and `error` otherwise.
pub(crate) fn settle_refusal_error(error: Error, refused: &Cell<bool>) -> Error {
    if refused.get() {
        Error::InvalidBatchOperation(SETTLE_OWNER_CHANGES_NOT_SET)
    } else {
        error
    }
}

/// The caller's flags-update callback with its answer as an
/// [`ElementFlagsUpdate`] (a `bool` answer is `Changed` or `Unchanged`).
#[cfg(feature = "estimated_costs")]
pub(crate) fn flags_update_answers<U>(
    mut flags_update: impl FnMut(
        &StorageCost,
        Option<ElementFlags>,
        &mut ElementFlags,
    ) -> Result<U, Error>,
) -> impl FnMut(&StorageCost, Option<ElementFlags>, &mut ElementFlags) -> Result<ElementFlagsUpdate, Error>
where
    U: Into<ElementFlagsUpdate>,
{
    move |storage_cost: &StorageCost,
          old_flags: Option<ElementFlags>,
          new_flags: &mut ElementFlags| {
        flags_update(storage_cost, old_flags, new_flags).map(Into::into)
    }
}

/// The storage cost of the stored value a write replaces.
pub(super) fn old_specialized_cost(
    key: &[u8],
    value: &[u8],
    in_tree_type: TreeType,
    grove_version: &GroveVersion,
) -> Result<u32, MerkError> {
    Element::specialized_costs_for_key_value(
        key,
        value,
        in_tree_type.inner_node_type(),
        grove_version,
    )
    .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
}

/// The new value re-serialized with the OLD value's flags (when it had
/// any), so the first size measurement compares like with like.
pub(super) fn new_value_with_old_flags(
    old_value: &[u8],
    new_value: &[u8],
    grove_version: &GroveVersion,
) -> Result<Option<Vec<u8>>, MerkError> {
    let old_element = Element::deserialize(old_value, grove_version)
        .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
    let maybe_old_flags = old_element.get_flags_owned();
    if maybe_old_flags.is_some() {
        let mut new_element = Element::deserialize(new_value, grove_version)
            .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
        new_element.set_flags(maybe_old_flags);
        new_element
            .serialize(grove_version)
            .map(Some)
            .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))
    } else {
        Ok(None)
    }
}

/// Run the caller's flags-update callback on the new value's flags; when it
/// changes them or settles an owner change, rewrite `new_value` with the
/// flags it leaves and return the value-defined cost the rewritten element
/// carries.
pub(super) fn update_value_flags_based_on_costs<G>(
    flags_update: &mut G,
    storage_costs: &StorageCost,
    old_value: &[u8],
    new_value: &mut Vec<u8>,
    grove_version: &GroveVersion,
) -> Result<(ElementFlagsUpdate, Option<ValueDefinedCostType>), MerkError>
where
    G: FnMut(
        &StorageCost,
        Option<ElementFlags>,
        &mut ElementFlags,
    ) -> Result<ElementFlagsUpdate, Error>,
{
    // todo: change the flags without full deserialization
    let old_element = Element::deserialize(old_value, grove_version)
        .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
    let maybe_old_flags = old_element.get_flags_owned();

    let mut new_element = Element::deserialize(new_value.as_slice(), grove_version)
        .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
    let maybe_new_flags = new_element.get_flags_mut();
    match maybe_new_flags {
        None => Ok((ElementFlagsUpdate::Unchanged, None)),
        Some(new_flags) => {
            let update =
                (flags_update)(storage_costs, maybe_old_flags, new_flags).map_err(|e| match e {
                    Error::JustInTimeElementFlagsClientError(_) => {
                        MerkError::ClientCorruptionError(e.to_string())
                    }
                    _ => MerkError::ClientCorruptionError("non client error".to_string()),
                })?;
            if update != ElementFlagsUpdate::Unchanged {
                let flags_len = new_flags.len() as u32;
                new_value.clone_from(
                    &new_element
                        .serialize(grove_version)
                        .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?,
                );
                // we need to give back the value defined cost in the case that the
                // new element is a tree.
                //
                // Look through wrapper variants for the cost
                // path (the wrapper byte costs +1 over the
                // bare type, mirroring `wrapper_overhead`
                // in `merk/src/element/costs.rs`).
                let wrapper_overhead = if new_element.is_wrapped() { 1u32 } else { 0 };
                match new_element.underlying() {
                    Element::Tree(..)
                    | Element::SumTree(..)
                    | Element::BigSumTree(..)
                    | Element::CountTree(..)
                    | Element::CountSumTree(..)
                    | Element::ProvableCountTree(..)
                    | Element::ProvableCountSumTree(..)
                    | Element::ProvableSumTree(..)
                    | Element::ProvableCountProvableSumTree(..)
                    | Element::CommitmentTree(..)
                    | Element::MmrTree(..)
                    | Element::BulkAppendTree(..)
                    | Element::DenseAppendOnlyFixedSizeTree(..)
                    | Element::ProvableSumIndexedTree(..)
                    | Element::ProvableCountIndexedTree(..)
                    | Element::ProvableCountProvableSumIndexedTree(..)
                    | Element::PrivateDocumentStore(..) => {
                        let tree_type = new_element
                            .tree_type()
                            .expect("tree_type guaranteed by match arm");
                        let tree_cost_size = tree_type.cost_size();
                        let tree_value_cost = tree_cost_size
                            + flags_len
                            + flags_len.required_space() as u32
                            + wrapper_overhead;
                        Ok((update, Some(LayeredValueDefinedCost(tree_value_cost))))
                    }
                    Element::SumItem(..) => {
                        let sum_item_value_cost = SUM_ITEM_COST_SIZE
                            + flags_len
                            + flags_len.required_space() as u32
                            + wrapper_overhead;
                        Ok((
                            update,
                            Some(SpecializedValueDefinedCost(sum_item_value_cost)),
                        ))
                    }
                    Element::ItemWithSumItem(item_value, ..) => {
                        let item_len = item_value.len() as u32;
                        let sum_item_value_cost = SUM_ITEM_COST_SIZE
                            + flags_len
                            + flags_len.required_space() as u32
                            + item_len
                            + item_len.required_space() as u32
                            + wrapper_overhead;
                        Ok((
                            update,
                            Some(SpecializedValueDefinedCost(sum_item_value_cost)),
                        ))
                    }
                    _ => Ok((update, None)),
                }
            } else {
                Ok((ElementFlagsUpdate::Unchanged, None))
            }
        }
    }
}

/// Section the bytes a write frees through the caller's removal callback
/// (unflagged values free them as plain basic removals).
pub(super) fn section_removal_bytes<SR>(
    split_removal_bytes: &mut SR,
    value: &[u8],
    removed_key_bytes: u32,
    removed_value_bytes: u32,
    grove_version: &GroveVersion,
) -> Result<(StorageRemovedBytes, StorageRemovedBytes), MerkError>
where
    SR: FnMut(
        &mut ElementFlags,
        u32,
        u32,
    ) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error>,
{
    let mut element = Element::deserialize(value, grove_version)
        .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
    let maybe_flags = element.get_flags_mut();
    match maybe_flags {
        None => Ok((
            BasicStorageRemoval(removed_key_bytes),
            BasicStorageRemoval(removed_value_bytes),
        )),
        Some(flags) => (split_removal_bytes)(flags, removed_key_bytes, removed_value_bytes)
            .map_err(|e| MerkError::ClientCorruptionError(e.to_string())),
    }
}

/// The bytes a batch apply finally stores when `put` of `new_serialized`
/// (with `feature_type`) replaces the stored `old_serialized` bytes at `key`
/// in a subtree of `in_tree_type`, with the caller's callbacks: Merk's own
/// just-in-time value update, run on a detached node loaded as the apply
/// loads the stored node.
///
/// The callbacks must answer the apply the same way they answer here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn predict_put_final_bytes<G, SR>(
    key: &[u8],
    old_serialized: Vec<u8>,
    new_serialized: Vec<u8>,
    feature_type: TreeFeatureType,
    put: PredictedPut,
    in_tree_type: TreeType,
    flags_update: &mut G,
    split_removal_bytes: &mut SR,
    grove_version: &GroveVersion,
) -> Result<Vec<u8>, MerkError>
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
    let old_value_defined_cost =
        Element::value_defined_cost_for_serialized_value(&old_serialized, grove_version);
    TreeNode::predict_put_final_value(
        key.to_vec(),
        old_serialized,
        old_value_defined_cost,
        new_serialized,
        feature_type,
        put,
        &|key, value| old_specialized_cost(key, value, in_tree_type, grove_version),
        &|old_value, new_value| new_value_with_old_flags(old_value, new_value, grove_version),
        &mut |storage_costs, old_value, new_value| {
            update_value_flags_based_on_costs(
                flags_update,
                storage_costs,
                old_value,
                new_value,
                grove_version,
            )
        },
        &mut |value, removed_key_bytes, removed_value_bytes| {
            section_removal_bytes(
                split_removal_bytes,
                value,
                removed_key_bytes,
                removed_value_bytes,
                grove_version,
            )
        },
        grove_version,
    )
}

/// The element a batch apply finally stores when it writes `new_element`
/// (a backward-references family element, which the apply writes as a
/// provided-value-hash put) over the stored `old_serialized` bytes at `key`
/// in a subtree of `in_tree_type`, with the caller's callbacks.
///
/// The callbacks must answer the apply the same way they answer here: the
/// batch checks after the apply that the stored bytes are the predicted ones.
pub(crate) fn predict_provided_value_hash_put<G, SR>(
    key: &[u8],
    old_serialized: &[u8],
    new_element: &Element,
    in_tree_type: TreeType,
    flags_update: &mut G,
    split_removal_bytes: &mut SR,
    grove_version: &GroveVersion,
) -> Result<Element, Error>
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
    let new_serialized = new_element.serialize(grove_version)?;
    let feature_type = new_element.get_feature_type(in_tree_type)?;
    let final_bytes = predict_put_final_bytes(
        key,
        old_serialized.to_vec(),
        new_serialized.clone(),
        feature_type,
        PredictedPut::ProvidedValueHash,
        in_tree_type,
        flags_update,
        split_removal_bytes,
        grove_version,
    )
    .map_err(|e| {
        Error::CorruptedData(format!(
            "predicting the just-in-time value update of a backward-references item: {e}"
        ))
    })?;
    if final_bytes == new_serialized {
        return Ok(new_element.clone());
    }
    Element::deserialize(&final_bytes, grove_version).map_err(Error::from)
}
