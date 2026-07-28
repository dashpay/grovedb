//! `CountIndexedTree` (cidx) helpers for the batch apply pipeline.
//!
//! These functions encapsulate the count-indexed-tree primary
//! propagation steps that `execute_ops_on_path` runs around the merk
//! apply boundary, plus the pre-apply consistency checks the
//! `apply_batch*` entry points run before any merk is touched:
//!
//! - [`capture_cidx_pre_state`] reads the *old* count value of every key
//!   in this batch level's ops, before the merk is mutated, so the
//!   post-apply mirror can compute `(old_count, new_count)` deltas.
//! - [`apply_cidx_secondary_mirror_post_apply`] runs after the primary
//!   merk's `apply_with_specialized_costs` returns. It re-reads each
//!   captured key's new count, builds a deterministic delta list, and
//!   applies one atomic Merk batch to the cidx secondary, returning its post-mirror
//!   `(root_hash, root_key)` for the bubble-up code to fold into the
//!   parent's H1-A composition.
//! - [`reject_freshly_inserted_cidx_with_descendants`] is the preflight
//!   guard that rejects batches which both create a cidx primary AND
//!   write under that cidx in the same batch — there is no
//!   `InsertAggregateIndexedTreeWithRootKeys` op, so the H1-A
//!   propagation cannot read the just-created cidx element from a
//!   parent merk that hasn't been flushed yet.
//! - [`inspect_cidx_overwrite`] runs inside the per-op loop when
//!   tree-override protection is OFF and the op could overwrite an
//!   existing element. It classifies every indexed-tree variant against
//!   the safe-subset rules (indexed → non-indexed OR indexed → empty
//!   indexed are allowed and scheduled for cleanup; indexed → non-empty
//!   indexed is rejected as ambiguous; descendants-in-same-batch are rejected
//!   because the post-apply cleanup would silently clear them).
//!
//! Kept out of `mod.rs` to keep that file focused on the generic batch
//! pipeline; the cidx-specific propagation pattern is self-contained
//! and reusable for the planned `SumIndexedTree` and other
//! aggregate-indexed variants once they land.

use std::collections::BTreeMap;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_element::indexed::IndexAxis;
use grovedb_merk::{
    element::{
        costs::ElementCostExtensions, delete::ElementDeleteFromStorageExtensions,
        insert::ElementInsertToStorageExtensions, tree_type::ElementTreeTypeExtensions,
    },
    tree_type::TreeType,
    BatchEntry, CryptoHash, Merk,
};
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;

use super::{GroveOp, KeyInfo, QualifiedGroveDbOp};
use crate::{operations::indexed_tree::MAX_CIDX_ITEM_KEY_LEN, Element, Error};

