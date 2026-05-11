//! Proof generation and verification for `AggregateCountOnRange` queries.
//!
//! This module implements the count-only proof shape described in the GroveDB
//! book chapter "Aggregate Count Queries". It is intentionally **separate**
//! from `create_proof_internal`: regular proofs always descend into a queried
//! subtree, but count proofs *stop* at fully-inside subtree roots and emit a
//! single `HashWithCount` op for the entire collapsed subtree.
//!
//! The proof targets a `ProvableCountTree` or `ProvableCountSumTree` (or
//! their `NonCounted*` wrapper variants — wrappers only affect whether the
//! tree contributes to its parent's count, not its own internal count
//! mechanics). On any other tree type the entry point returns
//! `Error::InvalidProofError`.

#[cfg(feature = "minimal")]
use std::collections::LinkedList;

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
#[cfg(feature = "minimal")]
use grovedb_version::version::GroveVersion;

#[cfg(feature = "minimal")]
use crate::{
    proofs::Op,
    tree::{kv::ValueDefinedCostType, AggregateData, Fetch, RefWalker},
    TreeType,
};
use crate::{
    proofs::{
        query::QueryItem,
        tree::{execute_with_options, Tree as ProofTree},
        Decoder, Node,
    },
    CryptoHash, Error,
};

/// All-zero `CryptoHash`, used in `Node::HashWithCount` for missing children.
const NULL_HASH: CryptoHash = [0u8; 32];

/// How a subtree's possible-key window relates to the inner range we're
/// counting over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SubtreeClassification {
    /// Every possible key in this subtree falls **outside** the range.
    Disjoint,
    /// Every possible key in this subtree falls **inside** the range.
    Contained,
    /// The subtree straddles a range boundary (or directly contains one).
    Boundary,
}

/// Classify a subtree relative to the inner range.
///
/// `subtree_lo_excl` and `subtree_hi_excl` are the **exclusive** bounds on
/// what keys can appear under the subtree (derived from ancestors during the
/// walk; both `None` at the root). The range bounds come from the inner
/// `QueryItem`'s `lower_bound` / `upper_bound`.
///
/// The comparisons treat `subtree_hi_excl` as exclusive (subtree keys are
/// strictly < `subtree_hi_excl`) and `subtree_lo_excl` as exclusive (subtree
/// keys are strictly > `subtree_lo_excl`). For the range bounds, the
/// inclusivity flag returned by `lower_bound`/`upper_bound` is **not**
/// load-bearing for the disjoint/contained tests below — see the inline
/// proofs.
fn classify_subtree(
    subtree_lo_excl: Option<&[u8]>,
    subtree_hi_excl: Option<&[u8]>,
    range: &QueryItem,
) -> SubtreeClassification {
    let (range_lo, _range_lo_excl) = range.lower_bound();
    let (range_hi, _range_hi_incl) = range.upper_bound();

    // Disjoint-LEFT: subtree entirely below the range.
    //
    // Subtree keys are < subtree_hi_excl. If subtree_hi_excl <= range_lo,
    // every subtree key < subtree_hi_excl <= range_lo is also < range_lo,
    // so excluded regardless of whether range_lo is inclusive or exclusive.
    if let (Some(s_hi), Some(r_lo)) = (subtree_hi_excl, range_lo)
        && s_hi <= r_lo
    {
        return SubtreeClassification::Disjoint;
    }

    // Disjoint-RIGHT: subtree entirely above the range.
    //
    // Subtree keys are > subtree_lo_excl. If subtree_lo_excl >= range_hi,
    // every subtree key > subtree_lo_excl >= range_hi is also > range_hi,
    // so excluded regardless of whether range_hi is inclusive or exclusive.
    if let (Some(s_lo), Some(r_hi)) = (subtree_lo_excl, range_hi)
        && s_lo >= r_hi
    {
        return SubtreeClassification::Disjoint;
    }

    // Contained: subtree (s_lo, s_hi) ⊆ range.
    //
    // Lower side: every subtree key > s_lo. If s_lo >= r_lo, every subtree
    // key > s_lo >= r_lo, so > r_lo, satisfying both inclusive and exclusive
    // r_lo. If subtree has no lower bound (s_lo = -inf) but range does, the
    // subtree could include arbitrarily small keys → not contained.
    let lower_contained = match range_lo {
        None => true,
        Some(r_lo) => match subtree_lo_excl {
            Some(s_lo) => s_lo >= r_lo,
            None => false,
        },
    };
    // Upper side: every subtree key < s_hi. If s_hi <= r_hi, every subtree
    // key < s_hi <= r_hi, so < r_hi, satisfying both inclusive and exclusive
    // r_hi. (We forgo the slightly tighter "s_hi <= r_hi+1" optimization for
    // inclusive r_hi because we don't have key arithmetic.)
    let upper_contained = match range_hi {
        None => true,
        Some(r_hi) => match subtree_hi_excl {
            Some(s_hi) => s_hi <= r_hi,
            None => false,
        },
    };

    if lower_contained && upper_contained {
        SubtreeClassification::Contained
    } else {
        SubtreeClassification::Boundary
    }
}

/// Returns true if `tree_type` is one of the four tree types that can host an
/// `AggregateCountOnRange` proof. Wrapper types are accepted by stripping
/// down to the inner tree type via `is_provable_count_bearing`.
#[cfg(feature = "minimal")]
fn is_provable_count_bearing(tree_type: TreeType) -> bool {
    matches!(
        tree_type,
        TreeType::ProvableCountTree | TreeType::ProvableCountSumTree
    )
}

/// Pull the count out of a `ProvableCount` / `ProvableCountAndSum` aggregate.
/// Returns `Err(InvalidProofError)` for any other variant — the entry point
/// has already gated `tree_type`, so reaching the error means the tree's
/// in-memory state disagrees with its declared type.
#[cfg(feature = "minimal")]
fn provable_count_from_aggregate(data: AggregateData) -> Result<u64, Error> {
    match data {
        AggregateData::ProvableCount(c) => Ok(c),
        AggregateData::ProvableCountAndSum(c, _) => Ok(c),
        other => Err(Error::InvalidProofError(format!(
            "expected ProvableCount aggregate data on a provable count tree, got {:?}",
            other
        ))),
    }
}

#[cfg(feature = "minimal")]
impl<S> RefWalker<'_, S>
where
    S: Fetch + Sized + Clone,
{
    /// Generate a count-only proof for an `AggregateCountOnRange` query.
    ///
    /// `inner_range` is the `QueryItem` wrapped by `AggregateCountOnRange`
    /// (already stripped at the caller). `tree_type` must be one of
    /// `ProvableCountTree` or `ProvableCountSumTree`; any other tree type is
    /// rejected with `Error::InvalidProofError` before any walking happens.
    ///
    /// The returned tuple is `(proof_ops, count)`:
    /// - `proof_ops` is the linear stream the verifier will replay to
    ///   reconstruct the tree's root hash.
    /// - `count` is the prover-side computed count (the verifier independently
    ///   recomputes it from the proof and compares against the expected root
    ///   hash; this value is returned as a convenience, not as ground truth).
    pub fn create_aggregate_count_on_range_proof(
        &mut self,
        inner_range: &QueryItem,
        tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> CostResult<(LinkedList<Op>, u64), Error> {
        if !is_provable_count_bearing(tree_type) {
            return Err(Error::InvalidProofError(format!(
                "AggregateCountOnRange is only valid against ProvableCountTree or \
                 ProvableCountSumTree, got {:?}",
                tree_type
            )))
            .wrap_with_cost(OperationCost::default());
        }

        let mut cost = OperationCost::default();
        let mut ops = LinkedList::new();
        let count = cost_return_on_error!(
            &mut cost,
            emit_count_proof(self, inner_range, None, None, &mut ops, grove_version)
        );
        Ok((ops, count)).wrap_with_cost(cost)
    }

    /// Walk the tree for an `AggregateCountOnRange` query and return the
    /// in-range count, **without** producing a proof.
    ///
    /// This is the no-proof counterpart of
    /// [`Self::create_aggregate_count_on_range_proof`]. It performs the same
    /// classification walk (Contained / Disjoint / Boundary) and reads each
    /// node's aggregate count directly from the merk, so it is O(log n) in
    /// the number of distinct keys under the indexed subtree — the same
    /// complexity as the proof variant but without the proof-op allocations,
    /// hash recomputations, or serialization round-trip.
    ///
    /// The caller (`Merk::count_aggregate_on_range`) is expected to have
    /// already validated `tree_type` is `ProvableCountTree` or
    /// `ProvableCountSumTree`; the per-node `provable_count_from_aggregate`
    /// check inside the walk surfaces any disagreement between the declared
    /// tree type and the in-memory aggregate.
    ///
    /// The result is **not** independently verifiable: the caller is trusting
    /// their own merk read path. Callers that need a verifiable count must
    /// use `prove_aggregate_count_on_range` + `verify_aggregate_count_on_range_proof`.
    pub fn count_aggregate_on_range(
        &mut self,
        inner_range: &QueryItem,
        grove_version: &GroveVersion,
    ) -> CostResult<u64, Error> {
        walk_count_only(self, inner_range, None, None, grove_version)
    }
}

/// Recursive proof emitter. Always called on a non-empty subtree.
///
/// At entry, `subtree_lo_excl` / `subtree_hi_excl` are the inherited
/// exclusive key bounds for the subtree this walker points at (both `None`
/// at the root call).
#[cfg(feature = "minimal")]
fn emit_count_proof<S>(
    walker: &mut RefWalker<'_, S>,
    range: &QueryItem,
    subtree_lo_excl: Option<&[u8]>,
    subtree_hi_excl: Option<&[u8]>,
    ops: &mut LinkedList<Op>,
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
                return Err(Error::InvalidProofError(format!("aggregate_data: {}", e)))
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
        ops.push_back(Op::Push(Node::HashWithCount(
            kv_hash,
            left_child_hash,
            right_child_hash,
            subtree_count,
        )));
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
    let node_count: u64 = match walker
        .tree()
        .aggregate_data()
        .map_err(|e| Error::InvalidProofError(format!("aggregate_data: {}", e)))
    {
        Ok(data) => match provable_count_from_aggregate(data) {
            Ok(c) => c,
            Err(e) => return Err(e).wrap_with_cost(cost),
        },
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
                grove_version,
            )
        );
        total = total.saturating_add(n);
        true
    } else {
        false
    };

    // Step 4: emit the current node as a boundary KVDigestCount + attach left
    // as its left child. The node's own contribution to the in-range count
    // is `own_count` (0 for `NonCounted`-wrapped, 1 for normal), derived as
    // `node_count − left_struct − right_struct`. This is what makes
    // NonCounted entries fall out of the count: a NonCounted leaf has
    // node_count = 0 and no children, so own_count = 0.
    ops.push_back(Op::Push(Node::KVDigestCount(
        node_key.clone(),
        node_value_hash,
        node_count,
    )));
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

