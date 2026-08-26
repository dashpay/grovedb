//! Recursive proof-emission engine for `AggregateCountOnRange`.
//!
//! For each subtree we visit, the bound classification (Disjoint /
//! Contained / Boundary) determines what op to push and whether to
//! descend:
//!
//! - **Disjoint** / **Contained** → emit a single `HashWithCount` op
//!   for the collapsed subtree root. Contained contributes its full
//!   subtree count to the running in-range total; Disjoint contributes
//!   0. (Both still need the count hash-bound so the verifier can
//!   reconstruct the parent's `own_count` later — see the inline
//!   comment on the `HashWithCount` emit for the long form.)
//! - **Boundary** → emit `KVDigestCount(key, value_hash, node_count)`
//!   for the current node, recurse into both children for descent, and
//!   add `own_count = node_count − left_struct − right_struct` to the
//!   running total if and only if the node's key is itself in range. This is what
//!   makes `NonCounted`-wrapped entries fall out of the in-range total
//!   automatically (their node_count is 0).

use std::collections::LinkedList;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_version::version::GroveVersion;

use super::provable_count_from_aggregate;
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
/// count proof must emit dual-axis Node variants so the verifier can
/// reconstruct the right hash function. For single-axis trees
/// (`ProvableCountTree`, `ProvableCountSumTree`) we keep the existing
/// `HashWithCount` / `KVDigestCount` shape — those trees use
/// `node_hash_with_count(kv, l, r, count)`, which is fully determined by
/// the count alone.
#[inline]
fn binds_sum_into_hash(tree_type: TreeType) -> bool {
    matches!(tree_type, TreeType::ProvableCountProvableSumTree)
}

