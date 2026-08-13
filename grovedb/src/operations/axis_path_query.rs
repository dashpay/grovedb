//! The three entry points for [`AxisPathQuery`]: read it, prove it,
//! verify it.
//!
//! This is dispatch, not new proof machinery. Each entry point
//! validates the query once, then routes to the indexed-axis primitive
//! that already serves that shape — so an axis path query produces the
//! same envelope, byte for byte, as the hand-rolled call it replaces.
//! What the caller gains is one vocabulary instead of a dozen bespoke
//! argument lists, and one place where the bounds-to-Merk-query
//! lowering lives (see [`AxisQuery::merk_query`]) rather than a copy on
//! each side of the prover/verifier boundary.

#[cfg(feature = "minimal")]
use grovedb_costs::{cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt};
use grovedb_element::indexed::IndexAxis;
#[cfg(feature = "minimal")]
use grovedb_version::version::GroveVersion;

#[cfg(feature = "minimal")]
use crate::TransactionArg;
use grovedb_merk::tree::CryptoHash;

use crate::{
    operations::proof::indexed_axis::AxisEntries,
    query::{AxisPathQuery, AxisTraversal},
    Error, GroveDb,
};

/// The verified answer to an [`AxisPathQuery`].
#[derive(Debug)]
pub struct VerifiedAxisPathQuery {
    /// GroveDB root hash the proof reconstructs. Compare it against the
    /// root you trust — verification alone proves internal consistency,
    /// not that the proof is about the state you meant.
    pub root_hash: CryptoHash,
    /// The entries, in the query's walk direction.
    pub entries: AxisEntries,
    /// Entries the walk attested as skipped before the returned page.
    ///
    /// Equals the requested `offset` on a full [`AxisTraversal::TopK`]
    /// page, and is smaller when the walk ran out during the skip — in
    /// which case `entries` is empty and the pair proves the secondary
    /// holds exactly `skipped` entries. Always `0` for
    /// [`AxisTraversal::Bounded`], which does not skip.
    pub skipped: u64,
}