/// Read the provable-count aggregate off the walker's current tree node.
/// Shared error-mapping helper used by [`walk_count_only`] at both the
/// Contained-leaf and Boundary positions.
#[cfg(feature = "minimal")]
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

/// No-proof variant of [`emit_count_proof`]: walks the same classification
/// path (Contained / Disjoint / Boundary) but only returns the running
/// in-range count.
///
/// At entry, `subtree_lo_excl` / `subtree_hi_excl` are the inherited
/// exclusive key bounds for the subtree this walker points at (both `None`
/// at the root call). The walk reads each node's `aggregate_data()` and
/// each child link's `aggregate_data().as_count_u64()` exactly the same way
/// the proof emitter does, so the returned count is identical to the
/// `count` field returned by `create_aggregate_count_on_range_proof`.
#[cfg(feature = "minimal")]
fn walk_count_only<S>(
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

/// Verify a count-only proof for an `AggregateCountOnRange` query.
///
/// `proof_bytes` is the encoded `Vec<Op>` produced by
/// [`Merk::prove_aggregate_count_on_range`]; `inner_range` is the same
/// `QueryItem` the prover counted over (caller-supplied — typically extracted
/// from the verifier's `PathQuery`).
///
/// On success returns `(merk_root_hash, count)`:
/// - `merk_root_hash` is the root hash of the reconstructed merk; the
///   caller must compare it against the expected root hash to complete
///   verification.
/// - `count` is the number of keys in the inner range, computed by replaying
///   the prover's classification walk against the reconstructed proof tree.
///
/// **Two-phase verification.** Allowlisting node types alone is unsound:
/// a malicious prover can substitute `Hash` for an in-range subtree (to
/// undercount), attach extra `KVDigestCount` children below a keyless
/// `Hash` / `HashWithCount` (to overcount, since their hash recomputation
/// ignores attached children and the root hash would still match), or send
/// a single `Push(Hash(expected_root))` for a non-empty tree (to receive a
/// count of 0 with the trusted root). To prevent all three, this function:
///
/// 1. Decodes the proof into a `ProofTree` via `execute_with_options` with
///    the AVL balance check disabled (count proofs intentionally collapse
///    one side to height 1) and **does not** count anything in the
///    `visit_node` callback.
/// 2. Walks the reconstructed tree with the same inherited exclusive
///    subtree-key bounds the prover used (`(None, None)` at the root).
///    At each position it calls `classify_subtree(bounds, inner_range)` and
///    requires the proof-tree node type to match the classification:
///    - `Disjoint` → must be a leaf `Hash(_)`. Contributes 0.
///    - `Contained` → must be a leaf `HashWithCount(...)`. Contributes its
///      count.
///    - `Boundary` → must be `KVDigestCount(key, ...)` with `key` strictly
///      inside `bounds`. Recurse left with `(lo, key)` and right with
///      `(key, hi)`; add 1 if `inner_range.contains(key)`.
///
/// Counts are summed with `checked_add`; an overflow is treated as proof
/// corruption (`u64::MAX` keys is not a real merk shape). The caller is
/// still responsible for verifying the returned `merk_root_hash` against
/// their trusted root.
///
/// **Empty merk case.** An empty merk is represented by an empty proof byte
/// stream and yields `(NULL_HASH, 0)`. Callers chaining this in a
/// multi-layer proof should recognize that shape explicitly.
pub fn verify_aggregate_count_on_range_proof(
    proof_bytes: &[u8],
    inner_range: &QueryItem,
) -> CostResult<(CryptoHash, u64), Error> {
    if proof_bytes.is_empty() {
        // Empty merk → empty proof → count = 0, hash = NULL_HASH. This
        // matches the prover-side behavior of returning an empty op stream
        // for an empty subtree.
        return Ok((NULL_HASH, 0u64)).wrap_with_cost(OperationCost::default());
    }

    let mut cost = OperationCost::default();
    let decoder = Decoder::new(proof_bytes);

    // Phase 1: reconstruct the proof tree. The visit_node closure only
    // performs a coarse allowlist; the per-position type/shape check happens
    // in Phase 2 below. We still reject blatantly wrong node types here so
    // execute() bails early on garbage input.
    let tree_result: CostResult<ProofTree, Error> =
        execute_with_options(decoder, false, false, |node| match node {
            // The count proof emits only `HashWithCount` (for collapsed
            // Disjoint or Contained subtrees) and `KVDigestCount` (for
            // Boundary nodes). Plain `Hash(_)` is no longer used here
            // because the structural count it would otherwise stand in
            // for is needed by the verifier's `own_count` derivation and
            // would not be hash-bound.
            Node::HashWithCount(_, _, _, _) | Node::KVDigestCount(_, _, _) => Ok(()),
            other => Err(Error::InvalidProofError(format!(
                "unexpected node type in aggregate count proof: {}",
                other
            ))),
        });
    let tree = cost_return_on_error!(&mut cost, tree_result);

    // Phase 2: shape-check + count by replaying the prover's classification
    // walk. This binds each leaf node's type to the (subtree_bounds × range)
    // classification, so the only valid count is the one a faithful prover
    // would have produced for this exact range.
    let (count, _structural) = match verify_count_shape(&tree, inner_range, None, None) {
        Ok(pair) => pair,
        Err(e) => return Err(e).wrap_with_cost(cost),
    };

    let root_hash = tree.hash().unwrap_add_cost(&mut cost);
    Ok((root_hash, count)).wrap_with_cost(cost)
}

/// Recursive shape-walk over the reconstructed proof tree. Returns the
/// pair `(in_range_count, structural_count)`:
///
/// - `in_range_count` — number of keys in the subtree that fall inside the
///   inner range AND have a non-zero own-count (i.e. are not
///   `NonCounted`-wrapped). This is what bubbles up to the verifier's
///   return value.
/// - `structural_count` — the merk-recorded aggregate count of this subtree
///   (counting normal entries as 1 and `NonCounted` entries as 0). The
///   parent uses it to compute its own `own_count` as
///   `parent_node_count − left_struct − right_struct` (since
///   `parent_node_count = own + left_struct + right_struct`).
///
/// The structural count of every child is **cryptographically bound** to
/// the parent's hash chain because every count-bearing node in a count
/// proof (`KVDigestCount`, `HashWithCount`) has its count fed into
/// `node_hash_with_count` for hash recomputation. Plain `Hash(_)` would
/// not carry a bound count and is therefore not allowed in count proofs;
/// see the prover-side comment in `emit_count_proof` for the full
/// justification.
///
/// At each node:
///
/// - Compute the expected classification from the inherited subtree bounds
///   and the inner range.
/// - Require the node's type to match the classification (and reject any
///   children attached under a leaf-shape classification — a malicious
///   prover could otherwise hide counted children under a `HashWithCount`
///   leaf, since its hash recomputation ignores reconstructed children).
/// - Recurse with tightened bounds at `Boundary` nodes, summing with
///   `checked_add` and computing `own_count` via `checked_sub`.
fn verify_count_shape(
    tree: &ProofTree,
    range: &QueryItem,
    lo: Option<&[u8]>,
    hi: Option<&[u8]>,
) -> Result<(u64, u64), Error> {
    let class = classify_subtree(lo, hi, range);
    match class {
        SubtreeClassification::Disjoint => match &tree.node {
            Node::HashWithCount(_, _, _, count) => {
                if tree.left.is_some() || tree.right.is_some() {
                    return Err(Error::InvalidProofError(
                        "aggregate-count proof: HashWithCount node at a Disjoint position \
                         must be a leaf"
                            .to_string(),
                    ));
                }
                // Disjoint subtree contributes 0 to the in-range count but
                // its full structural count to the parent's `own_count`
                // computation.
                Ok((0, *count))
            }
            other => Err(Error::InvalidProofError(format!(
                "aggregate-count proof: expected HashWithCount at Disjoint position, got {}",
                other
            ))),
        },
        SubtreeClassification::Contained => match &tree.node {
            Node::HashWithCount(_, _, _, count) => {
                if tree.left.is_some() || tree.right.is_some() {
                    return Err(Error::InvalidProofError(
                        "aggregate-count proof: HashWithCount node at a Contained position \
                         must be a leaf"
                            .to_string(),
                    ));
                }
                // Contained subtree's structural count (which excludes
                // NonCounted entries because their stored aggregate is 0)
                // is exactly its in-range count.
                Ok((*count, *count))
            }
            other => Err(Error::InvalidProofError(format!(
                "aggregate-count proof: expected HashWithCount at Contained position, got {}",
                other
            ))),
        },
        SubtreeClassification::Boundary => match &tree.node {
            Node::KVDigestCount(key, _, aggregate) => {
                if !key_strictly_inside(key.as_slice(), lo, hi) {
                    return Err(Error::InvalidProofError(format!(
                        "aggregate-count proof: KVDigestCount key {} falls outside its \
                         inherited subtree bounds (lo={:?}, hi={:?})",
                        hex::encode(key),
                        lo.map(hex::encode),
                        hi.map(hex::encode),
                    )));
                }
                let key_slice = key.as_slice();
                let (left_in, left_struct) = match &tree.left {
                    Some(child) => verify_count_shape(&child.tree, range, lo, Some(key_slice))?,
                    None => (0, 0),
                };
                let (right_in, right_struct) = match &tree.right {
                    Some(child) => verify_count_shape(&child.tree, range, Some(key_slice), hi)?,
                    None => (0, 0),
                };
                // own_count = aggregate − left_struct − right_struct.
                // Saturating sub here would silently mask a malformed
                // proof (children claiming more keys than the parent's
                // aggregate), so use checked_sub and reject.
                let own_count = aggregate
                    .checked_sub(left_struct)
                    .and_then(|s| s.checked_sub(right_struct))
                    .ok_or_else(|| {
                        Error::InvalidProofError(format!(
                            "aggregate-count proof: child structural counts ({} + {}) exceed \
                             parent's aggregate count ({}) at key {}",
                            left_struct,
                            right_struct,
                            aggregate,
                            hex::encode(key)
                        ))
                    })?;
                let self_contribution = if range.contains(key_slice) {
                    own_count
                } else {
                    0
                };
                let in_range = left_in
                    .checked_add(right_in)
                    .and_then(|s| s.checked_add(self_contribution))
                    .ok_or_else(|| {
                        Error::InvalidProofError(
                            "aggregate-count proof: in-range count overflowed u64".to_string(),
                        )
                    })?;
                Ok((in_range, *aggregate))
            }
            other => Err(Error::InvalidProofError(format!(
                "aggregate-count proof: expected KVDigestCount at Boundary position, got {}",
                other
            ))),
        },
    }
}

/// Returns true when `key` lies strictly between the exclusive bounds
/// `(lo, hi)`, where `None` represents `-inf` / `+inf`. Used to validate that
/// a `Boundary` `KVDigestCount` carries a key consistent with its inherited
/// subtree window.
fn key_strictly_inside(key: &[u8], lo: Option<&[u8]>, hi: Option<&[u8]>) -> bool {
    let lo_ok = lo.is_none_or(|l| key > l);
    let hi_ok = hi.is_none_or(|h| key < h);
    lo_ok && hi_ok
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Asserts the hardcoded fixture in the `verify_only_tests` module
    /// still matches the bytes a fresh prove run produces. If the proof
    /// encoding ever changes, this test fails and prints the new
    /// constants — copy them into `verify_only_tests`.
    #[test]
    fn verify_only_fixture_matches_fresh_prover_output() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
        let (ops, count) = merk
            .prove_aggregate_count_on_range(&inner_range, v)
            .unwrap()
            .expect("prove");
        let proof_hex = hex::encode(encode_proof(&ops));
        let root_hex = hex::encode(root);

        let drift_msg = format!(
            "aggregate_count proof encoding has drifted — update verify_only_tests:\n\
             const FIXTURE_15_KEY_C_TO_L_PROOF_HEX: &str = \"{}\";\n\
             const FIXTURE_15_KEY_C_TO_L_ROOT_HEX: &str = \"{}\";\n\
             const FIXTURE_15_KEY_C_TO_L_COUNT: u64 = {};",
            proof_hex, root_hex, count
        );
        assert_eq!(
            proof_hex,
            super::verify_only_tests::FIXTURE_15_KEY_C_TO_L_PROOF_HEX,
            "{}",
            drift_msg
        );
        assert_eq!(
            root_hex,
            super::verify_only_tests::FIXTURE_15_KEY_C_TO_L_ROOT_HEX,
            "{}",
            drift_msg
        );
        assert_eq!(
            count,
            super::verify_only_tests::FIXTURE_15_KEY_C_TO_L_COUNT,
            "{}",
            drift_msg
        );
    }

    fn range_inclusive(lo: &[u8], hi: &[u8]) -> QueryItem {
        QueryItem::RangeInclusive(lo.to_vec()..=hi.to_vec())
    }

    fn range_full() -> QueryItem {
        QueryItem::RangeFull(std::ops::RangeFull)
    }

    fn range_from(lo: &[u8]) -> QueryItem {
        QueryItem::RangeFrom(lo.to_vec()..)
    }

    fn range_after(lo: &[u8]) -> QueryItem {
        QueryItem::RangeAfter(lo.to_vec()..)
    }

    #[test]
    fn classify_disjoint_below() {
        let r = range_inclusive(b"d", b"f");
        // subtree (None, b"c") — keys < "c", entirely below ["d", "f"].
        assert_eq!(
            classify_subtree(None, Some(b"c"), &r),
            SubtreeClassification::Disjoint,
        );
    }

    #[test]
    fn classify_disjoint_above() {
        let r = range_inclusive(b"d", b"f");
        // subtree (b"g", None) — keys > "g", entirely above ["d", "f"].
        assert_eq!(
            classify_subtree(Some(b"g"), None, &r),
            SubtreeClassification::Disjoint,
        );
    }

    #[test]
    fn classify_disjoint_at_lower_boundary_inclusive() {
        let r = range_inclusive(b"d", b"f");
        // subtree (None, b"d") — keys < "d", just below the inclusive bound.
        assert_eq!(
            classify_subtree(None, Some(b"d"), &r),
            SubtreeClassification::Disjoint,
        );
    }

    #[test]
    fn classify_disjoint_at_upper_boundary_inclusive() {
        let r = range_inclusive(b"d", b"f");
        // subtree (b"f", None) — keys > "f", just above the inclusive bound.
        assert_eq!(
            classify_subtree(Some(b"f"), None, &r),
            SubtreeClassification::Disjoint,
        );
    }

    #[test]
    fn classify_contained_simple() {
        let r = range_inclusive(b"a", b"z");
        // subtree (b"d", b"f") — keys in ("d", "f"), all in ["a", "z"].
        assert_eq!(
            classify_subtree(Some(b"d"), Some(b"f"), &r),
            SubtreeClassification::Contained,
        );
    }

    #[test]
    fn classify_contained_full_range_full_subtree() {
        let r = range_full();
        // The full range matches everything — even an unbounded subtree is
        // contained.
        assert_eq!(
            classify_subtree(None, None, &r),
            SubtreeClassification::Contained,
        );
    }

    #[test]
    fn classify_boundary_overlapping_lower() {
        let r = range_inclusive(b"d", b"f");
        // subtree (b"c", b"e") — keys in ("c", "e"), straddles the lower bound.
        assert_eq!(
            classify_subtree(Some(b"c"), Some(b"e"), &r),
            SubtreeClassification::Boundary,
        );
    }

    #[test]
    fn classify_boundary_overlapping_upper() {
        let r = range_inclusive(b"d", b"f");
        // subtree (b"e", b"g") — keys in ("e", "g"), straddles the upper bound.
        assert_eq!(
            classify_subtree(Some(b"e"), Some(b"g"), &r),
            SubtreeClassification::Boundary,
        );
    }

    #[test]
    fn classify_boundary_unbounded_below_with_bounded_range() {
        let r = range_from(b"d");
        // subtree (None, b"e") — could include keys < "d", so boundary.
        assert_eq!(
            classify_subtree(None, Some(b"e"), &r),
            SubtreeClassification::Boundary,
        );
    }

    #[test]
    fn classify_contained_range_after_exclusive() {
        let r = range_after(b"b");
        // RangeAfter(b"b") = (b, +inf). subtree (b"b", b"e") — keys > "b" and
        // < "e", all in (b, +inf). Contained.
        assert_eq!(
            classify_subtree(Some(b"b"), Some(b"e"), &r),
            SubtreeClassification::Contained,
        );
    }

    // ---------- end-to-end integration tests on a real merk ----------
    //
    // These tests build a small ProvableCountTree, generate count proofs
    // through the merk-level API, then verify them with the count verifier.
    // They cover the four documented categories: open-range (lower-only and
    // upper-only) and closed-range (inclusive and after-to-inclusive). Empty
    // tree and single-bound edge cases are also exercised.

    use grovedb_costs::CostsExt as _;
    use grovedb_version::version::GroveVersion;

    use crate::{
        proofs::{encode_into, Op as ProofOp},
        test_utils::TempMerk,
        tree::{Op, TreeFeatureType::ProvableCountedMerkNode},
        Merk, TreeType,
    };

    /// Build a fresh `ProvableCountTree` populated with single-byte keys
    /// "a".."o" (15 keys) — same shape as the running example in the book
    /// chapter's "Closed ranges" section. Returns the merk and its current
    /// root hash.
    fn make_15_key_provable_count_tree(grove_version: &GroveVersion) -> (TempMerk, [u8; 32]) {
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::ProvableCountTree);
        let keys: Vec<Vec<u8>> = (b'a'..=b'o').map(|c| vec![c]).collect();
        let entries: Vec<(Vec<u8>, Op)> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| {
                (
                    k.clone(),
                    Op::Put(vec![i as u8], ProvableCountedMerkNode(1)),
                )
            })
            .collect();
        merk.apply::<_, Vec<_>>(&entries, &[], None, grove_version)
            .unwrap()
            .expect("apply should succeed");
        merk.commit(grove_version);
        let root_hash = merk.root_hash().unwrap();
        (merk, root_hash)
    }

    /// Encode a `LinkedList<Op>` into the wire format that the verifier
    /// consumes.
    fn encode_proof(ops: &LinkedList<ProofOp>) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(128);
        encode_into(ops.iter(), &mut bytes);
        bytes
    }

    /// Round-trip helper: prove the inner range, encode the proof, verify it,
    /// assert the recovered root hash matches and the recovered count matches
    /// `expected_count`.
    fn round_trip(
        merk: &Merk<impl grovedb_storage::StorageContext<'static>>,
        expected_root: [u8; 32],
        inner_range: QueryItem,
        expected_count: u64,
        grove_version: &GroveVersion,
    ) {
        let (ops, prover_count) = merk
            .prove_aggregate_count_on_range(&inner_range, grove_version)
            .unwrap()
            .expect("prove should succeed");
        assert_eq!(
            prover_count, expected_count,
            "prover count mismatch for range {:?}",
            inner_range
        );
        let bytes = encode_proof(&ops);
        let (root, verifier_count) = verify_aggregate_count_on_range_proof(&bytes, &inner_range)
            .unwrap()
            .expect("verify should succeed");
        assert_eq!(
            root, expected_root,
            "verifier reconstructed wrong root for range {:?}",
            inner_range
        );
        assert_eq!(
            verifier_count, expected_count,
            "verifier count mismatch for range {:?}",
            inner_range
        );
    }

    #[test]
    fn integration_open_range_from() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // RangeFrom("c"..) → keys c..o (13 keys).
        round_trip(&merk, root, QueryItem::RangeFrom(b"c".to_vec()..), 13, v);
    }

    #[test]
    fn integration_open_range_after() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // RangeAfter(("b", ..)) → keys c..o (13 keys), same set as RangeFrom("c"..)
        // but proof shape differs — the boundary lands on "b" exclusive.
        round_trip(&merk, root, QueryItem::RangeAfter(b"b".to_vec()..), 13, v);
    }

    #[test]
    fn integration_open_range_to() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // RangeTo(..b"e") → keys a..d (4 keys, exclusive upper).
        round_trip(&merk, root, QueryItem::RangeTo(..b"e".to_vec()), 4, v);
    }

    #[test]
    fn integration_open_range_to_inclusive() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // RangeToInclusive(..=b"e") → keys a..e (5 keys, inclusive upper).
        round_trip(
            &merk,
            root,
            QueryItem::RangeToInclusive(..=b"e".to_vec()),
            5,
            v,
        );
    }

    #[test]
    fn integration_closed_range_inclusive() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // RangeInclusive("c"..="l") → 10 keys.
        round_trip(
            &merk,
            root,
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
            10,
            v,
        );
    }

    #[test]
    fn integration_closed_range_exclusive() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // Range("c".."l") → c..k (9 keys, exclusive upper).
        round_trip(
            &merk,
            root,
            QueryItem::Range(b"c".to_vec()..b"l".to_vec()),
            9,
            v,
        );
    }

    #[test]
    fn integration_closed_range_after_to_inclusive() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // RangeAfterToInclusive(("c", "l")) → keys d..l (9 keys: d..=l excluding c).
        round_trip(
            &merk,
            root,
            QueryItem::RangeAfterToInclusive(b"c".to_vec()..=b"l".to_vec()),
            9,
            v,
        );
    }

    #[test]
    fn integration_closed_range_after_to_exclusive() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // RangeAfterTo(("c", "l")) → keys d..l (8 keys, both exclusive).
        round_trip(
            &merk,
            root,
            QueryItem::RangeAfterTo(b"c".to_vec()..b"l".to_vec()),
            8,
            v,
        );
    }

    #[test]
    fn integration_range_below_all_keys() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // Entire range below the smallest key — should produce count = 0
        // and a Disjoint proof at the root level.
        round_trip(
            &merk,
            root,
            QueryItem::RangeInclusive(vec![0x00]..=vec![0x10]),
            0,
            v,
        );
    }

    #[test]
    fn integration_range_above_all_keys() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_count_tree(v);
        // Entire range above the largest key.
        round_trip(
            &merk,
            root,
            QueryItem::RangeInclusive(b"z".to_vec()..=vec![0xff]),
            0,
            v,
        );
    }

    #[test]
    fn integration_empty_merk() {
        let v = GroveVersion::latest();
        let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableCountTree);
        let (ops, prover_count) = merk
            .prove_aggregate_count_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
            .unwrap()
            .expect("prove on empty merk should succeed");
        assert_eq!(prover_count, 0);
        // Empty proof means the verifier returns NULL_HASH and count = 0.
        let bytes = encode_proof(&ops);
        let (root, verifier_count) = verify_aggregate_count_on_range_proof(
            &bytes,
            &QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        )
        .unwrap()
        .expect("verify on empty merk should succeed");
        assert_eq!(root, NULL_HASH);
        assert_eq!(verifier_count, 0);
    }

    #[test]
    fn integration_rejected_on_normal_tree() {
        let v = GroveVersion::latest();
        let merk = TempMerk::new(v); // NormalTree
        let err = merk
            .prove_aggregate_count_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
            .unwrap();
        assert!(
            err.is_err(),
            "expected an InvalidProofError on NormalTree, got Ok({:?})",
            err.ok().map(|(_, c)| c)
        );
    }

    #[test]
    fn integration_count_forgery_is_rejected() {
        // Demonstrates the cryptographic binding: tamper with the count in a
        // HashWithCount op and the verifier's root-hash recomputation must
        // diverge from the expected root.
        let v = GroveVersion::latest();
        let (merk, expected_root) = make_15_key_provable_count_tree(v);
        let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
        let (mut ops, _prover_count) = merk
            .prove_aggregate_count_on_range(&inner_range, v)
            .unwrap()
            .expect("prove should succeed");

        // Forge: bump the count on the first HashWithCount op we see.
        let mut tampered = false;
        for op in ops.iter_mut() {
            if let ProofOp::Push(Node::HashWithCount(_, _, _, count))
            | ProofOp::PushInverted(Node::HashWithCount(_, _, _, count)) = op
            {
                *count = count.saturating_add(1);
                tampered = true;
                break;
            }
        }
        assert!(
            tampered,
            "test setup: expected at least one HashWithCount op"
        );

        let bytes = encode_proof(&ops);
        let (root, _count) = verify_aggregate_count_on_range_proof(&bytes, &inner_range)
            .unwrap()
            .expect("verify should still complete (root mismatch is the caller's job)");
        assert_ne!(
            root, expected_root,
            "tampered count must produce a different reconstructed root hash"
        );
    }

    // ---------- no-proof variant: count_aggregate_on_range ----------
    //
    // The no-proof entry point must return exactly the same count as the
    // proof path for every range shape, without producing any proof ops.
    // These tests cross-check the two paths on the same merk.

    /// Cross-check: assert that `count_aggregate_on_range` and the count
    /// returned by `prove_aggregate_count_on_range` agree for the given
    /// range, and that both equal `expected_count`.
    fn no_proof_matches_prover(
        merk: &Merk<impl grovedb_storage::StorageContext<'static>>,
        inner_range: QueryItem,
        expected_count: u64,
        grove_version: &GroveVersion,
    ) {
        let no_proof = merk
            .count_aggregate_on_range(&inner_range, grove_version)
            .unwrap()
            .expect("count_aggregate_on_range should succeed");
        assert_eq!(
            no_proof, expected_count,
            "no-proof variant returned wrong count for range {:?}",
            inner_range
        );
        let (_ops, prover_count) = merk
            .prove_aggregate_count_on_range(&inner_range, grove_version)
            .unwrap()
            .expect("prove should succeed");
        assert_eq!(
            no_proof, prover_count,
            "no-proof variant disagrees with prover count for range {:?}",
            inner_range
        );
    }

    #[test]
    fn no_proof_matches_prover_closed_range_inclusive() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_count_tree(v);
        no_proof_matches_prover(
            &merk,
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
            10,
            v,
        );
    }

    #[test]
    fn no_proof_matches_prover_closed_range_exclusive() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_count_tree(v);
        no_proof_matches_prover(&merk, QueryItem::Range(b"c".to_vec()..b"l".to_vec()), 9, v);
    }

    #[test]
    fn no_proof_matches_prover_open_range_from() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_count_tree(v);
        no_proof_matches_prover(&merk, QueryItem::RangeFrom(b"c".to_vec()..), 13, v);
    }

    #[test]
    fn no_proof_matches_prover_range_below_all_keys() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_count_tree(v);
        no_proof_matches_prover(
            &merk,
            QueryItem::RangeInclusive(vec![0x00]..=vec![0x10]),
            0,
            v,
        );
    }

    #[test]
    fn no_proof_empty_merk_returns_zero() {
        let v = GroveVersion::latest();
        let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableCountTree);
        let count = merk
            .count_aggregate_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
            .unwrap()
            .expect("count_aggregate_on_range on empty merk should succeed");
        assert_eq!(count, 0);
    }

    #[test]
    fn no_proof_rejected_on_normal_tree() {
        let v = GroveVersion::latest();
        let merk = TempMerk::new(v); // NormalTree
        let result = merk
            .count_aggregate_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
            .unwrap();
        assert!(
            result.is_err(),
            "expected InvalidProofError on NormalTree, got Ok({:?})",
            result.ok()
        );
    }

    #[test]
    fn no_proof_matches_prover_range_after() {
        // RangeAfter at the root pushes the left boundary exclusive to "b",
        // which causes the walk to descend into the right subtree from the
        // root — exercising the right-child arm of walk_count_only.
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_count_tree(v);
        no_proof_matches_prover(&merk, QueryItem::RangeAfter(b"b".to_vec()..), 13, v);
    }

    #[test]
    fn no_proof_matches_prover_range_to() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_count_tree(v);
        // RangeTo(..b"e") — exclusive upper, keys a..d (4 keys).
        no_proof_matches_prover(&merk, QueryItem::RangeTo(..b"e".to_vec()), 4, v);
    }

    #[test]
    fn no_proof_matches_prover_range_to_inclusive() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_count_tree(v);
        // RangeToInclusive(..=b"e") — keys a..=e (5 keys).
        no_proof_matches_prover(&merk, QueryItem::RangeToInclusive(..=b"e".to_vec()), 5, v);
    }

    #[test]
    fn no_proof_matches_prover_range_after_to_inclusive() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_count_tree(v);
        // RangeAfterToInclusive(("c", "l")) — keys d..=l (9 keys).
        no_proof_matches_prover(
            &merk,
            QueryItem::RangeAfterToInclusive(b"c".to_vec()..=b"l".to_vec()),
            9,
            v,
        );
    }

    #[test]
    fn no_proof_provable_count_sum_tree() {
        // Exercise the ProvableCountSumTree branch of the tree-type gate —
        // it should accept the walk and return the same count as a
        // ProvableCountTree with the same key set.
        let v = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(v, TreeType::ProvableCountSumTree);
        // ProvableCountedAndSummedMerkNode(count=1, sum=0): treats each
        // entry as count-1 with sum-contribution 0.
        let entries: Vec<(Vec<u8>, Op)> = (b'a'..=b'o')
            .enumerate()
            .map(|(i, c)| {
                (
                    vec![c],
                    Op::Put(
                        vec![i as u8],
                        crate::tree::TreeFeatureType::ProvableCountedSummedMerkNode(1, 0),
                    ),
                )
            })
            .collect();
        merk.apply::<_, Vec<_>>(&entries, &[], None, v)
            .unwrap()
            .expect("apply ProvableCountSumTree entries");
        merk.commit(v);

        let count = merk
            .count_aggregate_on_range(&QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()), v)
            .unwrap()
            .expect("count_aggregate_on_range on ProvableCountSumTree should succeed");
        assert_eq!(count, 10, "c..=l should be 10 keys");
    }

    // ---------- attack tests for the shape-walk verifier ----------
    //
    // These three tests exercise attacks the old allowlist-only verifier let
    // through. With the shape walk in `verify_count_shape`, each one is
    // rejected before the caller's root-hash check.

    /// A malicious prover sends a single `Push(Hash(expected_root))` for a
    /// non-empty tree. Without the shape check this would return
    /// `(expected_root, 0)` for any range. The shape check classifies the
    /// root with `(None, None)` against a bounded inner range as `Boundary`,
    /// expects `KVDigestCount`, and rejects.
    #[test]
    fn shape_walk_rejects_single_hash_undercount() {
        let v = GroveVersion::latest();
        let (merk, expected_root) = make_15_key_provable_count_tree(v);
        let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());

        // Forged proof: a single Hash op carrying the genuine root hash.
        let mut forged: LinkedList<ProofOp> = LinkedList::new();
        forged.push_back(ProofOp::Push(Node::Hash(expected_root)));
        let bytes = encode_proof(&forged);

        let result = verify_aggregate_count_on_range_proof(&bytes, &inner_range).unwrap();
        let err = result.expect_err("single-Hash forgery must be rejected");
        // keep merk alive for clarity in the test scope
        let _ = merk;
        // Plain `Hash` is no longer in the count-proof allowlist (it would
        // carry an unbound structural count), so the rejection now lands
        // in Phase 1's coarse allowlist rather than Phase 2's shape walk.
        // Either error message is fine — the attack is rejected.
        match err {
            Error::InvalidProofError(msg) => {
                assert!(
                    msg.contains("unexpected node type")
                        || msg.contains("expected KVDigestCount")
                        || msg.contains("Boundary"),
                    "unexpected message: {msg}"
                );
            }
            other => panic!("expected InvalidProofError, got {other:?}"),
        }
    }

    /// A malicious prover replaces an in-range `HashWithCount` subtree with
    /// a `Hash` carrying that subtree's node_hash, undercounting by the
    /// subtree's count. The hash chain still matches (same node_hash), so
    /// the old allowlist verifier would have happily returned a wrong
    /// count. The shape walk classifies that position as `Contained` and
    /// requires `HashWithCount`, rejecting the swap.
    #[test]
    fn shape_walk_rejects_hash_swap_for_contained_subtree() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_count_tree(v);
        let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
        let (mut ops, _) = merk
            .prove_aggregate_count_on_range(&inner_range, v)
            .unwrap()
            .expect("prove succeeds");

        // Swap the first HashWithCount op for a Hash op carrying the
        // computed node_hash for that subtree (so the chain check still
        // matches and only the shape walk can detect the attack).
        let mut swapped = false;
        for op in ops.iter_mut() {
            if let ProofOp::Push(Node::HashWithCount(kv_hash, l, r, c)) = op {
                let node_hash = crate::tree::node_hash_with_count(kv_hash, l, r, *c).unwrap();
                *op = ProofOp::Push(Node::Hash(node_hash));
                swapped = true;
                break;
            }
        }
        assert!(
            swapped,
            "test setup: expected at least one HashWithCount op"
        );

        let bytes = encode_proof(&ops);
        let result = verify_aggregate_count_on_range_proof(&bytes, &inner_range).unwrap();
        assert!(
            result.is_err(),
            "HashWithCount→Hash swap on a Contained subtree must be rejected by the shape walk"
        );
    }

    /// A malicious prover attaches a `KVDigestCount` child under a leaf
    /// `HashWithCount`. Because `Tree::hash()` for `HashWithCount` is
    /// computed from the four embedded fields and ignores any reconstructed
    /// children, the root hash check passes — but a naive verifier that
    /// counts every visited node would credit the bogus child as +1. The
    /// shape walk requires `Contained` positions to be **leaves**, so it
    /// rejects the smuggled-in child.
    #[test]
    fn shape_walk_rejects_keyless_node_with_attached_children() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_count_tree(v);
        let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
        let (mut ops, _honest_count) = merk
            .prove_aggregate_count_on_range(&inner_range, v)
            .unwrap()
            .expect("prove succeeds");

        // Smuggle a fake +1 child under the first HashWithCount op. After
        // any HashWithCount(...), insert: Push(Hash(zero)) Parent — that
        // attaches an extra hashed node as the LEFT child of the
        // HashWithCount during reconstruction. Then add a fake
        // Push(KVDigestCount) Child that would be picked up by an
        // allowlist verifier counting visited keys.
        //
        // Concretely we splice 4 ops right after the HashWithCount:
        //   Push(KVDigestCount(in_range_key, value_hash, 1))
        //   Parent             (attach KVDigestCount as the LEFT child of HashWithCount)
        //   Push(Hash([0; 32]))
        //   Child              (attach Hash as the RIGHT child of HashWithCount)
        //
        // The HashWithCount's hash() ignores these children, so the root
        // hash recomputation is unaffected. The shape walk catches the
        // Contained-position-with-children violation.
        let mut new_ops: LinkedList<ProofOp> = LinkedList::new();
        let mut spliced = false;
        for op in ops.iter() {
            new_ops.push_back(op.clone());
            if !spliced && matches!(op, ProofOp::Push(Node::HashWithCount(_, _, _, _))) {
                let in_range_key = b"d".to_vec();
                new_ops.push_back(ProofOp::Push(Node::KVDigestCount(
                    in_range_key,
                    [0u8; 32],
                    1,
                )));
                new_ops.push_back(ProofOp::Parent);
                new_ops.push_back(ProofOp::Push(Node::Hash([0u8; 32])));
                new_ops.push_back(ProofOp::Child);
                spliced = true;
            }
        }
        assert!(
            spliced,
            "test setup: expected to splice into a HashWithCount"
        );
        ops = new_ops;

        let bytes = encode_proof(&ops);
        let result = verify_aggregate_count_on_range_proof(&bytes, &inner_range).unwrap();
        assert!(
            result.is_err(),
            "attaching children under HashWithCount must be rejected (root hash alone wouldn't catch it)"
        );
    }

    /// `HashWithCount` is only safe inside the dedicated aggregate-count
    /// verifier (which shape-checks the collapsed subtree). The plain
    /// `Query::execute_proof` verifier must reject it on sight — otherwise
    /// a malicious prover could include `HashWithCount` in a regular
    /// query proof, attach fake KV children to it (whose pushes the
    /// verifier would credit as query results via `execute_node`), and
    /// have the parent's hash chain still verify because
    /// `Tree::hash()` for `HashWithCount` ignores attached children.
    #[test]
    fn regular_query_verifier_rejects_hash_with_count_node() {
        use crate::proofs::query::QueryProofVerify;
        let v = GroveVersion::latest();

        // Build a regular merk and a regular range query against it.
        let mut merk = TempMerk::new(v);
        for i in 0u8..5 {
            merk.apply::<_, Vec<_>>(
                &[(
                    vec![i],
                    Op::Put(vec![i], crate::TreeFeatureType::BasicMerkNode),
                )],
                &[],
                None,
                v,
            )
            .unwrap()
            .expect("apply");
        }
        merk.commit(v);
        let q = crate::proofs::query::Query::new_single_query_item(QueryItem::Range(
            vec![0u8]..vec![5u8],
        ));

        // Generate an honest proof, then splice a `HashWithCount` push into
        // it. The exact op sequence doesn't matter for what we're testing —
        // we just need the regular verifier to refuse to process the proof
        // because it contains a `HashWithCount`.
        let (mut ops, _) = merk
            .prove_unchecked_query_items(&[QueryItem::Range(vec![0u8]..vec![5u8])], None, true, v)
            .unwrap()
            .expect("prove");
        ops.push_front(ProofOp::Push(Node::HashWithCount(
            [0u8; 32], [0u8; 32], [0u8; 32], 0,
        )));
        let bytes = encode_proof(&ops);

        let result = q.execute_proof(&bytes, None, true, 0).unwrap();
        let err = result.expect_err("regular query verifier must reject HashWithCount on sight");
        let msg = format!("{}", err);
        assert!(
            msg.contains("HashWithCount") || msg.contains("aggregate-count"),
            "expected HashWithCount-rejection message, got: {msg}"
        );
    }

    // ---------- byte-mutation fuzzer ----------
    //
    // Stronger forgery-resistance check than the three hand-crafted attack
    // tests above: enumerate every byte of an honest proof, flip it to
    // each of three different values, and assert the verifier never
    // produces a "silent forgery" — i.e. an `Ok((root, count))` where
    // the root **matches** the honest one but the count **differs**.
    //
    // Three safe outcomes per mutation:
    //  - **Rejection** — Phase 1 decode error, or Phase 2 shape mismatch.
    //  - **Divergence** — `Ok((root', _))` where `root' != honest_root`,
    //    so any caller comparing against their trusted root catches it.
    //  - **Same outcome** — `Ok((honest_root, honest_count))`. This can
    //    happen for non-canonical re-encodings (e.g. swapping
    //    `Push` ↔ `PushInverted` doesn't change the reconstructed tree's
    //    root or the shape walk's count). Harmless: the verifier is
    //    deterministic on (root, count), and that pair is what the
    //    caller acts on.
    //
    // The **unsafe** outcome is `Ok((honest_root, count'))` where
    // `count' != honest_count`. The hash chain binds count via
    // `node_hash_with_count`, so this should be impossible — the test
    // panics if it ever happens.
    //
    // We also assert each safe branch fires at least once as a sanity
    // check that the test is actually exercising the surface.
    #[test]
    fn fuzz_byte_mutation_no_silent_forgery() {
        let v = GroveVersion::latest();
        let (merk, honest_root) = make_15_key_provable_count_tree(v);
        let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
        let (ops, honest_count) = merk
            .prove_aggregate_count_on_range(&inner_range, v)
            .unwrap()
            .expect("prove");
        let honest_bytes = encode_proof(&ops);
        assert!(!honest_bytes.is_empty());

        let mut rejected = 0usize;
        let mut diverged = 0usize;
        let mut same_outcome = 0usize;
        let mut total = 0usize;

        // Three different mutations per byte: +1, +0x55, XOR 0xff.
        let deltas: [u8; 3] = [1, 0x55, 0xff];
        for byte_idx in 0..honest_bytes.len() {
            for &delta in &deltas {
                let mut bytes = honest_bytes.clone();
                let original = bytes[byte_idx];
                let mutated = if delta == 0xff {
                    original ^ 0xff
                } else {
                    original.wrapping_add(delta)
                };
                if mutated == original {
                    continue; // no-op, don't count
                }
                bytes[byte_idx] = mutated;
                total += 1;

                let result = verify_aggregate_count_on_range_proof(&bytes, &inner_range).unwrap();
                match result {
                    Err(_) => rejected += 1,
                    Ok((root, count)) => {
                        if root == honest_root {
                            // Same root — the verifier MUST also produce
                            // the same count, otherwise we have a silent
                            // count-forgery: the caller would accept the
                            // forged count thinking it's the honest one.
                            assert_eq!(
                                count, honest_count,
                                "SILENT FORGERY at byte index {} (delta=0x{:02x}): \
                                 verifier returned the honest root but a wrong count \
                                 ({} != {}). The hash chain should bind count.",
                                byte_idx, delta, count, honest_count
                            );
                            same_outcome += 1;
                        } else {
                            // Different root — caller's root check catches it.
                            diverged += 1;
                        }
                    }
                }
            }
        }

        // Sanity: each safe branch should fire at least once on a real proof.
        assert!(
            rejected > 0,
            "expected at least one mutation to be rejected outright"
        );
        assert!(
            diverged > 0,
            "expected at least one mutation to diverge the root hash"
        );
        // `same_outcome` may legitimately be zero on some encoders, so we
        // don't require it. We just require no silent forgery occurred,
        // which the inner assert_eq! guarantees.
        let _ = same_outcome;
        assert_eq!(rejected + diverged + same_outcome, total);
    }

    // ---------- randomized round-trip property test ----------
    //
    // Build merks with varying sizes and key shapes from a deterministic
    // RNG, run a bunch of randomly-chosen ranges through the prove → encode
    // → verify pipeline, and assert the verifier's count agrees with a
    // ground-truth count computed by directly intersecting the inserted
    // keys with the range. Catches silent miscounts that the fixed
    // examples above would miss (off-by-one, edge-of-tree, exact-bound
    // matches against multi-byte keys, etc.).
    #[test]
    fn fuzz_random_trees_and_ranges_round_trip() {
        // Tiny custom xorshift RNG so we don't have to add a dev-dep.
        struct XorShift(u64);
        impl XorShift {
            fn next_u64(&mut self) -> u64 {
                let mut x = self.0;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                self.0 = x;
                x
            }
            fn gen_range(&mut self, lo: usize, hi: usize) -> usize {
                lo + (self.next_u64() as usize) % (hi - lo)
            }
            fn gen_key(&mut self, max_len: usize) -> Vec<u8> {
                let len = 1 + self.gen_range(0, max_len);
                (0..len).map(|_| (self.next_u64() & 0xff) as u8).collect()
            }
        }

        let v = GroveVersion::latest();
        let mut rng = XorShift(0xDEAD_BEEF_C0FFEE);
        let trials = 16;
        for trial in 0..trials {
            let key_count = rng.gen_range(1, 64);
            let mut keys: Vec<Vec<u8>> = (0..key_count).map(|_| rng.gen_key(8)).collect();
            keys.sort();
            keys.dedup();

            let mut merk = TempMerk::new_with_tree_type(v, TreeType::ProvableCountTree);
            let entries: Vec<(Vec<u8>, Op)> = keys
                .iter()
                .map(|k| (k.clone(), Op::Put(vec![0xAB], ProvableCountedMerkNode(1))))
                .collect();
            merk.apply::<_, Vec<_>>(&entries, &[], None, v)
                .unwrap()
                .expect("apply");
            merk.commit(v);
            let root = merk.root_hash().unwrap();

            // Try several random ranges per tree, picking shapes that
            // exercise both bounded and half-bounded variants.
            for sub_trial in 0..6 {
                let lo = rng.gen_key(8);
                let hi = rng.gen_key(8);
                let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };

                let inner_range = match sub_trial % 6 {
                    0 => QueryItem::Range(lo.clone()..hi.clone()),
                    1 => QueryItem::RangeInclusive(lo.clone()..=hi.clone()),
                    2 => QueryItem::RangeFrom(lo.clone()..),
                    3 => QueryItem::RangeAfter(lo.clone()..),
                    4 => QueryItem::RangeTo(..hi.clone()),
                    _ => QueryItem::RangeToInclusive(..=hi.clone()),
                };

                let expected = keys
                    .iter()
                    .filter(|k| inner_range.contains(k.as_slice()))
                    .count() as u64;

                let (ops, prover_count) = merk
                    .prove_aggregate_count_on_range(&inner_range, v)
                    .unwrap()
                    .expect("prove");
                assert_eq!(
                    prover_count, expected,
                    "trial {} sub {}: prover count mismatch for range {:?}",
                    trial, sub_trial, inner_range
                );
                let bytes = encode_proof(&ops);
                let (vroot, vcount) = verify_aggregate_count_on_range_proof(&bytes, &inner_range)
                    .unwrap()
                    .expect("verify");
                assert_eq!(
                    vroot, root,
                    "trial {} sub {}: verifier root mismatch",
                    trial, sub_trial
                );
                assert_eq!(
                    vcount, expected,
                    "trial {} sub {}: verifier count mismatch for range {:?}",
                    trial, sub_trial, inner_range
                );
            }
        }
    }

    // ---------- shape-walk rejection of malformed proof shapes ----------
    //
    // These tests synthesize op streams that are well-formed bytes (Phase 1
    // decode succeeds) but violate the structural invariants the shape walk
    // requires (Phase 2 rejection). They exist to lock down the defensive
    // error branches in `verify_count_shape` so future refactors that
    // accidentally relax them are caught by the test suite.

    /// `HashWithCount` is only valid as a leaf in the proof tree. If the
    /// prover attaches children to a Disjoint-position `HashWithCount`,
    /// the shape walk must reject — even though the parent's hash chain
    /// (which uses `Tree::hash()` for `HashWithCount`, computed from the
    /// four embedded fields and ignoring children) would still verify.
    #[test]
    fn shape_walk_rejects_disjoint_hashwithcount_with_children() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_count_tree(v);
        // RangeAfter("o") → all 15 keys are below; the entire tree is
        // Disjoint relative to the inner range, so the honest proof is a
        // single Push(HashWithCount(...)).
        let inner_range = QueryItem::RangeAfter(b"o".to_vec()..);
        let (mut ops, _) = merk
            .prove_aggregate_count_on_range(&inner_range, v)
            .unwrap()
            .expect("prove succeeds");

        // Splice in another HashWithCount as the child (no key, so no
        // ordering constraint at Phase 1) so we exercise Phase 2's
        // leaf-only assertion at the Disjoint position.
        let mut spliced = LinkedList::<ProofOp>::new();
        let mut done = false;
        for op in ops.iter() {
            spliced.push_back(op.clone());
            if !done && matches!(op, ProofOp::Push(Node::HashWithCount(_, _, _, _))) {
                spliced.push_back(ProofOp::Push(Node::HashWithCount(
                    [0u8; 32], [0u8; 32], [0u8; 32], 1,
                )));
                spliced.push_back(ProofOp::Parent);
                done = true;
            }
        }
        assert!(done, "test setup: expected at least one HashWithCount op");
        ops = spliced;

        let bytes = encode_proof(&ops);
        let result = verify_aggregate_count_on_range_proof(&bytes, &inner_range).unwrap();
        let err = result.expect_err("Disjoint HashWithCount with children must be rejected");
        match err {
            Error::InvalidProofError(msg) => assert!(
                msg.contains("Disjoint position must be a leaf"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected InvalidProofError, got {:?}", other),
        }
    }

    /// At a Disjoint position the shape walk requires `HashWithCount` (only
    /// node type with a hash-bound count). A `Hash` op there would carry an
    /// untrusted structural count for the parent's `own_count` derivation,
    /// so it must be rejected.
    #[test]
    fn shape_walk_rejects_non_hashwithcount_at_disjoint() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_count_tree(v);
        let inner_range = QueryItem::RangeAfter(b"o".to_vec()..);
        let (mut ops, _) = merk
            .prove_aggregate_count_on_range(&inner_range, v)
            .unwrap()
            .expect("prove succeeds");

        // Replace the single Disjoint HashWithCount with a plain Hash.
        let mut swapped = false;
        for op in ops.iter_mut() {
            if let ProofOp::Push(Node::HashWithCount(kv, l, r, c)) = op {
                let node_hash = crate::tree::node_hash_with_count(kv, l, r, *c).unwrap();
                *op = ProofOp::Push(Node::Hash(node_hash));
                swapped = true;
                break;
            }
        }
        assert!(swapped, "test setup: expected a HashWithCount op to swap");

        let bytes = encode_proof(&ops);
        let result = verify_aggregate_count_on_range_proof(&bytes, &inner_range).unwrap();
        // Phase 1 rejects plain Hash via the allowlist; Phase 2 would also
        // reject "expected HashWithCount at Disjoint position". Either is fine.
        let err = result.expect_err("plain Hash at Disjoint must be rejected");
        match err {
            Error::InvalidProofError(_) => {}
            other => panic!("expected InvalidProofError, got {:?}", other),
        }
    }

    /// At a Boundary position the shape walk requires the node's key to
    /// fall strictly inside the inherited subtree bounds. A prover that
    /// emits a `KVDigestCount` whose key is outside those bounds is trying
    /// to confuse the recursion's bound tracking — it must be rejected.
    #[test]
    fn shape_walk_rejects_kvdigestcount_outside_inherited_bounds() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_count_tree(v);
        let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
        let (mut ops, _) = merk
            .prove_aggregate_count_on_range(&inner_range, v)
            .unwrap()
            .expect("prove succeeds");

        // Find a Boundary KVDigestCount and rewrite its key to something
        // outside the tree (way past 'z'). This will violate the inherited
        // (lo, hi) bounds at the verifier's recursion frame.
        let mut rewrote = false;
        for op in ops.iter_mut() {
            if let ProofOp::Push(Node::KVDigestCount(key, _, _)) = op {
                *key = vec![0xff, 0xff];
                rewrote = true;
                break;
            }
        }
        assert!(rewrote, "test setup: expected a KVDigestCount to rewrite");

        let bytes = encode_proof(&ops);
        let result = verify_aggregate_count_on_range_proof(&bytes, &inner_range).unwrap();
        let err = result.expect_err("KVDigestCount outside bounds must be rejected");
        match err {
            Error::InvalidProofError(_) => {}
            other => panic!("expected InvalidProofError, got {:?}", other),
        }
    }
}

