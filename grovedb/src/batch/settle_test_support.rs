//! Fixtures shared by the tests of batch writes that run Drive's flags
//! update: owned and unowned storage flags, Drive's flags-update callbacks
//! (merging, or settling owner changes) and removal callback, and the
//! element at `[tree]/key1`.

#![cfg(feature = "minimal")]

use grovedb_costs::{
    storage_cost::{removal::StorageRemovedBytes, transition::ElementFlagsUpdate, StorageCost},
    OperationCost,
};
use grovedb_epoch_based_storage_flags::StorageFlags;
use grovedb_merk::{element::costs::ElementCostExtensions, tree_type::TreeType};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{BatchApplyOptions, QualifiedGroveDbOp},
    tests::TempGroveDb,
    Element, ElementFlags, Error,
};

pub(crate) const OLD_OWNER: [u8; 32] = [1; 32];
pub(crate) const NEW_OWNER: [u8; 32] = [2; 32];
pub(crate) const KEY: &[u8] = b"key1";

pub(crate) fn owned_flags(epoch: u16, owner: [u8; 32]) -> Option<Vec<u8>> {
    Some(StorageFlags::SingleEpochOwned(epoch, owner).to_element_flags())
}

pub(crate) fn unowned_flags(epoch: u16) -> Option<Vec<u8>> {
    Some(StorageFlags::SingleEpoch(epoch).to_element_flags())
}

/// How a batch is applied.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Mode {
    /// Drive's merging flags update, without the option.
    Merging,
    /// The settling flags update, with the option.
    Settling,
}

/// Drive's flags update, merging the storage flags.
pub(crate) fn merging_flags_update(
    cost: &StorageCost,
    old_flags: Option<ElementFlags>,
    new_flags: &mut ElementFlags,
) -> Result<ElementFlagsUpdate, Error> {
    StorageFlags::update_element_flags(cost, old_flags, new_flags)
        .map(ElementFlagsUpdate::from)
        .map_err(|e| Error::JustInTimeElementFlagsClientError(e.to_string()))
}

/// Drive's flags update, settling owner changes.
pub(crate) fn settling_flags_update(
    cost: &StorageCost,
    old_flags: Option<ElementFlags>,
    new_flags: &mut ElementFlags,
) -> Result<ElementFlagsUpdate, Error> {
    StorageFlags::update_element_flags_settling_owner_changes(cost, old_flags, new_flags)
        .map_err(|e| Error::JustInTimeElementFlagsClientError(e.to_string()))
}

/// The flags update of `mode`.
pub(crate) fn flags_update(
    mode: Mode,
) -> fn(&StorageCost, Option<ElementFlags>, &mut ElementFlags) -> Result<ElementFlagsUpdate, Error>
{
    match mode {
        Mode::Merging => merging_flags_update,
        Mode::Settling => settling_flags_update,
    }
}

/// Drive's removal callback, sectioning freed bytes by epoch and owner.
pub(crate) fn split_removal_bytes(
    flags: &mut ElementFlags,
    removed_key_bytes: u32,
    removed_value_bytes: u32,
) -> Result<(StorageRemovedBytes, StorageRemovedBytes), Error> {
    StorageFlags::split_removal_bytes(flags, removed_key_bytes, removed_value_bytes)
        .map_err(|e| Error::SplitRemovalBytesClientError(e.to_string()))
}

/// The batch options of `mode`.
pub(crate) fn options(mode: Mode) -> BatchApplyOptions {
    BatchApplyOptions {
        settle_owner_changes: mode == Mode::Settling,
        ..Default::default()
    }
}

/// Applies `ops` with the flags update and options of `mode`.
pub(crate) fn apply(
    db: &TempGroveDb,
    ops: Vec<QualifiedGroveDbOp>,
    mode: Mode,
    grove_version: &GroveVersion,
) -> Result<OperationCost, Error> {
    db.apply_batch_with_element_flags_update(
        ops,
        Some(options(mode)),
        flags_update(mode),
        split_removal_bytes,
        None,
        grove_version,
    )
    .cost_as_result()
}

/// A write of `element` at `[tree]/key1`.
pub(crate) fn write_op(element: &Element) -> QualifiedGroveDbOp {
    QualifiedGroveDbOp::insert_or_replace_op(vec![b"tree".to_vec()], KEY.to_vec(), element.clone())
}

/// The bytes the node of `element` at `key1` holds besides its key, as the
/// apply sizes them in a tree of `tree_type`.
pub(crate) fn value_bytes(
    element: &Element,
    tree_type: TreeType,
    grove_version: &GroveVersion,
) -> u32 {
    Element::specialized_costs_for_key_value(
        KEY,
        &element
            .serialize(grove_version)
            .expect("expected to serialize"),
        tree_type.inner_node_type(),
        grove_version,
    )
    .expect("expected value bytes")
}
