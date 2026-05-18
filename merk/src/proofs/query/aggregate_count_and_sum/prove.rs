//! Public prover entry point for `AggregateCountAndSumOnRange` queries.
//!
//! `impl RefWalker` block holding the proof-emitting entry point
//! (`create_aggregate_count_and_sum_on_range_proof`). Only
//! `ProvableCountProvableSumTree` (PCPS) is a valid host; any other
//! tree type is rejected up front.

use std::collections::LinkedList;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_version::version::GroveVersion;

use super::{emit::emit_count_and_sum_proof, is_provable_count_and_sum_bearing};
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
}