/// Verifier-only smoke tests that exercise the leaf-level verifier without
/// pulling in any prover-side machinery (no `TempMerk`, no `RefWalker`).
/// They consume hardcoded fixtures kept in lockstep with the prover by
/// `tests::verify_only_fixture_matches_fresh_prover_output` above —
/// when that drift check fails it prints fresh constants to paste here.
#[cfg(test)]
mod verify_only_tests {
    use super::*;

    /// Hex-encoded proof bytes for a 15-key `ProvableCountTree` (keys
    /// "a"..="o", each with feature `ProvableCountedMerkNode(1)`) queried
    /// with `RangeInclusive("c"..="l")`. Captured from
    /// `dump_verify_only_fixtures`; regenerate if the proof encoding ever
    /// changes.
    pub(super) const FIXTURE_15_KEY_C_TO_L_PROOF_HEX: &str = "1e76f2d62fbefb076d8902b2f25bcf9acbd1e903b740c98ea0d926473922f6bbb50000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000011a01622022ec9d571ba774cf9e83d0194962f5d1e3aa1a48d486a67e2762a6c79590150000000000000003101a0163b7d770040f780e9deff6bc038abea66e108b88d098d16d24cd7486eb671060b20000000000000001111a0164d2ad1a0bb9fdf4450bd87151c08b9968cd046bda6654aabdba2430b0a981e7900000000000000007101e28b724715b1fab1f72e0be7e944488dcfbeeb875867d27c06ad9bad8c739997207ce95cd4d1789e01c0a079a3f2c18a3888f2d69fa6d0eabf51c2b434f7cb99e212f1a0042798fb890e8203f007f4f58b033a72cb4e070bfddceb27687d641fe0000000000000003111a01683fc14ed7ecde203a90425ee191e9db5966336d737f0398ec93b764517b6df400000000000000000f101e6da2e2f8e4bdead2a8ac51909f0fa0fb88d47d6bc3b84858bb739fb28a36501031b7c191d5ac70764f815bd7a6c7d0e628f48cef5b813933c07d5ce0ac1dbd5a995443ca10193ebf20e64468deaecc061a981a6dbf4f30e7154b5e9ab806866d00000000000000031a016c55c024f95ca4cc338f7cc2e25db37be2a3fa3a40b151017e460bfc0779cf369f0000000000000007101e3673296561a4d6c3e1ec5cd02c5c468acbd3c8ccd4a42906e8ed06d3fb587a0d2b6d9e310b7c94d3f91fcbb3d5f7547b76c6d1ab3ac3d3540752c5f0b46be24a2f66bf541434a53eae46fa4e6092c03511538c0e1a2c5fc0f0deb72de08a71e500000000000000031111";
    pub(super) const FIXTURE_15_KEY_C_TO_L_ROOT_HEX: &str =
        "19ed16776ebe6643b342a238baf7508ddf687fc4bdd53e98f91df8bffb605d96";
    pub(super) const FIXTURE_15_KEY_C_TO_L_COUNT: u64 = 10;

