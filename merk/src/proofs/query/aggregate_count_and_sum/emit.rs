//! Recursive proof-emission engine for `AggregateCountAndSumOnRange`.
//!
//! Mirrors `super::super::aggregate_count::emit::emit_count_proof`
//! but tracks BOTH axes (count + sum) during a single walk and only
//! accepts `ProvableCountProvableSumTree` (PCPS) hosts. The op stream
//! produced is byte-identical to what `emit_count_proof` produces on
//! a PCPS host — the difference is that this walker also accumulates
//! the in-range sum so the prover can return both totals.
//!
//! For each subtree we visit, the bound classification (Disjoint /
//! Contained / Boundary) determines what op to push and whether to
//! descend:
//!
//! - **Disjoint** / **Contained** → emit a single `HashWithCountAndSum`
//!   op for the collapsed subtree root. Contained contributes its full
//!   subtree count AND sum to the running in-range totals; Disjoint
//!   contributes 0 to both. (Both still need the structural count and
//!   sum hash-bound so the verifier can reconstruct the parent's
//!   `own_count` / `own_sum` later.)
//! - **Boundary** → emit `KVDigestCountSum(key, value_hash, node_count,
//!   node_sum)` for the current node, recurse into both children, and
//!   add `own_count` / `own_sum` to the running totals if and only if the node's
//!   key is itself in range.

use std::collections::LinkedList;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_version::version::GroveVersion;

use super::provable_count_and_sum_from_aggregate;
use crate::{
    proofs::{
        query::{
            aggregate_common::{classify_subtree, SubtreeClassification, NULL_HASH},
            QueryItem,
        },
        Node, Op,
    },
    tree::{kv::ValueDefinedCostType, Fetch, RefWalker},
    CryptoHash, Error,
};