/// Capture, *before* batch ops are applied to the primary merk, the
/// pre-apply `count_value` for each key this level's ops will mutate.
/// Used by the post-apply mirror to compute `(old_count, new_count)`
/// deltas for each affected cidx primary entry.
///
/// Returns `Err(Error::InvalidInput)` if any key in `ops_at_path_by_key`
/// exceeds the cidx 247-byte ceiling (the secondary index prepends an
/// 8-byte count, and merk requires keys < 256 bytes). This check is
/// here rather than in the generic key-length validator because only
/// cidx primaries carry the 247 byte requirement.
///
/// Only ops whose `can_mutate_child_count()` is true are captured —
/// non-count-mutating ops (e.g., `CommitmentTreeInsert`) are skipped.
pub(crate) fn capture_cidx_pre_state<'db, S: StorageContext<'db>>(
    primary_merk: &Merk<S>,
    ops_at_path_by_key: &BTreeMap<KeyInfo, GroveOp>,
    grove_version: &GroveVersion,
) -> CostResult<BTreeMap<Vec<u8>, Option<u64>>, Error> {
    let mut cost = OperationCost::default();

    // Bound the item key length so the derived secondary key
    // (count_be ‖ item_key) stays under merk's 256-byte limit.
    // Generic batch validation only enforces the 255-byte cap; cidx
    // primaries need 247 bytes to leave room for the 8-byte count
    // prefix in the secondary.
    for key_info in ops_at_path_by_key.keys() {
        if key_info.as_slice().len() > MAX_CIDX_ITEM_KEY_LEN {
            return Err(Error::InvalidInput(
                "item key for a CountIndexedTree primary must be at most 247 bytes in batch \
                 ops (the secondary index prepends an 8-byte count, and Merk requires keys \
                 < 256 bytes)",
            ))
            .wrap_with_cost(cost);
        }
    }

    // Same rule the dedicated insert paths enforce
    // (`reject_non_empty_dedicated_indexed_child_claim`): a child of an
    // indexed primary may not claim a NON-ZERO aggregate while being
    // ROOTLESS. With no contents to derive the value from it is a bare
    // assertion, and under an indexed primary it becomes the authenticated
    // secondary sort key and the parent's aggregate contribution.
    //
    // The batch door has to be shut as well as the dedicated one: an
    // `insert_or_replace_op` carrying `ProvableCountTree(None, 9, None)` under
    // a PCIT was accepted and written, and `verify_grovedb` then reported the
    // child as an aggregate mismatch (recorded 9 against an empty inner Merk).
    // The legitimate way to reach count 9 is to insert the child empty and
    // populate it in the same batch, letting propagation derive it.
    for op in ops_at_path_by_key.values() {
        let element = match op {
            GroveOp::InsertOrReplace { element }
            | GroveOp::Replace { element }
            | GroveOp::InsertIfNotExists { element, .. }
            | GroveOp::InsertWithKnownToNotAlreadyExist { element } => element,
            _ => continue,
        };
        let rootless_with_aggregate = match element.underlying() {
            Element::SumTree(None, sum, _) | Element::ProvableSumTree(None, sum, _) => *sum != 0,
            Element::BigSumTree(None, big_sum, _) => *big_sum != 0,
            Element::CountTree(None, count, _) | Element::ProvableCountTree(None, count, _) => {
                *count != 0
            }
            Element::CountSumTree(None, count, sum, _)
            | Element::ProvableCountSumTree(None, count, sum, _)
            | Element::ProvableCountProvableSumTree(None, count, sum, _) => {
                *count != 0 || *sum != 0
            }
            _ => false,
        };
        if rootless_with_aggregate {
            return Err(Error::InvalidBatchOperation(
                "a child of an indexed-tree primary may not claim a non-zero \
                 aggregate while having no root key: with no contents to derive \
                 it from, the value is a bare assertion that would become the \
                 authenticated secondary sort key. Insert the child empty and \
                 populate it (in the same batch is fine) so the aggregate is derived",
            ))
            .wrap_with_cost(cost);
        }
    }

    let mut pre: BTreeMap<Vec<u8>, Option<u64>> = BTreeMap::new();
    for (key_info, op) in ops_at_path_by_key.iter() {
        let key_bytes = key_info.get_key_clone();
        // Single source of truth: `GroveOp::can_mutate_child_count`
        // uses an exhaustive match so adding a new variant forces
        // explicit classification at the type-system level. This is
        // the structural guard against the nested-cidx bug class
        // (commit a8bb34fb).
        if op.can_mutate_child_count() && !pre.contains_key(&key_bytes) {
            let maybe_bytes = cost_return_on_error!(
                &mut cost,
                primary_merk
                    .get(
                        key_bytes.as_slice(),
                        true,
                        Some(&Element::value_defined_cost_for_serialized_value),
                        grove_version,
                    )
                    .map_err(|e| Error::CorruptedData(format!(
                        "cidx pre-state read for key {}: {e}",
                        hex::encode(&key_bytes)
                    )))
            );
            let old_count = if let Some(bytes) = maybe_bytes {
                let elem = cost_return_on_error_no_add!(
                    cost,
                    Element::deserialize(bytes.as_slice(), grove_version).map_err(|e| {
                        Error::CorruptedData(format!("cidx pre-state deserialize: {e}"))
                    })
                );
                Some(elem.count_value_or_default())
            } else {
                None
            };
            pre.insert(key_bytes, old_count);
        }
    }
    Ok(pre).wrap_with_cost(cost)
}