#[cfg(feature = "minimal")]
impl GroveDb {
    /// Read an [`AxisPathQuery`] directly, without a proof.
    ///
    /// A missing path is an error rather than an empty result: the
    /// indexed tree is created before anything can be inserted into it,
    /// so its absence means the state is not what the query claims. An
    /// indexed tree that exists but holds nothing yields an empty
    /// entry list.
    pub fn query_axis_path_query(
        &self,
        query: &AxisPathQuery,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<AxisEntries, Error> {
        let mut cost = Default::default();
        cost_return_on_error_no_add!(cost, query.validate());
        let path = query.path_refs();
        let descending = query.query.descending;

        let entries = match (query.query.axis, query.query.traversal) {
            (IndexAxis::Count, AxisTraversal::TopK { k, offset }) => {
                AxisEntries::Count(cost_return_on_error!(
                    &mut cost,
                    self.indexed_count_top_k_paginated(
                        path.as_slice(),
                        k,
                        offset,
                        descending,
                        transaction,
                        grove_version
                    )
                ))
            }
            (IndexAxis::Count, AxisTraversal::Bounded { lo, hi, limit }) => {
                AxisEntries::Count(cost_return_on_error!(
                    &mut cost,
                    self.indexed_count_range(
                        path.as_slice(),
                        lo.max(0) as u64,
                        hi.min(u64::MAX as i128) as u64,
                        descending,
                        limit,
                        transaction,
                        grove_version
                    )
                ))
            }
            (IndexAxis::Sum, AxisTraversal::TopK { k, offset }) => {
                AxisEntries::Sum(cost_return_on_error!(
                    &mut cost,
                    self.indexed_sum_top_k_paginated(
                        path.as_slice(),
                        k,
                        offset,
                        descending,
                        transaction,
                        grove_version
                    )
                ))
            }
            (IndexAxis::Sum, AxisTraversal::Bounded { lo, hi, limit }) => {
                AxisEntries::Sum(cost_return_on_error!(
                    &mut cost,
                    self.indexed_sum_range(
                        path.as_slice(),
                        lo.max(i64::MIN as i128) as i64,
                        hi.min(i64::MAX as i128) as i64,
                        descending,
                        limit,
                        transaction,
                        grove_version
                    )
                ))
            }
            (IndexAxis::Avg, AxisTraversal::TopK { k, offset }) => {
                AxisEntries::Avg(cost_return_on_error!(
                    &mut cost,
                    self.indexed_avg_top_k_paginated(
                        path.as_slice(),
                        k,
                        offset,
                        descending,
                        transaction,
                        grove_version
                    )
                ))
            }
            (IndexAxis::Avg, AxisTraversal::Bounded { lo, hi, limit }) => {
                AxisEntries::Avg(cost_return_on_error!(
                    &mut cost,
                    self.indexed_avg_range(
                        path.as_slice(),
                        lo,
                        hi,
                        descending,
                        limit,
                        transaction,
                        grove_version
                    )
                ))
            }
        };
        Ok(entries).wrap_with_cost(cost)
    }

    /// Prove an [`AxisPathQuery`].
    ///
    /// Emits the existing indexed-axis envelope for the shape the
    /// traversal names — the paginated envelope for
    /// [`AxisTraversal::TopK`], the range envelope for
    /// [`AxisTraversal::Bounded`] — so this changes no proof bytes and
    /// no verification rule. Verify with
    /// [`GroveDb::verify_axis_path_query`].
    pub fn prove_axis_path_query(
        &self,
        query: &AxisPathQuery,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error> {
        let mut cost = Default::default();
        cost_return_on_error_no_add!(cost, query.validate());
        let path = query.path_refs();

        match query.query.traversal {
            AxisTraversal::TopK { k, offset } => self.prove_indexed_axis_top_k_paginated(
                path.as_slice(),
                query.query.axis,
                k,
                offset,
                query.query.descending,
                transaction,
                grove_version,
            ),
            AxisTraversal::Bounded { limit, .. } => {
                let secondary_query = cost_return_on_error_no_add!(cost, query.query.merk_query());
                self.prove_indexed_axis_query(
                    path.as_slice(),
                    query.query.axis,
                    secondary_query,
                    Some(limit),
                    transaction,
                    grove_version,
                )
            }
        }
        .add_cost(cost)
    }
}

impl GroveDb {
    /// Verify a proof produced by [`GroveDb::prove_axis_path_query`]
    /// against the same query.
    ///
    /// The query is the verifier's own — it rebuilds the path, the
    /// axis, the walk parameters and (for a bounded traversal) the
    /// secondary Merk query from it, and grovedb re-checks those
    /// against the values echoed in the envelope. A proof generated for
    /// a different page, direction, axis or bound therefore fails
    /// rather than being reinterpreted.
    ///
    /// Available in verifier-only builds: nothing here touches storage.
    pub fn verify_axis_path_query(
        proof: &[u8],
        query: &AxisPathQuery,
    ) -> Result<VerifiedAxisPathQuery, Error> {
        query.validate()?;
        let path = query.path_refs();

        match query.query.traversal {
            AxisTraversal::TopK { k, offset } => {
                let result = Self::verify_indexed_axis_top_k_paginated(
                    proof,
                    path.as_slice(),
                    query.query.axis,
                    k,
                    offset,
                    query.query.descending,
                )?;
                Ok(VerifiedAxisPathQuery {
                    root_hash: result.root_hash,
                    entries: result.entries,
                    skipped: result.skipped,
                })
            }
            AxisTraversal::Bounded { limit, .. } => {
                let secondary_query = query.query.merk_query()?;
                let result = Self::verify_indexed_axis_query(
                    proof,
                    path.as_slice(),
                    query.query.axis,
                    secondary_query,
                    Some(limit),
                )?;
                Ok(VerifiedAxisPathQuery {
                    root_hash: result.root_hash,
                    entries: result.entries,
                    skipped: 0,
                })
            }
        }
    }
}