    /// Empty proof bytes encode "empty merk" — the verifier returns
    /// `(NULL_HASH, 0)`. This case has no prover dependency at all and is
    /// the most basic compile-time signal that the verifier path is wired
    /// up correctly under `--no-default-features --features verify`.
    #[test]
    fn empty_merk_returns_null_hash_and_zero_count() {
        let inner = QueryItem::Range(b"a".to_vec()..b"z".to_vec());
        let (root, count) = verify_aggregate_count_on_range_proof(&[], &inner)
            .unwrap()
            .expect("verify on empty proof must succeed");
        assert_eq!(root, NULL_HASH);
        assert_eq!(count, 0);
    }

    /// A real `RangeInclusive("c"..="l")` proof against a 15-key
    /// `ProvableCountTree`. The verifier must reconstruct the expected
    /// merk root hash and recover count = 10.
    #[test]
    fn fixture_15_key_range_c_to_l_verifies() {
        let proof = hex::decode(FIXTURE_15_KEY_C_TO_L_PROOF_HEX).expect("valid hex");
        let mut expected_root = [0u8; 32];
        expected_root
            .copy_from_slice(&hex::decode(FIXTURE_15_KEY_C_TO_L_ROOT_HEX).expect("valid hex"));

        let inner = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
        let (root, count) = verify_aggregate_count_on_range_proof(&proof, &inner)
            .unwrap()
            .expect("fixture proof must verify");
        assert_eq!(
            root, expected_root,
            "verifier reconstructed an unexpected root — fixture stale?"
        );
        assert_eq!(count, FIXTURE_15_KEY_C_TO_L_COUNT);
    }

