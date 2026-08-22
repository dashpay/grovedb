//! Per-axis (count / sum / avg) convenience wrappers over the generic
//! [`super::generate`] / [`super::verify`] entry points.
//!
//! Each wrapper pins the [`IndexAxis`] and forwards; no logic lives here.

#[cfg(feature = "minimal")]
use grovedb_costs::CostResult;
use grovedb_element::indexed::IndexAxis;
use grovedb_merk::proofs::Query as MerkQuery;
#[cfg(feature = "minimal")]
use grovedb_path::SubtreePath;
use grovedb_query::AggregateFold;
use grovedb_version::version::GroveVersion;

#[cfg(feature = "minimal")]
use crate::TransactionArg;
use crate::{Error, GroveDb};

use super::{IndexedAxisAggregateResult, IndexedAxisPaginatedResult, IndexedAxisQueryResult};

impl GroveDb {
    // ---------- count axis ----------

    /// Prove the top-`k` entries of the count axis. Thin wrapper over
    /// [`Self::prove_indexed_axis_top_k`] with `axis = Count`.
    #[cfg(feature = "minimal")]
    pub fn prove_indexed_count_top_k<'b, B, P>(
        &self,
        path: P,
        k: u16,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_top_k(
            path,
            IndexAxis::Count,
            k,
            descending,
            transaction,
            grove_version,
        )
    }

    /// Prove an offset-paginated top-`k` window on the count axis.
    #[cfg(feature = "minimal")]
    pub fn prove_indexed_count_top_k_paginated<'b, B, P>(
        &self,
        path: P,
        k: u16,
        offset: u64,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_top_k_paginated(
            path,
            IndexAxis::Count,
            k,
            offset,
            descending,
            transaction,
            grove_version,
        )
    }

    /// Prove an arbitrary query against the count-axis secondary.
    #[cfg(feature = "minimal")]
    pub fn prove_indexed_count_query<'b, B, P>(
        &self,
        path: P,
        secondary_query: MerkQuery,
        limit: Option<u16>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_query(
            path,
            IndexAxis::Count,
            secondary_query,
            limit,
            transaction,
            grove_version,
        )
    }

    /// Prove the aggregate count of entries whose `count_value` is in
    /// `[lo_count, hi_count]`.
    #[cfg(feature = "minimal")]
    pub fn prove_indexed_count_aggregate_over_value_range<'b, B, P>(
        &self,
        path: P,
        lo_count: u64,
        hi_count: u64,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_aggregate_over_value_range(
            path,
            IndexAxis::Count,
            lo_count as i128,
            hi_count as i128,
            AggregateFold::Population,
            transaction,
            grove_version,
        )
    }

    /// Verify a count-axis top-k proof.
    pub fn verify_indexed_count_top_k(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_k: u16,
        expected_descending: bool,
        grove_version: &GroveVersion,
    ) -> Result<IndexedAxisQueryResult, Error> {
        Self::verify_indexed_axis_top_k(
            proof_bytes,
            path,
            IndexAxis::Count,
            expected_k,
            expected_descending,
            grove_version,
        )
    }

    /// Verify a count-axis paginated proof.
    pub fn verify_indexed_count_top_k_paginated(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_k: u16,
        expected_offset: u64,
        expected_descending: bool,
        grove_version: &GroveVersion,
    ) -> Result<IndexedAxisPaginatedResult, Error> {
        Self::verify_indexed_axis_top_k_paginated(
            proof_bytes,
            path,
            IndexAxis::Count,
            expected_k,
            expected_offset,
            expected_descending,
            grove_version,
        )
    }

    /// Verify a count-axis arbitrary-query proof.
    pub fn verify_indexed_count_query(
        proof_bytes: &[u8],
        path: &[&[u8]],
        secondary_query: MerkQuery,
        expected_limit: Option<u16>,
        grove_version: &GroveVersion,
    ) -> Result<IndexedAxisQueryResult, Error> {
        Self::verify_indexed_axis_query(
            proof_bytes,
            path,
            IndexAxis::Count,
            secondary_query,
            expected_limit,
            grove_version,
        )
    }

    /// Verify a count-axis aggregate proof.
    pub fn verify_indexed_count_aggregate_over_value_range(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_lo_count: u64,
        expected_hi_count: u64,
        grove_version: &GroveVersion,
    ) -> Result<IndexedAxisAggregateResult, Error> {
        Self::verify_indexed_axis_aggregate_over_value_range(
            proof_bytes,
            path,
            IndexAxis::Count,
            expected_lo_count as i128,
            expected_hi_count as i128,
            AggregateFold::Population,
            grove_version,
        )
    }

    // ---------- sum axis ----------

    /// Prove the top-`k` entries of the sum axis.
    #[cfg(feature = "minimal")]
    pub fn prove_indexed_sum_top_k<'b, B, P>(
        &self,
        path: P,
        k: u16,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_top_k(
            path,
            IndexAxis::Sum,
            k,
            descending,
            transaction,
            grove_version,
        )
    }

    /// Prove an offset-paginated top-`k` window on the sum axis. The
    /// secondary is a `ProvableCountProvableSumTree`, so the skipped
    /// prefix is attested by counted subtree commitments and the proof
    /// size is O(log n + k) regardless of `offset`.
    #[cfg(feature = "minimal")]
    pub fn prove_indexed_sum_top_k_paginated<'b, B, P>(
        &self,
        path: P,
        k: u16,
        offset: u64,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_top_k_paginated(
            path,
            IndexAxis::Sum,
            k,
            offset,
            descending,
            transaction,
            grove_version,
        )
    }

    /// Prove an arbitrary query against the sum-axis secondary.
    #[cfg(feature = "minimal")]
    pub fn prove_indexed_sum_query<'b, B, P>(
        &self,
        path: P,
        secondary_query: MerkQuery,
        limit: Option<u16>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_query(
            path,
            IndexAxis::Sum,
            secondary_query,
            limit,
            transaction,
            grove_version,
        )
    }

    /// Prove the aggregate sum of entries whose `sum_value` is in
    /// `[lo_sum, hi_sum]`.
    #[cfg(feature = "minimal")]
    pub fn prove_indexed_sum_aggregate_over_value_range<'b, B, P>(
        &self,
        path: P,
        lo_sum: i64,
        hi_sum: i64,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_aggregate_over_value_range(
            path,
            IndexAxis::Sum,
            lo_sum as i128,
            hi_sum as i128,
            AggregateFold::Total,
            transaction,
            grove_version,
        )
    }

    /// Verify a sum-axis top-k proof.
    pub fn verify_indexed_sum_top_k(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_k: u16,
        expected_descending: bool,
        grove_version: &GroveVersion,
    ) -> Result<IndexedAxisQueryResult, Error> {
        Self::verify_indexed_axis_top_k(
            proof_bytes,
            path,
            IndexAxis::Sum,
            expected_k,
            expected_descending,
            grove_version,
        )
    }

    /// Verify a sum-axis paginated proof.
    pub fn verify_indexed_sum_top_k_paginated(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_k: u16,
        expected_offset: u64,
        expected_descending: bool,
        grove_version: &GroveVersion,
    ) -> Result<IndexedAxisPaginatedResult, Error> {
        Self::verify_indexed_axis_top_k_paginated(
            proof_bytes,
            path,
            IndexAxis::Sum,
            expected_k,
            expected_offset,
            expected_descending,
            grove_version,
        )
    }

    /// Verify a sum-axis arbitrary-query proof.
    pub fn verify_indexed_sum_query(
        proof_bytes: &[u8],
        path: &[&[u8]],
        secondary_query: MerkQuery,
        expected_limit: Option<u16>,
        grove_version: &GroveVersion,
    ) -> Result<IndexedAxisQueryResult, Error> {
        Self::verify_indexed_axis_query(
            proof_bytes,
            path,
            IndexAxis::Sum,
            secondary_query,
            expected_limit,
            grove_version,
        )
    }

    /// Verify a sum-axis aggregate proof.
    pub fn verify_indexed_sum_aggregate_over_value_range(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_lo_sum: i64,
        expected_hi_sum: i64,
        grove_version: &GroveVersion,
    ) -> Result<IndexedAxisAggregateResult, Error> {
        Self::verify_indexed_axis_aggregate_over_value_range(
            proof_bytes,
            path,
            IndexAxis::Sum,
            expected_lo_sum as i128,
            expected_hi_sum as i128,
            AggregateFold::Total,
            grove_version,
        )
    }

    // ---------- avg axis ----------

    /// Prove the top-`k` entries of the avg axis. PCPSIT-only. No
    /// aggregate variant exists — averaging an average over a range is
    /// not closed-form.
    #[cfg(feature = "minimal")]
    pub fn prove_indexed_avg_top_k<'b, B, P>(
        &self,
        path: P,
        k: u16,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_top_k(
            path,
            IndexAxis::Avg,
            k,
            descending,
            transaction,
            grove_version,
        )
    }

    /// Prove an offset-paginated top-`k` window on the avg axis.
    #[cfg(feature = "minimal")]
    pub fn prove_indexed_avg_top_k_paginated<'b, B, P>(
        &self,
        path: P,
        k: u16,
        offset: u64,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_top_k_paginated(
            path,
            IndexAxis::Avg,
            k,
            offset,
            descending,
            transaction,
            grove_version,
        )
    }

    /// Prove an arbitrary query against the avg-axis secondary.
    #[cfg(feature = "minimal")]
    pub fn prove_indexed_avg_query<'b, B, P>(
        &self,
        path: P,
        secondary_query: MerkQuery,
        limit: Option<u16>,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<Vec<u8>, Error>
    where
        B: AsRef<[u8]> + 'b,
        P: Into<SubtreePath<'b, B>>,
    {
        self.prove_indexed_axis_query(
            path,
            IndexAxis::Avg,
            secondary_query,
            limit,
            transaction,
            grove_version,
        )
    }

    /// Verify an avg-axis top-k proof.
    pub fn verify_indexed_avg_top_k(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_k: u16,
        expected_descending: bool,
        grove_version: &GroveVersion,
    ) -> Result<IndexedAxisQueryResult, Error> {
        Self::verify_indexed_axis_top_k(
            proof_bytes,
            path,
            IndexAxis::Avg,
            expected_k,
            expected_descending,
            grove_version,
        )
    }

    /// Verify an avg-axis paginated proof.
    pub fn verify_indexed_avg_top_k_paginated(
        proof_bytes: &[u8],
        path: &[&[u8]],
        expected_k: u16,
        expected_offset: u64,
        expected_descending: bool,
        grove_version: &GroveVersion,
    ) -> Result<IndexedAxisPaginatedResult, Error> {
        Self::verify_indexed_axis_top_k_paginated(
            proof_bytes,
            path,
            IndexAxis::Avg,
            expected_k,
            expected_offset,
            expected_descending,
            grove_version,
        )
    }

    /// Verify an avg-axis arbitrary-query proof.
    pub fn verify_indexed_avg_query(
        proof_bytes: &[u8],
        path: &[&[u8]],
        secondary_query: MerkQuery,
        expected_limit: Option<u16>,
        grove_version: &GroveVersion,
    ) -> Result<IndexedAxisQueryResult, Error> {
        Self::verify_indexed_axis_query(
            proof_bytes,
            path,
            IndexAxis::Avg,
            secondary_query,
            expected_limit,
            grove_version,
        )
    }
}
