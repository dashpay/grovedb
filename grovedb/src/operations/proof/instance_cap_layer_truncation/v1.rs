//! `instance_cap_layer_truncation` **v1** (`GROVE_V5`).
//!
//! A layer with subquery branches under a per-instance cap stops after the
//! key whose rows spent the budget, and the verifier accepts the early stop
//! only once it has seen that budget spent. See the module documentation for
//! the rule and for why it cannot drop an owed row.
//!
//! The prover walks the layer as v0 does and runs the descents, which it
//! needs anyway, then proves the layer a second time at a Merk limit equal to
//! the number of rows it processed before the budget ran out. A Merk proof at
//! a limit reveals nothing after its last result (the V1 shape, see
//! `merk_proof_version`), so everything past the cut is hashes. The second
//! proof carries the same row nodes as the first for the keys it keeps; the
//! rewrites the walk applied to them (resolved references, child hashes) are
//! moved over, since they belong to the key and not to the walk.

#[cfg(feature = "minimal")]
use grovedb_costs::{cost_return_on_error, CostResult, CostsExt, OperationCost};
use grovedb_merk::proofs::query::ProofLimitMode;
#[cfg(feature = "minimal")]
use grovedb_merk::{
    proofs::{query::QueryItem, Op},
    Merk, ProofWithoutEncodingResult,
};
#[cfg(feature = "minimal")]
use grovedb_storage::StorageContext;
#[cfg(feature = "minimal")]
use grovedb_version::version::GroveVersion;

#[cfg(feature = "minimal")]
use super::proof_node_row_key;
use crate::{Error, GroveDb, PathQuery};

impl GroveDb {
    /// `truncate_instance_capped_layer` v1: see the module documentation.
    ///
    /// Kept out of line: the V1 prover recurses once per subtree level, and
    /// this is a self-contained section of it.
    #[cfg(feature = "minimal")]
    #[inline(never)]
    pub(super) fn truncate_instance_capped_layer_v1<'a, S>(
        &self,
        subtree: &'a Merk<S>,
        query_items: &[QueryItem],
        left_to_right: bool,
        merk_proof: &mut ProofWithoutEncodingResult,
        first_op_past_the_budget: Option<usize>,
        charged_an_empty_plain_tree_row: bool,
        grove_version: &GroveVersion,
    ) -> CostResult<(), Error>
    where
        S: StorageContext<'a> + 'a,
    {
        let mut cost = OperationCost::default();

        // The budget outlasted the layer: every key it reveals is owed.
        let Some(first_op_past_the_budget) = first_op_past_the_budget else {
            return Ok(()).wrap_with_cost(cost);
        };
        // The cut is only as good as the count behind it. The prover charges
        // an empty plain `Tree` that is itself a result row; a verifier that
        // leaves empty trees out of its result (the default) does not. After
        // such a row the verifier's budgets run ahead of the prover's, and it
        // would refuse a layer cut by the prover's count as one that stops
        // with budget left. The layer stays whole, the shape v0 gives it.
        if charged_an_empty_plain_tree_row {
            return Ok(()).wrap_with_cost(cost);
        }

        let mut rows_within_budget = 0usize;
        let mut reveals_a_row_past_the_budget = false;
        for (op_index, op) in merk_proof.proof.iter().enumerate() {
            let (Op::Push(node) | Op::PushInverted(node)) = op else {
                continue;
            };
            if proof_node_row_key(node).is_none() {
                continue;
            }
            if op_index < first_op_past_the_budget {
                rows_within_budget += 1;
            } else {
                reveals_a_row_past_the_budget = true;
                break;
            }
        }

        // The budget ran out on the layer's last revealed row: there is
        // nothing to hide.
        if !reveals_a_row_past_the_budget {
            return Ok(()).wrap_with_cost(cost);
        }
        // A Merk limit is a non-zero `u16`. A layer that needs more rows than
        // that before its budget is spent (tens of thousands of empty
        // subtrees ahead of the page) stays whole, which the verifier
        // accepts as it accepts any layer that stops later than it must.
        // The budget is spent by a row, so the count is never zero.
        let walk_limit = match u16::try_from(rows_within_budget) {
            Ok(walk_limit) if walk_limit > 0 => walk_limit,
            _ => return Ok(()).wrap_with_cost(cost),
        };

        let mut truncated = cost_return_on_error!(
            &mut cost,
            self.generate_merk_proof(
                subtree,
                query_items,
                left_to_right,
                Some(walk_limit),
                grove_version
            )
        );

        // Both proofs push their rows in the walk's order, so the rows the
        // truncated proof reveals are the processed ones, in the same order.
        let processed = std::mem::take(&mut merk_proof.proof);
        let mut processed_rows = processed
            .into_iter()
            .take(first_op_past_the_budget)
            .filter_map(|op| match op {
                Op::Push(node) | Op::PushInverted(node) if proof_node_row_key(&node).is_some() => {
                    Some(node)
                }
                _ => None,
            });
        for op in truncated.proof.iter_mut() {
            let (Op::Push(node) | Op::PushInverted(node)) = op else {
                continue;
            };
            if proof_node_row_key(node).is_none() {
                continue;
            }
            let processed_row = processed_rows.next();
            let same_key = processed_row.as_ref().is_some_and(|processed_row| {
                proof_node_row_key(processed_row) == proof_node_row_key(node)
            });
            match processed_row {
                Some(processed_row) if same_key => *node = processed_row,
                _ => {
                    return Err(Error::CorruptedCodeExecution(
                        "a truncated layer proof reveals a row the layer walk did not process",
                    ))
                    .wrap_with_cost(cost);
                }
            }
        }
        if processed_rows.next().is_some() {
            return Err(Error::CorruptedCodeExecution(
                "a truncated layer proof hides a row the layer walk processed",
            ))
            .wrap_with_cost(cost);
        }

        *merk_proof = truncated;
        Ok(()).wrap_with_cost(cost)
    }

    /// `instance_capped_layer_limit_mode` v1: a gap-free prefix of the layer
    /// query, which may stop before the limit it is read at.
    pub(super) fn instance_capped_layer_limit_mode_v1() -> ProofLimitMode {
        ProofLimitMode::UpperBound
    }

    /// `check_instance_capped_layer_stop` v1: a layer that stopped revealing
    /// its query before its limit ran out is accepted only when the rows
    /// below the keys it does reveal spent the budget.
    pub(super) fn check_instance_capped_layer_stop_v1(
        query: &PathQuery,
        stopped_before_its_limit: bool,
        budget_spent: bool,
    ) -> Result<(), Error> {
        if stopped_before_its_limit && !budget_spent {
            return Err(Error::InvalidProof(
                query.clone(),
                "V1 proof layer under a per-instance limit stops before the limit is spent: the \
                 keys it hides may still hold rows the query asks for"
                    .to_string(),
            ));
        }
        Ok(())
    }
}
