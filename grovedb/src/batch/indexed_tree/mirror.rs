//! The per-axis secondary mirror, run after the primary merk's batch apply.
//!
//! Each configured axis maintains a secondary Merk ordered by that axis's
//! sort key. The mirror re-reads every captured key's post-apply aggregates,
//! turns each old → new transition into that axis's row moves, and applies
//! them as ONE atomic Merk batch so the secondary makes a single pre/post
//! transition.

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
    BatchEntry, CryptoHash, Merk,
};
use grovedb_storage::StorageContext;
use grovedb_version::version::GroveVersion;

use super::{read_entry_state, AggregateTransition, MaybeEntryState};
use crate::{
    operations::indexed_tree::{axis_row_reference, make_axis_secondary_key},
    Element, Error,
};

/// Enforce the precise per-axis item-key bound: avg prepends a 16-byte sort
/// key, count and sum 8, so the same item key can be legal on one axis and
/// not another. Runs before any secondary write; erroring here aborts the
/// whole batch with its storage batch discarded, so nothing is committed.
fn enforce_axis_item_key_bound<'a>(
    keys: impl Iterator<Item = &'a Vec<u8>>,
    axis: IndexAxis,
) -> Result<(), Error> {
    let max_item_key_len = crate::operations::indexed_tree::max_item_key_len_for_axis(axis);
    for key in keys {
        if key.len() > max_item_key_len {
            return Err(Error::InvalidInput(
                "item key for an indexed-tree primary is too long for a configured axis's \
                 sort key (count/sum allow 247 bytes, avg 239); the secondary key is \
                 sort_key ‖ item_key and Merk requires keys < 256 bytes",
            ));
        }
    }
    Ok(())
}

/// Re-read each captured key's aggregates after the primary apply, pairing
/// them with the captured pre-state into one transition per key.
///
/// Called ONCE per level, not once per axis: the `(count, sum)` transitions
/// are axis-independent — only the sort-key encoding differs per axis — so
/// the caller computes them once and hands the same slice to every axis's
/// [`apply_indexed_secondary_mirror_post_apply`]. Re-reading inside the
/// per-axis call charged a PCPSIT batch three primary reads per key where
/// one suffices.
///
/// The result is key-sorted (built from the `BTreeMap` capture), which is
/// what makes each axis's assembled batch deterministic.
pub(crate) fn read_post_apply_transitions<'db, S: StorageContext<'db>>(
    primary_merk: &Merk<S>,
    pre: &BTreeMap<Vec<u8>, MaybeEntryState>,
    grove_version: &GroveVersion,
) -> CostResult<Vec<AggregateTransition>, Error> {
    let mut cost = OperationCost::default();
    let mut transitions: Vec<AggregateTransition> = Vec::with_capacity(pre.len());
    for (key, old_state) in pre {
        let new_state = cost_return_on_error!(
            &mut cost,
            read_entry_state(primary_merk, key, "post", grove_version)
        );
        transitions.push((key.clone(), *old_state, new_state));
    }
    Ok(transitions).wrap_with_cost(cost)
}