/// Recursive proof emitter for the combined count+sum aggregate. Always
/// called on a non-empty subtree.
///
/// At entry, `subtree_lo_excl` / `subtree_hi_excl` are the inherited
/// exclusive key bounds for the subtree this walker points at (both
/// `None` at the root call).
///
/// Returns the `(in_range_count, in_range_sum_i128)` pair this subtree
/// contributes to the totals. The sum accumulator is widened to i128
/// during traversal so adversarial-input combinations cannot wrap on
/// the way up; the prover-side caller narrows back to i64 once at the
/// top — the host's own merk maintains its aggregate as i64 at every
/// level so an honest prove call lands inside i64's range.
pub(super) fn emit_count_and_sum_proof<S>(
    walker: &mut RefWalker<'_, S>,
    range: &QueryItem,
    subtree_lo_excl: Option<&[u8]>,
    subtree_hi_excl: Option<&[u8]>,
    ops: &mut LinkedList<Op>,
    grove_version: &GroveVersion,
) -> CostResult<(u64, i128), Error>
where
    S: Fetch + Sized + Clone,
{
    let mut cost = OperationCost::default();

    // Step 1: classify the current subtree against the inner range.
    let class = classify_subtree(subtree_lo_excl, subtree_hi_excl, range);

    if matches!(
        class,
        SubtreeClassification::Disjoint | SubtreeClassification::Contained
    ) {
        // Whole subtree is either entirely outside or entirely inside the
        // range. Either way we emit a single self-verifying
        // `HashWithCountAndSum(kv_hash, left_child_hash, right_child_hash,
        // count, sum)` op for the subtree's root.
        //
        // PCPS commits BOTH count and sum into its node hash via
        // `node_hash_with_count_and_sum(kv, l, r, count, sum)`, so the
        // verifier needs both fields to recompute the hash. Even for
        // Disjoint subtrees we emit the same op type: the parent's
        // `own_count` / `own_sum` derivations both subtract the
        // structural count/sum of every child (including disjoint
        // outside subtrees), so both must be hash-bound.
        let aggregate = match walker.tree().aggregate_data() {
            Ok(a) => a,
            Err(e) => {
                return Err(Error::CorruptedData(format!("aggregate_data: {}", e)))
                    .wrap_with_cost(cost);
            }
        };
        let (subtree_count, subtree_sum) = match provable_count_and_sum_from_aggregate(aggregate) {
            Ok(pair) => pair,
            Err(e) => return Err(e).wrap_with_cost(cost),
        };
        let kv_hash = *walker.tree().kv_hash();
        let left_child_hash = walker
            .tree()
            .link(true)
            .map(|l| *l.hash())
            .unwrap_or(NULL_HASH);
        let right_child_hash = walker
            .tree()
            .link(false)
            .map(|l| *l.hash())
            .unwrap_or(NULL_HASH);
        ops.push_back(Op::Push(Node::HashWithCountAndSum(
            kv_hash,
            left_child_hash,
            right_child_hash,
            subtree_count,
            subtree_sum,
        )));
        // Contained subtree contributes its full count and sum;
        // Disjoint contributes 0 to both.
        let (count_contribution, sum_contribution) = match class {
            SubtreeClassification::Contained => (subtree_count, subtree_sum as i128),
            SubtreeClassification::Disjoint => (0u64, 0i128),
            SubtreeClassification::Boundary => unreachable!(),
        };
        return Ok((count_contribution, sum_contribution)).wrap_with_cost(cost);
    }
    // class == Boundary — fall through to descent + KVDigestCountSum emission.

    // Step 2: snapshot what we need from the current node before walking.
    // walk(true/false) takes &mut self.tree, so we must drop any existing
    // borrows on walker.tree() before calling it.
    let node_key: Vec<u8> = walker.tree().key().to_vec();
    let node_value_hash: CryptoHash = *walker.tree().value_hash();
    let node_aggregate = match walker
        .tree()
        .aggregate_data()
        .map_err(|e| Error::CorruptedData(format!("aggregate_data: {}", e)))
    {
        Ok(a) => a,
        Err(e) => return Err(e).wrap_with_cost(cost),
    };
    let (node_count, node_sum) = match provable_count_and_sum_from_aggregate(node_aggregate) {
        Ok(pair) => pair,
        Err(e) => return Err(e).wrap_with_cost(cost),
    };

    // Snapshot each child link's structural aggregate count/sum from the
    // link itself (avoids loading the child for this lookup). The
    // verifier needs these to compute `own_count` / `own_sum` at this
    // boundary node.
    let left_link_count_sum: (u64, i128) = walker
        .tree()
        .link(true)
        .map(|l| {
            let agg = l.aggregate_data();
            (agg.as_count_u64(), agg.as_sum_i64() as i128)
        })
        .unwrap_or((0, 0));
    let right_link_count_sum: (u64, i128) = walker
        .tree()
        .link(false)
        .map(|l| {
            let agg = l.aggregate_data();
            (agg.as_count_u64(), agg.as_sum_i64() as i128)
        })
        .unwrap_or((0, 0));
    let left_link_present = walker.tree().link(true).is_some();
    let right_link_present = walker.tree().link(false).is_some();

    let mut total_count: u64 = 0;
    let mut total_sum: i128 = 0;

    // Step 3: handle the LEFT child.
    let left_emitted = if left_link_present {
        let left_lo = subtree_lo_excl;
        let left_hi: Option<&[u8]> = Some(node_key.as_slice());
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
                .wrap_with_cost(cost)
            }
        };
        let (lc, ls) = cost_return_on_error!(
            &mut cost,
            emit_count_and_sum_proof(
                &mut left_walker,
                range,
                left_lo,
                left_hi,
                ops,
                grove_version,
            )
        );
        total_count = total_count.saturating_add(lc);
        total_sum = total_sum.saturating_add(ls);
        true
    } else {
        false
    };

    // Step 4: emit the current node as a boundary KVDigestCountSum +
    // attach left as its left child. The node's own contribution to the
    // in-range totals is `(own_count, own_sum)` derived as
    // `node_aggregate − left_struct − right_struct` for each axis.
    ops.push_back(Op::Push(Node::KVDigestCountSum(
        node_key.clone(),
        node_value_hash,
        node_count,
        node_sum,
    )));
    if left_emitted {
        ops.push_back(Op::Parent);
    }
    if range.contains(&node_key) {
        let own_count = node_count
            .saturating_sub(left_link_count_sum.0)
            .saturating_sub(right_link_count_sum.0);
        // Sum arithmetic is signed; widen to i128 for the subtraction
        // and keep the running sum in i128 throughout.
        let own_sum = (node_sum as i128) - left_link_count_sum.1 - right_link_count_sum.1;
        total_count = total_count.saturating_add(own_count);
        total_sum = total_sum.saturating_add(own_sum);
    }

    // Step 5: handle the RIGHT child.
    let right_emitted = if right_link_present {
        let right_lo: Option<&[u8]> = Some(node_key.as_slice());
        let right_hi = subtree_hi_excl;
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
                .wrap_with_cost(cost)
            }
        };
        let (rc, rs) = cost_return_on_error!(
            &mut cost,
            emit_count_and_sum_proof(
                &mut right_walker,
                range,
                right_lo,
                right_hi,
                ops,
                grove_version,
            )
        );
        total_count = total_count.saturating_add(rc);
        total_sum = total_sum.saturating_add(rs);
        true
    } else {
        false
    };

    if right_emitted {
        ops.push_back(Op::Child);
    }

    Ok((total_count, total_sum)).wrap_with_cost(cost)
}
