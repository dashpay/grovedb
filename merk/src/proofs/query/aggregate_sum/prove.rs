//! Public prover entry points for `AggregateSumOnRange` queries.
//!
//! `impl RefWalker` block holding both the proof-emitting entry point
//! (`create_aggregate_sum_on_range_proof`) and its no-proof read
//! counterpart (`sum_aggregate_on_range`). Both narrow the prover-side
//! `i128` accumulator down to the on-the-wire `i64`, rejecting any
//! out-of-range result as corruption (the prover walks its own merk, so
//! an out-of-range honest result is unreachable — defense-in-depth here
//! keeps the contract symmetric with the verifier).

use std::collections::LinkedList;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_version::version::GroveVersion;

use super::{emit::emit_sum_proof, is_provable_sum_bearing, walk::walk_sum_only};
use crate::{
    proofs::{query::QueryItem, Op},
    tree::{Fetch, RefWalker},
    {Error, TreeType},
};

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