/// Apply the secondary-mirror update for every key captured by
/// [`capture_cidx_pre_state`], after the primary merk's batch ops
/// have been applied. Returns the secondary merk's post-mirror
/// `(root_hash, root_key)` for the caller to fold into the parent's
/// H1-A `combine_hash_three` composition.
///
/// **Determinism and atomic-transition note:** the input `pre` is a
/// `BTreeMap`, so iteration is key-sorted. All secondary deletes and inserts
/// are assembled into one sorted Merk batch. Applying them as separate Merk
/// mutations can retain a stale row after multiple same-level count changes;
/// one atomic batch gives the secondary a single pre/post transition.
pub(crate) fn apply_cidx_secondary_mirror_post_apply<'db, S: StorageContext<'db>>(
    primary_merk: &Merk<S>,
    pre: BTreeMap<Vec<u8>, Option<u64>>,
    secondary_merk: &mut Merk<S>,
    grove_version: &GroveVersion,
) -> CostResult<(CryptoHash, Option<Vec<u8>>), Error> {
    let mut cost = OperationCost::default();

    let mut deltas: Vec<(Vec<u8>, Option<u64>, Option<u64>)> = Vec::with_capacity(pre.len());
    for (key, old_count) in pre {
        let maybe_bytes = cost_return_on_error!(
            &mut cost,
            primary_merk
                .get(
                    key.as_slice(),
                    true,
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version,
                )
                .map_err(|e| Error::CorruptedData(format!(
                    "cidx post-state read for key {}: {e}",
                    hex::encode(&key)
                )))
        );
        let new_count = if let Some(bytes) = maybe_bytes {
            let elem = cost_return_on_error_no_add!(
                cost,
                Element::deserialize(bytes.as_slice(), grove_version).map_err(|e| {
                    Error::CorruptedData(format!("cidx post-state deserialize: {e}"))
                })
            );
            Some(elem.count_value_or_default())
        } else {
            None
        };
        deltas.push((key, old_count, new_count));
    }

    let secondary_tree_type = TreeType::ProvableCountTree;
    let mut secondary_batch: Vec<BatchEntry<Vec<u8>>> = Vec::with_capacity(deltas.len() * 2);
    for (key, old_count, new_count) in &deltas {
        if old_count == new_count {
            continue;
        }
        if let Some(old_count) = old_count {
            let old_secondary_key = crate::operations::indexed_tree::make_axis_secondary_key(
                IndexAxis::Count,
                *old_count,
                0,
                key,
            );
            cost_return_on_error!(
                &mut cost,
                Element::delete_into_batch_operations(
                    old_secondary_key,
                    false,
                    secondary_tree_type,
                    &mut secondary_batch,
                    grove_version,
                )
                .map_err(Error::MerkError)
            );
        }
        if let Some(new_count) = new_count {
            let new_secondary_key = crate::operations::indexed_tree::make_axis_secondary_key(
                IndexAxis::Count,
                *new_count,
                0,
                key,
            );
            let entry = Element::new_item(Vec::new());
            let feature_type = cost_return_on_error_no_add!(
                cost,
                entry
                    .get_feature_type(secondary_tree_type)
                    .map_err(Error::MerkError)
            );
            cost_return_on_error!(
                &mut cost,
                entry
                    .insert_into_batch_operations(
                        new_secondary_key,
                        &mut secondary_batch,
                        feature_type,
                        grove_version,
                    )
                    .map_err(Error::MerkError)
            );
        }
    }
    secondary_batch.sort_by(|a, b| a.0.cmp(&b.0));
    if !secondary_batch.is_empty() {
        cost_return_on_error!(
            &mut cost,
            secondary_merk
                .apply_with_specialized_costs::<_, Vec<u8>>(
                    &secondary_batch,
                    &[],
                    None,
                    &|key, value| {
                        Element::specialized_costs_for_key_value(
                            key,
                            value,
                            secondary_tree_type.inner_node_type(),
                            grove_version,
                        )
                        .map_err(|e| grovedb_merk::Error::ClientCorruptionError(e.to_string()))
                    },
                    Some(&Element::value_defined_cost_for_serialized_value),
                    grove_version,
                )
                .map_err(Error::MerkError)
        );
    }
    let (sec_hash, sec_root_key, _) = cost_return_on_error!(
        &mut cost,
        secondary_merk
            .root_hash_key_and_aggregate_data()
            .map_err(|e| Error::CorruptedData(format!(
                "cidx secondary root hash capture after mirror: {e}"
            )))
    );
    Ok((sec_hash, sec_root_key)).wrap_with_cost(cost)
}

