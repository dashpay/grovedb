//! Public prover entry points for `AggregateCountOnRange` queries.
//!
//! `impl RefWalker` block holding both the proof-emitting entry point
//! (`create_aggregate_count_on_range_proof`) and its no-proof read
//! counterpart (`count_aggregate_on_range`).

use std::collections::LinkedList;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_version::version::GroveVersion;

use super::{emit::emit_count_proof, is_provable_count_bearing, walk::walk_count_only};
use crate::{
    proofs::{query::QueryItem, Op},
    tree::{Fetch, RefWalker},
    {Error, TreeType},
};

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
