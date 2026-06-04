//! Public prover entry point for `AggregateCountAndSumOnRange` queries.
//!
//! `impl RefWalker` block holding the proof-emitting entry point
//! (`create_aggregate_count_and_sum_on_range_proof`). Only
//! `ProvableCountProvableSumTree` (PCPS) is a valid host; any other
//! tree type is rejected up front.

use std::collections::LinkedList;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_version::version::GroveVersion;

use super::{
    emit::emit_count_and_sum_proof, is_provable_count_and_sum_bearing, walk::walk_count_and_sum,
};
use crate::{
    proofs::{query::QueryItem, Op},
    tree::{Fetch, RefWalker},
    {Error, TreeType},
};

impl<S> RefWalker<'_, S>
where
    S: Fetch + Sized + Clone,
{
    /// Generate a combined count+sum proof for an
    /// `AggregateCountAndSumOnRange` query.
    ///
    /// `inner_range` is the `QueryItem` wrapped by
    /// `AggregateCountAndSumOnRange` (already stripped at the caller).
    /// `tree_type` must be `ProvableCountProvableSumTree`; any other
    /// tree type is rejected with `Error::InvalidProofError` before
    /// any walking happens.
    ///
    /// The returned tuple is `(proof_ops, count, sum)`:
    /// - `proof_ops` is the linear stream the verifier will replay to
    ///   reconstruct the tree's root hash.
    /// - `count` is the prover-side computed count.
    /// - `sum` is the prover-side computed sum (narrowed from `i128`
    ///   to `i64`; an overflow surfaces as
    ///   `Error::InvalidProofError`).
    ///
    /// Both values are returned as a convenience — the verifier
    /// independently recomputes them from the proof and compares
    /// against the expected root hash; the trust anchor is the chain
    /// of node-hash recomputations, not these returned numbers.
    pub fn create_aggregate_count_and_sum_on_range_proof(
        &mut self,
        inner_range: &QueryItem,
        tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> CostResult<(LinkedList<Op>, u64, i64), Error> {
        if !is_provable_count_and_sum_bearing(tree_type) {
            return Err(Error::InvalidProofError(format!(
                "AggregateCountAndSumOnRange is only valid against \
                 ProvableCountProvableSumTree, got {:?}",
                tree_type
            )))
            .wrap_with_cost(OperationCost::default());
        }

        let mut cost = OperationCost::default();
        let mut ops = LinkedList::new();
        let (count, sum_i128) = cost_return_on_error!(
            &mut cost,
            emit_count_and_sum_proof(self, inner_range, None, None, &mut ops, grove_version,)
        );

        // The prover walks its own merk, whose ProvableSum aggregate
        // is maintained as i64 at every node — an honest walk lands
        // inside i64's range. Anything else is local state
        // corruption; surface it as InvalidProofError so callers see
        // the same error class the verifier would produce for an
        // adversarial proof composing extremes.
        let sum: i64 = match i64::try_from(sum_i128) {
            Ok(v) => v,
            Err(_) => {
                return Err(Error::InvalidProofError(format!(
                    "aggregate-count-and-sum prover: in-range sum overflowed i64 ({})",
                    sum_i128
                )))
                .wrap_with_cost(cost);
            }
        };

        Ok((ops, count, sum)).wrap_with_cost(cost)
    }

    /// Walk the tree for an `AggregateCountAndSumOnRange` query and
    /// return the in-range `(count, sum)` pair, **without** producing
    /// a proof.
    ///
    /// This is the no-proof counterpart of
    /// [`Self::create_aggregate_count_and_sum_on_range_proof`]. It
    /// performs the same classification walk (Contained / Disjoint /
    /// Boundary) and reads each node's aggregate `(count, sum)`
    /// directly from the merk, so it is O(log n) in the number of
    /// distinct keys under the indexed subtree — the same complexity
    /// as the proof variant but without the proof-op allocations,
    /// hash recomputations, or serialization round-trip.
    ///
    /// The caller (`Merk::count_and_sum_aggregate_on_range`) is
    /// expected to have already validated `tree_type` is
    /// `ProvableCountProvableSumTree`; the per-node
    /// `provable_count_and_sum_from_aggregate` check inside the walk
    /// surfaces any disagreement between the declared tree type and
    /// the in-memory aggregate.
    ///
    /// The accumulators carry `(u128, i128)` end-to-end and narrow to
    /// `(u64, i64)` at the very last step, exactly the way the prover
    /// and verifier do. Any value outside the narrower ranges is
    /// treated as corruption (a real PCPS tree maintains every
    /// aggregate as `(u64, i64)` at every level, so the wider path
    /// only ever holds an out-of-range value if the tree state is
    /// internally inconsistent).
    ///
    /// The result is **not** independently verifiable: the caller is
    /// trusting their own merk read path. Callers that need a
    /// verifiable pair must use `prove_aggregate_count_and_sum_on_range`
    /// + `verify_aggregate_count_and_sum_on_range_proof`.
    pub fn count_and_sum_aggregate_on_range(
        &mut self,
        inner_range: &QueryItem,
        grove_version: &GroveVersion,
    ) -> CostResult<(u64, i64), Error> {
        let mut cost = OperationCost::default();
        let (count_u128, sum_i128) = cost_return_on_error!(
            &mut cost,
            walk_count_and_sum(self, inner_range, None, None, grove_version)
        );
        narrow_count_and_sum(count_u128, sum_i128).wrap_with_cost(cost)
    }
}

/// Narrow the no-proof walker's `(u128, i128)` accumulator pair to the
/// on-the-wire `(u64, i64)` shape, returning `CorruptedData` if either
/// axis is out of range. Extracted into a free function so the narrowing
/// arms are unit-testable without standing up a corrupted merk.
///
/// A real PCPS tree maintains every aggregate as `(u64, i64)` at every
/// level, so an honest walk lands inside both narrower ranges. An
/// out-of-range value implies the merk's in-memory state disagrees with
/// its type contract — local invariant failure, so `CorruptedData` per
/// the repo's error-handling convention.
pub(super) fn narrow_count_and_sum(count_u128: u128, sum_i128: i128) -> Result<(u64, i64), Error> {
    let count = u64::try_from(count_u128).map_err(|_| {
        Error::CorruptedData(format!(
            "no-proof aggregate-count-and-sum: in-range count overflowed u64 ({})",
            count_u128
        ))
    })?;
    let sum = i64::try_from(sum_i128).map_err(|_| {
        Error::CorruptedData(format!(
            "no-proof aggregate-count-and-sum: in-range sum overflowed i64 ({})",
            sum_i128
        ))
    })?;
    Ok((count, sum))
}
