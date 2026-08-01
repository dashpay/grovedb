//! Classification of batch overwrites that target an existing indexed tree.
//!
//! Runs inside the per-op loop when tree-override protection is OFF and the
//! op could overwrite an existing element — on V4+ only: the stored-element
//! read this starts with is gated by `overwrite_indexed_cleanup_inspection`,
//! so released versions keep their cost shape.

use std::collections::BTreeMap;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{element::costs::ElementCostExtensions, Merk};
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{GroveOp, KeyInfo},
    Element, Error,
};

/// What "empty" means for each indexed variant offered as a replacement:
/// `Some(true)` for an empty indexed element, `Some(false)` for a non-empty
/// one, `None` when the element is not indexed at all.
///
/// Emptiness cannot be read off any single field. The primary root key, the
/// aggregate(s) AND every secondary root key must all be unset — and for
/// PCPSIT the axes TLV itself stays canonically non-empty even for an empty
/// tree, so only its per-axis root keys count, not its length.
fn replacement_indexed_emptiness(new_element: &Element) -> Option<bool> {
    match new_element.underlying() {
        Element::ProvableSumIndexedTree(p, s, sum, _) => {
            Some(p.is_none() && s.is_none() && *sum == 0)
        }
        Element::ProvableCountIndexedTree(p, s, c, _) => {
            Some(p.is_none() && s.is_none() && *c == 0)
        }
        Element::ProvableCountProvableSumIndexedTree(p, c, sum, axes, _) => Some(
            p.is_none()
                && *c == 0
                && *sum == 0
                && axes.iter().all(|(_, root_key)| root_key.is_none()),
        ),
        _ => None,
    }
}

/// Classify an `op_could_overwrite` insert at `path / key_info` against
/// the existing primary-merk entry, when tree-override protection is
/// **OFF** for this batch. Allows indexed-tree safe-subset overwrites and
/// rejects the ambiguous ones:
///
/// |  existing              |  new                       |  outcome                      |
/// |------------------------|----------------------------|-------------------------------|
/// |  none                  |  *                         |  `Ok(None)`                   |
/// |  non-indexed           |  *                         |  `Ok(None)`                   |
/// |  indexed              |  non-indexed               |  `Ok(Some(indexed_path))`     |
/// |  indexed              |  empty indexed             |  `Ok(Some(indexed_path))`     |
/// |  indexed              |  non-empty indexed         |  `Err(NotSupported)`          |
///
/// When `Ok(Some(cidx_path))` is returned, the caller should push
/// `cidx_path` onto its `cidx_overwrite_cleanup_paths` list so the
/// post-apply pass can clear the old cidx's storage namespaces
/// (subtree prefixes + secondary namespace at
/// `Blake3(primary_prefix ‖ 0x01)`). Non-empty cidx replacement stays
/// rejected because the new element's primary_root_key /
/// secondary_root_key would point at on-disk data while our post-apply
/// cleanup of the OLD cidx's prefixes also clears that data — the
/// storage-pointer semantics are ambiguous (reuse old? fresh?) and
/// the safe answer is to force the caller through delete-then-recreate.
///
/// Additionally, if a safe-subset overwrite is detected but the batch
/// contains *any* write whose qualified path lies strictly under the
/// cidx primary's path, the function returns
/// `Err(InvalidBatchOperation)` — the post-apply cleanup would
/// silently lose those writes. The generic consistency check
/// (`verify_consistency_of_operations`) only blocks writes under
/// `Delete` / `DeleteTree` paths; it does not know about safe-subset
/// cidx-overwrite cleanup, so the descendant-check lives here.
pub(crate) fn inspect_cidx_overwrite<'db, S: StorageContext<'db>>(
    primary_merk: &Merk<S>,
    path: &[Vec<u8>],
    key_info: &KeyInfo,
    new_element: &Element,
    ops_by_qualified_paths: &BTreeMap<Vec<Vec<u8>>, GroveOp>,
    grove_version: &GroveVersion,
) -> CostResult<Option<Vec<Vec<u8>>>, Error> {
    let mut cost = OperationCost::default();

    let maybe_existing = cost_return_on_error!(
        &mut cost,
        primary_merk
            .get(
                key_info.get_key_clone().as_slice(),
                true,
                Some(&Element::value_defined_cost_for_serialized_value),
                grove_version,
            )
            .map_err(|e| Error::CorruptedData(format!(
                "unable to check for existing element: {e}"
            )))
    );

    let Some(existing_bytes) = maybe_existing else {
        return Ok(None).wrap_with_cost(cost);
    };

    let existing_element = cost_return_on_error_no_add!(
        cost,
        Element::deserialize(existing_bytes.as_slice(), grove_version).map_err(|_| {
            Error::CorruptedData("unable to deserialize existing element".to_string())
        })
    );

    if !existing_element.is_indexed_tree() {
        return Ok(None).wrap_with_cost(cost);
    }

    if matches!(replacement_indexed_emptiness(new_element), Some(false)) {
        return Err(Error::NotSupported(
            "overwriting an existing indexed tree with a NON-EMPTY indexed tree via the \
             batch path is not supported (storage-pointer semantics \
             are ambiguous: the new element's root_keys would refer to data while the \
             post-apply cleanup also clears it). DeleteTree the old indexed tree and re-create \
             the new state in a follow-up batch"
                .to_string(),
        ))
        .wrap_with_cost(cost);
    }

    // Safe subset: indexed → non-indexed OR indexed → empty indexed.
    // Schedule the OLD indexed tree's storage namespaces for cleanup. Its path is
    // `path + key_info`.
    let mut cidx_path = path.to_vec();
    cidx_path.push(key_info.get_key_clone());

    // CONSISTENCY CHECK: writes UNDER the cidx's path in the same batch
    // would be silently lost when the post-apply cleanup clears the prefix.
    //
    // This loop is currently UNREACHABLE, and deliberately kept anyway.
    // The reason it cannot fire is self-cancelling: a descendant write makes
    // the deeper level bubble a `ReplaceTreeRootKey` into this key's slot
    // before the shallower level runs, so by the time this function reads the
    // existing element it no longer sees an indexed tree and returns above,
    // never reaching here. In other words the very condition being checked
    // for is what prevents the check from running. Verified by instrumenting
    // both this loop and the function entry: for a PCIT overwritten with a
    // plain tree alongside a write underneath it, the function is entered and
    // returns early, and the loop is never reached.
    //
    // It stays because the unreachability is a property of the ORDER the
    // levels are processed in, not of this function — reorder the bubble-up,
    // or call the classifier from anywhere that reads the pre-batch element,
    // and the hazard is live again. Do not treat it as protection that exists
    // today.
    let cidx_path_len = cidx_path.len();
    for q_path in ops_by_qualified_paths.keys() {
        if q_path.len() > cidx_path_len && q_path[..cidx_path_len] == cidx_path[..] {
            return Err(Error::InvalidBatchOperation(
                "batch contains a write under a cidx primary path that is being \
                 safe-subset-overwritten in the same batch; the post-apply cleanup \
                 would silently clear the descendant write. Split into two batches: \
                 delete + recreate first, then populate.",
            ))
            .wrap_with_cost(cost);
        }
    }

    Ok(Some(cidx_path)).wrap_with_cost(cost)
}
