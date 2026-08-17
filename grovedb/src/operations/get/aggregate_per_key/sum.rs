//! `query_aggregate_sum_per_key` — trusted per-key reads on the signed-sum
//! axis.
//!
//! See [`super`] for the leaf-vs-carrier vocabulary and
//! [`super::carrier`] for the shared walk this delegates to.

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::Merk;
use grovedb_version::{check_grovedb_v0_with_cost, version::GroveVersion};

use crate::{util::TxRef, Error, GroveDb, PathQuery, TransactionArg};

impl GroveDb {
    /// Executes an `AggregateSumOnRange` query in either the **leaf** or
    /// **carrier** shape without generating a proof, returning one
    /// `(outer_key, sum)` pair per matched outer key.
    ///
    /// Sum-axis mirror of [`Self::query_aggregate_count_per_key`], and
    /// the no-proof counterpart of
    /// [`GroveDb::verify_aggregate_sum_query_per_key`]: it performs the
    /// same merk-level boundary walks the per-key verifier reconstructs
    /// from a proof but skips proof generation, encoding, decoding, and
    /// chain verification entirely.
    ///
    /// For a **leaf** query the returned vector contains exactly one
    /// entry whose key is an empty byte string and whose sum is the same
    /// `i64` [`Self::query_aggregate_sum`] would have returned.
    ///
    /// The empty stand-in key in the leaf shape is deliberate: it is the
    /// convention the three per-key *verifiers* already collapse a leaf
    /// proof to, so keeping it here is what lets a caller swap
    /// `query_aggregate_*_per_key` for `prove_query` +
    /// `verify_aggregate_*_query_per_key` (or back) and compare results
    /// element-for-element without branching on the shape. A leaf query
    /// has no outer key to report, so *some* stand-in is unavoidable;
    /// matching the already-shipped proof-side convention is worth more
    /// than a prettier one. Callers that need to distinguish "leaf" from
    /// "carrier whose outer key happens to be empty" should classify the
    /// query — outer keys are never empty in a valid carrier, since
    /// [`PathQuery::validate_aggregate_sum_on_range`] requires every
    /// `subquery_path` element to be a non-empty key.
    ///
    /// For a **carrier** query the outer items must be `Key(_)` /
    /// `Range*(_)` and the `default_subquery_branch.subquery` must
    /// validate as a leaf `AggregateSumOnRange`. The optional
    /// `subquery_path` is followed exactly (single-key step per element)
    /// before the sum walk. The returned vector has one entry per
    /// matched outer key in query-direction order (ascending lex when
    /// `left_to_right = true`, descending otherwise). Outer-key
    /// candidates that don't exist contribute no entry; outer-key
    /// candidates whose leaf subtree is empty contribute `(key, 0)`.
    ///
    /// `path_query` must satisfy
    /// [`PathQuery::validate_aggregate_sum_on_range`] in either shape.
    /// Pagination rules differ by shape: for **leaf** queries both
    /// `SizedQuery::limit` and `SizedQuery::offset` are rejected (a leaf
    /// returns a single `i64` and pagination would silently change the
    /// answer); for **carrier** queries `SizedQuery::limit` is accepted
    /// and caps the number of outer-key matches the walk returns (each
    /// matched outer key still produces a complete leaf-ASOR `i64`, the
    /// inner range is not capped), while `SizedQuery::offset` is still
    /// rejected. Each leaf subtree the walk terminates in must be a
    /// `ProvableSumTree` or `ProvableCountProvableSumTree` — the
    /// merk-level walk rejects any other tree type.
    ///
    /// The returned sums are **not** independently verifiable — callers
    /// are trusting their own merk read path. For verifiable sums, use
    /// [`Self::prove_query`] +
    /// [`GroveDb::verify_aggregate_sum_query_per_key`].
    pub fn query_aggregate_sum_per_key(
        &self,
        path_query: &PathQuery,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<(Vec<u8>, i64)>, Error> {
        check_grovedb_v0_with_cost!(
            "query_aggregate_sum_per_key",
            grove_version
                .grovedb_versions
                .operations
                .query
                .query_aggregate_sum_on_range
        );

        let mut cost = OperationCost::default();

        // Up-front shape validation: accept both leaf and carrier
        // shapes. We classify by what the top-level query owns: a direct
        // `AggregateSumOnRange` item means leaf; otherwise the validator
        // has confirmed a valid carrier subquery exists.
        let inner_range = cost_return_on_error_no_add!(
            cost,
            path_query.validate_aggregate_sum_on_range().cloned()
        );

        if path_query.query.query.aggregate_sum_on_range().is_some() {
            // Leaf shape: delegate to the existing single-`i64` entry
            // point and wrap as a one-entry vector with an empty key.
            let sum = cost_return_on_error!(
                &mut cost,
                self.query_aggregate_sum(path_query, transaction, grove_version)
            );
            return Ok(vec![(Vec::new(), sum)]).wrap_with_cost(cost);
        }

        // Carrier shape: delegate to the shared carrier driver, which
        // terminates each per-key walk in the sum primitive.
        let tx = TxRef::new(&self.db, transaction);
        let results = cost_return_on_error!(
            &mut cost,
            self.query_aggregate_carrier_per_key(
                path_query,
                &inner_range,
                "carrier aggregate-sum matched a non-tree element; outer items must resolve to \
                 tree elements",
                Merk::sum_aggregate_on_range,
                tx.as_ref(),
                transaction,
                grove_version,
            )
        );

        Ok(results).wrap_with_cost(cost)
    }
}
