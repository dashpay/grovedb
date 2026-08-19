//! Public prover entry point for offset-paginated count-tree range
//! queries. Owns the `impl RefWalker` block; the actual emission
//! recursion lives in [`super::emit`].

use std::collections::LinkedList;

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_version::version::GroveVersion;

use super::{
    emit::{emit_count_offset_proof, EmitState},
    is_provable_count_bearing,
};
use crate::{
    proofs::{query::QueryItem, Op},
    tree::{Fetch, RefWalker},
    {Error, TreeType},
};

/// Outcome of a `create_count_offset_on_range_proof` call. The verifier
/// independently re-derives `returned` and the skipped-count from the
/// proof bytes, so these values are *informational only* — the caller
/// can compare them against expectations for sanity checks, but they
/// are not part of the proof's trust input.
pub struct ProverCountOffsetResult {
    /// Linear ops the verifier will replay.
    pub ops: LinkedList<Op>,
    /// How many in-range items the prover returned. ≤ `limit` (if set).
    pub returned: u64,
    /// Remaining offset the prover did not get to consume because the
    /// in-range population was smaller than the requested offset.
    /// `requested_offset − offset_remaining` is the number of in-range
    /// items the prover skipped. Useful for callers that want to detect
    /// "offset past the end" without re-walking.
    pub offset_remaining: u64,
}

impl<S> RefWalker<'_, S>
where
    S: Fetch + Sized + Clone,
{
    /// Generate an offset-paginated proof for a single-range query
    /// against a `ProvableCountTree` or `ProvableCountSumTree`.
    ///
    /// `inner_range` is the `QueryItem` the caller wants to range-scan
    /// (already validated at the `Query` / `PathQuery` level). `offset`
    /// is how many leading in-range items to skip; `limit` is the
    /// maximum number of items to return after the offset (`None` means
    /// unlimited). `left_to_right` controls ascending vs descending
    /// iteration: for descending the prover walks the right child
    /// first and emits the inverted op family, so "the first N in-range
    /// items" become the N highest in-range keys.
    ///
    /// `tree_type` must be one of `ProvableCountTree`,
    /// `ProvableCountSumTree`, or `ProvableCountProvableSumTree`. Any
    /// other tree type is rejected with `Error::InvalidProofError`
    /// before any walking happens — count commitments only make sense
    /// against trees that bind their count into the node hash. For
    /// PCPS hosts the emit path additionally commits the sum into the
    /// collapsed-subtree ops so the verifier can reconstruct
    /// `node_hash_with_count_and_sum`.
    pub fn create_count_offset_on_range_proof(
        &mut self,
        inner_range: &QueryItem,
        offset: u64,
        limit: Option<u64>,
        left_to_right: bool,
        allow_raw_references: bool,
        tree_type: TreeType,
        grove_version: &GroveVersion,
    ) -> CostResult<ProverCountOffsetResult, Error> {
        if !is_provable_count_bearing(tree_type) {
            return Err(Error::InvalidProofError(format!(
                "count-offset paginated proof is only valid against ProvableCountTree, \
                 ProvableCountSumTree, or ProvableCountProvableSumTree, got {:?}",
                tree_type
            )))
            .wrap_with_cost(OperationCost::default());
        }

        let mut cost = OperationCost::default();
        let mut ops = LinkedList::new();
        let mut state = EmitState {
            offset_remaining: offset,
            limit_remaining: limit,
            returned: 0,
            left_to_right,
            allow_raw_references,
        };
        cost_return_on_error!(
            &mut cost,
            emit_count_offset_proof(
                self,
                inner_range,
                None,
                None,
                &mut state,
                &mut ops,
                tree_type,
                grove_version
            )
        );
        Ok(ProverCountOffsetResult {
            ops,
            returned: state.returned,
            offset_remaining: state.offset_remaining,
        })
        .wrap_with_cost(cost)
    }
}
