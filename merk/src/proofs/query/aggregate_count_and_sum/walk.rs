//! No-proof walker: same classification logic as the proof emitter, but
//! returns the in-range `(count, sum)` pair without allocating proof
//! ops. Dual-axis sibling of
//! [`super::super::aggregate_count::walk::walk_count_only`] and
//! [`super::super::aggregate_sum::walk::walk_sum_only`].

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_version::version::GroveVersion;

use super::provable_count_and_sum_from_aggregate;
use crate::{
    proofs::query::{
        aggregate_common::{classify_subtree, SubtreeClassification},
        QueryItem,
    },
    tree::{kv::ValueDefinedCostType, Fetch, RefWalker},
    Error,
};

/// Read the `(count, sum)` provable aggregate off the walker's current
/// tree node. Shared error-mapping helper used by [`walk_count_and_sum`]
/// at both the Contained-leaf and Boundary positions.
fn provable_count_and_sum_from_walker<S>(walker: &RefWalker<'_, S>) -> Result<(u64, i64), Error>
where
    S: Fetch + Sized + Clone,
{
    let aggregate = walker
        .tree()
        .aggregate_data()
        .map_err(|e| Error::CorruptedData(format!("aggregate_data: {}", e)))?;
    provable_count_and_sum_from_aggregate(aggregate)
}

/// No-proof variant of [`super::emit::emit_count_and_sum_proof`]:
/// walks the same classification path (Contained / Disjoint /
/// Boundary) but only returns the running in-range `(count, sum)`
/// pair.
///
/// At entry, `subtree_lo_excl` / `subtree_hi_excl` are the inherited
/// exclusive key bounds for the subtree this walker points at (both
/// `None` at the root call). The walk reads each node's
/// `aggregate_data()` and each child link's
/// `aggregate_data().as_count_u64()` / `as_sum_i64()` exactly the same
/// way the proof emitter does, so the returned pair is identical to
/// the `(count, sum)` tuple `create_aggregate_count_and_sum_on_range_proof`
/// returns.
///
/// The sum accumulator is `i128` so the no-proof side never overflows
/// mid-walk on adversarial intermediate sums (matching the prover's
/// guarantee). The count accumulator is `u128` for the same reason:
/// node-aggregate counts are `u64` per node, but the sum across
/// adversarial intermediate states could theoretically wrap; widening
/// keeps the walk consistent. Narrowing to `(u64, i64)` happens in the
/// public entry point `Merk::count_and_sum_aggregate_on_range`.
pub(super) fn walk_count_and_sum<S>(
    walker: &mut RefWalker<'_, S>,
    range: &QueryItem,
    subtree_lo_excl: Option<&[u8]>,
    subtree_hi_excl: Option<&[u8]>,
    grove_version: &GroveVersion,
) -> CostResult<(u128, i128), Error>
where
    S: Fetch + Sized + Clone,
{
    let mut cost = OperationCost::default();

    match classify_subtree(subtree_lo_excl, subtree_hi_excl, range) {
        // Disjoint: subtree contributes 0 on both axes.
        SubtreeClassification::Disjoint => Ok((0u128, 0i128)).wrap_with_cost(cost),
        // Contained: subtree contributes its full stored aggregate
        // count and sum. NotSummed / NonCounted wrapper variants are
        // rejected as parents of PCPS (see the PCPS parent-shape gate),
        // so the only way a Contained PCPS subtree contributes 0 is
        // if it actually holds no in-range entries.
        SubtreeClassification::Contained => {
            let (count, sum) =
                cost_return_on_error_no_add!(cost, provable_count_and_sum_from_walker(walker));
            Ok((count as u128, sum as i128)).wrap_with_cost(cost)
        }
        // Boundary: descend into both children and add own contribution.
        SubtreeClassification::Boundary => {
            // Snapshot what we need from the current node before walking.
            // walk(...) takes &mut self.tree, so we must drop any existing
            // borrows on walker.tree() before calling it.
            let node_key: Vec<u8> = walker.tree().key().to_vec();
            let (node_count, node_sum) =
                cost_return_on_error_no_add!(cost, provable_count_and_sum_from_walker(walker));
            let left_link_count: u64 = walker
                .tree()
                .link(true)
                .map(|l| l.aggregate_data().as_count_u64())
                .unwrap_or(0);
            let left_link_sum: i64 = walker
                .tree()
                .link(true)
                .map(|l| l.aggregate_data().as_sum_i64())
                .unwrap_or(0);
            let right_link_count: u64 = walker
                .tree()
                .link(false)
                .map(|l| l.aggregate_data().as_count_u64())
                .unwrap_or(0);
            let right_link_sum: i64 = walker
                .tree()
                .link(false)
                .map(|l| l.aggregate_data().as_sum_i64())
                .unwrap_or(0);
            let left_link_present = walker.tree().link(true).is_some();
            let right_link_present = walker.tree().link(false).is_some();

            let mut total_count: u128 = 0;
            let mut total_sum: i128 = 0;

            // LEFT child. If link is Some, walk(true) must yield Some;
            // the proof variant has the verifier to catch silent
            // inconsistencies, but this no-proof path returns the pair
            // straight to the caller — so we fail loudly on impossible
            // state rather than silently under-aggregating.
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
                let (c, s) = cost_return_on_error!(
                    &mut cost,
                    walk_count_and_sum(
                        &mut left_walker,
                        range,
                        subtree_lo_excl,
                        Some(node_key.as_slice()),
                        grove_version,
                    )
                );
                total_count = total_count.saturating_add(c);
                total_sum = total_sum.saturating_add(s);
            }

            // Current node's own contribution. Both axes derive
            // `own = node_aggregate − left_struct − right_struct`.
            // For the count axis we use `checked_sub` since children
            // claiming more keys than the parent is corrupted state
            // (mirrors aggregate_count's walker). For the sum axis the
            // arithmetic is signed and the same node can legitimately
            // produce a negative own_sum (e.g. positive children plus
            // a negative own value); we widen to i128 and use
            // `wrapping_sub` purely to satisfy the type checker — the
            // hash chain in the verifying variant catches tampering;
            // here we trust the merk read path per the API contract.
            if range.contains(&node_key) {
                let own_count = node_count
                    .checked_sub(left_link_count)
                    .and_then(|n| n.checked_sub(right_link_count))
                    .ok_or(Error::CorruptedState(
                        "child structural counts exceed parent's aggregate count",
                    ));
                let own_count = cost_return_on_error_no_add!(cost, own_count);
                let own_sum: i128 = (node_sum as i128)
                    .wrapping_sub(left_link_sum as i128)
                    .wrapping_sub(right_link_sum as i128);
                total_count = total_count.saturating_add(own_count as u128);
                total_sum = total_sum.saturating_add(own_sum);
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
                let (c, s) = cost_return_on_error!(
                    &mut cost,
                    walk_count_and_sum(
                        &mut right_walker,
                        range,
                        Some(node_key.as_slice()),
                        subtree_hi_excl,
                        grove_version,
                    )
                );
                total_count = total_count.saturating_add(c);
                total_sum = total_sum.saturating_add(s);
            }

            Ok((total_count, total_sum)).wrap_with_cost(cost)
        }
    }
}
