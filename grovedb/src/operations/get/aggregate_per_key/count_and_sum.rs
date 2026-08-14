//! `query_aggregate_count_and_sum_per_key` — trusted per-key reads on the
//! combined count+sum axis.
//!
//! See [`super`] for the leaf-vs-carrier vocabulary and
//! [`super::carrier`] for the shared walk this delegates to. Only
//! `ProvableCountProvableSumTree` can terminate a walk on this axis — it
//! is the only tree type whose node hash binds both aggregates.

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use grovedb_merk::Merk;
use grovedb_version::{check_grovedb_v0_with_cost, version::GroveVersion};

use crate::{util::TxRef, Error, GroveDb, PathQuery, TransactionArg};

impl GroveDb {
    /// Executes an `AggregateCountAndSumOnRange` query in either the
    /// **leaf** or **carrier** shape without generating a proof,
    /// returning one `(outer_key, count, sum)` triple per matched outer
    /// key.
    ///
    /// Combined-axis mirror of [`Self::query_aggregate_count_per_key`]
    /// and [`Self::query_aggregate_sum_per_key`], and the no-proof
    /// counterpart of
    /// [`GroveDb::verify_aggregate_count_and_sum_query_per_key`]: it
    /// performs the same merk-level boundary walks the per-key verifier
    /// reconstructs from a proof but skips proof generation, encoding,
    /// decoding, and chain verification entirely.
    ///
    /// Each matched outer key costs **one** classification walk over its
    /// leaf merk (the same shape the combined prover walks) with both
    /// axes accumulated in parallel — strictly cheaper than calling
    /// [`Self::query_aggregate_count_per_key`] and
    /// [`Self::query_aggregate_sum_per_key`] separately.
    ///
    /// For a **leaf** query the returned vector contains exactly one
    /// entry whose key is an empty byte string and whose `(count, sum)`
    /// is the same pair [`Self::query_aggregate_count_and_sum`] would
    /// have returned — see [`Self::query_aggregate_sum_per_key`] for why
    /// the empty stand-in key is deliberate.
    ///
    /// For a **carrier** query the outer items must be `Key(_)` /
    /// `Range*(_)` and the `default_subquery_branch.subquery` must
    /// validate as a leaf `AggregateCountAndSumOnRange`. The optional
    /// `subquery_path` is followed exactly (single-key step per element)
    /// before the combined walk. The returned vector has one entry per
    /// matched outer key in query-direction order (ascending lex when
    /// `left_to_right = true`, descending otherwise). Outer-key
    /// candidates that don't exist contribute no entry; outer-key
    /// candidates whose leaf subtree is empty contribute `(key, 0, 0)`.
    ///
    /// `path_query` must satisfy
    /// [`PathQuery::validate_aggregate_count_and_sum_on_range`] in
    /// either shape. Pagination rules differ by shape: for **leaf**
    /// queries both `SizedQuery::limit` and `SizedQuery::offset` are
    /// rejected (a leaf returns a single `(u64, i64)` and pagination
    /// would silently change both answers); for **carrier** queries
    /// `SizedQuery::limit` is accepted and caps the number of outer-key
    /// matches the walk returns (each matched outer key still produces a
    /// complete leaf pair, the inner range is not capped), while
    /// `SizedQuery::offset` is still rejected. Each leaf subtree the
    /// walk terminates in must be a `ProvableCountProvableSumTree` —
    /// PCPS is the only tree type whose node hash binds *both*
    /// aggregates, so the merk-level walk rejects every single-axis host.
    ///
    /// The returned pairs are **not** independently verifiable — callers
    /// are trusting their own merk read path. For verifiable pairs, use
    /// [`Self::prove_query`] +
    /// [`GroveDb::verify_aggregate_count_and_sum_query_per_key`].
    pub fn query_aggregate_count_and_sum_per_key(
        &self,
        path_query: &PathQuery,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<(Vec<u8>, u64, i64)>, Error> {
        check_grovedb_v0_with_cost!(
            "query_aggregate_count_and_sum_per_key",
            grove_version
                .grovedb_versions
                .operations
                .query
                .query_aggregate_count_and_sum_on_range
        );

        let mut cost = OperationCost::default();

        // Up-front shape validation: accept both leaf and carrier
        // shapes. We classify by what the top-level query owns: a direct
        // `AggregateCountAndSumOnRange` item means leaf; otherwise the
        // validator has confirmed a valid carrier subquery exists.
        let inner_range = cost_return_on_error_no_add!(
            cost,
            path_query
                .validate_aggregate_count_and_sum_on_range()
                .cloned()
        );

        if path_query
            .query
            .query
            .aggregate_count_and_sum_on_range()
            .is_some()
        {
            // Leaf shape: delegate to the existing single-pair entry
            // point and wrap as a one-entry vector with an empty key.
            let (count, sum) = cost_return_on_error!(
                &mut cost,
                self.query_aggregate_count_and_sum(path_query, transaction, grove_version)
            );
            return Ok(vec![(Vec::new(), count, sum)]).wrap_with_cost(cost);
        }

        // Carrier shape: delegate to the shared carrier driver, which
        // terminates each per-key walk in the combined primitive. The
        // driver is generic over the per-key payload, so the `(u64,
        // i64)` pairs come back tupled and are flattened to triples
        // here to match the per-key verifier's surface.
        let tx = TxRef::new(&self.db, transaction);
        let pairs = cost_return_on_error!(
            &mut cost,
            self.query_aggregate_carrier_per_key(
                path_query,
                &inner_range,
                "carrier aggregate-count-and-sum matched a non-tree element; outer items must \
                 resolve to tree elements",
                Merk::count_and_sum_aggregate_on_range,
                tx.as_ref(),
                transaction,
                grove_version,
            )
        );

        let results = pairs
            .into_iter()
            .map(|(key, (count, sum))| (key, count, sum))
            .collect();

        Ok(results).wrap_with_cost(cost)
    }
}