/// Preflight check: reject any batch that both **creates** a
/// `CountIndexedTree` / `ProvableCountIndexedTree` element AND
/// contains other ops targeting paths inside the freshly-created
/// cidx in the same batch.
///
/// Why: cidx propagation needs both primary and secondary root state
/// to bubble up via the H1-A `combine_hash_three` composition. There
/// is no `InsertAggregateIndexedTreeWithRootKeys` counterpart to
/// `ReplaceAggregateIndexedTreeRootKeys`, and the secondary merk
/// cannot be opened during propagation because the parent's cidx
/// element bytes aren't on disk yet. Without this preflight, callers
/// hit a confusing `MerkError(PathKeyNotFound)` mid-batch as the
/// secondary-merk closure tries to read the cidx element from a
/// parent merk that doesn't yet contain it.
///
/// Workaround: split into two batches. First batch creates the
/// empty cidx; second batch populates it (or call
/// `db.insert_into_count_indexed_tree` directly for individual
/// items).
pub(crate) fn reject_freshly_inserted_cidx_with_descendants(
    ops: &[QualifiedGroveDbOp],
) -> Result<(), Error> {
    // Collect paths where a cidx element is being CREATED in this batch
    // (via any Insert-style op carrying a cidx Element). The path of the
    // cidx primary is `op.path + op.key`.
    let mut fresh_cidx_paths: Vec<Vec<Vec<u8>>> = Vec::new();
    for op in ops {
        let elem = match &op.op {
            GroveOp::InsertOrReplace { element }
            | GroveOp::InsertWithKnownToNotAlreadyExist { element }
            | GroveOp::InsertIfNotExists { element, .. }
            | GroveOp::Replace { element }
            | GroveOp::Patch { element, .. } => element,
            _ => continue,
        };
        if matches!(elem.underlying(), Element::ProvableCountIndexedTree(..))
            && let Some(key) = &op.key
        {
            let mut cidx_path = op.path.to_path();
            cidx_path.push(key.get_key_clone());
            fresh_cidx_paths.push(cidx_path);
        }
    }
    if fresh_cidx_paths.is_empty() {
        return Ok(());
    }
    // Reject any op whose effective target path is strictly under one
    // of the fresh cidx paths. The effective path is
    // `op.path + op.key` (keyless ops use just `op.path`). The
    // cidx-creation op itself doesn't trigger (its target equals the
    // cidx path exactly).
    for op in ops {
        let mut op_target = op.path.to_path();
        if let Some(key) = &op.key {
            op_target.push(key.get_key_clone());
        }
        for cidx_path in &fresh_cidx_paths {
            if op_target.len() > cidx_path.len() && op_target[..cidx_path.len()] == cidx_path[..] {
                return Err(Error::NotSupported(
                    "populating a freshly-inserted CountIndexedTree / \
                     ProvableCountIndexedTree in the same batch as its creation is not \
                     supported (no Insert variant for aggregate-indexed two-Merk \
                     propagation exists, and the secondary merk cannot be opened from \
                     stale parent state during bubble-up). Split into two batches: \
                     insert the empty cidx first, then populate it via \
                     `db.insert_into_count_indexed_tree` or a follow-up batch."
                        .to_string(),
                ));
            }
        }
    }
    Ok(())
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

    // Classify the complete indexed family. Empty PCPSIT elements retain a
    // canonical non-empty axes list, but every axis root key must be None.
    let new_indexed_empty = match new_element.underlying() {
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
    };
    if matches!(new_indexed_empty, Some(false)) {
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

    // CONSISTENCY CHECK: writes UNDER the cidx's path in the same
    // batch would be silently lost when the post-apply cleanup
    // clears the prefix.
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
