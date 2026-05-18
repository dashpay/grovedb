//! Recursive proof-emission engine for `AggregateSumOnRange`.
//!
//! For each subtree we visit, the bound classification (Disjoint /
//! Contained / Boundary) determines what op to push and whether to
//! descend:
//!
//! - **Disjoint** / **Contained** → emit a single `HashWithSum` op for
//!   the collapsed subtree root. Contained contributes its full subtree
//!   sum to the running in-range total; Disjoint contributes 0. (Both
//!   still need the sum hash-bound so the verifier can reconstruct the
//!   parent's `own_sum` later — see the inline comment on the
//!   `HashWithSum` emit for the long form.)
//! - **Boundary** → emit `KVDigestSum(key, value_hash, node_sum)` for
//!   the current node, recurse into both children for descent, and add
//!   `own_sum = node_sum − left_struct − right_struct` to the running
//!   total iff the node's key is itself in range.

use std::collections::LinkedList;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_version::version::GroveVersion;

use super::provable_sum_from_aggregate;
use crate::{
    proofs::{
        query::{
            aggregate_common::{classify_subtree, SubtreeClassification, NULL_HASH},
            QueryItem,
        },
        Node, Op,
    },
    tree::{kv::ValueDefinedCostType, AggregateData, Fetch, RefWalker},
    CryptoHash, Error, TreeType,
};

/// Returns `true` when the host tree binds **both** count and sum into
/// its node hash (i.e. `ProvableCountProvableSumTree`). In that case the
/// sum proof must emit dual-axis Node variants so the verifier can
/// reconstruct the right hash function. For single-axis trees
/// (`ProvableSumTree`) we keep the existing `HashWithSum` / `KVDigestSum`
/// shape — those trees use `node_hash_with_sum(kv, l, r, sum)`, which is
/// fully determined by the sum alone.
#[inline]
fn binds_count_into_hash(tree_type: TreeType) -> bool {
    matches!(tree_type, TreeType::ProvableCountProvableSumTree)
}

