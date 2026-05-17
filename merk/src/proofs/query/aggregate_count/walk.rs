//! No-proof walker: same classification logic as the proof emitter, but
//! returns only the in-range count without allocating proof ops.

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_version::version::GroveVersion;

use super::provable_count_from_aggregate;
use crate::{
    proofs::query::{
        aggregate_common::{classify_subtree, SubtreeClassification},
        QueryItem,
    },
    tree::{kv::ValueDefinedCostType, Fetch, RefWalker},
    Error,
};

/// Read the provable-count aggregate off the walker's current tree node.
/// Shared error-mapping helper used by [`walk_count_only`] at both the
/// Contained-leaf and Boundary positions.
fn provable_count_from_walker<S>(walker: &RefWalker<'_, S>) -> Result<u64, Error>
where
    S: Fetch + Sized + Clone,
{
    let aggregate = walker
        .tree()
        .aggregate_data()
        .map_err(|e| Error::InvalidProofError(format!("aggregate_data: {}", e)))?;
    provable_count_from_aggregate(aggregate)
}

/// No-proof variant of [`super::emit::emit_count_proof`]: walks the same
/// classification path (Contained / Disjoint / Boundary) but only
/// returns the running in-range count.
///
/// At entry, `subtree_lo_excl` / `subtree_hi_excl` are the inherited
/// exclusive key bounds for the subtree this walker points at (both
/// `None` at the root call). The walk reads each node's
/// `aggregate_data()` and each child link's `aggregate_data().as_count_u64()`
/// exactly the same way the proof emitter does, so the returned count
/// is identical to the `count` field returned by
/// `create_aggregate_count_on_range_proof`.
pub(super) fn walk_count_only<S>(
    walker: &mut RefWalker<'_, S>,
    range: &QueryItem,
    subtree_lo_excl: Option<&[u8]>,
    subtree_hi_excl: Option<&[u8]>,
    grove_version: &GroveVersion,
) -> CostResult<u64, Error>
where
    S: Fetch + Sized + Clone,
{
    let mut cost = OperationCost::default();

    // Classify the current subtree against the inner range.
    match classify_subtree(subtree_lo_excl, subtree_hi_excl, range) {
        // Disjoint: subtree contributes 0 to the in-range count.
        SubtreeClassification::Disjoint => Ok(0).wrap_with_cost(cost),
        // Contained: subtree contributes its full stored aggregate
        // (NonCounted entries are already excluded — their stored
        // aggregate is 0).
        SubtreeClassification::Contained => {
            let count = cost_return_on_error_no_add!(cost, provable_count_from_walker(walker));
            Ok(count).wrap_with_cost(cost)
        }
        // Boundary: descend into both children and add own_count.
        SubtreeClassification::Boundary => {
            // Snapshot what we need from the current node before walking.
            // walk(...) takes &mut self.tree, so we must drop any existing
            // borrows on walker.tree() before calling it.
            let node_key: Vec<u8> = walker.tree().key().to_vec();
            let node_count = cost_return_on_error_no_add!(cost, provable_count_from_walker(walker));
            let left_link_aggregate: u64 = walker
                .tree()
                .link(true)
                .map(|l| l.aggregate_data().as_count_u64())
                .unwrap_or(0);
            let right_link_aggregate: u64 = walker
                .tree()
                .link(false)
                .map(|l| l.aggregate_data().as_count_u64())
                .unwrap_or(0);
            let left_link_present = walker.tree().link(true).is_some();
            let right_link_present = walker.tree().link(false).is_some();

            let mut total: u64 = 0;

            // LEFT child. If link is Some, walk(true) must yield Some; the
            // proof variant has the verifier to catch silent inconsistencies,
            // but this no-proof path returns the count straight to the
            // caller — so we fail loudly on impossible state rather than
            // silently undercounting.
            if left_link_present {
                let walked = cost_return_on_error!(
                    &mut cost,
                    walker.walk(
                        true,
                        None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                        grove_version,
                    )
                );
                let mut left_walker = match walked {
                    Some(lw) => lw,
                    None => {
                        return Err(Error::CorruptedState(
                            "tree.link(true) was Some but walk(true) returned None",
                        ))
                        .wrap_with_cost(cost);
                    }
                };
                let n = cost_return_on_error!(
                    &mut cost,
                    walk_count_only(
                        &mut left_walker,
                        range,
                        subtree_lo_excl,
                        Some(node_key.as_slice()),
                        grove_version,
                    )
                );
                total = total.saturating_add(n);
            }

            // Current node's own_count: 1 if in-range and counted, 0 for
            // NonCounted-wrapped (which has stored aggregate 0, so the
            // subtraction yields 0). `checked_sub` (not `saturating_sub`)
            // because children claiming more keys than the parent's
            // aggregate is corrupted state, not something to silently
            // clamp to 0.
            if range.contains(&node_key) {
                let own_count = node_count
                    .checked_sub(left_link_aggregate)
                    .and_then(|n| n.checked_sub(right_link_aggregate))
                    .ok_or(Error::CorruptedState(
                        "child structural counts exceed parent's aggregate count",
                    ));
                let own_count = cost_return_on_error_no_add!(cost, own_count);
                total = total.saturating_add(own_count);
            }

            // RIGHT child — same fail-fast pattern as LEFT.
            if right_link_present {
                let walked = cost_return_on_error!(
                    &mut cost,
                    walker.walk(
                        false,
                        None::<&fn(&[u8], &GroveVersion) -> Option<ValueDefinedCostType>>,
                        grove_version,
                    )
                );
                let mut right_walker = match walked {
                    Some(rw) => rw,
                    None => {
                        return Err(Error::CorruptedState(
                            "tree.link(false) was Some but walk(false) returned None",
                        ))
                        .wrap_with_cost(cost);
                    }
                };
                let n = cost_return_on_error!(
                    &mut cost,
                    walk_count_only(
                        &mut right_walker,
                        range,
                        Some(node_key.as_slice()),
                        subtree_hi_excl,
                        grove_version,
                    )
                );
                total = total.saturating_add(n);
            }

            Ok(total).wrap_with_cost(cost)
        }
    }
}