/// Recursive proof emitter. Always called on a non-empty subtree.
///
/// At entry, `subtree_lo_excl` / `subtree_hi_excl` are the inherited
/// exclusive key bounds for the subtree this walker points at (both `None`
/// at the root call).
///
/// `tree_type` is the **host tree's** type. It controls the proof-node
/// variant chosen at each emit site: a host tree that hashes both count
/// and sum (`ProvableCountProvableSumTree`) requires dual-axis variants
/// (`HashWithCountAndSum`, `KVDigestCountSum`) so the verifier can
/// reconstruct `node_hash_with_count_and_sum`. Other count-bearing
/// trees use the count-only variants.
pub(super) fn emit_count_proof<S>(
    walker: &mut RefWalker<'_, S>,
    range: &QueryItem,
    subtree_lo_excl: Option<&[u8]>,
    subtree_hi_excl: Option<&[u8]>,
    ops: &mut LinkedList<Op>,
    tree_type: TreeType,
    grove_version: &GroveVersion,
) -> CostResult<u64, Error>
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
        // `HashWithCount(kv_hash, left_child_hash, right_child_hash, count)`
        // op for the subtree's root.
        //
        // Why HashWithCount even for Disjoint subtrees (rather than the
        // smaller `Hash(node_hash)` that an in-range count would never
        // need)?  Because the parent's `own_count` is computed by the
        // verifier as `parent_aggregate − left_struct − right_struct` (see
        // `verify_count_shape`), so the *structural* count of every child
        // — including disjoint outside subtrees — has to be
        // cryptographically bound to the parent's hash chain. The only
        // node type that carries a hash-bound count is `HashWithCount`
        // (its four committed fields recompute `node_hash_with_count` and
        // would diverge under any count tampering). Plain `Hash(node_hash)`
        // carries no count, so a malicious prover could lie about the
        // structural count and skew the parent's `own_count`
        // derivation — leading to silent over/under-counts at boundary
        // ancestors.
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
        let subtree_count = match provable_count_from_aggregate(aggregate) {
            Ok(c) => c,
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
        // ProvableCountProvableSumTree binds BOTH count and sum into the
        // node hash via `node_hash_with_count_and_sum`. To let the
        // verifier reconstruct that hash, we must emit the dual-axis
        // variant carrying both aggregates. Pull the sum from the
        // ProvableCountAndProvableSum variant — `provable_count_from_aggregate`
        // already accepted this aggregate above, so its variant tag is
        // known to be one we can read both fields from.
        if binds_sum_into_hash(tree_type) {
            let subtree_sum = match aggregate {
                AggregateData::ProvableCountAndProvableSum(_, s) => s,
                other => {
                    // Prover-side: a host tree declared as
                    // ProvableCountProvableSumTree must carry a
                    // ProvableCountAndProvableSum aggregate. Anything
                    // else is local state corruption.
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
            ops.push_back(Op::Push(Node::HashWithCount(
                kv_hash,
                left_child_hash,
                right_child_hash,
                subtree_count,
            )));
        }
        // For the prover-side in-range total: Contained contributes its
        // entire subtree count (which already excludes NonCounted entries
        // because their stored aggregate is 0); Disjoint contributes 0.
        let in_range_contribution = match class {
            SubtreeClassification::Contained => subtree_count,
            SubtreeClassification::Disjoint => 0,
            SubtreeClassification::Boundary => unreachable!(),
        };
        return Ok(in_range_contribution).wrap_with_cost(cost);
    }
    // class == Boundary — fall through to descent + KVDigestCount emission.

    // Step 2: snapshot what we need from the current node before walking.
    // walk(true/false) takes &mut self.tree, so we must drop any existing
    // borrows on walker.tree() before calling it.
    let node_key: Vec<u8> = walker.tree().key().to_vec();
    let node_value_hash: CryptoHash = *walker.tree().value_hash();
    // Read the full aggregate so a dual-axis host tree can pick up both
    // count and sum below; the single-axis path only needs count.
    let node_aggregate = match walker
        .tree()
        .aggregate_data()
        // Local prover-side walk — failure to read aggregate_data is
        // local state corruption, not a peer-supplied invalid proof.
        .map_err(|e| Error::CorruptedData(format!("aggregate_data: {}", e)))
    {
        Ok(a) => a,
        Err(e) => return Err(e).wrap_with_cost(cost),
    };
    let node_count: u64 = match provable_count_from_aggregate(node_aggregate) {
        Ok(c) => c,
        Err(e) => return Err(e).wrap_with_cost(cost),
    };

    // Snapshot each child link's structural aggregate count from the link
    // itself (avoids loading the child for this lookup). The verifier needs
    // these to compute `own_count = node_count − left_struct − right_struct`
    // at this boundary node.
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

    // Step 3: handle the LEFT child. Both Disjoint and Contained require a
    // one-level walk so the recursive Disjoint/Contained arm can emit a
    // self-verifying `HashWithCount` (plain `Hash` is no longer used here
    // — see the Disjoint branch comment above).
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
            emit_count_proof(
                &mut left_walker,
                range,
                left_lo,
                left_hi,
                ops,
                tree_type,
                grove_version,
            )
        );
        total = total.saturating_add(n);
        true
    } else {
        false
    };

    // Step 4: emit the current node as a boundary KVDigestCount /
    // KVDigestCountSum + attach left as its left child. The node's own
    // contribution to the in-range count is `own_count` (0 for
    // `NonCounted`-wrapped, 1 for normal), derived as `node_count −
    // left_struct − right_struct`. This is what makes NonCounted entries
    // fall out of the count: a NonCounted leaf has node_count = 0 and
    // no children, so own_count = 0.
    if binds_sum_into_hash(tree_type) {
        let node_sum = match node_aggregate {
            AggregateData::ProvableCountAndProvableSum(_, s) => s,
            other => {
                // Prover-side invariant: a host tree declared as
                // ProvableCountProvableSumTree must carry the dual-axis
                // aggregate at every node. Anything else is local
                // state corruption.
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
        ops.push_back(Op::Push(Node::KVDigestCount(
            node_key.clone(),
            node_value_hash,
            node_count,
        )));
    }
    if left_emitted {
        ops.push_back(Op::Parent);
    }
    if range.contains(&node_key) {
        let own_count = node_count
            .saturating_sub(left_link_aggregate)
            .saturating_sub(right_link_aggregate);
        total = total.saturating_add(own_count);
    }

    // Step 5: handle the RIGHT child. Same descent pattern as LEFT.
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
            emit_count_proof(
                &mut right_walker,
                range,
                right_lo,
                right_hi,
                ops,
                tree_type,
                grove_version,
            )
        );
        total = total.saturating_add(n);
        true
    } else {
        false
    };

    if right_emitted {
        ops.push_back(Op::Child);
    }

    Ok(total).wrap_with_cost(cost)
}
