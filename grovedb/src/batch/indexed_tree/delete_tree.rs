//! The `DeleteTree` type check behind the `delete_tree_cleanup_type_source`
//! V4 gate.
//!
//! A batch `DeleteTree` op carries a caller-declared `TreeType`, which on
//! V1..V3 is taken at face value (reading the stored element costs an extra
//! seek + load per op, and cost feeds fees on the released versions). On V4+
//! the stored element is read and its ACTUAL type selects the cleanup
//! namespaces — a declared type that hid a stored indexed primary would skip
//! the per-axis secondary sweep and leave authenticated stale rows.

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_merk::{
    element::{get::ElementFetchFromStorageExtensions, tree_type::ElementTreeTypeExtensions},
    TreeType,
};
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;

use crate::{Element, Error};

/// Treat the tree type carried by `DeleteTree` as a checked claim, not as
/// storage-ownership authority. Cleanup namespaces must be selected from the
/// element that is actually stored at the target key.
pub(crate) fn validate_delete_tree_type<'db, S: StorageContext<'db>>(
    parent_storage: &S,
    key: &[u8],
    declared_tree_type: TreeType,
    grove_version: &GroveVersion,
) -> CostResult<TreeType, Error> {
    let mut cost = OperationCost::default();
    let stored_element = cost_return_on_error!(
        &mut cost,
        Element::get_from_storage(parent_storage, key, grove_version).map_err(Error::MerkError)
    );
    let Some(actual_tree_type) = stored_element.tree_type() else {
        return Err(Error::InvalidBatchOperation(
            "DeleteTree target exists but is not a tree element",
        ))
        .wrap_with_cost(cost);
    };
    // Reject a mismatch only when an indexed tree is involved on either
    // side. The security property this guards is cleanup-namespace
    // selection: a declared type that hides a stored indexed primary would
    // skip the per-axis secondary sweep and leave authenticated stale rows.
    // For two non-indexed types the declaration is merely advisory — the
    // caller-supplied value is discarded in favour of `actual_tree_type`
    // below, so nothing downstream can be steered by it.
    //
    // Rejecting every mismatch instead would change acceptance behaviour on
    // GROVE_V1/V2/V3, which are live: a batch declaring `NormalTree` for a
    // `SumTree` delete succeeds today (the repo's own deletion-cost tests do
    // exactly that), and nodes carrying a strict check would diverge from
    // nodes that do not. Indexed trees exist on no live version, so scoping
    // the rejection to them preserves live behaviour exactly while closing
    // the actual hole.
    if actual_tree_type != declared_tree_type
        && (actual_tree_type.is_indexed_primary() || declared_tree_type.is_indexed_primary())
    {
        return Err(Error::InvalidBatchOperation(
            "DeleteTree declared tree type does not match the stored element",
        ))
        .wrap_with_cost(cost);
    }
    Ok(actual_tree_type).wrap_with_cost(cost)
}