/// Assemble one axis's secondary row moves into a sorted Merk batch.
///
/// Each transition is compared as this axis's `(key, payload)` rather than
/// the raw aggregates: on the avg axis two different `(count, sum)` pairs
/// can share a sort key while carrying different payloads, and on the count
/// axis a sum change moves nothing at all.
fn build_axis_mirror_batch(
    transitions: &[AggregateTransition],
    axis: IndexAxis,
    grove_version: &GroveVersion,
) -> CostResult<Vec<BatchEntry<Vec<u8>>>, Error> {
    let mut cost = OperationCost::default();
    let secondary_tree_type = crate::operations::indexed_tree::axis_secondary_tree_type(axis);
    let mut secondary_batch: Vec<BatchEntry<Vec<u8>>> = Vec::with_capacity(transitions.len() * 2);
    for (key, old_state, new_state) in transitions {
        if old_state == new_state {
            continue;
        }
        let old_key =
            old_state.map(|state| make_axis_secondary_key(axis, state.count, state.sum, key));
        let new_entry = new_state
            .map(|state| {
                Ok((
                    make_axis_secondary_key(axis, state.count, state.sum, key),
                    axis_row_reference(axis, key, state.count, state.sum)?,
                    state.value_hash,
                ))
            })
            .transpose();
        let new_entry = cost_return_on_error_no_add!(cost, new_entry);
        // Delete the old row ONLY if the new one lands on a different key.
        // On the avg axis a change can alter the payload while leaving the
        // sort key fixed — (count, sum) going (1, 5) -> (2, 10) keeps
        // avg = 5 — and emitting a delete plus a put for one key in a single
        // Merk batch is rejected outright ("Keys in batch must be unique"),
        // failing the whole GroveDB batch. Where the key is unchanged the put
        // alone overwrites the payload, which is what the row needs.
        let new_secondary_key_ref = new_entry.as_ref().map(|(key, ..)| key);
        if let Some(old_secondary_key) = &old_key
            && Some(old_secondary_key) != new_secondary_key_ref
        {
            cost_return_on_error!(
                &mut cost,
                Element::delete_into_batch_operations(
                    old_secondary_key.clone(),
                    false,
                    secondary_tree_type,
                    &mut secondary_batch,
                    grove_version,
                )
                .map_err(Error::MerkError)
            );
        }
        if let Some((new_secondary_key, entry, target_hash)) = new_entry {
            let feature_type = cost_return_on_error_no_add!(
                cost,
                entry
                    .get_feature_type(secondary_tree_type)
                    .map_err(Error::MerkError)
            );
            cost_return_on_error!(
                &mut cost,
                entry
                    .insert_reference_into_batch_operations(
                        new_secondary_key,
                        target_hash,
                        &mut secondary_batch,
                        feature_type,
                        grove_version,
                    )
                    .map_err(Error::MerkError)
            );
        }
    }
    secondary_batch.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(secondary_batch).wrap_with_cost(cost)
}

/// Apply one axis's secondary-mirror update for every captured transition,
/// after the primary merk's batch ops have been applied. Returns that axis
/// secondary's post-mirror `(root_hash, root_key)` so the caller can fold it
/// into the parent's H1-A composition — directly for the single-axis
/// variants, or through `axes_digest` for PCPSIT.
///
/// Call once per configured axis, with the SAME transitions slice from one
/// [`read_post_apply_transitions`] call. Each axis derives its own sort key
/// from the same `(count, sum)` pair, so an entry can move in the count
/// index while staying put in the sum index, and the avg index can move when
/// neither of the other two does.
///
/// **Determinism and atomic-transition note:** the transitions slice is
/// key-sorted (built from the `BTreeMap` capture). All deletes and inserts
/// for this axis are assembled into one sorted Merk batch. Applying them as
/// separate Merk mutations can retain a stale row after multiple same-level
/// changes; one atomic batch gives the secondary a single pre/post
/// transition.
pub(crate) fn apply_indexed_secondary_mirror_post_apply<'db, S: StorageContext<'db>>(
    transitions: &[AggregateTransition],
    axis: IndexAxis,
    secondary_merk: &mut Merk<S>,
    grove_version: &GroveVersion,
) -> CostResult<(CryptoHash, Option<Vec<u8>>), Error> {
    let mut cost = OperationCost::default();

    cost_return_on_error_no_add!(
        cost,
        enforce_axis_item_key_bound(transitions.iter().map(|(key, ..)| key), axis)
    );
    let secondary_batch = cost_return_on_error!(
        &mut cost,
        build_axis_mirror_batch(transitions, axis, grove_version)
    );

    if !secondary_batch.is_empty() {
        let secondary_tree_type = crate::operations::indexed_tree::axis_secondary_tree_type(axis);
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
                "indexed secondary root hash capture after mirror: {e}"
            )))
    );
    Ok((sec_hash, sec_root_key)).wrap_with_cost(cost)
}