    /// Mutating any single byte of the fixture proof must not yield a
    /// `(honest_root, wrong_count)` outcome — the hash chain binds count via
    /// `node_hash_with_count`, so any successful verify with the honest root
    /// must reproduce the honest count. Single-fixture analogue of
    /// `fuzz_byte_mutation_no_silent_forgery` that runs without the prover.
    #[test]
    fn fixture_byte_mutation_does_not_silently_forge_count() {
        let proof = hex::decode(FIXTURE_15_KEY_C_TO_L_PROOF_HEX).expect("valid hex");
        let mut expected_root = [0u8; 32];
        expected_root
            .copy_from_slice(&hex::decode(FIXTURE_15_KEY_C_TO_L_ROOT_HEX).expect("valid hex"));
        let inner = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());

        // Sanity check: the honest fixture verifies under the same code path
        // the mutation loop will exercise. Without this, an `Err`-on-honest
        // fixture would silently make every mutation a vacuous pass.
        let (honest_root, honest_count) = verify_aggregate_count_on_range_proof(&proof, &inner)
            .unwrap()
            .expect("honest fixture must verify (regenerate fixture if this fails)");
        assert_eq!(honest_root, expected_root);
        assert_eq!(honest_count, FIXTURE_15_KEY_C_TO_L_COUNT);

        for byte_idx in 0..proof.len() {
            for &delta in &[1u8, 0x55, 0xff] {
                let mut bytes = proof.clone();
                let original = bytes[byte_idx];
                let mutated = if delta == 0xff {
                    original ^ 0xff
                } else {
                    original.wrapping_add(delta)
                };
                if mutated == original {
                    continue;
                }
                bytes[byte_idx] = mutated;
                if let Ok((root, count)) =
                    verify_aggregate_count_on_range_proof(&bytes, &inner).unwrap()
                    && root == expected_root
                {
                    assert_eq!(
                        count, FIXTURE_15_KEY_C_TO_L_COUNT,
                        "SILENT FORGERY at byte {} (delta=0x{:02x}): \
                         verifier returned the honest root but a wrong count \
                         ({} != {}).",
                        byte_idx, delta, count, FIXTURE_15_KEY_C_TO_L_COUNT
                    );
                }
                // Err and Ok-with-different-root are both safe outcomes.
            }
        }
    }
}
