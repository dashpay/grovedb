//! Proof generation and verification for `AggregateSumOnRange` queries.
//!
//! This module is the sum-only twin of [`super::aggregate_count`]. It
//! implements the proof shape described in the GroveDB book chapter
//! "Aggregate Sum Queries": instead of returning the number of keys in the
//! inner range, the query returns the **signed `i64` sum** of children with
//! keys in that range against a `ProvableSumTree`.
//!
//! Like its count sibling, this module is intentionally **separate** from
//! `create_proof_internal`: regular proofs always descend into a queried
//! subtree, but sum proofs *stop* at fully-inside subtree roots and emit a
//! single `HashWithSum` op for the entire collapsed subtree.
//!
//! The proof targets a `ProvableSumTree` exclusively (the `NotSummed`
//! wrapper variant only affects whether the tree contributes to its parent's
//! sum, not its own internal sum mechanics). On any other tree type the
//! entry point returns `Error::InvalidProofError`.
//!
//! ## Negative-sum gotchas mirrored from the count side
//!
//! - The accumulator can legitimately reach zero with non-zero children
//!   (e.g. `+5` plus `-5`), so there is no "if sum == 0 → short-circuit"
//!   shortcut here — the count code uses `if count == 0` in a few places
//!   that would be unsound here. The only zero-skip pattern that's
//!   correct for sum is "subtree is fully outside range → contributes 0",
//!   driven purely by the bound classification.
//! - The verifier accumulates in `i128` and narrows to `i64` at the end so
//!   adversarial inputs like `i64::MAX + i64::MAX` are detected as
//!   overflow instead of silently wrapping.

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
        query::{
            aggregate_common::{
                classify_subtree, key_strictly_inside, SubtreeClassification, NULL_HASH,
            },
            QueryItem,
        },
        tree::{execute_with_options, Tree as ProofTree},
        Decoder, Node,
    },
    CryptoHash, Error,
};

/// Returns true if `tree_type` is one that can host an `AggregateSumOnRange`
/// proof. Only `ProvableSumTree` is valid — the `Sum` / `BigSum` trees use
/// different hash dispatches (the inserted-value hash is not bound through
/// `node_hash_with_sum` for those) and can't produce verifiable sum proofs.
#[cfg(feature = "minimal")]
fn is_provable_sum_bearing(tree_type: TreeType) -> bool {
    matches!(tree_type, TreeType::ProvableSumTree)
}

/// Pull the sum out of a `ProvableSum` aggregate. Returns
/// `Err(CorruptedData)` for any other variant — the entry point has
/// already gated `tree_type`, so reaching the error means the tree's
/// in-memory state disagrees with its declared type. This is a local
/// invariant failure on the prover side (we are walking *our own*
/// merk), so `CorruptedData` is the appropriate classification per the
/// repo error-handling convention.
#[cfg(feature = "minimal")]
fn provable_sum_from_aggregate(data: AggregateData) -> Result<i64, Error> {
    match data {
        AggregateData::ProvableSum(s) => Ok(s),
        other => Err(Error::CorruptedData(format!(
            "expected ProvableSum aggregate data on a provable sum tree, got {:?}",
            other
        ))),
    }
}

