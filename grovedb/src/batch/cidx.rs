//! Cidx-specific helpers for the batch apply pipeline.
//!
//! These functions encapsulate the count-indexed-tree (cidx) primary
//! propagation steps that `execute_ops_on_path` runs around the merk
//! apply boundary:
//!
//! - [`capture_cidx_pre_state`] reads the *old* count value of every key
//!   in this batch level's ops, before the merk is mutated, so the
//!   post-apply mirror can compute `(old_count, new_count)` deltas.
//! - [`apply_cidx_secondary_mirror_post_apply`] runs after the primary
//!   merk's `apply_with_specialized_costs` returns. It re-reads each
//!   captured key's new count, builds a deterministic delta list, and
//!   applies the corresponding `mirror_to_secondary_for_batch` calls
//!   on the cidx secondary merk, returning the secondary's post-mirror
//!   `(root_hash, root_key)` for the bubble-up code to fold into the
//!   parent's H1-A composition.
//!
//! Kept out of `mod.rs` to keep that file focused on the generic batch
//! pipeline; the cidx-specific propagation pattern is self-contained
//! and reusable for the planned `SumIndexedTree` and other
//! aggregate-indexed variants once they land.

use std::collections::{BTreeMap, HashMap};

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::{element::costs::ElementCostExtensions, CryptoHash, Merk};
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;

use super::{GroveOp, KeyInfo};
use crate::{operations::count_indexed_tree::MAX_CIDX_ITEM_KEY_LEN, Element, Error};

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
) -> CostResult<HashMap<Vec<u8>, Option<u64>>, Error> {
    let mut cost = OperationCost::default();

    // Bound the item key length so the derived secondary key
    // (count_be ‖ item_key) stays under merk's 256-byte limit.
    // Generic batch validation only enforces the 255-byte cap; cidx
    // primaries need 247 bytes to leave room for the 8-byte count
    // prefix in the secondary.
    for (key_info, _) in ops_at_path_by_key.iter() {
        if key_info.get_key_clone().len() > MAX_CIDX_ITEM_KEY_LEN {
            return Err(Error::InvalidInput(
                "item key for a CountIndexedTree primary must be at most 247 bytes in batch \
                 ops (the secondary index prepends an 8-byte count, and Merk requires keys \
                 < 256 bytes)",
            ))
            .wrap_with_cost(cost);
        }
    }

    let mut pre: HashMap<Vec<u8>, Option<u64>> = HashMap::new();
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
/// **Determinism note:** the secondary mirror deltas are sorted *pure
/// deletes first, then by key* before being applied. Without this
/// sort, HashMap iteration order produces non-deterministic ordering
/// of secondary mirror operations. Empirical reproduction showed that
/// when an INSERT delta runs BEFORE a DELETE delta on the same
/// secondary merk, the delete sometimes fails to actually remove the
/// entry — leaving stale secondary state. The underlying merk-level
/// bug (delete-after-insert on a count-bearing Merk) needs separate
/// investigation; this sort enforces a known-good order in the
/// meantime.
///
/// Classification: a delta `(_, Some(_), None)` is a pure delete
/// (key removed from primary). For each individual delta,
/// `mirror_to_secondary_for_batch` does a delete-then-insert ON ONE
/// KEY which is fine; the order issue only surfaces ACROSS deltas of
/// different keys.
pub(crate) fn apply_cidx_secondary_mirror_post_apply<'db, S: StorageContext<'db>>(
    primary_merk: &Merk<S>,
    pre: HashMap<Vec<u8>, Option<u64>>,
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

    deltas.sort_by(|a, b| {
        let a_is_pure_delete = a.1.is_some() && a.2.is_none();
        let b_is_pure_delete = b.1.is_some() && b.2.is_none();
        match b_is_pure_delete.cmp(&a_is_pure_delete) {
            std::cmp::Ordering::Equal => a.0.cmp(&b.0),
            other => other,
        }
    });
    for (key, old_count, new_count) in deltas {
        cost_return_on_error!(
            &mut cost,
            crate::operations::count_indexed_tree::mirror_to_secondary_for_batch(
                secondary_merk,
                key.as_slice(),
                old_count,
                new_count,
                grove_version,
            )
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
