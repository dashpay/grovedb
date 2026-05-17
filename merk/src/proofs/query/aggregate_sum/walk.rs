//! No-proof walker: same classification logic as the proof emitter, but
//! returns only the in-range signed sum without allocating proof ops.

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_version::version::GroveVersion;

use super::provable_sum_from_aggregate;
use crate::{
    proofs::query::{
        aggregate_common::{classify_subtree, SubtreeClassification},
        QueryItem,
    },
    tree::{kv::ValueDefinedCostType, Fetch, RefWalker},
    Error,
};

/// Read the provable-sum aggregate off the walker's current tree node.
/// Shared error-mapping helper used by [`walk_sum_only`] at both the
/// Contained-leaf and Boundary positions.
fn provable_sum_from_walker<S>(walker: &RefWalker<'_, S>) -> Result<i64, Error>
where
    S: Fetch + Sized + Clone,
{
    let aggregate = walker
        .tree()
        .aggregate_data()
        .map_err(|e| Error::CorruptedData(format!("aggregate_data: {}", e)))?;
    provable_sum_from_aggregate(aggregate)
}

/// No-proof variant of [`super::emit::emit_sum_proof`]: walks the same
/// classification path (Contained / Disjoint / Boundary) but only
/// returns the running in-range sum.
///
/// At entry, `subtree_lo_excl` / `subtree_hi_excl` are the inherited
/// exclusive key bounds for the subtree this walker points at (both
/// `None` at the root call). The walk reads each node's
/// `aggregate_data()` and each child link's `aggregate_data().as_sum_i64()`
/// exactly the same way the proof emitter does, so the returned sum is
/// identical to the `sum` value returned by
/// `create_aggregate_sum_on_range_proof`.
///
/// The accumulator is `i128` so the no-proof side never overflows
/// mid-walk on adversarial intermediate sums (matching the prover's
/// guarantee). Narrowing to `i64` happens in the public entry point
/// `Merk::sum_aggregate_on_range`.
pub(super) fn walk_sum_only<S>(
    walker: &mut RefWalker<'_, S>,
    range: &QueryItem,
    subtree_lo_excl: Option<&[u8]>,
    subtree_hi_excl: Option<&[u8]>,
    grove_version: &GroveVersion,
) -> CostResult<i128, Error>
where
    S: Fetch + Sized + Clone,
{
    let mut cost = OperationCost::default();

    match classify_subtree(subtree_lo_excl, subtree_hi_excl, range) {
        // Disjoint: subtree contributes 0 to the in-range sum.
        SubtreeClassification::Disjoint => Ok(0i128).wrap_with_cost(cost),
        // Contained: subtree contributes its full stored aggregate sum
        // (NotSummed-wrapped entries are already excluded — their stored
        // aggregate is 0 by the wrapper's contract).
        SubtreeClassification::Contained => {
            let sum = cost_return_on_error_no_add!(cost, provable_sum_from_walker(walker));
            Ok(sum as i128).wrap_with_cost(cost)
        }
        // Boundary: descend into both children and add own_sum.
        SubtreeClassification::Boundary => {
            // Snapshot what we need from the current node before walking.
            // walk(...) takes &mut self.tree, so we must drop any existing
            // borrows on walker.tree() before calling it.
            let node_key: Vec<u8> = walker.tree().key().to_vec();
            let node_sum = cost_return_on_error_no_add!(cost, provable_sum_from_walker(walker));
            let left_link_aggregate: i64 = walker
                .tree()
                .link(true)
                .map(|l| l.aggregate_data().as_sum_i64())
                .unwrap_or(0);
            let right_link_aggregate: i64 = walker
                .tree()
                .link(false)
                .map(|l| l.aggregate_data().as_sum_i64())
                .unwrap_or(0);
            let left_link_present = walker.tree().link(true).is_some();
            let right_link_present = walker.tree().link(false).is_some();

            let mut total: i128 = 0;

            // LEFT child. If link is Some, walk(true) must yield Some;
            // the proof variant has the verifier to catch silent
            // inconsistencies, but this no-proof path returns the sum
            // straight to the caller — so we fail loudly on impossible
            // state rather than silently under-summing.
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
                let s = cost_return_on_error!(
                    &mut cost,
                    walk_sum_only(
                        &mut left_walker,
                        range,
                        subtree_lo_excl,
                        Some(node_key.as_slice()),
                        grove_version,
                    )
                );
                total = total.saturating_add(s);
            }

            // Current node's own_sum: when the key is in range, the
            // contribution is `node_sum − left_struct − right_struct`.
            // Signed arithmetic — unlike the count side this can be
            // negative (and so cannot be checked-sub-vs-corruption like
            // count's). The hash chain in the verifying variant catches
            // tampering; here we trust the merk read path per the API
            // contract. `i128` accumulation keeps adversarial inputs
            // from wrapping mid-walk.
            if range.contains(&node_key) {
                let own_sum: i128 = (node_sum as i128)
                    .wrapping_sub(left_link_aggregate as i128)
                    .wrapping_sub(right_link_aggregate as i128);
                total = total.saturating_add(own_sum);
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
                let s = cost_return_on_error!(
                    &mut cost,
                    walk_sum_only(
                        &mut right_walker,
                        range,
                        Some(node_key.as_slice()),
                        subtree_hi_excl,
                        grove_version,
                    )
                );
                total = total.saturating_add(s);
            }

            Ok(total).wrap_with_cost(cost)
        }
    }
}