/// Recursive proof emitter. Always called on a non-empty subtree.
///
/// At entry, `subtree_lo_excl` / `subtree_hi_excl` are the inherited
/// exclusive key bounds for the subtree this walker points at (both `None`
/// at the root call). The accumulator is `i128` so the prover side never
/// overflows mid-walk on adversarial intermediate sums.
///
/// `tree_type` is the **host tree's** type. It controls the proof-node
/// variant chosen at each emit site: a host tree that hashes both count
/// and sum (`ProvableCountProvableSumTree`) requires dual-axis variants
/// (`HashWithCountAndSum`, `KVDigestCountSum`) so the verifier can
/// reconstruct `node_hash_with_count_and_sum`. Plain `ProvableSumTree`
/// uses the sum-only variants.
pub(super) fn emit_sum_proof<S>(
    walker: &mut RefWalker<'_, S>,
    range: &QueryItem,
    subtree_lo_excl: Option<&[u8]>,
    subtree_hi_excl: Option<&[u8]>,
    ops: &mut LinkedList<Op>,
    tree_type: TreeType,
    grove_version: &GroveVersion,
) -> CostResult<i128, Error>
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
        // `HashWithSum(kv_hash, left_child_hash, right_child_hash, sum)`
        // op for the subtree's root.
        //
        // Why `HashWithSum` even for Disjoint subtrees? Same reason the
        // count proof uses `HashWithCount` at Disjoint positions: the
        // verifier derives the parent boundary node's `own_sum` as
        // `parent_aggregate − left_struct − right_struct`, so the
        // *structural* sum of every child — including disjoint outside
        // subtrees — has to be cryptographically bound to the parent's
        // hash chain. Plain `Hash(node_hash)` would carry an unbound sum
        // and let a malicious prover skew the boundary's `own_sum`
        // derivation. See the count-side comment for the long form.
        let aggregate = match walker.tree().aggregate_data() {
            Ok(a) => a,
            Err(e) => {
                // Local prover-side walk over our own merk — if the
                // node refuses to surface aggregate_data, that is a
                // storage/state corruption, not a peer-supplied
                // invalid proof.
                return Err(Error::CorruptedData(format!("aggregate_data: {}", e)))
                    .wrap_with_cost(cost);
            }
        };
        let subtree_sum = match provable_sum_from_aggregate(aggregate) {
            Ok(s) => s,
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
        // ProvableCountProvableSumTree binds BOTH count and sum into its
        // node hash. To let the verifier reconstruct that hash, emit the
        // dual-axis variant carrying both aggregates. Pull the count
        // from the ProvableCountAndProvableSum variant —
        // `provable_sum_from_aggregate` already accepted this aggregate
        // above, so its variant tag is known to be one we can read both
        // fields from.
        if binds_count_into_hash(tree_type) {
            let subtree_count = match aggregate {
                AggregateData::ProvableCountAndProvableSum(c, _) => c,
                other => {
                    return Err(Error::CorruptedData(format!(
                        "expected ProvableCountAndProvableSum for \
                         ProvableCountProvableSumTree, got {:?}",
                        other
                    )))
                    .wrap_with_cost(cost);
                }
            };
            ops.push_back(Op::Push(Node::HashWithCountAndSum(
                kv_hash,
                left_child_hash,
                right_child_hash,
                subtree_count,
                subtree_sum,
            )));
        } else {
            ops.push_back(Op::Push(Node::HashWithSum(
                kv_hash,
                left_child_hash,
                right_child_hash,
                subtree_sum,
            )));
        }
        // For the prover-side in-range total: Contained contributes its
        // entire subtree sum (which already excludes `NotSummed` entries
        // because their stored aggregate is 0); Disjoint contributes 0.
        let in_range_contribution: i128 = match class {
            SubtreeClassification::Contained => subtree_sum as i128,
            SubtreeClassification::Disjoint => 0,
            SubtreeClassification::Boundary => unreachable!(),
        };
        return Ok(in_range_contribution).wrap_with_cost(cost);
    }
    // class == Boundary — fall through to descent + KVDigestSum emission.

    // Step 2: snapshot what we need from the current node before walking.
    let node_key: Vec<u8> = walker.tree().key().to_vec();
    let node_value_hash: CryptoHash = *walker.tree().value_hash();
    // Read the full aggregate so a dual-axis host tree can pick up both
    // count and sum below; the single-axis path only needs sum.
    let node_aggregate = match walker
        .tree()
        .aggregate_data()
        // Local prover-side walk over our own merk — failure to read
        // aggregate_data is local state corruption, not a peer-supplied
        // invalid proof.
        .map_err(|e| Error::CorruptedData(format!("aggregate_data: {}", e)))
    {
        Ok(a) => a,
        Err(e) => return Err(e).wrap_with_cost(cost),
    };
    let node_sum: i64 = match provable_sum_from_aggregate(node_aggregate) {
        Ok(s) => s,
        Err(e) => return Err(e).wrap_with_cost(cost),
    };

    // Snapshot each child link's structural aggregate sum from the link
    // itself (avoids loading the child for this lookup). The verifier needs
    // these to compute `own_sum = node_sum − left_struct − right_struct`
    // at this boundary node.
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
        let n = cost_return_on_error!(
            &mut cost,
            emit_sum_proof(
                &mut left_walker,
                range,
                left_lo,
                left_hi,
                ops,
                tree_type,
                grove_version,
            )
        );
        // Plain `+` on i128 cannot overflow with i64-sized inputs at the
        // realistic depths a Merk tree reaches, so no saturating-add
        // safeguard here (the i128 range is ~3.4e38, more than enough for
        // any tree of i64 children).
        total += n;
        true
    } else {
        false
    };

    // Step 4: emit the current node as a boundary KVDigestSum /
    // KVDigestCountSum + attach left as its left child. The node's own
    // contribution to the in-range sum is `own_sum = node_sum −
    // left_struct − right_struct`. `NotSummed` wrapping forces
    // `node_sum = 0` so its own contribution is 0 by construction.
    if binds_count_into_hash(tree_type) {
        let node_count = match node_aggregate {
            AggregateData::ProvableCountAndProvableSum(c, _) => c,
            other => {
                return Err(Error::CorruptedData(format!(
                    "expected ProvableCountAndProvableSum for \
                     ProvableCountProvableSumTree, got {:?}",
                    other
                )))
                .wrap_with_cost(cost);
            }
        };
        ops.push_back(Op::Push(Node::KVDigestCountSum(
            node_key.clone(),
            node_value_hash,
            node_count,
            node_sum,
        )));
    } else {
        ops.push_back(Op::Push(Node::KVDigestSum(
            node_key.clone(),
            node_value_hash,
            node_sum,
        )));
    }
    if left_emitted {
        ops.push_back(Op::Parent);
    }
    if range.contains(&node_key) {
        // Compute own_sum in i128 to mirror the verifier's overflow-safe
        // accumulator. Saturating semantics would silently mask malformed
        // intermediates; we propagate the literal arithmetic here and the
        // verifier rejects any overflow at the final i64-narrow step.
        let own_sum_i128 =
            (node_sum as i128) - (left_link_aggregate as i128) - (right_link_aggregate as i128);
        total += own_sum_i128;
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
        let n = cost_return_on_error!(
            &mut cost,
            emit_sum_proof(
                &mut right_walker,
                range,
                right_lo,
                right_hi,
                ops,
                tree_type,
                grove_version,
            )
        );
        total += n;
        true
    } else {
        false
    };

    if right_emitted {
        ops.push_back(Op::Child);
    }

    Ok(total).wrap_with_cost(cost)
}