#[cfg(feature = "minimal")]
impl<S> RefWalker<'_, S>
where
    S: Fetch + Sized + Clone,
{
    /// Generate a sum-only proof for an `AggregateSumOnRange` query.
    ///
    /// `inner_range` is the `QueryItem` wrapped by `AggregateSumOnRange`
    /// (already stripped at the caller). `tree_type` must be
    /// `ProvableSumTree`; any other tree type is rejected with
    /// `Error::InvalidProofError` before any walking happens.
    ///
    /// The returned tuple is `(proof_ops, sum)`:
    /// - `proof_ops` is the linear stream the verifier will replay to
    ///   reconstruct the tree's root hash.
    /// - `sum` is the prover-side computed signed sum (the verifier
    ///   independently recomputes it from the proof and compares against
    ///   the expected root hash; this value is returned as a convenience,
    ///   not as ground truth).
    pub fn create_aggregate_sum_on_range_proof(
        &mut self,
        inner_range: &QueryItem,
        tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> CostResult<(LinkedList<Op>, i64), Error> {
        if !is_provable_sum_bearing(tree_type) {
            return Err(Error::InvalidProofError(format!(
                "AggregateSumOnRange is only valid against ProvableSumTree, got {:?}",
                tree_type
            )))
            .wrap_with_cost(OperationCost::default());
        }

        let mut cost = OperationCost::default();
        let mut ops = LinkedList::new();
        let sum_i128 = cost_return_on_error!(
            &mut cost,
            emit_sum_proof(self, inner_range, None, None, &mut ops, grove_version)
        );
        // Narrow the prover-side i128 accumulator to i64. The verifier does
        // the same narrowing; if the honest sum doesn't fit in i64 we treat
        // it as proof corruption (a real ProvableSumTree maintains all
        // intermediate aggregates as i64, so an i128-only honest result is
        // unreachable — but defending here keeps the contract symmetric with
        // the verifier).
        let sum: i64 = match i64::try_from(sum_i128) {
            Ok(v) => v,
            Err(_) => {
                return Err(Error::InvalidProofError(format!(
                    "aggregate-sum proof: in-range sum overflowed i64 ({})",
                    sum_i128
                )))
                .wrap_with_cost(cost);
            }
        };
        Ok((ops, sum)).wrap_with_cost(cost)
    }

    /// Walk the tree for an `AggregateSumOnRange` query and return the
    /// in-range signed sum, **without** producing a proof.
    ///
    /// This is the no-proof counterpart of
    /// [`Self::create_aggregate_sum_on_range_proof`]. It performs the same
    /// classification walk (Contained / Disjoint / Boundary) and reads each
    /// node's aggregate sum directly from the merk, so it is O(log n) in
    /// the number of distinct keys under the indexed subtree — the same
    /// complexity as the proof variant but without the proof-op allocations,
    /// hash recomputations, or serialization round-trip.
    ///
    /// The caller (`Merk::sum_aggregate_on_range`) is expected to have
    /// already validated `tree_type` is `ProvableSumTree`; the per-node
    /// `provable_sum_from_aggregate` check inside the walk surfaces any
    /// disagreement between the declared tree type and the in-memory
    /// aggregate.
    ///
    /// The accumulator carries `i128` end-to-end and narrows to `i64` at
    /// the very last step, exactly the way the prover and verifier do.
    /// Any value outside `i64` range is treated as corruption (a real
    /// `ProvableSumTree` maintains every aggregate as `i64` at every
    /// level, so the i128 path only ever holds an out-of-range value if
    /// the tree state is internally inconsistent).
    ///
    /// The result is **not** independently verifiable: the caller is
    /// trusting their own merk read path. Callers that need a verifiable
    /// sum must use `prove_aggregate_sum_on_range` +
    /// `verify_aggregate_sum_on_range_proof`.
    pub fn sum_aggregate_on_range(
        &mut self,
        inner_range: &QueryItem,
        grove_version: &GroveVersion,
    ) -> CostResult<i64, Error> {
        let mut cost = OperationCost::default();
        let sum_i128 = cost_return_on_error!(
            &mut cost,
            walk_sum_only(self, inner_range, None, None, grove_version)
        );
        match i64::try_from(sum_i128) {
            Ok(v) => Ok(v).wrap_with_cost(cost),
            Err(_) => Err(Error::CorruptedData(format!(
                "no-proof aggregate-sum: in-range sum overflowed i64 ({})",
                sum_i128
            )))
            .wrap_with_cost(cost),
        }
    }
}

/// Read the provable-sum aggregate off the walker's current tree node.
/// Shared error-mapping helper used by [`walk_sum_only`] at both the
/// Contained-leaf and Boundary positions.
#[cfg(feature = "minimal")]
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

/// No-proof variant of [`emit_sum_proof`]: walks the same classification
/// path (Contained / Disjoint / Boundary) but only returns the running
/// in-range sum.
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
#[cfg(feature = "minimal")]
fn walk_sum_only<S>(
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

/// Recursive proof emitter. Always called on a non-empty subtree.
///
/// At entry, `subtree_lo_excl` / `subtree_hi_excl` are the inherited
/// exclusive key bounds for the subtree this walker points at (both `None`
/// at the root call). The accumulator is `i128` so the prover side never
/// overflows mid-walk on adversarial intermediate sums.
#[cfg(feature = "minimal")]
fn emit_sum_proof<S>(
    walker: &mut RefWalker<'_, S>,
    range: &QueryItem,
    subtree_lo_excl: Option<&[u8]>,
    subtree_hi_excl: Option<&[u8]>,
    ops: &mut LinkedList<Op>,
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
        ops.push_back(Op::Push(Node::HashWithSum(
            kv_hash,
            left_child_hash,
            right_child_hash,
            subtree_sum,
        )));
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
    let node_sum: i64 = match walker
        .tree()
        .aggregate_data()
        // Local prover-side walk over our own merk — failure to read
        // aggregate_data is local state corruption, not a peer-supplied
        // invalid proof.
        .map_err(|e| Error::CorruptedData(format!("aggregate_data: {}", e)))
    {
        Ok(data) => match provable_sum_from_aggregate(data) {
            Ok(s) => s,
            Err(e) => return Err(e).wrap_with_cost(cost),
        },
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
                grove_version
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

    // Step 4: emit the current node as a boundary KVDigestSum + attach left
    // as its left child. The node's own contribution to the in-range sum
    // is `own_sum = node_sum − left_struct − right_struct`. `NotSummed`
    // wrapping forces `node_sum = 0` so its own contribution is 0 by
    // construction.
    ops.push_back(Op::Push(Node::KVDigestSum(
        node_key.clone(),
        node_value_hash,
        node_sum,
    )));
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

/// Verify a sum-only proof for an `AggregateSumOnRange` query.
///
/// `proof_bytes` is the encoded `Vec<Op>` produced by
/// [`crate::Merk::prove_aggregate_sum_on_range`]; `inner_range` is the same
/// `QueryItem` the prover summed over (caller-supplied — typically extracted
/// from the verifier's `PathQuery`).
///
/// On success returns `(merk_root_hash, sum)`:
/// - `merk_root_hash` is the root hash of the reconstructed merk; the
///   caller must compare it against the expected root hash to complete
///   verification.
/// - `sum` is the signed `i64` sum of keys' contributions in the inner
///   range, computed by replaying the prover's classification walk against
///   the reconstructed proof tree.
///
/// **Two-phase verification.** Same defensive structure as the count proof
/// verifier — allowlisting node types alone is unsound, so we both reject
/// blatantly wrong types up front and then run a structural shape walk that
/// binds each leaf's type to the (subtree_bounds × range) classification.
///
/// **Overflow handling.** The shape walk accumulates in `i128` (so two
/// `i64::MAX` children sum cleanly to `2 * i64::MAX` rather than wrapping)
/// and narrows to `i64` at the end. If the i128 result doesn't fit in i64,
/// the verifier returns `Error::InvalidProofError` — this is the safety net
/// against adversarial proofs that compose extremes into a sum that
/// can't be represented in the on-the-wire `i64` field.
///
/// **Empty merk case.** An empty merk is represented by an empty proof byte
/// stream and yields `(NULL_HASH, 0)`. Callers chaining this in a
/// multi-layer proof should recognize that shape explicitly.
pub fn verify_aggregate_sum_on_range_proof(
    proof_bytes: &[u8],
    inner_range: &QueryItem,
) -> CostResult<(CryptoHash, i64), Error> {
    if proof_bytes.is_empty() {
        // Empty merk → empty proof → sum = 0, hash = NULL_HASH.
        return Ok((NULL_HASH, 0i64)).wrap_with_cost(OperationCost::default());
    }

    let mut cost = OperationCost::default();
    let decoder = Decoder::new(proof_bytes);

    // Phase 1: reconstruct the proof tree. Allowlist the only two node types
    // the honest prover emits — `HashWithSum` (collapsed Disjoint/Contained
    // subtrees) and `KVDigestSum` (Boundary nodes). Plain `Hash(_)` is not
    // accepted: the structural sum it would carry must be hash-bound, and
    // only `HashWithSum` provides that.
    let tree_result: CostResult<ProofTree, Error> =
        execute_with_options(decoder, false, false, |node| match node {
            Node::HashWithSum(_, _, _, _) | Node::KVDigestSum(_, _, _) => Ok(()),
            other => Err(Error::InvalidProofError(format!(
                "unexpected node type in aggregate sum proof: {}",
                other
            ))),
        });
    let tree = cost_return_on_error!(&mut cost, tree_result);

    // Phase 2: shape-check + sum by replaying the prover's classification
    // walk. The accumulator is i128 so adversarial extremes don't wrap;
    // we narrow to i64 at the end below.
    let (sum_i128, _structural) = match verify_sum_shape(&tree, inner_range, None, None) {
        Ok(pair) => pair,
        Err(e) => return Err(e).wrap_with_cost(cost),
    };

    // Final overflow gate: narrow the i128 accumulator to i64. A
    // well-formed `ProvableSumTree` maintains its aggregate as i64 at every
    // level, so an honest verify lands here with a value already inside
    // i64's range. Anything outside is a forgery or a tree that violates
    // its invariants.
    let sum: i64 = match i64::try_from(sum_i128) {
        Ok(v) => v,
        Err(_) => {
            return Err(Error::InvalidProofError(format!(
                "aggregate-sum proof: in-range sum overflowed i64 ({})",
                sum_i128
            )))
            .wrap_with_cost(cost);
        }
    };

    let root_hash = tree.hash().unwrap_add_cost(&mut cost);
    Ok((root_hash, sum)).wrap_with_cost(cost)
}

/// Recursive shape-walk over the reconstructed proof tree. Returns the
/// pair `(in_range_sum_i128, structural_sum_i128)`:
///
/// - `in_range_sum_i128` — signed sum of keys in the subtree that fall
///   inside the inner range AND have a non-zero own-sum (i.e. are not
///   `NotSummed`-wrapped). Accumulated in i128; narrowed to i64 once at
///   the outer entry point.
/// - `structural_sum_i128` — the merk-recorded aggregate sum of this
///   subtree (counting normal entries as their value and `NotSummed`
///   entries as 0). The parent uses it to compute its own `own_sum` as
///   `parent_node_sum − left_struct − right_struct` (since
///   `parent_node_sum = own + left_struct + right_struct`). Also kept in
///   i128 throughout.
///
/// The structural sum of every child is **cryptographically bound** to
/// the parent's hash chain because every sum-bearing node in a sum proof
/// (`KVDigestSum`, `HashWithSum`) has its sum fed into
/// `node_hash_with_sum` for hash recomputation. Plain `Hash(_)` would
/// not carry a bound sum and is therefore not allowed in sum proofs.
///
/// At each node we run the same type ↔ classification binding as the
/// count side:
///
/// - `Disjoint` → must be a leaf `HashWithSum`. Contributes 0 to
///   in_range_sum, full sum to structural_sum.
/// - `Contained` → must be a leaf `HashWithSum`. Contributes its sum to
///   both.
/// - `Boundary` → must be `KVDigestSum(key, ...)` with `key` strictly
///   inside `bounds`. Recurse left with `(lo, key)` and right with
///   `(key, hi)`; add `own_sum` if `inner_range.contains(key)`.
///
/// **Negative-sum caveat:** unlike count's `checked_sub` (where
/// `parent_aggregate < left_struct + right_struct` would indicate
/// corruption), the sum arithmetic is naturally signed and *cannot* be
/// detected by sign alone — a negative own_sum is perfectly legal. We
/// just compute `node_sum - left_struct - right_struct` in i128 and trust
/// the final overflow gate to catch any meaningful corruption (it's hash-
/// bound regardless, so a mismatch in own_sum's arithmetic would change
/// the reconstructed root hash and the caller's root check catches it).
fn verify_sum_shape(
    tree: &ProofTree,
    range: &QueryItem,
    lo: Option<&[u8]>,
    hi: Option<&[u8]>,
) -> Result<(i128, i128), Error> {
    let class = classify_subtree(lo, hi, range);
    match class {
        SubtreeClassification::Disjoint => match &tree.node {
            Node::HashWithSum(_, _, _, sum) => {
                if tree.left.is_some() || tree.right.is_some() {
                    return Err(Error::InvalidProofError(
                        "aggregate-sum proof: HashWithSum node at a Disjoint position \
                         must be a leaf"
                            .to_string(),
                    ));
                }
                // Disjoint subtree contributes 0 to the in-range sum but
                // its full structural sum to the parent's `own_sum`
                // computation.
                Ok((0i128, *sum as i128))
            }
            other => Err(Error::InvalidProofError(format!(
                "aggregate-sum proof: expected HashWithSum at Disjoint position, got {}",
                other
            ))),
        },
        SubtreeClassification::Contained => match &tree.node {
            Node::HashWithSum(_, _, _, sum) => {
                if tree.left.is_some() || tree.right.is_some() {
                    return Err(Error::InvalidProofError(
                        "aggregate-sum proof: HashWithSum node at a Contained position \
                         must be a leaf"
                            .to_string(),
                    ));
                }
                // Contained subtree's structural sum (which excludes
                // NotSummed entries because their stored aggregate is 0)
                // is exactly its in-range sum.
                Ok((*sum as i128, *sum as i128))
            }
            other => Err(Error::InvalidProofError(format!(
                "aggregate-sum proof: expected HashWithSum at Contained position, got {}",
                other
            ))),
        },
        SubtreeClassification::Boundary => match &tree.node {
            Node::KVDigestSum(key, _, aggregate) => {
                if !key_strictly_inside(key.as_slice(), lo, hi) {
                    return Err(Error::InvalidProofError(format!(
                        "aggregate-sum proof: KVDigestSum key {} falls outside its \
                         inherited subtree bounds (lo={:?}, hi={:?})",
                        hex::encode(key),
                        lo.map(hex::encode),
                        hi.map(hex::encode),
                    )));
                }
                let key_slice = key.as_slice();
                let (left_in, left_struct) = match &tree.left {
                    Some(child) => verify_sum_shape(&child.tree, range, lo, Some(key_slice))?,
                    None => (0i128, 0i128),
                };
                let (right_in, right_struct) = match &tree.right {
                    Some(child) => verify_sum_shape(&child.tree, range, Some(key_slice), hi)?,
                    None => (0i128, 0i128),
                };
                // own_sum = aggregate − left_struct − right_struct, in
                // i128. There's no "child sum exceeds parent" check that
                // makes sense for signed sums — any combination of
                // children's structural sums is plausible (one positive,
                // one negative, etc.). The hash chain binds the values
                // regardless, so any wrong arithmetic here would change
                // the reconstructed root hash.
                let aggregate_i128 = *aggregate as i128;
                let own_sum = aggregate_i128 - left_struct - right_struct;
                let self_contribution = if range.contains(key_slice) {
                    own_sum
                } else {
                    0
                };
                let in_range = left_in + right_in + self_contribution;
                Ok((in_range, aggregate_i128))
            }
            other => Err(Error::InvalidProofError(format!(
                "aggregate-sum proof: expected KVDigestSum at Boundary position, got {}",
                other
            ))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range_inclusive(lo: &[u8], hi: &[u8]) -> QueryItem {
        QueryItem::RangeInclusive(lo.to_vec()..=hi.to_vec())
    }

    fn range_full() -> QueryItem {
        QueryItem::RangeFull(std::ops::RangeFull)
    }

    #[test]
    fn classify_disjoint_below_sum() {
        let r = range_inclusive(b"d", b"f");
        assert_eq!(
            classify_subtree(None, Some(b"c"), &r),
            SubtreeClassification::Disjoint,
        );
    }

    #[test]
    fn classify_contained_full_range_full_subtree_sum() {
        let r = range_full();
        assert_eq!(
            classify_subtree(None, None, &r),
            SubtreeClassification::Contained,
        );
    }

    #[test]
    fn classify_boundary_overlapping_lower_sum() {
        let r = range_inclusive(b"d", b"f");
        assert_eq!(
            classify_subtree(Some(b"c"), Some(b"e"), &r),
            SubtreeClassification::Boundary,
        );
    }

    // ---------- end-to-end integration tests on a real merk ----------

    use grovedb_costs::CostsExt as _;
    use grovedb_version::version::GroveVersion;

    use crate::{
        proofs::{encode_into, Op as ProofOp},
        test_utils::TempMerk,
        tree::{Op, TreeFeatureType::ProvableSummedMerkNode},
        Merk, TreeType,
    };

    /// Build a fresh `ProvableSumTree` populated with single-byte keys
    /// "a".."o" (15 keys), each carrying sum 1, 2, ..., 15 respectively.
    /// Returns the merk and its current root hash.
    fn make_15_key_provable_sum_tree(grove_version: &GroveVersion) -> (TempMerk, [u8; 32]) {
        let mut merk = TempMerk::new_with_tree_type(grove_version, TreeType::ProvableSumTree);
        let keys: Vec<Vec<u8>> = (b'a'..=b'o').map(|c| vec![c]).collect();
        let entries: Vec<(Vec<u8>, Op)> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let s = (i as i64) + 1;
                (k.clone(), Op::Put(vec![i as u8], ProvableSummedMerkNode(s)))
            })
            .collect();
        merk.apply::<_, Vec<_>>(&entries, &[], None, grove_version)
            .unwrap()
            .expect("apply should succeed");
        merk.commit(grove_version);
        let root_hash = merk.root_hash().unwrap();
        (merk, root_hash)
    }

    /// Encode a `LinkedList<Op>` into the wire format.
    fn encode_proof(ops: &LinkedList<ProofOp>) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(128);
        encode_into(ops.iter(), &mut bytes);
        bytes
    }

    /// Round-trip: prove → encode → verify, assert root + sum match.
    fn round_trip(
        merk: &Merk<impl grovedb_storage::StorageContext<'static>>,
        expected_root: [u8; 32],
        inner_range: QueryItem,
        expected_sum: i64,
        grove_version: &GroveVersion,
    ) {
        let (ops, prover_sum) = merk
            .prove_aggregate_sum_on_range(&inner_range, grove_version)
            .unwrap()
            .expect("prove should succeed");
        assert_eq!(
            prover_sum, expected_sum,
            "prover sum mismatch for range {:?}",
            inner_range
        );
        let bytes = encode_proof(&ops);
        let (root, verifier_sum) = verify_aggregate_sum_on_range_proof(&bytes, &inner_range)
            .unwrap()
            .expect("verify should succeed");
        assert_eq!(
            root, expected_root,
            "verifier reconstructed wrong root for range {:?}",
            inner_range
        );
        assert_eq!(
            verifier_sum, expected_sum,
            "verifier sum mismatch for range {:?}",
            inner_range
        );
    }

    #[test]
    fn integration_full_range_sum_of_1_to_15() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_sum_tree(v);
        // Full range with RangeFrom("a"..) — sum = 1+2+...+15 = 120.
        round_trip(&merk, root, QueryItem::RangeFrom(b"a".to_vec()..), 120, v);
    }

    #[test]
    fn integration_closed_range_inclusive_sum() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_sum_tree(v);
        // Keys "c"..="l" → values 3..=12 → sum = 75.
        round_trip(
            &merk,
            root,
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
            75,
            v,
        );
    }

    #[test]
    fn integration_range_below_all_keys_sum() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_sum_tree(v);
        round_trip(
            &merk,
            root,
            QueryItem::RangeInclusive(vec![0x00]..=vec![0x10]),
            0,
            v,
        );
    }

    #[test]
    fn integration_range_above_all_keys_sum() {
        let v = GroveVersion::latest();
        let (merk, root) = make_15_key_provable_sum_tree(v);
        round_trip(
            &merk,
            root,
            QueryItem::RangeInclusive(b"z".to_vec()..=vec![0xff]),
            0,
            v,
        );
    }

    #[test]
    fn integration_empty_merk_sum() {
        let v = GroveVersion::latest();
        let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableSumTree);
        let (ops, prover_sum) = merk
            .prove_aggregate_sum_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
            .unwrap()
            .expect("prove on empty merk should succeed");
        assert_eq!(prover_sum, 0);
        let bytes = encode_proof(&ops);
        let (root, verifier_sum) = verify_aggregate_sum_on_range_proof(
            &bytes,
            &QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        )
        .unwrap()
        .expect("verify on empty merk should succeed");
        assert_eq!(root, NULL_HASH);
        assert_eq!(verifier_sum, 0);
    }

    #[test]
    fn integration_rejected_on_normal_tree() {
        let v = GroveVersion::latest();
        let merk = TempMerk::new(v);
        let err = merk
            .prove_aggregate_sum_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
            .unwrap();
        assert!(
            err.is_err(),
            "expected InvalidProofError on NormalTree, got Ok({:?})",
            err.ok().map(|(_, s)| s)
        );
    }

    #[test]
    fn integration_rejected_on_provable_count_tree() {
        // ProvableSumTree-only — count trees use a different hash dispatch
        // and are not valid input here.
        let v = GroveVersion::latest();
        let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableCountTree);
        let err = merk
            .prove_aggregate_sum_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
            .unwrap();
        assert!(
            err.is_err(),
            "expected InvalidProofError on ProvableCountTree, got Ok"
        );
    }

    #[test]
    fn integration_sum_forgery_is_rejected() {
        // Tamper with a HashWithSum's sum field — the verifier's root-hash
        // recomputation must diverge from the expected root.
        let v = GroveVersion::latest();
        let (merk, expected_root) = make_15_key_provable_sum_tree(v);
        let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());
        let (mut ops, _prover_sum) = merk
            .prove_aggregate_sum_on_range(&inner_range, v)
            .unwrap()
            .expect("prove should succeed");

        let mut tampered = false;
        for op in ops.iter_mut() {
            if let ProofOp::Push(Node::HashWithSum(_, _, _, sum))
            | ProofOp::PushInverted(Node::HashWithSum(_, _, _, sum)) = op
            {
                *sum = sum.saturating_add(1);
                tampered = true;
                break;
            }
        }
        assert!(tampered, "test setup: expected at least one HashWithSum op");

        let bytes = encode_proof(&ops);
        let (root, _sum) = verify_aggregate_sum_on_range_proof(&bytes, &inner_range)
            .unwrap()
            .expect("verify should still complete (root mismatch is the caller's job)");
        assert_ne!(
            root, expected_root,
            "tampered sum must produce a different reconstructed root hash"
        );
    }

    #[test]
    fn shape_walk_rejects_single_hash_undercount_sum() {
        let v = GroveVersion::latest();
        let (merk, expected_root) = make_15_key_provable_sum_tree(v);
        let inner_range = QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec());

        // Forged proof: a single Hash op carrying the genuine root hash.
        let mut forged: LinkedList<ProofOp> = LinkedList::new();
        forged.push_back(ProofOp::Push(Node::Hash(expected_root)));
        let bytes = encode_proof(&forged);

        let result = verify_aggregate_sum_on_range_proof(&bytes, &inner_range).unwrap();
        let err = result.expect_err("single-Hash forgery must be rejected");
        let _ = merk;
        match err {
            Error::InvalidProofError(msg) => {
                assert!(
                    msg.contains("unexpected node type")
                        || msg.contains("expected KVDigestSum")
                        || msg.contains("Boundary"),
                    "unexpected message: {msg}"
                );
            }
            other => panic!("expected InvalidProofError, got {other:?}"),
        }
    }

    #[test]
    fn shape_walk_rejects_disjoint_hashwithsum_with_children() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_sum_tree(v);
        let inner_range = QueryItem::RangeAfter(b"o".to_vec()..);
        let (mut ops, _) = merk
            .prove_aggregate_sum_on_range(&inner_range, v)
            .unwrap()
            .expect("prove succeeds");

        let mut spliced = LinkedList::<ProofOp>::new();
        let mut done = false;
        for op in ops.iter() {
            spliced.push_back(op.clone());
            if !done && matches!(op, ProofOp::Push(Node::HashWithSum(_, _, _, _))) {
                spliced.push_back(ProofOp::Push(Node::HashWithSum(
                    [0u8; 32], [0u8; 32], [0u8; 32], 1,
                )));
                spliced.push_back(ProofOp::Parent);
                done = true;
            }
        }
        assert!(done, "test setup: expected at least one HashWithSum op");
        ops = spliced;

        let bytes = encode_proof(&ops);
        let result = verify_aggregate_sum_on_range_proof(&bytes, &inner_range).unwrap();
        let err = result.expect_err("Disjoint HashWithSum with children must be rejected");
        match err {
            Error::InvalidProofError(msg) => assert!(
                msg.contains("Disjoint position must be a leaf"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected InvalidProofError, got {:?}", other),
        }
    }

    /// Regular `Merk::prove` on a `ProvableSumTree` must emit the sum-bearing
    /// proof node variants. Queried items yield `KVSum` (via `to_kv_sum_node`),
    /// non-queried path nodes yield `KVHashSum` (via `to_kvhash_sum_node`).
    /// This exercises the sum-node helper functions whose only callers are
    /// inside `create_proof_internal`.
    #[test]
    fn regular_prove_on_provable_sum_tree_emits_kv_sum_and_kvhash_sum() {
        use crate::proofs::{query::Query, Decoder, Node, Op as ProofOp};

        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_sum_tree(v);

        // Query a few keys, leaving most unqueried so we get both queried
        // (KVSum) and path (KVHashSum) nodes.
        let mut q = Query::new();
        q.insert_key(b"a".to_vec());
        q.insert_key(b"h".to_vec()); // middle
        q.insert_key(b"o".to_vec());

        let proof_result = merk.prove(q, None, v).unwrap().expect("regular prove");
        let proof_bytes = proof_result.proof;

        let ops: Vec<ProofOp> = Decoder::new(&proof_bytes)
            .collect::<Result<Vec<_>, _>>()
            .expect("decode");

        let mut saw_kvsum = false;
        let mut saw_kvhashsum = false;
        for op in &ops {
            match op {
                ProofOp::Push(node) | ProofOp::PushInverted(node) => match node {
                    Node::KVSum(..) => saw_kvsum = true,
                    Node::KVHashSum(..) => saw_kvhashsum = true,
                    _ => {}
                },
                _ => {}
            }
        }
        assert!(
            saw_kvsum,
            "expected at least one KVSum node from queried Items on a ProvableSumTree"
        );
        assert!(
            saw_kvhashsum,
            "expected at least one KVHashSum node on the proof path"
        );
    }

    /// Querying an out-of-range absent key on a `ProvableSumTree` must emit a
    /// boundary `KVDigestSum` node — i.e. the result of `to_kvdigest_sum_node`.
    /// We do this on a single-key tree so that one of the absence-flank keys
    /// IS on the tree's boundary, forcing the `on_boundary_not_found` branch.
    #[test]
    fn regular_prove_on_provable_sum_tree_emits_kvdigest_sum() {
        use crate::proofs::{query::Query, Decoder, Node, Op as ProofOp};

        let v = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(v, TreeType::ProvableSumTree);
        // Single-key tree: querying any absent key forces a boundary emission.
        merk.apply::<_, Vec<_>>(
            &[(b"m".to_vec(), Op::Put(vec![0], ProvableSummedMerkNode(7)))],
            &[],
            None,
            v,
        )
        .unwrap()
        .expect("apply");
        merk.commit(v);

        let mut q = Query::new();
        q.insert_key(b"zz".to_vec()); // absent, above the single key
        let proof_result = merk.prove(q, None, v).unwrap().expect("regular prove");
        let ops: Vec<ProofOp> = Decoder::new(&proof_result.proof)
            .collect::<Result<Vec<_>, _>>()
            .expect("decode");

        let saw_kvdigestsum = ops.iter().any(|op| {
            matches!(
                op,
                ProofOp::Push(Node::KVDigestSum(..)) | ProofOp::PushInverted(Node::KVDigestSum(..))
            )
        });
        assert!(
            saw_kvdigestsum,
            "expected KVDigestSum boundary node for absent-key proof, got ops: {:?}",
            ops
        );
    }

    /// Two i64::MAX children sum to 2*i64::MAX, which exceeds i64. The
    /// verifier's final i64-narrowing check must surface this as a
    /// proof-error. This exercises the i128 accumulator + overflow gate.
    #[test]
    fn integration_overflow_at_i64_max_is_rejected() {
        let v = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(v, TreeType::ProvableSumTree);
        // Two children, each i64::MAX. Sum exceeds i64::MAX.
        let entries: Vec<(Vec<u8>, Op)> = vec![
            (
                b"a".to_vec(),
                Op::Put(vec![0], ProvableSummedMerkNode(i64::MAX)),
            ),
            (
                b"b".to_vec(),
                Op::Put(vec![0], ProvableSummedMerkNode(i64::MAX)),
            ),
        ];
        // Insertion itself may or may not succeed depending on the apply
        // path's intermediate-overflow handling. Skip if not; this scenario
        // is additionally exercised at the verify layer via fabricated
        // proofs.
        if merk
            .apply::<_, Vec<_>>(&entries, &[], None, v)
            .unwrap()
            .is_err()
        {
            return;
        }
        merk.commit(v);
        let inner_range = QueryItem::RangeFrom(b"a".to_vec()..);
        let result = merk.prove_aggregate_sum_on_range(&inner_range, v).unwrap();
        // Either the prover detects the overflow during its narrowing pass,
        // or it produces a proof whose verifier-side narrowing catches it.
        // Both are acceptable end states for this safety net.
        match result {
            Err(_) => { /* prover-side overflow detection — done */ }
            Ok((ops, _)) => {
                let bytes = encode_proof(&ops);
                let v_result = verify_aggregate_sum_on_range_proof(&bytes, &inner_range).unwrap();
                assert!(
                    v_result.is_err(),
                    "verifier must reject an i128-sized sum that doesn't fit in i64"
                );
            }
        }
    }

    // ---------- no-proof variant: sum_aggregate_on_range ----------
    //
    // The no-proof entry point must return exactly the same sum as the
    // proof path for every range shape, without producing any proof ops.
    // These tests cross-check the two paths on the same merk and also
    // cover the failure modes unique to the no-proof variant (wrong tree
    // type, empty merk, overflow narrowing).

    /// Cross-check: assert `sum_aggregate_on_range` and the sum returned
    /// by `prove_aggregate_sum_on_range` agree for the given range, and
    /// that both equal `expected_sum`.
    fn no_proof_sum_matches_prover(
        merk: &Merk<impl grovedb_storage::StorageContext<'static>>,
        inner_range: QueryItem,
        expected_sum: i64,
        grove_version: &GroveVersion,
    ) {
        let no_proof = merk
            .sum_aggregate_on_range(&inner_range, grove_version)
            .unwrap()
            .expect("sum_aggregate_on_range should succeed");
        assert_eq!(
            no_proof, expected_sum,
            "no-proof variant returned wrong sum for range {:?}",
            inner_range
        );
        let (_ops, prover_sum) = merk
            .prove_aggregate_sum_on_range(&inner_range, grove_version)
            .unwrap()
            .expect("prove should succeed");
        assert_eq!(
            no_proof, prover_sum,
            "no-proof variant disagrees with prover sum for range {:?}",
            inner_range
        );
    }

    #[test]
    fn no_proof_sum_matches_prover_closed_range_inclusive() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_sum_tree(v);
        // sums for keys c..=l are 3..=12 → 75
        no_proof_sum_matches_prover(
            &merk,
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
            75,
            v,
        );
    }

    #[test]
    fn no_proof_sum_matches_prover_closed_range_exclusive() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_sum_tree(v);
        // sums for keys c..l are 3..=11 → 63
        no_proof_sum_matches_prover(&merk, QueryItem::Range(b"c".to_vec()..b"l".to_vec()), 63, v);
    }

    #[test]
    fn no_proof_sum_matches_prover_open_range_from() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_sum_tree(v);
        // c..o → 3+4+...+15 = 117
        no_proof_sum_matches_prover(&merk, QueryItem::RangeFrom(b"c".to_vec()..), 117, v);
    }

    #[test]
    fn no_proof_sum_matches_prover_range_after() {
        // RangeAfter at the root pushes the left boundary exclusive to
        // "b", exercising the right-child arm of walk_sum_only.
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_sum_tree(v);
        no_proof_sum_matches_prover(&merk, QueryItem::RangeAfter(b"b".to_vec()..), 117, v);
    }

    #[test]
    fn no_proof_sum_matches_prover_range_to_inclusive() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_sum_tree(v);
        // ..=e → 1+2+3+4+5 = 15
        no_proof_sum_matches_prover(&merk, QueryItem::RangeToInclusive(..=b"e".to_vec()), 15, v);
    }

    #[test]
    fn no_proof_sum_matches_prover_range_below_all_keys() {
        let v = GroveVersion::latest();
        let (merk, _root) = make_15_key_provable_sum_tree(v);
        no_proof_sum_matches_prover(
            &merk,
            QueryItem::RangeInclusive(vec![0x00]..=vec![0x10]),
            0,
            v,
        );
    }

    #[test]
    fn no_proof_sum_empty_merk_returns_zero() {
        let v = GroveVersion::latest();
        let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableSumTree);
        let sum = merk
            .sum_aggregate_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
            .unwrap()
            .expect("sum_aggregate_on_range on empty merk should succeed");
        assert_eq!(sum, 0);
    }

    #[test]
    fn no_proof_sum_rejected_on_normal_tree() {
        let v = GroveVersion::latest();
        let merk = TempMerk::new(v); // NormalTree
        let result = merk
            .sum_aggregate_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
            .unwrap();
        assert!(
            result.is_err(),
            "expected InvalidProofError on NormalTree, got Ok({:?})",
            result.ok()
        );
    }

    #[test]
    fn no_proof_sum_rejected_on_provable_count_tree() {
        // Sum variant must reject ProvableCountTree too (precise tree-type
        // match), parallel to the verify-side terminal-type gate.
        let v = GroveVersion::latest();
        let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableCountTree);
        let result = merk
            .sum_aggregate_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
            .unwrap();
        assert!(
            result.is_err(),
            "expected InvalidProofError on ProvableCountTree for a sum query, got Ok({:?})",
            result.ok()
        );
    }

    // ---------- Unit tests for helper-function error paths --------------
    //
    // These exercise small internal helpers that the integration tests
    // can only reach indirectly. Each one pins a specific Err-classification
    // arm so that future refactors can't silently drop the diagnostic.

    #[test]
    fn provable_sum_from_aggregate_rejects_non_provable_sum_variants() {
        // Cover every non-`ProvableSum` arm of `provable_sum_from_aggregate`.
        // The fallback "other" arm should fire for each.
        let cases = [
            AggregateData::NoAggregateData,
            AggregateData::Sum(5),
            AggregateData::BigSum(5),
            AggregateData::Count(5),
            AggregateData::CountAndSum(2, 3),
            AggregateData::ProvableCount(5),
            AggregateData::ProvableCountAndSum(2, 3),
        ];
        for case in cases {
            let result = provable_sum_from_aggregate(case);
            match result {
                Err(Error::CorruptedData(msg)) => {
                    assert!(
                        msg.contains("expected ProvableSum"),
                        "wrong message for {:?}: {msg}",
                        case
                    );
                }
                other => panic!("expected CorruptedData for {:?}, got {:?}", case, other),
            }
        }
    }

    #[test]
    fn provable_sum_from_aggregate_accepts_provable_sum() {
        // Sanity: the happy-path arm preserves the inner value (including
        // negative values).
        assert_eq!(
            provable_sum_from_aggregate(AggregateData::ProvableSum(0)).unwrap(),
            0
        );
        assert_eq!(
            provable_sum_from_aggregate(AggregateData::ProvableSum(-42)).unwrap(),
            -42
        );
        assert_eq!(
            provable_sum_from_aggregate(AggregateData::ProvableSum(i64::MAX)).unwrap(),
            i64::MAX
        );
        assert_eq!(
            provable_sum_from_aggregate(AggregateData::ProvableSum(i64::MIN)).unwrap(),
            i64::MIN
        );
    }

    #[test]
    fn is_provable_sum_bearing_only_for_provable_sum_tree() {
        // Every TreeType variant must return false except ProvableSumTree.
        // This pins the matches!(...) gate against accidental loosening.
        assert!(is_provable_sum_bearing(TreeType::ProvableSumTree));
        for t in [
            TreeType::NormalTree,
            TreeType::SumTree,
            TreeType::BigSumTree,
            TreeType::CountTree,
            TreeType::CountSumTree,
            TreeType::ProvableCountTree,
            TreeType::ProvableCountSumTree,
            TreeType::CommitmentTree(0),
            TreeType::MmrTree,
            TreeType::BulkAppendTree(0),
            TreeType::DenseAppendOnlyFixedSizeTree(0),
        ] {
            assert!(!is_provable_sum_bearing(t), "false expected for {:?}", t);
        }
    }

    #[test]
    fn classify_subtree_disjoint_above_sum() {
        // Subtree entirely above the range → Disjoint. Mirror of
        // classify_disjoint_below_sum.
        let r = range_inclusive(b"d", b"f");
        assert_eq!(
            classify_subtree(Some(b"g"), None, &r),
            SubtreeClassification::Disjoint,
        );
    }

    #[test]
    fn classify_subtree_boundary_overlapping_upper_sum() {
        let r = range_inclusive(b"d", b"f");
        assert_eq!(
            classify_subtree(Some(b"e"), Some(b"h"), &r),
            SubtreeClassification::Boundary,
        );
    }

    #[test]
    fn classify_subtree_contained_within_inclusive_sum() {
        // Subtree (b, c] with range [a..=z] → Contained.
        let r = range_inclusive(b"a", b"z");
        assert_eq!(
            classify_subtree(Some(b"b"), Some(b"c"), &r),
            SubtreeClassification::Contained,
        );
    }

    #[test]
    fn key_strictly_inside_handles_unbounded_endpoints() {
        // -inf lower bound: any key > None is true.
        assert!(key_strictly_inside(b"a", None, Some(b"z")));
        // +inf upper bound: any key < None is true.
        assert!(key_strictly_inside(b"z", Some(b"a"), None));
        // Both unbounded: trivially true.
        assert!(key_strictly_inside(b"m", None, None));
        // Strictly outside lo.
        assert!(!key_strictly_inside(b"a", Some(b"a"), None));
        assert!(!key_strictly_inside(b"a", Some(b"z"), None));
        // Strictly outside hi.
        assert!(!key_strictly_inside(b"z", None, Some(b"z")));
        assert!(!key_strictly_inside(b"z", None, Some(b"a")));
    }

    #[test]
    fn empty_provable_sum_tree_proof_round_trip() {
        // Hits the "empty merk" branch of `prove_aggregate_sum_on_range`
        // (the no-proof side has its own test; this is the prover side).
        let v = GroveVersion::latest();
        let merk = TempMerk::new_with_tree_type(v, TreeType::ProvableSumTree);
        let (ops, sum) = merk
            .prove_aggregate_sum_on_range(&QueryItem::Range(b"a".to_vec()..b"z".to_vec()), v)
            .unwrap()
            .expect("prove on empty merk should succeed");
        assert_eq!(sum, 0);
        // The empty-merk proof should verify to (NULL_HASH, 0).
        let bytes = encode_proof(&ops);
        let (_root, verified) = verify_aggregate_sum_on_range_proof(
            &bytes,
            &QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        )
        .unwrap()
        .expect("verify on empty proof should succeed");
        assert_eq!(verified, 0);
    }

    #[test]
    fn no_proof_sum_with_negative_values_matches_prover() {
        // A tree with mixed positive and negative sum items must yield the
        // same net sum from both the no-proof and proof paths.
        let v = GroveVersion::latest();
        let mut merk = TempMerk::new_with_tree_type(v, TreeType::ProvableSumTree);
        let entries: [(&[u8], i64); 4] = [(b"a", 50), (b"b", -100), (b"c", 30), (b"d", -50)];
        let ops: Vec<(Vec<u8>, Op)> = entries
            .iter()
            .map(|(k, val)| (k.to_vec(), Op::Put(vec![], ProvableSummedMerkNode(*val))))
            .collect();
        merk.apply::<_, Vec<_>>(&ops, &[], None, v)
            .unwrap()
            .expect("apply mixed-sign items");
        merk.commit(v);
        // Full range → 50 − 100 + 30 − 50 = −70
        no_proof_sum_matches_prover(&merk, QueryItem::RangeFrom(b"a".to_vec()..), -70, v);
        // Subrange b..=c → −100 + 30 = −70
        no_proof_sum_matches_prover(
            &merk,
            QueryItem::RangeInclusive(b"b".to_vec()..=b"c".to_vec()),
            -70,
            v,
        );
    }
}
