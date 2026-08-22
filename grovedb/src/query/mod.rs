//! Queries

pub mod aggregate_sum_path_query;
pub(crate) mod axis_lowering;
mod grove_branch_query_result;
mod grove_trunk_query_result;
mod path_branch_chunk_query;
mod path_trunk_chunk_query;
pub(crate) mod shape;

use std::{
    borrow::{Cow, Cow::Borrowed},
    cmp::Ordering,
    fmt,
};

use bincode::{Decode, Encode};
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use grove_branch_query_result::GroveBranchQueryResult;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use grove_trunk_query_result::{GroveTrunkQueryResult, LeafInfo};
#[cfg(any(feature = "minimal", feature = "verify"))]
use grovedb_merk::proofs::query::query_item::QueryItem;
use grovedb_merk::proofs::query::{
    AggregateFold, AxisQuery, IndexAxis, Key, ReadMode, SubqueryBranch, SumBudgetRead,
};

use grovedb_merk::proofs::Query;
use grovedb_version::{check_grovedb_v0, version::GroveVersion};
use indexmap::IndexMap;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use path_branch_chunk_query::PathBranchChunkQuery;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use path_trunk_chunk_query::PathTrunkChunkQuery;
#[cfg(any(feature = "minimal", feature = "verify"))]
pub use shape::{AggregateKind, PathQueryShape};

use crate::operations::proof::util::hex_to_ascii;

use crate::query_result_type::PathKey;

use crate::Error;

#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
/// Path query
///
/// Represents a path to a specific GroveDB tree and a corresponding query to
/// apply to the given tree.
pub struct PathQuery {
    /// Path
    pub path: Vec<Vec<u8>>,
    /// Query
    pub query: SizedQuery,
}

impl fmt::Display for PathQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PathQuery {{ path: [")?;
        for (i, path_element) in self.path.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{}", hex_to_ascii(path_element))?;
        }
        write!(f, "], query: {} }}", self.query)
    }
}

#[derive(Debug, Clone, PartialEq, Encode, Decode)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
/// Holds a query to apply to a tree and an optional limit/offset value.
/// Limit and offset values affect the size of the result set.
pub struct SizedQuery {
    /// Query
    pub query: Query,
    /// Limit
    pub limit: Option<u16>,
    /// Offset
    pub offset: Option<u16>,
}

impl fmt::Display for SizedQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SizedQuery {{ query: {}", self.query)?;
        if let Some(limit) = self.limit {
            write!(f, ", limit: {}", limit)?;
        }
        if let Some(offset) = self.offset {
            write!(f, ", offset: {}", offset)?;
        }
        write!(f, " }}")
    }
}

impl SizedQuery {
    /// New sized query
    pub const fn new(query: Query, limit: Option<u16>, offset: Option<u16>) -> Self {
        Self {
            query,
            limit,
            offset,
        }
    }

    /// New sized query with one key
    pub fn new_single_key(key: Vec<u8>) -> Self {
        Self {
            query: Query::new_single_key(key),
            limit: None,
            offset: None,
        }
    }

    /// New sized query with one key
    pub fn new_single_query_item(query_item: QueryItem) -> Self {
        Self {
            query: Query::new_single_query_item(query_item),
            limit: None,
            offset: None,
        }
    }

    /// Validates that this sized query is a well-formed
    /// `AggregateCountOnRange` query in either the **leaf** or **carrier**
    /// shape. On success, returns a reference to the leaf inner range item
    /// (the `QueryItem` wrapped by the underlying `AggregateCountOnRange`,
    /// whether at this level for leaf queries or inside the
    /// `default_subquery_branch.subquery` for carrier queries).
    ///
    /// This is the `SizedQuery`-level entry point: it forwards to
    /// [`Query::validate_aggregate_count_on_range`] and additionally
    /// enforces the appropriate per-shape size-constraint rules:
    ///
    /// - **Leaf** shape (single `AggregateCountOnRange(_)` item, no
    ///   subqueries): both `SizedQuery::limit` and `SizedQuery::offset`
    ///   are rejected. A leaf returns a single `u64`; pagination would
    ///   silently change the answer.
    /// - **Carrier** shape (outer `Key`/`Range*` items routing to a leaf
    ///   `AggregateCountOnRange` subquery): `SizedQuery::limit` is
    ///   **allowed** and caps the number of outer-key matches the
    ///   carrier walks (each matched outer key still produces a complete
    ///   leaf-ACOR `u64`). `SizedQuery::offset` is still rejected —
    ///   skipping outer matches changes which `(outer_key, u64)` pairs
    ///   end up in the proof, and the use case for that hasn't been
    ///   designed yet.
    pub fn validate_aggregate_count_on_range(&self) -> Result<&QueryItem, Error> {
        // Inner classification first, then per-shape size-constraint
        // check. Queries that aren't aggregate-count at all (neither leaf
        // nor carrier) fall through to the Query-level validator below,
        // which surfaces the canonical "no aggregate-count item" error.
        if self.query.aggregate_count_on_range().is_some() {
            self.check_leaf_aggregate_count_size_constraints()?;
        } else if self.query.has_aggregate_count_on_range_anywhere() {
            self.check_carrier_aggregate_count_size_constraints()?;
        }
        self.query
            .validate_aggregate_count_on_range()
            .map_err(query_validation_error_to_static_str)
            .map_err(Error::InvalidQuery)
    }

    /// Strict variant of [`Self::validate_aggregate_count_on_range`] that
    /// only accepts the **leaf** shape (single `AggregateCountOnRange(_)`
    /// item, no subqueries). Used by entry points that produce a single
    /// `u64` and need to reject the carrier shape up front. Pagination
    /// (`SizedQuery::limit` / `SizedQuery::offset`) is rejected — see
    /// [`Self::check_leaf_aggregate_count_size_constraints`].
    pub fn validate_leaf_aggregate_count_on_range(&self) -> Result<&QueryItem, Error> {
        self.check_leaf_aggregate_count_size_constraints()?;
        self.query
            .validate_leaf_aggregate_count_on_range()
            .map_err(query_validation_error_to_static_str)
            .map_err(Error::InvalidQuery)
    }

    /// Size-constraint check used for **leaf** `AggregateCountOnRange`
    /// queries. A leaf returns a single `u64`; setting `limit` or
    /// `offset` would silently change the answer, so both are rejected.
    fn check_leaf_aggregate_count_size_constraints(&self) -> Result<(), Error> {
        if self.limit.is_some() {
            return Err(Error::InvalidQuery(
                "leaf AggregateCountOnRange queries may not set SizedQuery::limit — a leaf \
                 returns a single u64 and pagination would silently change the answer",
            ));
        }
        if self.offset.is_some() {
            return Err(Error::InvalidQuery(
                "leaf AggregateCountOnRange queries may not set SizedQuery::offset — same \
                 reason as limit",
            ));
        }
        Ok(())
    }

    /// Size-constraint check used for **carrier** `AggregateCountOnRange`
    /// queries. `SizedQuery::limit` is allowed and caps the number of
    /// outer-key matches the carrier walks (each matched outer key still
    /// produces a complete leaf-ACOR `u64`; the inner range is *not*
    /// capped). `SizedQuery::offset` is still rejected — paginating into
    /// the outer dimension changes which `(outer_key, u64)` pairs end up
    /// in the proof, and the use case for that hasn't been designed yet.
    fn check_carrier_aggregate_count_size_constraints(&self) -> Result<(), Error> {
        if self.offset.is_some() {
            return Err(Error::InvalidQuery(
                "carrier AggregateCountOnRange queries may not set SizedQuery::offset — \
                 skipping outer matches changes which (outer_key, u64) pairs end up in the \
                 proof; the use case for this isn't designed yet",
            ));
        }
        Ok(())
    }

    /// Validates that this `SizedQuery` is a well-formed offset-paginated
    /// range query against a `ProvableCountTree` / `ProvableCountSumTree` /
    /// `ProvableCountProvableSumTree`. On success returns a reference
    /// to the single range `QueryItem`.
    ///
    /// Eligibility rules (all required):
    ///
    /// - `offset.is_some() && offset != Some(0)` — there must actually be
    ///   an offset to honor. (Queries with offset = `None` / `Some(0)`
    ///   take the regular proof path, which already handles them.)
    /// - The underlying `Query` has exactly one item, and that item is a
    ///   plain range (`Range`, `RangeInclusive`, `RangeFrom`, `RangeFull`,
    ///   `RangeTo`, `RangeToInclusive`, or `RangeAfter*`). `QueryItem::Key`
    ///   is explicitly rejected — it matches at most one element, so any
    ///   offset > 0 is structurally guaranteed to return zero items.
    ///   Aggregate-count / aggregate-sum wrappers are rejected — they
    ///   have their own paginated semantics.
    /// - No subqueries (`default_subquery_branch.subquery.is_none()` and
    ///   `conditional_subquery_branches.is_empty()`). Pagination across
    ///   subqueries is out of scope for the initial PR.
    ///
    /// The tree-type check (`ProvableCountTree` / `ProvableCountSumTree` /
    /// `ProvableCountProvableSumTree`) happens later, at proof generation
    /// time, because it requires opening the merk. This function is
    /// purely syntactic.
    pub fn validate_count_offset_paginated(&self) -> Result<&QueryItem, Error> {
        // Must actually be paginated.
        if !matches!(self.offset, Some(o) if o > 0) {
            return Err(Error::InvalidQuery(
                "count-offset paginated queries must set SizedQuery::offset to a non-zero value",
            ));
        }
        // Reject queries that already have aggregate wrappers — they
        // have separate pagination semantics.
        if self.query.has_aggregate_count_on_range_anywhere() {
            return Err(Error::InvalidQuery(
                "count-offset paginated queries cannot wrap AggregateCountOnRange",
            ));
        }
        if self.query.has_aggregate_sum_on_range_anywhere() {
            return Err(Error::InvalidQuery(
                "count-offset paginated queries cannot wrap AggregateSumOnRange",
            ));
        }
        if self.query.has_aggregate_count_and_sum_on_range_anywhere() {
            return Err(Error::InvalidQuery(
                "count-offset paginated queries cannot wrap AggregateCountAndSumOnRange",
            ));
        }
        // Reject subqueries. We support a single-range scan only.
        if self.query.default_subquery_branch.subquery.is_some()
            || self.query.default_subquery_branch.subquery_path.is_some()
        {
            return Err(Error::InvalidQuery(
                "count-offset paginated queries cannot have a default subquery branch",
            ));
        }
        if let Some(branches) = self.query.conditional_subquery_branches.as_ref()
            && !branches.is_empty()
        {
            return Err(Error::InvalidQuery(
                "count-offset paginated queries cannot have conditional subquery branches",
            ));
        }
        // Must be exactly one range item.
        if self.query.items.len() != 1 {
            return Err(Error::InvalidQuery(
                "count-offset paginated queries must consist of exactly one range QueryItem",
            ));
        }
        let item = &self.query.items[0];
        // Range-shaped variants are fine. `QueryItem::Key(_)` is
        // **rejected**: it matches at most one key, so an offset > 0
        // is structurally guaranteed to return zero items — pagination
        // semantics on a single-key match are nonsensical and almost
        // always a user error (the caller probably meant a range).
        // Returning an explicit `InvalidQuery` here is clearer than
        // silently producing an empty result.
        //
        // Aggregate wrappers were rejected earlier; the explicit
        // match-all-variants pattern below means adding a new
        // `QueryItem` variant elsewhere produces a compile-time visit
        // to this match.
        match item {
            QueryItem::Range(_)
            | QueryItem::RangeInclusive(_)
            | QueryItem::RangeFrom(_)
            | QueryItem::RangeFull(_)
            | QueryItem::RangeTo(_)
            | QueryItem::RangeToInclusive(_)
            | QueryItem::RangeAfter(_)
            | QueryItem::RangeAfterTo(_)
            | QueryItem::RangeAfterToInclusive(_) => Ok(item),
            QueryItem::Key(_) => Err(Error::InvalidQuery(
                "count-offset paginated queries do not support QueryItem::Key — a \
                 single-key match has at most one in-range item, so offset > 0 is \
                 guaranteed to return zero items. Use a range variant instead",
            )),
            QueryItem::AggregateCountOnRange(_)
            | QueryItem::AggregateSumOnRange(_)
            | QueryItem::AggregateCountAndSumOnRange(_) => Err(Error::InvalidQuery(
                "count-offset paginated queries cannot wrap an aggregate QueryItem",
            )),
        }
    }

    /// Mirror of [`Self::validate_aggregate_count_on_range`] for
    /// `AggregateSumOnRange`. Forwards to
    /// [`Query::validate_aggregate_sum_on_range`] and additionally
    /// enforces the appropriate per-shape size-constraint rules:
    ///
    /// - **Leaf** shape (single `AggregateSumOnRange(_)` item, no
    ///   subqueries): both `SizedQuery::limit` and `SizedQuery::offset`
    ///   are rejected. A leaf returns a single `i64`; pagination would
    ///   silently change the answer.
    /// - **Carrier** shape (outer `Key`/`Range*` items routing to a leaf
    ///   `AggregateSumOnRange` subquery): `SizedQuery::limit` is
    ///   **allowed** and caps the number of outer-key matches the
    ///   carrier walks (each matched outer key still produces a complete
    ///   leaf-ASOR `i64`). `SizedQuery::offset` is still rejected —
    ///   skipping outer matches changes which `(outer_key, i64)` pairs
    ///   end up in the proof, and the use case for that hasn't been
    ///   designed yet.
    pub fn validate_aggregate_sum_on_range(&self) -> Result<&QueryItem, Error> {
        // Inner classification first, then per-shape size-constraint
        // check. Queries that aren't aggregate-sum at all (neither leaf
        // nor carrier) fall through to the Query-level validator below,
        // which surfaces the canonical "no aggregate-sum item" error.
        if self.query.aggregate_sum_on_range().is_some() {
            self.check_leaf_aggregate_sum_size_constraints()?;
        } else if self.query.has_aggregate_sum_on_range_anywhere() {
            self.check_carrier_aggregate_sum_size_constraints()?;
        }
        self.query
            .validate_aggregate_sum_on_range()
            .map_err(sum_query_validation_error_to_static_str)
            .map_err(Error::InvalidQuery)
    }

    /// Strict variant of [`Self::validate_aggregate_sum_on_range`] that
    /// only accepts the **leaf** shape (single `AggregateSumOnRange(_)`
    /// item, no subqueries). Used by entry points that produce a single
    /// `i64` and need to reject the carrier shape up front. Pagination
    /// (`SizedQuery::limit` / `SizedQuery::offset`) is rejected — see
    /// [`Self::check_leaf_aggregate_sum_size_constraints`].
    pub fn validate_leaf_aggregate_sum_on_range(&self) -> Result<&QueryItem, Error> {
        self.check_leaf_aggregate_sum_size_constraints()?;
        self.query
            .validate_leaf_aggregate_sum_on_range()
            .map_err(sum_query_validation_error_to_static_str)
            .map_err(Error::InvalidQuery)
    }

    /// Size-constraint check used for **leaf** `AggregateSumOnRange`
    /// queries. A leaf returns a single `i64`; setting `limit` or
    /// `offset` would silently change the answer, so both are rejected.
    fn check_leaf_aggregate_sum_size_constraints(&self) -> Result<(), Error> {
        if self.limit.is_some() {
            return Err(Error::InvalidQuery(
                "leaf AggregateSumOnRange queries may not set SizedQuery::limit — a leaf \
                 returns a single i64 and pagination would silently change the answer",
            ));
        }
        if self.offset.is_some() {
            return Err(Error::InvalidQuery(
                "leaf AggregateSumOnRange queries may not set SizedQuery::offset — same \
                 reason as limit",
            ));
        }
        Ok(())
    }

    /// Size-constraint check used for **carrier** `AggregateSumOnRange`
    /// queries. `SizedQuery::limit` is allowed and caps the number of
    /// outer-key matches the carrier walks (each matched outer key still
    /// produces a complete leaf-ASOR `i64`; the inner range is *not*
    /// capped). `SizedQuery::offset` is still rejected — paginating into
    /// the outer dimension changes which `(outer_key, i64)` pairs end up
    /// in the proof, and the use case for that hasn't been designed yet.
    fn check_carrier_aggregate_sum_size_constraints(&self) -> Result<(), Error> {
        if self.offset.is_some() {
            return Err(Error::InvalidQuery(
                "carrier AggregateSumOnRange queries may not set SizedQuery::offset — \
                 skipping outer matches changes which (outer_key, i64) pairs end up in the \
                 proof; the use case for this isn't designed yet",
            ));
        }
        Ok(())
    }

    /// Mirror of [`Self::validate_aggregate_sum_on_range`] for the combined
    /// `AggregateCountAndSumOnRange` variant. Forwards to
    /// [`Query::validate_aggregate_count_and_sum_on_range`] and
    /// additionally enforces the appropriate per-shape size-constraint
    /// rules — same model as the sum side.
    pub fn validate_aggregate_count_and_sum_on_range(&self) -> Result<&QueryItem, Error> {
        if self.query.aggregate_count_and_sum_on_range().is_some() {
            self.check_leaf_aggregate_count_and_sum_size_constraints()?;
        } else if self.query.has_aggregate_count_and_sum_on_range_anywhere() {
            self.check_carrier_aggregate_count_and_sum_size_constraints()?;
        }
        self.query
            .validate_aggregate_count_and_sum_on_range()
            .map_err(count_and_sum_query_validation_error_to_static_str)
            .map_err(Error::InvalidQuery)
    }

    /// Strict variant of
    /// [`Self::validate_aggregate_count_and_sum_on_range`] that only
    /// accepts the **leaf** shape. Used by entry points that produce a
    /// single `(u64, i64)` and need to reject the carrier shape up front.
    pub fn validate_leaf_aggregate_count_and_sum_on_range(&self) -> Result<&QueryItem, Error> {
        self.check_leaf_aggregate_count_and_sum_size_constraints()?;
        self.query
            .validate_leaf_aggregate_count_and_sum_on_range()
            .map_err(count_and_sum_query_validation_error_to_static_str)
            .map_err(Error::InvalidQuery)
    }

    /// Size-constraint check used for **leaf**
    /// `AggregateCountAndSumOnRange` queries. A leaf returns a single
    /// `(u64, i64)` pair; setting `limit` or `offset` would silently
    /// change both answers, so both are rejected.
    fn check_leaf_aggregate_count_and_sum_size_constraints(&self) -> Result<(), Error> {
        if self.limit.is_some() {
            return Err(Error::InvalidQuery(
                "leaf AggregateCountAndSumOnRange queries may not set SizedQuery::limit — a \
                 leaf returns a single (u64, i64) and pagination would silently change both \
                 answers",
            ));
        }
        if self.offset.is_some() {
            return Err(Error::InvalidQuery(
                "leaf AggregateCountAndSumOnRange queries may not set SizedQuery::offset — \
                 same reason as limit",
            ));
        }
        Ok(())
    }

    /// Size-constraint check used for **carrier**
    /// `AggregateCountAndSumOnRange` queries. `SizedQuery::limit` is
    /// allowed and caps the number of outer-key matches the carrier
    /// walks (each matched outer key still produces a complete
    /// leaf-ACASOR `(u64, i64)`; the inner range is *not* capped).
    /// `SizedQuery::offset` is still rejected.
    fn check_carrier_aggregate_count_and_sum_size_constraints(&self) -> Result<(), Error> {
        if self.offset.is_some() {
            return Err(Error::InvalidQuery(
                "carrier AggregateCountAndSumOnRange queries may not set SizedQuery::offset — \
                 skipping outer matches changes which (outer_key, u64, i64) triples end up in \
                 the proof; the use case for this isn't designed yet",
            ));
        }
        Ok(())
    }
}

/// Converts an aggregate-count-validation error into a `&'static str`.
/// Validation only ever returns
/// `grovedb_query::error::Error::InvalidOperation(&'static str)`, so this is
/// just a projection of that variant; any other error variant (which would
/// indicate an unrelated bug) is forwarded as a generic catch-all label.
pub(crate) fn query_validation_error_to_static_str(e: grovedb_query::error::Error) -> &'static str {
    match e {
        grovedb_query::error::Error::InvalidOperation(msg) => msg,
        _ => "AggregateCountOnRange query validation failed",
    }
}

/// Sum-side mirror of [`query_validation_error_to_static_str`]. Same
/// projection contract; only the catch-all label differs so logs and
/// error surfaces stay self-describing per-aggregate-variant.
pub(crate) fn sum_query_validation_error_to_static_str(
    e: grovedb_query::error::Error,
) -> &'static str {
    match e {
        grovedb_query::error::Error::InvalidOperation(msg) => msg,
        _ => "AggregateSumOnRange query validation failed",
    }
}

/// Combined-variant mirror of [`query_validation_error_to_static_str`].
/// Same projection contract; only the catch-all label differs.
pub(crate) fn count_and_sum_query_validation_error_to_static_str(
    e: grovedb_query::error::Error,
) -> &'static str {
    match e {
        grovedb_query::error::Error::InvalidOperation(msg) => msg,
        _ => "AggregateCountAndSumOnRange query validation failed",
    }
}

impl PathQuery {
    /// New path query
    pub const fn new(path: Vec<Vec<u8>>, query: SizedQuery) -> Self {
        Self { path, query }
    }

    /// New path query with a single key
    pub fn new_single_key(path: Vec<Vec<u8>>, key: Vec<u8>) -> Self {
        Self {
            path,
            query: SizedQuery::new_single_key(key),
        }
    }

    /// New path query with a single query item
    pub fn new_single_query_item(path: Vec<Vec<u8>>, query_item: QueryItem) -> Self {
        Self {
            path,
            query: SizedQuery::new_single_query_item(query_item),
        }
    }

    /// New unsized path query
    pub const fn new_unsized(path: Vec<Vec<u8>>, query: Query) -> Self {
        let query = SizedQuery::new(query, None, None);
        Self { path, query }
    }

    /// Construct a `PathQuery` for an aggregate-count-on-range query against
    /// the subtree at `path`. `range` is the inner `QueryItem` describing the
    /// keys to count over; see [`Query::new_aggregate_count_on_range`] for the
    /// allowed range variants.
    pub fn new_aggregate_count_on_range(path: Vec<Vec<u8>>, range: QueryItem) -> Self {
        Self::new_unsized(path, Query::new_aggregate_count_on_range(range))
    }

    /// Mirror of [`Self::new_aggregate_count_on_range`] for
    /// `AggregateSumOnRange`. Builds a `PathQuery` whose underlying query
    /// asks for the cryptographically-verifiable sum of children with keys
    /// in `range` against the `ProvableSumTree` rooted at `path`.
    pub fn new_aggregate_sum_on_range(path: Vec<Vec<u8>>, range: QueryItem) -> Self {
        Self::new_unsized(path, Query::new_aggregate_sum_on_range(range))
    }

    /// Mirror of [`Self::new_aggregate_count_on_range`] /
    /// [`Self::new_aggregate_sum_on_range`] for the combined
    /// `AggregateCountAndSumOnRange` variant. Builds a `PathQuery` whose
    /// underlying query asks for BOTH the count AND the signed sum of
    /// children with keys in `range` against the
    /// `ProvableCountProvableSumTree` (PCPS) rooted at `path` — both
    /// values come from a single proof.
    pub fn new_aggregate_count_and_sum_on_range(path: Vec<Vec<u8>>, range: QueryItem) -> Self {
        Self::new_unsized(path, Query::new_aggregate_count_and_sum_on_range(range))
    }

    /// A `Query` node whose whole read is `axis_query` — the terminal
    /// node of every axis-read shape.
    fn axis_read_node(axis_query: AxisQuery) -> Query {
        Query {
            read_mode: Some(Box::new(ReadMode::Axis(axis_query))),
            ..Query::new()
        }
    }

    /// An axis-ordered read of the indexed tree at `path`, from an
    /// already-built [`AxisQuery`] — use it to set a non-default
    /// projection (`AxisQuery::keys_only`); the typed constructors below
    /// cover the default cases.
    pub fn new_axis(path: Vec<Vec<u8>>, axis_query: AxisQuery) -> Self {
        Self::new_unsized(path, Self::axis_read_node(axis_query))
    }

    /// An axis-ordered read of the indexed tree at `path`: a page of
    /// `k` entries on `axis`, starting at rank `offset` (0 = first
    /// page).
    ///
    /// `descending` chooses which end the ranking starts from: `true`
    /// gives the `k` largest by aggregate (top-k), `false` the `k`
    /// smallest (bottom-k).
    pub fn new_axis_top_k(
        path: Vec<Vec<u8>>,
        axis: IndexAxis,
        k: u16,
        offset: u64,
        descending: bool,
    ) -> Self {
        Self::new_unsized(
            path,
            Self::axis_read_node(AxisQuery::top_k(axis, k, offset, descending)),
        )
    }

    /// An axis-ordered read of the indexed tree at `path`: every entry
    /// whose `axis` aggregate is in the inclusive `[lo, hi]`, up to
    /// `limit` entries.
    pub fn new_axis_bounded(
        path: Vec<Vec<u8>>,
        axis: IndexAxis,
        lo: i128,
        hi: i128,
        limit: u16,
        descending: bool,
    ) -> Self {
        Self::new_unsized(
            path,
            Self::axis_read_node(AxisQuery::bounded(axis, lo, hi, limit, descending)),
        )
    }

    /// The rank of `key` in the directional walk over `axis` of the
    /// indexed tree at `path`.
    pub fn new_axis_rank_of_key(
        path: Vec<Vec<u8>>,
        axis: IndexAxis,
        key: Vec<u8>,
        descending: bool,
    ) -> Self {
        Self::new_unsized(
            path,
            Self::axis_read_node(AxisQuery::rank_of_key(axis, key, descending)),
        )
    }

    /// `[lo, hi]` selects the entries of the indexed tree at `path` by
    /// their own `axis` value; `fold` says which scalar over exactly
    /// those entries is the answer. Count and Sum axes only.
    ///
    /// The fold is explicit because both readings are meaningful on
    /// both axes and the "obvious" one flips per axis. Over counts
    /// `[3, 1, 5]`, the band `[2, 10]` selects the `3` and the `5`:
    /// [`AggregateFold::Population`](grovedb_query::AggregateFold)
    /// answers `2`, [`AggregateFold::Total`](grovedb_query::AggregateFold)
    /// answers `8`. See
    /// [`AxisTraversal::AggregateOverValueRange`](grovedb_query::AxisTraversal::AggregateOverValueRange)
    /// for the full matrix; every (Count/Sum, fold) cell is served.
    pub fn new_axis_aggregate_over_value_range(
        path: Vec<Vec<u8>>,
        axis: IndexAxis,
        lo: i128,
        hi: i128,
        fold: AggregateFold,
    ) -> Self {
        Self::new_unsized(
            path,
            Self::axis_read_node(AxisQuery::aggregate_over_value_range(axis, lo, hi, fold)),
        )
    }

    /// The same axis read fanned over N sibling branches: for each key
    /// in `branch_keys` (selected under `prefix`), descend the shared
    /// `suffix` to an indexed tree and perform `axis_query` on it.
    ///
    /// This is the query form of the branched indexed-axis proof:
    /// `prefix / branch_key_i / suffix -> axis read`.
    pub fn new_branched_axis(
        prefix: Vec<Vec<u8>>,
        branch_keys: Vec<Vec<u8>>,
        suffix: Vec<Vec<u8>>,
        axis_query: AxisQuery,
    ) -> Self {
        let mut query = Query::new();
        for key in branch_keys {
            query.insert_key(key);
        }
        query.set_subquery_path(suffix);
        query.set_subquery(Self::axis_read_node(axis_query));
        Self::new_unsized(prefix, query)
    }

    /// A key-ordered read of `items` under `path` that stops once the
    /// running sum of matched sum-item values reaches `sum_limit` —
    /// the unified form of `AggregateSumPathQuery`.
    pub fn new_sum_budget(
        path: Vec<Vec<u8>>,
        items: Vec<QueryItem>,
        left_to_right: bool,
        sum_limit: u64,
        match_limit: Option<u16>,
    ) -> Self {
        let mut query = Query::new_with_direction(left_to_right);
        query.items = items;
        query.read_mode = Some(Box::new(ReadMode::SumBudget(SumBudgetRead {
            sum_limit,
            match_limit,
        })));
        Self::new_unsized(path, query)
    }

    /// Whether this query — at any nesting level — carries a
    /// [`ReadMode`]. Entry points that don't serve read modes use this
    /// to fail closed instead of silently running a read-mode query as
    /// plain key selection.
    pub fn has_read_mode(&self) -> bool {
        self.query.query.has_read_mode_anywhere()
    }

    /// Fail-closed gate for entry points that don't (yet) serve
    /// read-mode queries. Serving arrives with the unified read/prove
    /// dispatch; until then every existing entry point rejects rather
    /// than misreading an axis or sum-budget query as key selection.
    pub(crate) fn reject_unserved_read_mode(&self) -> Result<(), Error> {
        if self.has_read_mode() {
            Err(Error::NotSupported(
                "this entry point does not serve read-mode (axis / sum-budget) path queries"
                    .to_string(),
            ))
        } else {
            Ok(())
        }
    }

    /// Validates that this `PathQuery` is a well-formed
    /// `AggregateCountOnRange` query in either the leaf or carrier shape.
    /// On success, returns a reference to the leaf inner range item.
    ///
    /// Empty-path handling is **shape-aware**. The GroveDB root merk is
    /// always a `NormalTree` by API construction (never a
    /// `ProvableCountTree`), so a **leaf** aggregate-count query at the
    /// root has no valid target and is rejected up front. A **carrier**
    /// query, by contrast, may legitimately fan out across the root's
    /// top-level keys and descend (via `subquery_path` or directly) to
    /// leaf count merks at lower depths; empty-path carriers are
    /// permitted and the per-key verifier handles depth-0 execution.
    ///
    /// Forwards to [`SizedQuery::validate_aggregate_count_on_range`].
    pub fn validate_aggregate_count_on_range(&self) -> Result<&QueryItem, Error> {
        // Reject empty path only for the leaf shape — carrier shape can
        // legitimately have the root merk as the outer fan-out layer.
        // We must classify before validating because the leaf-shape
        // rejection's semantics depend on knowing the shape.
        if self.path.is_empty() && self.query.query.aggregate_count_on_range().is_some() {
            return Err(Error::InvalidQuery(
                "AggregateCountOnRange leaf queries may not target the root \
                 merk: the GroveDB root is always a NormalTree, never a \
                 ProvableCountTree / ProvableCountSumTree, so a leaf count \
                 aggregate at the root layer has no valid target. Carrier \
                 queries (outer fan-out + subquery descent) may target the \
                 root merk; use verify_aggregate_count_query_per_key.",
            ));
        }
        self.query.validate_aggregate_count_on_range()
    }

    /// Validates that this `PathQuery` is a well-formed
    /// `AggregateSumOnRange` query in either the leaf or carrier shape.
    /// On success, returns a reference to the leaf inner range item.
    ///
    /// Empty-path handling is **shape-aware**. The GroveDB root merk is
    /// always a `NormalTree`, never a `ProvableSumTree`, so a **leaf**
    /// aggregate-sum query at the root has no valid target and is
    /// rejected up front. A **carrier** query may legitimately fan out
    /// across the root's top-level keys and descend (via `subquery_path`
    /// or directly) to leaf sum merks at lower depths; empty-path
    /// carriers are permitted and the per-key verifier handles depth-0
    /// execution. Forwards to [`SizedQuery::validate_aggregate_sum_on_range`].
    pub fn validate_aggregate_sum_on_range(&self) -> Result<&QueryItem, Error> {
        if self.path.is_empty() && self.query.query.aggregate_sum_on_range().is_some() {
            return Err(Error::InvalidQuery(
                "AggregateSumOnRange leaf queries may not target the root \
                 merk: the GroveDB root is always a NormalTree, never a \
                 ProvableSumTree, so a leaf sum aggregate at the root layer \
                 has no valid target. Carrier queries (outer fan-out + \
                 subquery descent) may target the root merk; use \
                 verify_aggregate_sum_query_per_key.",
            ));
        }
        self.query.validate_aggregate_sum_on_range()
    }

    /// Validates that this `PathQuery` is a well-formed
    /// `AggregateCountAndSumOnRange` query in either the leaf or carrier
    /// shape. On success, returns a reference to the leaf inner range
    /// item.
    ///
    /// Empty-path handling is **shape-aware**. The GroveDB root merk is
    /// always a `NormalTree`, never a `ProvableCountProvableSumTree`, so
    /// a **leaf** combined aggregate at the root has no valid target and
    /// is rejected up front. A **carrier** query may legitimately fan
    /// out across the root's top-level keys and descend (via
    /// `subquery_path` or directly) to a PCPS leaf merk at a lower
    /// depth; empty-path carriers are permitted and the per-key verifier
    /// handles depth-0 execution. Forwards to
    /// [`SizedQuery::validate_aggregate_count_and_sum_on_range`].
    pub fn validate_aggregate_count_and_sum_on_range(&self) -> Result<&QueryItem, Error> {
        if self.path.is_empty()
            && self
                .query
                .query
                .aggregate_count_and_sum_on_range()
                .is_some()
        {
            return Err(Error::InvalidQuery(
                "AggregateCountAndSumOnRange leaf queries may not target the \
                 root merk: the GroveDB root is always a NormalTree, never a \
                 ProvableCountProvableSumTree, so a leaf combined count+sum \
                 aggregate at the root layer has no valid target. Carrier \
                 queries (outer fan-out + subquery descent) may target the \
                 root merk; use verify_aggregate_count_and_sum_query_per_key.",
            ));
        }
        self.query.validate_aggregate_count_and_sum_on_range()
    }

    /// Strict variant of [`Self::validate_aggregate_count_on_range`] that
    /// only accepts the **leaf** shape (single `AggregateCountOnRange(_)`
    /// item, no subqueries). Always rejects empty paths — the GroveDB
    /// root is always a `NormalTree`, never a count tree.
    pub fn validate_leaf_aggregate_count_on_range(&self) -> Result<&QueryItem, Error> {
        if self.path.is_empty() {
            return Err(Error::InvalidQuery(
                "AggregateCountOnRange leaf queries may not target the root \
                 merk: the GroveDB root is always a NormalTree, never a \
                 ProvableCountTree / ProvableCountSumTree, so a leaf count \
                 aggregate at the root layer has no valid target",
            ));
        }
        self.query.validate_leaf_aggregate_count_on_range()
    }

    /// Strict variant of [`Self::validate_aggregate_sum_on_range`] that
    /// only accepts the **leaf** shape (single `AggregateSumOnRange(_)`
    /// item, no subqueries). Used by
    /// [`crate::GroveDb::verify_aggregate_sum_query`] which produces a
    /// single `i64` and needs to reject the carrier shape up front.
    /// Always rejects empty paths.
    pub fn validate_leaf_aggregate_sum_on_range(&self) -> Result<&QueryItem, Error> {
        if self.path.is_empty() {
            return Err(Error::InvalidQuery(
                "AggregateSumOnRange leaf queries may not target the root \
                 merk: the GroveDB root is always a NormalTree, never a \
                 ProvableSumTree, so a leaf sum aggregate at the root layer \
                 has no valid target",
            ));
        }
        self.query.validate_leaf_aggregate_sum_on_range()
    }

    /// Strict variant of
    /// [`Self::validate_aggregate_count_and_sum_on_range`] that only
    /// accepts the **leaf** shape (single
    /// `AggregateCountAndSumOnRange(_)` item, no subqueries). Used by
    /// [`crate::GroveDb::verify_aggregate_count_and_sum_query`] which
    /// produces a single `(u64, i64)` and needs to reject the carrier
    /// shape up front. Always rejects empty paths.
    pub fn validate_leaf_aggregate_count_and_sum_on_range(&self) -> Result<&QueryItem, Error> {
        if self.path.is_empty() {
            return Err(Error::InvalidQuery(
                "AggregateCountAndSumOnRange leaf queries may not target the \
                 root merk: the GroveDB root is always a NormalTree, never a \
                 ProvableCountProvableSumTree, so a leaf combined count+sum \
                 aggregate at the root layer has no valid target",
            ));
        }
        self.query.validate_leaf_aggregate_count_and_sum_on_range()
    }

    /// Validates that this `PathQuery` is an offset-paginated range query
    /// against a `ProvableCountTree` / `ProvableCountSumTree` /
    /// `ProvableCountProvableSumTree`. Returns the single range
    /// `QueryItem` on success.
    ///
    /// The tree-type check happens later when the leaf merk is opened.
    /// This function is purely syntactic — it gates the *query shape*
    /// (single range, no subqueries, offset > 0). Forwards to
    /// [`SizedQuery::validate_count_offset_paginated`].
    ///
    /// Rejects empty paths up-front for the same reason as
    /// [`Self::validate_aggregate_count_on_range`]: the GroveDB root
    /// merk is always a `NormalTree`, never a count tree, so a
    /// root-level offset-paginated query has no valid target.
    pub fn validate_count_offset_paginated(&self) -> Result<&QueryItem, Error> {
        if self.path.is_empty() {
            return Err(Error::InvalidQuery(
                "count-offset paginated queries may not target the root merk: \
                 the GroveDB root is always a NormalTree, never a \
                 ProvableCountTree / ProvableCountSumTree / ProvableCountProvableSumTree",
            ));
        }
        self.query.validate_count_offset_paginated()
    }

    /// Returns `true` if this `PathQuery` has a non-zero offset set.
    /// Used to detect "the caller wants pagination" before deciding
    /// whether the query is eligible for the count-offset paginated
    /// proof flow.
    pub fn has_non_zero_offset(&self) -> bool {
        matches!(self.query.offset, Some(o) if o > 0)
    }

    /// Returns `true` if this `PathQuery`'s underlying query carries an
    /// `AggregateCountOnRange` item (whether well-formed or not). Use
    /// [`Self::validate_aggregate_count_on_range`] when you also need
    /// well-formedness.
    pub fn has_aggregate_count_on_range(&self) -> bool {
        self.query.query.aggregate_count_on_range().is_some()
    }

    /// Mirror of [`Self::has_aggregate_count_on_range`] for the sum variant.
    pub fn has_aggregate_sum_on_range(&self) -> bool {
        self.query.query.aggregate_sum_on_range().is_some()
    }

    /// Mirror of [`Self::has_aggregate_count_on_range`] for the combined
    /// `AggregateCountAndSumOnRange` variant.
    pub fn has_aggregate_count_and_sum_on_range(&self) -> bool {
        self.query
            .query
            .aggregate_count_and_sum_on_range()
            .is_some()
    }

    /// The max depth of the query, this is the maximum layers we could get back
    /// from grovedb
    /// If the max depth can not be calculated we get None
    /// This would occur if the recursion level was too high
    pub fn max_depth(&self) -> Option<u16> {
        self.query.query.max_depth()
    }

    /// Gets the path of all terminal keys
    ///
    /// Version dispatch — see the `terminal_keys` module in `grovedb-query`:
    /// v0 is the legacy walk frozen for `GROVE_V1`..`GROVE_V3`; v1
    /// (`GROVE_V4`+) resolves conditional subquery branches per queried item
    /// (issue #689).
    pub fn terminal_keys(
        &self,
        max_results: usize,
        grove_version: &GroveVersion,
    ) -> Result<Vec<PathKey>, Error> {
        let mut result: Vec<(Vec<Vec<u8>>, Vec<u8>)> = vec![];
        match grove_version
            .grovedb_versions
            .path_query_methods
            .terminal_keys
        {
            0 => self
                .query
                .query
                .terminal_keys_v0(self.path.clone(), max_results, &mut result),
            1 => self
                .query
                .query
                .terminal_keys_v1(self.path.clone(), max_results, &mut result),
            version => {
                return Err(Error::VersionError(
                    grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                        method: "terminal_keys".to_string(),
                        known_versions: vec![0, 1],
                        received: version,
                    },
                ))
            }
        }
        .map_err(Error::QueryError)?;
        Ok(result)
    }

    /// Combines multiple path queries into one equivalent path query
    pub fn merge(
        mut path_queries: Vec<&PathQuery>,
        grove_version: &GroveVersion,
    ) -> Result<Self, Error> {
        let merge_version = grove_version.grovedb_versions.path_query_methods.merge;
        if merge_version > 1 {
            return Err(Error::VersionError(
                grovedb_version::error::GroveVersionError::UnknownVersionMismatch {
                    method: "merge".to_string(),
                    known_versions: vec![0, 1],
                    received: merge_version,
                },
            ));
        }
        if path_queries.is_empty() {
            return Err(Error::InvalidInput(
                "merge function requires at least 1 path query",
            ));
        }
        // Read-mode queries do not merge (yet): the underlying
        // Query::merge_multiple machinery would silently drop the read
        // mode and mangle an axis or sum-budget read into key
        // selection. Merging sibling axis reads into the branched shape
        // arrives with explicit read-mode merge rules.
        if path_queries
            .iter()
            .any(|path_query| path_query.has_read_mode())
        {
            return Err(Error::NotSupported(
                "can not merge path queries carrying read modes (axis / sum-budget reads)"
                    .to_string(),
            ));
        }
        if path_queries.len() == 1 {
            return Ok(path_queries.remove(0).clone());
        }

        // Direction handling, version-gated. `merge` slot 0 (V1..V3)
        // keeps the long-standing behavior: input directions are
        // silently dropped (sub-level inputs end up under a synthesized
        // root whose direction is the default). Slot 1 (V4+) requires
        // every input to agree and propagates the shared direction to
        // the merged root. Merged queries feed proofs and the verifier
        // re-runs the same merge with the same grove version, so both
        // sides stay in agreement at every version.
        let shared_direction = path_queries[0].query.query.left_to_right;
        if merge_version >= 1
            && path_queries
                .iter()
                .any(|path_query| path_query.query.query.left_to_right != shared_direction)
        {
            return Err(Error::NotSupported(
                "can not merge path queries with conflicting directions (left_to_right \
                 differs); align the directions before merging"
                    .to_string(),
            ));
        }

        let (common_path, next_index) = PathQuery::get_common_path(&path_queries);

        let mut queries_for_common_path_this_level: Vec<Query> = vec![];

        let mut queries_for_common_path_sub_level: Vec<SubqueryBranch> = vec![];

        // convert all the paths after the common path to queries
        path_queries.into_iter().try_for_each(|path_query| {
            if path_query.query.offset.is_some() {
                return Err(Error::NotSupported(
                    "can not merge pathqueries with offsets".to_string(),
                ));
            }
            if path_query.query.limit.is_some() {
                return Err(Error::NotSupported(
                    "can not merge pathqueries with limits, consider setting the limit after the \
                     merge"
                        .to_string(),
                ));
            }
            path_query
                .to_subquery_branch_with_offset_start_index(next_index)
                .and_then(|unsized_path_query| {
                    if unsized_path_query.subquery_path.is_none() {
                        queries_for_common_path_this_level.push(
                            *unsized_path_query
                                .subquery
                                .ok_or(Error::CorruptedCodeExecution(
                                    "subquery must exist when subquery_path is none in merge",
                                ))?,
                        );
                    } else {
                        queries_for_common_path_sub_level.push(unsized_path_query);
                    }
                    Ok(())
                })
        })?;

        // Version-gated direction handling. The `merge` slot's `0`
        // (V1..V3) keeps the long-standing silent first-wins behavior;
        // `1` (V4+) requires every merged query to agree on
        // `left_to_right` and propagates it, erroring on conflict —
        // merged queries feed proofs, and the verifier re-runs the same
        // merge with the same grove version, so both sides stay in
        // agreement at every version.
        let mut merged_query = match merge_version {
            0 => Query::merge_multiple(queries_for_common_path_this_level)
                .map_err(|e| Error::NotSupported(e.to_string()))?,
            _ => Query::merge_multiple_directional(queries_for_common_path_this_level)
                .map_err(|e| Error::NotSupported(e.to_string()))?,
        };
        // add conditional subqueries
        for sub_path_query in queries_for_common_path_sub_level {
            let SubqueryBranch {
                subquery_path,
                subquery,
            } = sub_path_query;
            let mut subquery_path =
                subquery_path.ok_or(Error::CorruptedCodeExecution("subquery path must exist"))?;
            let key = subquery_path.remove(0); // must exist
            merged_query.insert_item(QueryItem::Key(key.clone()));
            let rest_of_path = if subquery_path.is_empty() {
                None
            } else {
                Some(subquery_path)
            };
            let subquery_branch = SubqueryBranch {
                subquery_path: rest_of_path,
                subquery,
            };
            // The read-mode gate at the top of `merge` already rejected
            // any input carrying one, so this cannot fire today —
            // propagate rather than discard, so a future path that
            // reaches here with a read mode surfaces it instead of
            // silently dropping the mode.
            merged_query
                .merge_conditional_boxed_subquery(QueryItem::Key(key), subquery_branch)
                .map_err(|e| Error::NotSupported(e.to_string()))?;
        }

        // V4+: the agreed direction travels to the merged root (it
        // would otherwise be lost whenever the inputs land at a sub
        // level under a synthesized root query).
        if merge_version >= 1 {
            merged_query.left_to_right = shared_direction;
        }

        Ok(PathQuery::new_unsized(common_path, merged_query))
    }

    /// Given a set of path queries, this returns an array of path keys that are
    /// common across all the path queries.
    /// Also returns the point at which they stopped being equal.
    fn get_common_path(path_queries: &[&PathQuery]) -> (Vec<Vec<u8>>, usize) {
        let min_path_length = path_queries
            .iter()
            .map(|path_query| path_query.path.len())
            .min()
            .expect("expect path_queries length to be 2 or more");

        let mut common_path = vec![];
        let mut level = 0;

        while level < min_path_length {
            let keys_at_level = path_queries
                .iter()
                .map(|path_query| &path_query.path[level])
                .collect::<Vec<_>>();
            let first_key = keys_at_level[0];

            let keys_are_uniform = keys_at_level.iter().all(|&curr_key| curr_key == first_key);

            if keys_are_uniform {
                common_path.push(first_key.to_vec());
                level += 1;
            } else {
                break;
            }
        }
        (common_path, level)
    }

    /// Given a path and a starting point, a query that is equivalent to the
    /// path is generated example: [a, b, c] =>
    ///     query a
    ///         cond a
    ///             query b
    ///                 cond b
    ///                    query c
    fn to_subquery_branch_with_offset_start_index(
        &self,
        start_index: usize,
    ) -> Result<SubqueryBranch, Error> {
        let path = &self.path;

        match path.len().cmp(&start_index) {
            Ordering::Equal => Ok(SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(self.query.query.clone())),
            }),
            Ordering::Less => Err(Error::CorruptedCodeExecution(
                "invalid start index for path query merge",
            )),
            _ => {
                let (_, remainder) = path.split_at(start_index);

                Ok(SubqueryBranch {
                    subquery_path: Some(remainder.to_vec()),
                    subquery: Some(Box::new(self.query.query.clone())),
                })
            }
        }
    }

    /// Returns whether the parent tree element should be included in results at the given path.
    pub fn should_add_parent_tree_at_path(
        &self,
        path: &[&[u8]],
        grove_version: &GroveVersion,
    ) -> Result<bool, Error> {
        check_grovedb_v0!(
            "should_add_parent_tree_at_path",
            grove_version
                .grovedb_versions
                .path_query_methods
                .should_add_parent_tree_at_path
        );

        fn recursive_should_add_parent_tree_at_path(query: &Query, path: &[&[u8]]) -> bool {
            if path.is_empty() {
                return query.add_parent_tree_on_subquery;
            }

            let key = path[0];
            let path_after_top_removed = &path[1..];

            if let Some(conditional_branches) = &query.conditional_subquery_branches {
                for (query_item, subquery_branch) in conditional_branches {
                    if query_item.contains(key) {
                        if let Some(subquery_path) = &subquery_branch.subquery_path {
                            if path_after_top_removed.len() <= subquery_path.len() {
                                if path_after_top_removed
                                    .iter()
                                    .zip(subquery_path)
                                    .all(|(a, b)| *a == b.as_slice())
                                {
                                    return if path_after_top_removed.len() == subquery_path.len() {
                                        subquery_branch.subquery.as_ref().is_some_and(|subquery| {
                                            subquery.add_parent_tree_on_subquery
                                        })
                                    } else {
                                        false
                                    };
                                }
                            } else if path_after_top_removed
                                .iter()
                                .take(subquery_path.len())
                                .zip(subquery_path)
                                .all(|(a, b)| *a == b.as_slice())
                                && let Some(subquery) = &subquery_branch.subquery
                            {
                                return recursive_should_add_parent_tree_at_path(
                                    subquery,
                                    &path_after_top_removed[subquery_path.len()..],
                                );
                            }
                        } else if let Some(subquery) = &subquery_branch.subquery {
                            return recursive_should_add_parent_tree_at_path(
                                subquery,
                                path_after_top_removed,
                            );
                        }

                        return false;
                    }
                }
            }

            if let Some(subquery_path) = &query.default_subquery_branch.subquery_path {
                if path_after_top_removed.len() <= subquery_path.len() {
                    if path_after_top_removed
                        .iter()
                        .zip(subquery_path)
                        .all(|(a, b)| *a == b.as_slice())
                    {
                        return if path_after_top_removed.len() == subquery_path.len() {
                            query
                                .default_subquery_branch
                                .subquery
                                .as_ref()
                                .is_some_and(|subquery| subquery.add_parent_tree_on_subquery)
                        } else {
                            false
                        };
                    }
                } else if path_after_top_removed
                    .iter()
                    .take(subquery_path.len())
                    .zip(subquery_path)
                    .all(|(a, b)| *a == b.as_slice())
                    && let Some(subquery) = &query.default_subquery_branch.subquery
                {
                    return recursive_should_add_parent_tree_at_path(
                        subquery,
                        &path_after_top_removed[subquery_path.len()..],
                    );
                }
            } else if let Some(subquery) = &query.default_subquery_branch.subquery {
                return recursive_should_add_parent_tree_at_path(subquery, path_after_top_removed);
            }

            false
        }

        let self_path_len = self.path.len();
        let given_path_len = path.len();

        Ok(match given_path_len.cmp(&self_path_len) {
            Ordering::Less => false,
            Ordering::Equal => {
                if path.iter().zip(&self.path).all(|(a, b)| *a == b.as_slice()) {
                    self.query.query.add_parent_tree_on_subquery
                } else {
                    false
                }
            }
            Ordering::Greater => {
                if !self.path.iter().zip(path).all(|(a, b)| a.as_slice() == *b) {
                    return Ok(false);
                }
                recursive_should_add_parent_tree_at_path(&self.query.query, &path[self_path_len..])
            }
        })
    }

    /// Returns the axis read governing the subtree at `path`, if the
    /// query node resolved at exactly that path carries
    /// [`ReadMode::Axis`]. `path` is a full path from the GroveDB root
    /// (the same convention as [`Self::query_items_at_path`]).
    ///
    /// This is how the proof walk — prover and verifier alike — learns
    /// that a layer is an axis-ordered read of an indexed tree rather
    /// than a key-selecting descent into its primary. Both sides
    /// resolve from the same query through this one function, so they
    /// cannot disagree about which layers are axis reads.
    ///
    /// Positions *inside* a subquery branch's `subquery_path` resolve
    /// to `None` (a read mode lives on a query node, never mid-path),
    /// as do paths that diverge from the query entirely.
    pub fn axis_read_at_path(&self, path: &[&[u8]]) -> Option<&AxisQuery> {
        match self.read_mode_at_path(path) {
            Some(ReadMode::Axis(axis_query)) => Some(axis_query),
            _ => None,
        }
    }

    /// The sum-budget read governing the subtree at `path`, if any —
    /// the sum-budget sibling of [`Self::axis_read_at_path`]. The
    /// budget's items live on the same node; callers re-resolve them
    /// from the query root (a sum-budget read only classifies at the
    /// root node).
    pub fn sum_budget_read_at_path(&self, path: &[&[u8]]) -> Option<&SumBudgetRead> {
        match self.read_mode_at_path(path) {
            Some(ReadMode::SumBudget(budget)) => Some(budget),
            _ => None,
        }
    }

    /// Returns the read mode of the query node resolved at exactly
    /// `path`, if any. `path` is a full path from the GroveDB root (the
    /// same convention as [`Self::query_items_at_path`]).
    ///
    /// This is how the proof walk — prover and verifier alike — learns
    /// that a layer is a read-mode layer rather than a key-selecting
    /// descent. Both sides resolve from the same query through this one
    /// function, so they cannot disagree about which layers carry read
    /// modes.
    fn read_mode_at_path(&self, path: &[&[u8]]) -> Option<&ReadMode> {
        /// Resolve the query NODE at exactly `path` below `query`,
        /// following conditional and default subquery branches the same
        /// way `query_items_at_path`'s resolver does — but returning
        /// the node itself rather than its per-layer view, and `None`
        /// for mid-`subquery_path` positions.
        fn resolve_node_at_path<'b>(query: &'b Query, path: &[&[u8]]) -> Option<&'b Query> {
            if path.is_empty() {
                return Some(query);
            }
            let key = path[0];
            let rest = &path[1..];

            if let Some(conditional_branches) = &query.conditional_subquery_branches {
                for (query_item, subquery_branch) in conditional_branches {
                    if query_item.contains(key) {
                        return resolve_branch_at_path(subquery_branch, rest);
                    }
                }
            }
            resolve_branch_at_path(&query.default_subquery_branch, rest)
        }

        fn resolve_branch_at_path<'b>(
            branch: &'b SubqueryBranch,
            rest: &[&[u8]],
        ) -> Option<&'b Query> {
            match &branch.subquery_path {
                Some(subquery_path) => {
                    if rest.len() < subquery_path.len() {
                        // Mid-subquery_path: no query node here.
                        return None;
                    }
                    if !rest
                        .iter()
                        .take(subquery_path.len())
                        .zip(subquery_path)
                        .all(|(a, b)| *a == b.as_slice())
                    {
                        return None;
                    }
                    let after = &rest[subquery_path.len()..];
                    branch
                        .subquery
                        .as_deref()
                        .and_then(|subquery| resolve_node_at_path(subquery, after))
                }
                None => branch
                    .subquery
                    .as_deref()
                    .and_then(|subquery| resolve_node_at_path(subquery, rest)),
            }
        }

        let self_path_len = self.path.len();
        if path.len() < self_path_len {
            // Above the query root: nothing can carry a read mode.
            return None;
        }
        if !self.path.iter().zip(path).all(|(a, b)| a.as_slice() == *b) {
            return None;
        }
        let node = resolve_node_at_path(&self.query.query, &path[self_path_len..])?;
        node.read_mode.as_deref()
    }

    /// Returns the query items applicable at the given path, if any.
    pub fn query_items_at_path(
        &self,
        path: &[&[u8]],
        grove_version: &GroveVersion,
    ) -> Result<Option<SinglePathSubquery<'_>>, Error> {
        check_grovedb_v0!(
            "query_items_at_path",
            grove_version
                .grovedb_versions
                .path_query_methods
                .query_items_at_path
        );
        fn recursive_query_items<'b>(
            query: &'b Query,
            path: &[&[u8]],
        ) -> Option<SinglePathSubquery<'b>> {
            if path.is_empty() {
                return Some(SinglePathSubquery::from_query(query));
            }

            let key = path[0];
            let path_after_top_removed = &path[1..];

            if let Some(conditional_branches) = &query.conditional_subquery_branches {
                for (query_item, subquery_branch) in conditional_branches {
                    if query_item.contains(key) {
                        if let Some(subquery_path) = &subquery_branch.subquery_path {
                            if path_after_top_removed.len() <= subquery_path.len() {
                                if path_after_top_removed
                                    .iter()
                                    .zip(subquery_path)
                                    .all(|(a, b)| *a == b.as_slice())
                                {
                                    return if path_after_top_removed.len() == subquery_path.len() {
                                        subquery_branch.subquery.as_ref().map(|subquery| {
                                            SinglePathSubquery::from_query(subquery)
                                        })
                                    } else {
                                        let last_path_item = path.len() == subquery_path.len();
                                        let has_subquery = subquery_branch.subquery.is_some();
                                        Some(SinglePathSubquery::from_key_when_in_path(
                                            &subquery_path[path_after_top_removed.len()],
                                            last_path_item,
                                            has_subquery,
                                        ))
                                    };
                                }
                            } else if path_after_top_removed
                                .iter()
                                .take(subquery_path.len())
                                .zip(subquery_path)
                                .all(|(a, b)| *a == b.as_slice())
                                && let Some(subquery) = &subquery_branch.subquery
                            {
                                return recursive_query_items(
                                    subquery,
                                    &path_after_top_removed[subquery_path.len()..],
                                );
                            }
                        } else if let Some(subquery) = &subquery_branch.subquery {
                            return recursive_query_items(subquery, path_after_top_removed);
                        }

                        return None;
                    }
                }
            }

            if let Some(subquery_path) = &query.default_subquery_branch.subquery_path {
                if path_after_top_removed.len() <= subquery_path.len() {
                    if path_after_top_removed
                        .iter()
                        .zip(subquery_path)
                        .all(|(a, b)| *a == b.as_slice())
                    {
                        // The paths are equal for example if we had a sub path of
                        // path : 1 / 2
                        // subquery : All items

                        // If we are asking what is the subquery when we are at 1 / 2
                        // we should get
                        return if path_after_top_removed.len() == subquery_path.len() {
                            query
                                .default_subquery_branch
                                .subquery
                                .as_ref()
                                .map(|subquery| SinglePathSubquery::from_query(subquery))
                        } else {
                            let last_path_item = path.len() == subquery_path.len();
                            let has_subquery = query.default_subquery_branch.subquery.is_some();
                            Some(SinglePathSubquery::from_key_when_in_path(
                                &subquery_path[path_after_top_removed.len()],
                                last_path_item,
                                has_subquery,
                            ))
                        };
                    }
                } else if path_after_top_removed
                    .iter()
                    .take(subquery_path.len())
                    .zip(subquery_path)
                    .all(|(a, b)| *a == b.as_slice())
                    && let Some(subquery) = &query.default_subquery_branch.subquery
                {
                    return recursive_query_items(
                        subquery,
                        &path_after_top_removed[subquery_path.len()..],
                    );
                }
            } else if let Some(subquery) = &query.default_subquery_branch.subquery {
                return recursive_query_items(subquery, path_after_top_removed);
            }

            None
        }

        let self_path_len = self.path.len();
        let given_path_len = path.len();

        Ok(match given_path_len.cmp(&self_path_len) {
            Ordering::Less => {
                if path.iter().zip(&self.path).all(|(a, b)| *a == b.as_slice()) {
                    Some(SinglePathSubquery::from_key_when_in_path(
                        &self.path[given_path_len],
                        false,
                        true,
                    ))
                } else {
                    None
                }
            }
            Ordering::Equal => {
                if path.iter().zip(&self.path).all(|(a, b)| *a == b.as_slice()) {
                    Some(SinglePathSubquery::from_path_query(self))
                } else {
                    None
                }
            }
            Ordering::Greater => {
                if !self.path.iter().zip(path).all(|(a, b)| a.as_slice() == *b) {
                    return Ok(None);
                }
                recursive_query_items(&self.query.query, &path[self_path_len..])
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum HasSubquery<'a> {
    NoSubquery,
    Always,
    Conditionally(Cow<'a, IndexMap<QueryItem, SubqueryBranch>>),
}

impl fmt::Display for HasSubquery<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HasSubquery::NoSubquery => write!(f, "NoSubquery"),
            HasSubquery::Always => write!(f, "Always"),
            HasSubquery::Conditionally(map) => {
                writeln!(f, "Conditionally {{")?;
                for (query_item, subquery_branch) in map.iter() {
                    writeln!(f, "  {query_item}: {subquery_branch},")?;
                }
                write!(f, "}}")
            }
        }
    }
}

impl HasSubquery<'_> {
    /// Checks to see if we have a subquery on a specific key
    pub fn has_subquery_on_key(&self, key: &[u8]) -> bool {
        match self {
            HasSubquery::NoSubquery => false,
            HasSubquery::Conditionally(conditionally) => conditionally
                .keys()
                .any(|query_item| query_item.contains(key)),
            HasSubquery::Always => true,
        }
    }
}

/// This represents a query where the items might be borrowed, it is used to get
/// subquery information

#[derive(Debug, Clone, PartialEq)]
pub struct SinglePathSubquery<'a> {
    /// Items
    #[allow(clippy::owned_cow)]
    pub items: Cow<'a, Vec<QueryItem>>,
    /// Default subquery branch
    pub has_subquery: HasSubquery<'a>,
    /// Left to right?
    pub left_to_right: bool,
    /// In the path of the path_query, or in a subquery path
    pub in_path: Option<Cow<'a, Key>>,
    /// True when this level was *synthesized* from a path component
    /// instead of resolved to a real query node — the `Ordering::Less`
    /// arm of [`PathQuery::query_items_at_path`] plus every
    /// mid-`subquery_path` arm, all of which go through
    /// [`SinglePathSubquery::from_key_when_in_path`].
    ///
    /// A synthesized level's `items` is exactly one `QueryItem::Key`,
    /// so its `left_to_right` carries no query semantics at all: the
    /// answer is that one key or nothing, and there is no ordering or
    /// limit interaction to observe. The field is a placeholder, fixed
    /// at `true`, because the direction the *generating* query used at
    /// this path — which is what decided the op family the prover
    /// emitted — is not recoverable from a subset query.
    ///
    /// Proof verifiers must therefore not take the stream's
    /// orientation from `left_to_right` on a synthesized level; they
    /// read it off the proof's own op family via
    /// `grovedb_merk::proofs::query::proof_stream_direction`, which
    /// `execute` independently pins to the stream's key ordering. Proof
    /// *generation* keeps using `left_to_right` verbatim, so proof
    /// bytes are unaffected.
    pub synthesized_path_component: bool,
}

impl fmt::Display for SinglePathSubquery<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "InternalCowItemsQuery {{")?;
        writeln!(f, "  items: [")?;
        for item in self.items.iter() {
            writeln!(f, "    {item},")?;
        }
        writeln!(f, "  ]")?;
        writeln!(f, "  has_subquery: {}", self.has_subquery)?;
        writeln!(f, "  left_to_right: {}", self.left_to_right)?;
        match &self.in_path {
            Some(path) => writeln!(f, "  in_path: Some({})", hex_to_ascii(path)),
            None => writeln!(f, "  in_path: None"),
        }?;
        writeln!(
            f,
            "  synthesized_path_component: {}",
            self.synthesized_path_component
        )?;
        write!(f, "}}")
    }
}

impl<'a> SinglePathSubquery<'a> {
    /// Checks to see if we have a subquery on a specific key
    pub fn has_subquery_or_matching_in_path_on_key(&self, key: &[u8]) -> bool {
        if self.has_subquery.has_subquery_on_key(key) {
            true
        } else if let Some(path) = self.in_path.as_ref() {
            path.as_slice() == key
        } else {
            false
        }
    }

    pub fn from_key_when_in_path(
        key: &'a Vec<u8>,
        subquery_is_last_path_item: bool,
        subquery_has_inner_subquery: bool,
    ) -> SinglePathSubquery<'a> {
        // in this case there should be no in_path, because we are trying to get this
        // level of items and nothing underneath
        let in_path = if subquery_is_last_path_item && !subquery_has_inner_subquery {
            None
        } else {
            Some(Borrowed(key))
        };
        SinglePathSubquery {
            items: Cow::Owned(vec![QueryItem::Key(key.clone())]),
            has_subquery: HasSubquery::NoSubquery,
            // Placeholder — see `synthesized_path_component`. Nothing
            // here knows which direction the generating query walked
            // this level in, and for a one-key level nothing needs to:
            // the direction is an encoding detail of the proof, which
            // is where verifiers read it from.
            left_to_right: true,
            in_path,
            synthesized_path_component: true,
        }
    }

    pub fn from_path_query(path_query: &PathQuery) -> SinglePathSubquery<'_> {
        Self::from_query(&path_query.query.query)
    }

    pub fn from_query(query: &Query) -> SinglePathSubquery<'_> {
        let has_subquery = if query.default_subquery_branch.subquery.is_some()
            || query.default_subquery_branch.subquery_path.is_some()
        {
            HasSubquery::Always
        } else if let Some(conditional) = query.conditional_subquery_branches.as_ref() {
            HasSubquery::Conditionally(Cow::Borrowed(conditional))
        } else {
            HasSubquery::NoSubquery
        };
        SinglePathSubquery {
            items: Cow::Borrowed(&query.items),
            has_subquery,
            left_to_right: query.left_to_right,
            in_path: None,
            synthesized_path_component: false,
        }
    }
}

#[cfg(feature = "minimal")]
#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::query::AggregateFold;
    use std::{borrow::Cow, ops::RangeFull};

    use bincode::{config::standard, decode_from_slice, encode_to_vec};
    use grovedb_merk::proofs::{
        query::{query_item::QueryItem, SubqueryBranch},
        Query,
    };
    use grovedb_version::version::GroveVersion;
    use indexmap::IndexMap;

    use crate::{
        query::{HasSubquery, SinglePathSubquery},
        query_result_type::QueryResultType,
        tests::{common::compare_result_tuples, make_deep_tree, TEST_LEAF},
        Element, Error, GroveDb, PathQuery, SizedQuery,
    };

    #[test]
    fn test_same_path_different_query_merge() {
        let grove_version = GroveVersion::latest();
        let temp_db = make_deep_tree(grove_version);

        // starting with no subquery, just a single path and a key query
        let mut query_one = Query::new();
        query_one.insert_key(b"key1".to_vec());
        let path_query_one =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_one);

        let proof = temp_db
            .prove_query(&path_query_one, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set_one) =
            GroveDb::verify_query_raw(proof.as_slice(), &path_query_one, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set_one.len(), 1);

        let mut query_two = Query::new();
        query_two.insert_key(b"key2".to_vec());
        let path_query_two =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_two);

        let proof = temp_db
            .prove_query(&path_query_two, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set_two) =
            GroveDb::verify_query_raw(proof.as_slice(), &path_query_two, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set_two.len(), 1);

        let merged_path_query =
            PathQuery::merge(vec![&path_query_one, &path_query_two], grove_version)
                .expect("should merge path queries");

        let proof = temp_db
            .prove_query(&merged_path_query, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set_tree) =
            GroveDb::verify_query_raw(proof.as_slice(), &merged_path_query, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set_tree.len(), 2);
    }

    #[test]
    fn test_different_same_length_path_with_different_query_merge() {
        let grove_version = GroveVersion::latest();
        // Tests for
        // [a, c, Q]
        // [a, m, Q]
        let temp_db = make_deep_tree(grove_version);

        let mut query_one = Query::new();
        query_one.insert_key(b"key1".to_vec());
        let path_query_one =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_one);

        let proof = temp_db
            .prove_query(&path_query_one, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set_one) =
            GroveDb::verify_query_raw(proof.as_slice(), &path_query_one, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set_one.len(), 1);

        let mut query_two = Query::new();
        query_two.insert_key(b"key4".to_vec());
        let path_query_two =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()], query_two);

        let proof = temp_db
            .prove_query(&path_query_two, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set_two) =
            GroveDb::verify_query_raw(proof.as_slice(), &path_query_two, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set_two.len(), 1);

        let merged_path_query =
            PathQuery::merge(vec![&path_query_one, &path_query_two], grove_version)
                .expect("expect to merge path queries");
        assert_eq!(merged_path_query.path, vec![TEST_LEAF.to_vec()]);
        assert_eq!(merged_path_query.query.query.items.len(), 2);

        let proof = temp_db
            .prove_query(&merged_path_query, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set_merged) =
            GroveDb::verify_query_raw(proof.as_slice(), &merged_path_query, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set_merged.len(), 2);

        let keys = [b"key1".to_vec(), b"key4".to_vec()];
        let values = [b"value1".to_vec(), b"value4".to_vec()];
        let elements = values.map(|x| Element::new_item(x).serialize(grove_version).unwrap());
        let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
        compare_result_tuples(result_set_merged, expected_result_set);

        // longer length path queries
        let mut query_one = Query::new();
        query_one.insert_all();
        let path_query_one = PathQuery::new_unsized(
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_1".to_vec(),
                b"deeper_2".to_vec(),
            ],
            query_one.clone(),
        );

        let proof = temp_db
            .prove_query(&path_query_one, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set_one) =
            GroveDb::verify_query_raw(proof.as_slice(), &path_query_one, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set_one.len(), 3);

        let mut query_two = Query::new();
        query_two.insert_all();

        let path_query_two = PathQuery::new_unsized(
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_2".to_vec(),
                b"deeper_4".to_vec(),
            ],
            query_two.clone(),
        );

        let proof = temp_db
            .prove_query(&path_query_two, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set_two) =
            GroveDb::verify_query_raw(proof.as_slice(), &path_query_two, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set_two.len(), 2);

        let mut query_three = Query::new();
        query_three.insert_range_after(b"key7".to_vec()..);

        let path_query_three = PathQuery::new_unsized(
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_2".to_vec(),
                b"deeper_3".to_vec(),
            ],
            query_three.clone(),
        );

        let proof = temp_db
            .prove_query(&path_query_three, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set_two) =
            GroveDb::verify_query_raw(proof.as_slice(), &path_query_three, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set_two.len(), 2);

        #[rustfmt::skip]
        mod explanation {

    // Tree Structure
    //                                   root
    //              /                      |                       \ (not representing Merk)
    // -----------------------------------------------------------------------------------------
    //         test_leaf            another_test_leaf                deep_leaf
    //       /           \             /         \              /                 \
    // -----------------------------------------------------------------------------------------
    //   innertree     innertree4  innertree2  innertree3  deep_node_1          deep_node_2
    //       |             |           |           |      /          \         /         \
    // -----------------------------------------------------------------------------------------
    //      k2,v2        k4,v4       k3,v3      k4,v4   deeper_1   deeper_2  deeper_3   deeper_4
    //     /     \         |                           |            |         |          |
    //  k1,v1    k3,v3   k5,v5                        /            /          |          |
    // -----------------------------------------------------------------------------------------
    //                                            k2,v2         k5,v5        k8,v8     k10,v10
    //                                           /     \        /    \       /    \       \
    //                                       k1,v1    k3,v3  k4,v4   k6,v6 k7,v7  k9,v9  k11,v11
    //                                                            ↑ (all 3)   ↑     (all 2) ↑
    //                                                      path_query_one    ↑   path_query_two
    //                                                                 path_query_three (2)
    //                                                                   (after 7, so {8,9})

        }

        let merged_path_query = PathQuery::merge(
            vec![&path_query_one, &path_query_two, &path_query_three],
            grove_version,
        )
        .expect("expect to merge path queries");
        assert_eq!(merged_path_query.path, vec![b"deep_leaf".to_vec()]);
        assert_eq!(merged_path_query.query.query.items.len(), 2);
        let conditional_subquery_branches = merged_path_query
            .query
            .query
            .conditional_subquery_branches
            .clone()
            .expect("expected to have conditional subquery branches");
        assert_eq!(conditional_subquery_branches.len(), 2);
        let (deep_node_1_query_item, deep_node_1_subquery_branch) =
            conditional_subquery_branches.first().unwrap();
        let (deep_node_2_query_item, deep_node_2_subquery_branch) =
            conditional_subquery_branches.last().unwrap();
        assert_eq!(
            deep_node_1_query_item,
            &QueryItem::Key(b"deep_node_1".to_vec())
        );
        assert_eq!(
            deep_node_2_query_item,
            &QueryItem::Key(b"deep_node_2".to_vec())
        );

        assert_eq!(
            deep_node_1_subquery_branch
                .subquery_path
                .as_ref()
                .expect("expected a subquery_path for deep_node_1"),
            &vec![b"deeper_2".to_vec()]
        );
        assert_eq!(
            *deep_node_1_subquery_branch
                .subquery
                .as_ref()
                .expect("expected a subquery for deep_node_1"),
            Box::new(query_one)
        );

        assert!(
            deep_node_2_subquery_branch.subquery_path.is_none(),
            "there should be no subquery path here"
        );
        let deep_node_2_subquery = deep_node_2_subquery_branch
            .subquery
            .as_ref()
            .expect("expected a subquery for deep_node_2")
            .as_ref();

        assert_eq!(deep_node_2_subquery.items.len(), 2);

        let deep_node_2_conditional_subquery_branches = deep_node_2_subquery
            .conditional_subquery_branches
            .as_ref()
            .expect("expected to have conditional subquery branches");
        assert_eq!(deep_node_2_conditional_subquery_branches.len(), 2);

        // deeper 4 was query 2
        let (deeper_4_query_item, deeper_4_subquery_branch) =
            deep_node_2_conditional_subquery_branches.first().unwrap();
        let (deeper_3_query_item, deeper_3_subquery_branch) =
            deep_node_2_conditional_subquery_branches.last().unwrap();

        assert_eq!(deeper_3_query_item, &QueryItem::Key(b"deeper_3".to_vec()));
        assert_eq!(deeper_4_query_item, &QueryItem::Key(b"deeper_4".to_vec()));

        assert!(
            deeper_3_subquery_branch.subquery_path.is_none(),
            "there should be no subquery path here"
        );
        assert_eq!(
            *deeper_3_subquery_branch
                .subquery
                .as_ref()
                .expect("expected a subquery for deeper_3"),
            Box::new(query_three)
        );

        assert!(
            deeper_4_subquery_branch.subquery_path.is_none(),
            "there should be no subquery path here"
        );
        assert_eq!(
            *deeper_4_subquery_branch
                .subquery
                .as_ref()
                .expect("expected a subquery for deeper_4"),
            Box::new(query_two)
        );

        let (result_set_merged, _) = temp_db
            .query_raw(
                &merged_path_query,
                true,
                true,
                true,
                QueryResultType::QueryPathKeyElementTrioResultType,
                None,
                grove_version,
            )
            .value
            .expect("expected to get results");
        assert_eq!(result_set_merged.len(), 7);

        let proof = temp_db
            .prove_query(&merged_path_query, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, proved_result_set_merged) =
            GroveDb::verify_query_raw(proof.as_slice(), &merged_path_query, grove_version)
                .expect("should execute proof");
        assert_eq!(proved_result_set_merged.len(), 7);

        let keys = [
            b"key4".to_vec(),
            b"key5".to_vec(),
            b"key6".to_vec(),
            b"key8".to_vec(),
            b"key9".to_vec(),
            b"key10".to_vec(),
            b"key11".to_vec(),
        ];
        let values = [
            b"value4".to_vec(),
            b"value5".to_vec(),
            b"value6".to_vec(),
            b"value8".to_vec(),
            b"value9".to_vec(),
            b"value10".to_vec(),
            b"value11".to_vec(),
        ];
        let elements = values.map(|x| Element::new_item(x).serialize(grove_version).unwrap());
        let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
        compare_result_tuples(proved_result_set_merged, expected_result_set);
    }

    #[test]
    fn test_different_length_paths_merge() {
        let grove_version = GroveVersion::latest();
        let temp_db = make_deep_tree(grove_version);

        let mut query_one = Query::new();
        query_one.insert_all();

        let mut subq = Query::new();
        subq.insert_all();
        query_one.set_subquery(subq);

        let path_query_one = PathQuery::new_unsized(
            vec![b"deep_leaf".to_vec(), b"deep_node_1".to_vec()],
            query_one,
        );

        let proof = temp_db
            .prove_query(&path_query_one, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set_one) =
            GroveDb::verify_query_raw(proof.as_slice(), &path_query_one, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set_one.len(), 6);

        let mut query_two = Query::new();
        query_two.insert_all();

        let path_query_two = PathQuery::new_unsized(
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_2".to_vec(),
                b"deeper_4".to_vec(),
            ],
            query_two,
        );

        let proof = temp_db
            .prove_query(&path_query_two, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set_two) =
            GroveDb::verify_query_raw(proof.as_slice(), &path_query_two, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set_two.len(), 2);

        let merged_path_query =
            PathQuery::merge(vec![&path_query_one, &path_query_two], grove_version)
                .expect("expect to merge path queries");
        assert_eq!(merged_path_query.path, vec![b"deep_leaf".to_vec()]);

        let proof = temp_db
            .prove_query(&merged_path_query, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set_merged) =
            GroveDb::verify_query_raw(proof.as_slice(), &merged_path_query, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set_merged.len(), 8);

        let keys = [
            b"key1".to_vec(),
            b"key2".to_vec(),
            b"key3".to_vec(),
            b"key4".to_vec(),
            b"key5".to_vec(),
            b"key6".to_vec(),
            b"key10".to_vec(),
            b"key11".to_vec(),
        ];
        let values = [
            b"value1".to_vec(),
            b"value2".to_vec(),
            b"value3".to_vec(),
            b"value4".to_vec(),
            b"value5".to_vec(),
            b"value6".to_vec(),
            b"value10".to_vec(),
            b"value11".to_vec(),
        ];
        let elements = values.map(|x| Element::new_item(x).serialize(grove_version).unwrap());
        let expected_result_set: Vec<(Vec<u8>, Vec<u8>)> = keys.into_iter().zip(elements).collect();
        compare_result_tuples(result_set_merged, expected_result_set);
    }

    #[test]
    fn test_same_path_and_different_path_query_merge() {
        let grove_version = GroveVersion::latest();
        let temp_db = make_deep_tree(grove_version);

        let mut query_one = Query::new();
        query_one.insert_key(b"key1".to_vec());
        let path_query_one =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_one);

        let proof = temp_db
            .prove_query(&path_query_one, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set) =
            GroveDb::verify_query_raw(proof.as_slice(), &path_query_one, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set.len(), 1);

        let mut query_two = Query::new();
        query_two.insert_key(b"key2".to_vec());
        let path_query_two =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_two);

        let proof = temp_db
            .prove_query(&path_query_two, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set) =
            GroveDb::verify_query_raw(proof.as_slice(), &path_query_two, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set.len(), 1);

        let mut query_three = Query::new();
        query_three.insert_all();
        let path_query_three = PathQuery::new_unsized(
            vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()],
            query_three,
        );

        let proof = temp_db
            .prove_query(&path_query_three, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set) =
            GroveDb::verify_query_raw(proof.as_slice(), &path_query_three, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set.len(), 2);

        let merged_path_query = PathQuery::merge(
            vec![&path_query_one, &path_query_two, &path_query_three],
            grove_version,
        )
        .expect("should merge three queries");

        let proof = temp_db
            .prove_query(&merged_path_query, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set) =
            GroveDb::verify_query_raw(proof.as_slice(), &merged_path_query, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set.len(), 4);
    }

    #[test]
    fn test_equal_path_merge() {
        let grove_version = GroveVersion::latest();
        // [a, b, Q]
        // [a, b, Q2]
        // We should be able to merge this if Q and Q2 have no subqueries.

        let temp_db = make_deep_tree(grove_version);

        let mut query_one = Query::new();
        query_one.insert_key(b"key1".to_vec());
        let path_query_one =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_one);

        let proof = temp_db
            .prove_query(&path_query_one, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set) =
            GroveDb::verify_query_raw(proof.as_slice(), &path_query_one, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set.len(), 1);

        let mut query_two = Query::new();
        query_two.insert_key(b"key2".to_vec());
        let path_query_two =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query_two);

        let proof = temp_db
            .prove_query(&path_query_two, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set) =
            GroveDb::verify_query_raw(proof.as_slice(), &path_query_two, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set.len(), 1);

        let merged_path_query =
            PathQuery::merge(vec![&path_query_one, &path_query_two], grove_version)
                .expect("should merge three queries");

        let proof = temp_db
            .prove_query(&merged_path_query, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set) =
            GroveDb::verify_query_raw(proof.as_slice(), &merged_path_query, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set.len(), 2);

        // [a, b, Q]
        // [a, b, c, Q2] (rolled up to) [a, b, Q3] where Q3 combines [c, Q2]
        // this should fail as [a, b] is a subpath of [a, b, c]
        let mut query_one = Query::new();
        query_one.insert_all();
        let path_query_one = PathQuery::new_unsized(
            vec![b"deep_leaf".to_vec(), b"deep_node_1".to_vec()],
            query_one,
        );

        let proof = temp_db
            .prove_query(&path_query_one, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set) =
            GroveDb::verify_query_raw(proof.as_slice(), &path_query_one, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set.len(), 2);

        let mut query_one = Query::new();
        query_one.insert_key(b"deeper_1".to_vec());

        let mut subq = Query::new();
        subq.insert_all();
        query_one.set_subquery(subq.clone());

        let path_query_two = PathQuery::new_unsized(
            vec![b"deep_leaf".to_vec(), b"deep_node_1".to_vec()],
            query_one,
        );

        let proof = temp_db
            .prove_query(&path_query_two, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set) =
            GroveDb::verify_query_raw(proof.as_slice(), &path_query_two, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set.len(), 3);

        #[rustfmt::skip]
        mod explanation {

    // Tree Structure
    //                                   root
    //              /                      |                       \ (not representing Merk)
    // -----------------------------------------------------------------------------------------
    //         test_leaf            another_test_leaf                deep_leaf
    //       /           \             /         \              /                 \
    // -----------------------------------------------------------------------------------------
    //   innertree     innertree4  innertree2  innertree3  deep_node_1          deep_node_2
    //       |             |           |           |      /          \         /         \
    // -----------------------------------------------------------------------------------------
    //      k2,v2        k4,v4       k3,v3      k4,v4   deeper_1   deeper_2  deeper_3   deeper_4
    //     /     \         |                           |   ↑  (2)  ↑ |         |          |
    //  k1,v1    k3,v3   k5,v5                        /path_query_1 /          |          |
    // -----------------------------------------------------------------------------------------
    //                                            k2,v2         k5,v5        k8,v8     k10,v10
    //                                           /     \        /    \       /    \       \
    //                                       k1,v1    k3,v3  k4,v4   k6,v6 k7,v7  k9,v9  k11,v11
    //                                            ↑ (3)
    //                                       path_query_2



        }

        let merged_path_query =
            PathQuery::merge(vec![&path_query_one, &path_query_two], grove_version)
                .expect("expected to be able to merge path_query");

        // we expect the common path to be the path of both before merge
        assert_eq!(
            merged_path_query.path,
            vec![b"deep_leaf".to_vec(), b"deep_node_1".to_vec()]
        );

        // we expect all items (a range full)
        assert_eq!(merged_path_query.query.query.items.len(), 1);
        assert!(merged_path_query
            .query
            .query
            .items
            .iter()
            .all(|a| a == &QueryItem::RangeFull(RangeFull)));

        // we expect a conditional subquery on deeper 1 for all elements
        let conditional_subquery_branches = merged_path_query
            .query
            .query
            .conditional_subquery_branches
            .as_ref()
            .expect("expected conditional subquery branches");

        assert_eq!(conditional_subquery_branches.len(), 1);
        let (conditional_query_item, conditional_subquery_branch) =
            conditional_subquery_branches.first().unwrap();
        assert_eq!(
            conditional_query_item,
            &QueryItem::Key(b"deeper_1".to_vec())
        );

        assert_eq!(conditional_subquery_branch.subquery, Some(Box::new(subq)));

        assert_eq!(conditional_subquery_branch.subquery_path, None);

        let (result_set_merged, _) = temp_db
            .query_raw(
                &merged_path_query,
                true,
                true,
                true,
                QueryResultType::QueryPathKeyElementTrioResultType,
                None,
                grove_version,
            )
            .value
            .expect("expected to get results");
        assert_eq!(result_set_merged.len(), 4);

        let proof = temp_db
            .prove_query(&merged_path_query, None, grove_version)
            .unwrap()
            .unwrap();
        let (_, result_set) =
            GroveDb::verify_query_raw(proof.as_slice(), &merged_path_query, grove_version)
                .expect("should execute proof");
        assert_eq!(result_set.len(), 4);
    }

    #[test]
    fn test_path_query_items_with_subquery_and_inner_subquery_path() {
        let grove_version = GroveVersion::latest();
        // Constructing the keys and paths
        let root_path_key_1 = b"root_path_key_1".to_vec();
        let root_path_key_2 = b"root_path_key_2".to_vec();
        let root_item_key = b"root_item_key".to_vec();
        let subquery_path_key_1 = b"subquery_path_key_1".to_vec();
        let subquery_path_key_2 = b"subquery_path_key_2".to_vec();
        let subquery_item_key = b"subquery_item_key".to_vec();
        let inner_subquery_path_key = b"inner_subquery_path_key".to_vec();

        // Constructing the subquery
        let subquery = Query {
            items: vec![QueryItem::Key(subquery_item_key.clone())],
            default_subquery_branch: SubqueryBranch {
                subquery_path: Some(vec![inner_subquery_path_key.clone()]),
                subquery: None,
            },
            left_to_right: true,
            conditional_subquery_branches: None,
            add_parent_tree_on_subquery: false,
            read_mode: None,
        };

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![root_path_key_1.clone(), root_path_key_2.clone()],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::Key(root_item_key.clone())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![
                            subquery_path_key_1.clone(),
                            subquery_path_key_2.clone(),
                        ]),
                        subquery: Some(Box::new(subquery)),
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                },
                limit: Some(2),
                offset: None,
            },
        };

        {
            let path = vec![root_path_key_1.as_slice()];
            let first = path_query
                .query_items_at_path(&path, grove_version)
                .expect("expected valid version")
                .expect("expected query items");

            assert_eq!(
                first,
                SinglePathSubquery {
                    items: Cow::Owned(vec![QueryItem::Key(root_path_key_2.clone())]),
                    has_subquery: HasSubquery::NoSubquery,
                    left_to_right: true,
                    in_path: Some(Cow::Borrowed(&root_path_key_2)),
                    synthesized_path_component: true,
                }
            );
        }

        {
            let path = vec![root_path_key_1.as_slice(), root_path_key_2.as_slice()];

            let second = path_query
                .query_items_at_path(&path, grove_version)
                .expect("expected valid version")
                .expect("expected query items");

            assert_eq!(
                second,
                SinglePathSubquery {
                    items: Cow::Owned(vec![QueryItem::Key(root_item_key.clone())]),
                    has_subquery: HasSubquery::Always, /* This is correct because there's a
                                                        * subquery for one item */
                    left_to_right: true,
                    in_path: None,
                    synthesized_path_component: false,
                }
            );
        }

        {
            let path = vec![
                root_path_key_1.as_slice(),
                root_path_key_2.as_slice(),
                root_item_key.as_slice(),
            ];

            let third = path_query
                .query_items_at_path(&path, grove_version)
                .expect("expected valid version")
                .expect("expected query items");

            assert_eq!(
                third,
                SinglePathSubquery {
                    items: Cow::Owned(vec![QueryItem::Key(subquery_path_key_1.clone())]),
                    has_subquery: HasSubquery::NoSubquery,
                    left_to_right: true,
                    in_path: Some(Cow::Borrowed(&subquery_path_key_1)),
                    synthesized_path_component: true,
                }
            );
        }

        {
            let path = vec![
                root_path_key_1.as_slice(),
                root_path_key_2.as_slice(),
                root_item_key.as_slice(),
                subquery_path_key_1.as_slice(),
            ];

            let fourth = path_query
                .query_items_at_path(&path, grove_version)
                .expect("expected valid version")
                .expect("expected query items");

            assert_eq!(
                fourth,
                SinglePathSubquery {
                    items: Cow::Owned(vec![QueryItem::Key(subquery_path_key_2.clone())]),
                    has_subquery: HasSubquery::NoSubquery,
                    left_to_right: true,
                    in_path: Some(Cow::Borrowed(&subquery_path_key_2)),
                    synthesized_path_component: true,
                }
            );
        }

        {
            let path = vec![
                root_path_key_1.as_slice(),
                root_path_key_2.as_slice(),
                root_item_key.as_slice(),
                subquery_path_key_1.as_slice(),
                subquery_path_key_2.as_slice(),
            ];

            let fifth = path_query
                .query_items_at_path(&path, grove_version)
                .expect("expected valid version")
                .expect("expected query items");

            assert_eq!(
                fifth,
                SinglePathSubquery {
                    items: Cow::Owned(vec![QueryItem::Key(subquery_item_key.clone())]),
                    has_subquery: HasSubquery::Always, /* This means that we should be able to
                                                        * add items underneath */
                    left_to_right: true,
                    in_path: None,
                    synthesized_path_component: false,
                }
            );
        }

        {
            let path = vec![
                root_path_key_1.as_slice(),
                root_path_key_2.as_slice(),
                root_item_key.as_slice(),
                subquery_path_key_1.as_slice(),
                subquery_path_key_2.as_slice(),
                subquery_item_key.as_slice(),
            ];

            let sixth = path_query
                .query_items_at_path(&path, grove_version)
                .expect("expected valid version")
                .expect("expected query items");

            assert_eq!(
                sixth,
                SinglePathSubquery {
                    items: Cow::Owned(vec![QueryItem::Key(inner_subquery_path_key.clone())]),
                    has_subquery: HasSubquery::NoSubquery,
                    left_to_right: true,
                    in_path: None,
                    synthesized_path_component: true,
                }
            );
        }
    }

    #[test]
    fn test_path_query_items_with_subquery_path() {
        let grove_version = GroveVersion::latest();
        // Constructing the keys and paths
        let root_path_key = b"higher".to_vec();
        let dash_key = b"dash".to_vec();
        let quantum_key = b"quantum".to_vec();

        // Constructing the PathQuery
        let path_query = PathQuery {
            path: vec![root_path_key.clone()],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeFull(RangeFull)],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![quantum_key.clone()]),
                        subquery: None,
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                },
                limit: Some(100),
                offset: None,
            },
        };

        // Validating the PathQuery structure
        {
            let path = vec![root_path_key.as_slice()];
            let first = path_query
                .query_items_at_path(&path, grove_version)
                .expect("expected valid version")
                .expect("expected query items");

            assert_eq!(
                first,
                SinglePathSubquery {
                    items: Cow::Owned(vec![QueryItem::RangeFull(RangeFull)]),
                    has_subquery: HasSubquery::Always,
                    left_to_right: true,
                    in_path: None,
                    synthesized_path_component: false,
                }
            );
        }

        {
            let path = vec![root_path_key.as_slice(), dash_key.as_slice()];

            let second = path_query
                .query_items_at_path(&path, grove_version)
                .expect("expected valid version")
                .expect("expected query items");

            assert_eq!(
                second,
                SinglePathSubquery {
                    items: Cow::Owned(vec![QueryItem::Key(quantum_key.clone())]),
                    has_subquery: HasSubquery::NoSubquery,
                    left_to_right: true,
                    // There should be no path: we are at the end of the path
                    in_path: None,
                    synthesized_path_component: true,
                }
            );
        }
    }

    #[test]
    fn test_conditional_subquery_refusing_elements() {
        let grove_version = GroveVersion::latest();
        let empty_vec: Vec<u8> = vec![];
        let zero_vec: Vec<u8> = vec![0];

        let mut conditional_subquery_branches = IndexMap::new();
        conditional_subquery_branches.insert(
            QueryItem::Key(b"".to_vec()),
            SubqueryBranch {
                subquery_path: Some(vec![zero_vec.clone()]),
                subquery: Some(Query::new().into()),
            },
        );

        let path_query = PathQuery {
            path: vec![TEST_LEAF.to_vec()],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeFull(RangeFull)],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![zero_vec.clone()]),
                        subquery: None,
                    },
                    left_to_right: true,
                    conditional_subquery_branches: Some(conditional_subquery_branches),
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                },
                limit: Some(100),
                offset: None,
            },
        };

        {
            let path = vec![TEST_LEAF, empty_vec.as_slice()];

            let second = path_query
                .query_items_at_path(&path, grove_version)
                .expect("expected valid version")
                .expect("expected query items");

            assert_eq!(
                second,
                SinglePathSubquery {
                    items: Cow::Owned(vec![QueryItem::Key(zero_vec.clone())]),
                    has_subquery: HasSubquery::NoSubquery,
                    left_to_right: true,
                    in_path: Some(Cow::Borrowed(&zero_vec)),
                    synthesized_path_component: true,
                }
            );
        }
    }

    #[test]
    fn test_complex_path_query_with_conditional_subqueries() {
        let grove_version = GroveVersion::latest();
        let identity_id =
            hex::decode("8b8948a6801501bbe0431e3d994dcf71cf5a2a0939fe51b0e600076199aba4fb")
                .unwrap();

        let key_20 = vec![20u8];

        let key_80 = vec![80u8];

        let inner_conditional_subquery_branches = IndexMap::from([(
            QueryItem::Key(vec![80]),
            SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(Query {
                    items: vec![QueryItem::RangeFull(RangeFull)],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: None,
                    },
                    left_to_right: true,
                    conditional_subquery_branches: None,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                })),
            },
        )]);

        let conditional_subquery_branches = IndexMap::from([
            (
                QueryItem::Key(vec![]),
                SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(Box::new(Query {
                        items: vec![QueryItem::Key(identity_id.to_vec())],
                        default_subquery_branch: SubqueryBranch {
                            subquery_path: None,
                            subquery: None,
                        },
                        left_to_right: true,
                        conditional_subquery_branches: None,
                        add_parent_tree_on_subquery: false,
                        read_mode: None,
                    })),
                },
            ),
            (
                QueryItem::Key(vec![20]),
                SubqueryBranch {
                    subquery_path: Some(vec![identity_id.to_vec()]),
                    subquery: Some(Box::new(Query {
                        items: vec![QueryItem::Key(vec![80]), QueryItem::Key(vec![0xc0])],
                        default_subquery_branch: SubqueryBranch {
                            subquery_path: None,
                            subquery: None,
                        },
                        conditional_subquery_branches: Some(
                            inner_conditional_subquery_branches.clone(),
                        ),
                        left_to_right: true,
                        add_parent_tree_on_subquery: false,
                        read_mode: None,
                    })),
                },
            ),
        ]);

        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::Key(vec![20]), QueryItem::Key(vec![96])],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: None,
                        subquery: None,
                    },
                    conditional_subquery_branches: Some(conditional_subquery_branches.clone()),
                    left_to_right: true,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                },
                limit: Some(100),
                offset: None,
            },
        };

        assert_eq!(path_query.max_depth(), Some(4));

        {
            let path = vec![];
            let first = path_query
                .query_items_at_path(&path, grove_version)
                .expect("expected valid version")
                .expect("expected query items");

            assert_eq!(
                first,
                SinglePathSubquery {
                    items: Cow::Owned(vec![QueryItem::Key(vec![20]), QueryItem::Key(vec![96]),]),
                    has_subquery: HasSubquery::Conditionally(Cow::Borrowed(
                        &conditional_subquery_branches
                    )),
                    left_to_right: true,
                    in_path: None,
                    synthesized_path_component: false,
                }
            );
        }

        {
            let path = vec![key_20.as_slice()];
            let query = path_query
                .query_items_at_path(&path, grove_version)
                .expect("expected valid version")
                .expect("expected query items");

            assert_eq!(
                query,
                SinglePathSubquery {
                    items: Cow::Owned(vec![QueryItem::Key(identity_id.clone()),]),
                    has_subquery: HasSubquery::NoSubquery,
                    left_to_right: true,
                    in_path: Some(Cow::Borrowed(&identity_id)),
                    synthesized_path_component: true,
                }
            );
        }

        {
            let path = vec![key_20.as_slice(), identity_id.as_slice()];
            let query = path_query
                .query_items_at_path(&path, grove_version)
                .expect("expected valid version")
                .expect("expected query items");

            assert_eq!(
                query,
                SinglePathSubquery {
                    items: Cow::Owned(vec![QueryItem::Key(vec![80]), QueryItem::Key(vec![0xc0]),]),
                    has_subquery: HasSubquery::Conditionally(Cow::Borrowed(
                        &inner_conditional_subquery_branches
                    )),
                    left_to_right: true,
                    in_path: None,
                    synthesized_path_component: false,
                }
            );
        }

        {
            let path = vec![key_20.as_slice(), identity_id.as_slice(), key_80.as_slice()];
            let query = path_query
                .query_items_at_path(&path, grove_version)
                .expect("expected valid version")
                .expect("expected query items");

            assert_eq!(
                query,
                SinglePathSubquery {
                    items: Cow::Owned(vec![QueryItem::RangeFull(RangeFull)]),
                    has_subquery: HasSubquery::NoSubquery,
                    left_to_right: true,
                    in_path: None,
                    synthesized_path_component: false,
                }
            );
        }
    }

    #[test]
    fn test_max_depth_limit() {
        /// Creates a `Query` with nested `SubqueryBranch` up to the specified
        /// depth non-recursively.
        fn create_non_recursive_query(subquery_depth: usize) -> Query {
            let mut root_query = Query::new_range_full();
            let mut current_query = &mut root_query;

            for _ in 0..subquery_depth {
                let new_query = Query::new_range_full();
                current_query.default_subquery_branch = SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(Box::new(new_query)),
                };
                current_query = current_query
                    .default_subquery_branch
                    .subquery
                    .as_mut()
                    .unwrap();
            }

            root_query
        }

        let query = create_non_recursive_query(100);

        assert_eq!(query.max_depth(), Some(101));

        let query = create_non_recursive_query(500);

        assert_eq!(query.max_depth(), None);
    }

    #[test]
    fn test_simple_path_query_serialization() {
        let path_query = PathQuery {
            path: vec![b"root".to_vec(), b"subtree".to_vec()],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::Key(b"key1".to_vec())],
                    default_subquery_branch: SubqueryBranch::default(),
                    conditional_subquery_branches: None,
                    left_to_right: true,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                },
                limit: None,
                offset: None,
            },
        };

        let encoded = encode_to_vec(&path_query, standard()).unwrap();
        let decoded: PathQuery = decode_from_slice(&encoded, standard()).unwrap().0;

        assert_eq!(path_query, decoded);
    }

    #[test]
    fn test_range_query_serialization() {
        let path_query = PathQuery {
            path: vec![b"root".to_vec()],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::Range(b"a".to_vec()..b"z".to_vec())],
                    default_subquery_branch: SubqueryBranch::default(),
                    conditional_subquery_branches: None,
                    left_to_right: false,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                },
                limit: Some(10),
                offset: Some(2),
            },
        };

        let encoded = encode_to_vec(&path_query, standard()).unwrap();
        let decoded: PathQuery = decode_from_slice(&encoded, standard()).unwrap().0;

        assert_eq!(path_query, decoded);
    }

    #[test]
    fn test_range_inclusive_query_serialization() {
        let path_query = PathQuery {
            path: vec![b"root".to_vec()],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeInclusive(b"a".to_vec()..=b"z".to_vec())],
                    default_subquery_branch: SubqueryBranch::default(),
                    conditional_subquery_branches: None,
                    left_to_right: true,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                },
                limit: Some(5),
                offset: None,
            },
        };

        let encoded = encode_to_vec(&path_query, standard()).unwrap();
        let decoded: PathQuery = decode_from_slice(&encoded, standard()).unwrap().0;

        assert_eq!(path_query, decoded);
    }

    #[test]
    fn test_conditional_subquery_serialization() {
        let mut conditional_branches = IndexMap::new();
        conditional_branches.insert(
            QueryItem::Key(b"key1".to_vec()),
            SubqueryBranch {
                subquery_path: Some(vec![b"conditional_path".to_vec()]),
                subquery: Some(Box::new(Query::default())),
            },
        );

        let path_query = PathQuery {
            path: vec![b"root".to_vec()],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::Key(b"key1".to_vec())],
                    default_subquery_branch: SubqueryBranch::default(),
                    conditional_subquery_branches: Some(conditional_branches),
                    left_to_right: true,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                },
                limit: None,
                offset: None,
            },
        };

        let encoded = encode_to_vec(&path_query, standard()).unwrap();
        let decoded: PathQuery = decode_from_slice(&encoded, standard()).unwrap().0;

        assert_eq!(path_query, decoded);
    }

    #[test]
    fn test_empty_path_query_serialization() {
        let path_query = PathQuery {
            path: vec![],
            query: SizedQuery {
                query: Query::default(),
                limit: None,
                offset: None,
            },
        };

        let encoded = encode_to_vec(&path_query, standard()).unwrap();
        let decoded: PathQuery = decode_from_slice(&encoded, standard()).unwrap().0;

        assert_eq!(path_query, decoded);
    }

    #[test]
    fn test_path_query_with_multiple_keys() {
        let path_query = PathQuery {
            path: vec![b"root".to_vec()],
            query: SizedQuery {
                query: Query {
                    items: vec![
                        QueryItem::Key(b"key1".to_vec()),
                        QueryItem::Key(b"key2".to_vec()),
                        QueryItem::Key(b"key3".to_vec()),
                    ],
                    default_subquery_branch: SubqueryBranch::default(),
                    conditional_subquery_branches: None,
                    left_to_right: true,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                },
                limit: None,
                offset: None,
            },
        };

        let encoded = encode_to_vec(&path_query, standard()).unwrap();
        let decoded: PathQuery = decode_from_slice(&encoded, standard()).unwrap().0;

        assert_eq!(path_query, decoded);
    }

    #[test]
    fn test_path_query_with_full_range() {
        let path_query = PathQuery {
            path: vec![b"root".to_vec()],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::RangeFull(RangeFull)],
                    default_subquery_branch: SubqueryBranch::default(),
                    conditional_subquery_branches: None,
                    left_to_right: false,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                },
                limit: Some(100),
                offset: Some(10),
            },
        };

        let encoded = encode_to_vec(&path_query, standard()).unwrap();
        let decoded: PathQuery = decode_from_slice(&encoded, standard()).unwrap().0;

        assert_eq!(path_query, decoded);
    }

    #[test]
    fn test_path_query_with_complex_conditions() {
        let mut conditional_branches = IndexMap::new();
        conditional_branches.insert(
            QueryItem::Key(b"key1".to_vec()),
            SubqueryBranch {
                subquery_path: Some(vec![b"conditional_path1".to_vec()]),
                subquery: Some(Box::new(Query {
                    items: vec![QueryItem::Range(b"a".to_vec()..b"m".to_vec())],
                    default_subquery_branch: SubqueryBranch::default(),
                    conditional_subquery_branches: None,
                    left_to_right: true,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                })),
            },
        );
        conditional_branches.insert(
            QueryItem::Range(b"n".to_vec()..b"z".to_vec()),
            SubqueryBranch {
                subquery_path: Some(vec![b"conditional_path2".to_vec()]),
                subquery: Some(Box::new(Query {
                    items: vec![QueryItem::Key(b"key2".to_vec())],
                    default_subquery_branch: SubqueryBranch::default(),
                    conditional_subquery_branches: None,
                    left_to_right: false,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                })),
            },
        );

        let path_query = PathQuery {
            path: vec![b"root".to_vec()],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::Key(b"key3".to_vec())],
                    default_subquery_branch: SubqueryBranch::default(),
                    conditional_subquery_branches: Some(conditional_branches),
                    left_to_right: true,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                },
                limit: Some(50),
                offset: Some(5),
            },
        };

        let encoded = encode_to_vec(&path_query, standard()).unwrap();
        let decoded: PathQuery = decode_from_slice(&encoded, standard()).unwrap().0;

        assert_eq!(path_query, decoded);
    }

    #[test]
    fn test_path_query_with_subquery_path() {
        let path_query = PathQuery {
            path: vec![b"root".to_vec()],
            query: SizedQuery {
                query: Query {
                    items: vec![QueryItem::Key(b"key1".to_vec())],
                    default_subquery_branch: SubqueryBranch {
                        subquery_path: Some(vec![b"subtree_path".to_vec()]),
                        subquery: Some(Box::new(Query {
                            items: vec![QueryItem::Key(b"key2".to_vec())],
                            default_subquery_branch: SubqueryBranch::default(),
                            conditional_subquery_branches: None,
                            left_to_right: true,
                            add_parent_tree_on_subquery: false,
                            read_mode: None,
                        })),
                    },
                    conditional_subquery_branches: None,
                    left_to_right: true,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                },
                limit: None,
                offset: None,
            },
        };

        let encoded = encode_to_vec(&path_query, standard()).unwrap();
        let decoded: PathQuery = decode_from_slice(&encoded, standard()).unwrap().0;

        assert_eq!(path_query, decoded);
    }

    #[test]
    fn test_path_query_with_empty_query_items() {
        let path_query = PathQuery {
            path: vec![b"root".to_vec()],
            query: SizedQuery {
                query: Query {
                    items: vec![], // No items in the query
                    default_subquery_branch: SubqueryBranch::default(),
                    conditional_subquery_branches: None,
                    left_to_right: true,
                    add_parent_tree_on_subquery: false,
                    read_mode: None,
                },
                limit: Some(20),
                offset: None,
            },
        };

        let encoded = encode_to_vec(&path_query, standard()).unwrap();
        let decoded: PathQuery = decode_from_slice(&encoded, standard()).unwrap().0;

        assert_eq!(path_query, decoded);
    }

    #[test]
    fn test_should_add_parent_tree_at_path_empty_path() {
        let grove_version = GroveVersion::latest();

        // Test with add_parent_tree_on_subquery = true
        let mut query = Query::new();
        query.add_parent_tree_on_subquery = true;
        let path_query = PathQuery::new_unsized(vec![], query);

        // Empty path should return the query's add_parent_tree_on_subquery value
        let result = path_query.should_add_parent_tree_at_path(&[], grove_version);
        assert!(result.unwrap());

        // Test with add_parent_tree_on_subquery = false
        let mut query = Query::new();
        query.add_parent_tree_on_subquery = false;
        let path_query = PathQuery::new_unsized(vec![], query);

        let result = path_query.should_add_parent_tree_at_path(&[], grove_version);
        assert!(!result.unwrap());
    }

    #[test]
    fn test_should_add_parent_tree_at_path_exact_match() {
        let grove_version = GroveVersion::latest();

        let mut query = Query::new();
        query.add_parent_tree_on_subquery = true;
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec(), b"subtree".to_vec()], query);

        // Exact path match
        let path = vec![b"root".as_ref(), b"subtree".as_ref()];
        let result = path_query.should_add_parent_tree_at_path(&path, grove_version);
        assert!(result.unwrap());

        // Different path of same length
        let path = vec![b"root".as_ref(), b"other".as_ref()];
        let result = path_query.should_add_parent_tree_at_path(&path, grove_version);
        assert!(!result.unwrap());
    }

    #[test]
    fn test_should_add_parent_tree_at_path_shorter_path() {
        let grove_version = GroveVersion::latest();

        let mut query = Query::new();
        query.add_parent_tree_on_subquery = true;
        let path_query = PathQuery::new_unsized(
            vec![b"root".to_vec(), b"subtree".to_vec(), b"leaf".to_vec()],
            query,
        );

        // Shorter path should return false
        let path = vec![b"root".as_ref()];
        let result = path_query.should_add_parent_tree_at_path(&path, grove_version);
        assert!(!result.unwrap());

        let path = vec![b"root".as_ref(), b"subtree".as_ref()];
        let result = path_query.should_add_parent_tree_at_path(&path, grove_version);
        assert!(!result.unwrap());
    }

    #[test]
    fn test_should_add_parent_tree_at_path_with_subqueries() {
        let grove_version = GroveVersion::latest();

        // Create a nested query structure
        let mut inner_query = Query::new();
        inner_query.add_parent_tree_on_subquery = true;
        inner_query.insert_key(b"inner_key".to_vec());

        let mut query = Query::new();
        query.add_parent_tree_on_subquery = false;
        query.insert_key(b"key1".to_vec());
        query.default_subquery_branch = SubqueryBranch {
            subquery_path: Some(vec![b"subpath".to_vec()]),
            subquery: Some(Box::new(inner_query)),
        };

        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], query);

        // Test path leading to the inner query
        let path = vec![b"root".as_ref(), b"key1".as_ref(), b"subpath".as_ref()];
        let result = path_query.should_add_parent_tree_at_path(&path, grove_version);
        assert!(result.unwrap()); // Should return inner query's value

        // Test root path
        let path = vec![b"root".as_ref()];
        let result = path_query.should_add_parent_tree_at_path(&path, grove_version);
        assert!(!result.unwrap()); // Should return root query's value
    }

    #[test]
    fn test_should_add_parent_tree_at_path_conditional_subqueries() {
        let grove_version = GroveVersion::latest();

        // Create conditional subqueries
        let mut conditional_branches = IndexMap::new();

        let mut branch1_query = Query::new();
        branch1_query.add_parent_tree_on_subquery = true;
        conditional_branches.insert(
            QueryItem::Key(b"branch1".to_vec()),
            SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(branch1_query)),
            },
        );

        let mut branch2_query = Query::new();
        branch2_query.add_parent_tree_on_subquery = false;
        conditional_branches.insert(
            QueryItem::Key(b"branch2".to_vec()),
            SubqueryBranch {
                subquery_path: Some(vec![b"nested".to_vec()]),
                subquery: Some(Box::new(branch2_query)),
            },
        );

        let mut query = Query::new();
        query.add_parent_tree_on_subquery = false;
        query.conditional_subquery_branches = Some(conditional_branches);

        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], query);

        // Test path to branch1
        let path = vec![b"root".as_ref(), b"branch1".as_ref()];
        let result = path_query.should_add_parent_tree_at_path(&path, grove_version);
        assert!(result.unwrap());

        // Test path to branch2 with nested path
        let path = vec![b"root".as_ref(), b"branch2".as_ref(), b"nested".as_ref()];
        let result = path_query.should_add_parent_tree_at_path(&path, grove_version);
        assert!(!result.unwrap());
    }

    #[test]
    fn test_should_add_parent_tree_at_path_deep_nesting() {
        let grove_version = GroveVersion::latest();

        // Create deeply nested query structure
        let mut level3_query = Query::new();
        level3_query.add_parent_tree_on_subquery = true;

        let mut level2_query = Query::new();
        level2_query.add_parent_tree_on_subquery = false;
        level2_query.insert_key(b"level3".to_vec());
        level2_query.default_subquery_branch = SubqueryBranch {
            subquery_path: None,
            subquery: Some(Box::new(level3_query)),
        };

        let mut level1_query = Query::new();
        level1_query.add_parent_tree_on_subquery = false;
        level1_query.insert_key(b"level2".to_vec());
        level1_query.default_subquery_branch = SubqueryBranch {
            subquery_path: None,
            subquery: Some(Box::new(level2_query)),
        };

        let mut root_query = Query::new();
        root_query.add_parent_tree_on_subquery = false;
        root_query.insert_key(b"level1".to_vec());
        root_query.default_subquery_branch = SubqueryBranch {
            subquery_path: None,
            subquery: Some(Box::new(level1_query)),
        };

        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], root_query);

        // Test various depths
        let path = vec![b"root".as_ref()];
        let result = path_query.should_add_parent_tree_at_path(&path, grove_version);
        assert!(!result.unwrap());

        let path = vec![b"root".as_ref(), b"level1".as_ref()];
        let result = path_query.should_add_parent_tree_at_path(&path, grove_version);
        assert!(!result.unwrap());

        let path = vec![b"root".as_ref(), b"level1".as_ref(), b"level2".as_ref()];
        let result = path_query.should_add_parent_tree_at_path(&path, grove_version);
        assert!(!result.unwrap());

        let path = vec![
            b"root".as_ref(),
            b"level1".as_ref(),
            b"level2".as_ref(),
            b"level3".as_ref(),
        ];
        let result = path_query.should_add_parent_tree_at_path(&path, grove_version);
        assert!(result.unwrap());
    }

    #[test]
    fn test_should_add_parent_tree_at_path_nonexistent_path() {
        let grove_version = GroveVersion::latest();

        let mut query = Query::new();
        query.add_parent_tree_on_subquery = true;
        query.insert_key(b"existing".to_vec());

        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], query);

        // Path that doesn't exist in the query structure
        let path = vec![b"root".as_ref(), b"nonexistent".as_ref()];
        let result = path_query.should_add_parent_tree_at_path(&path, grove_version);
        assert!(!result.unwrap());

        // Longer path that doesn't match
        let path = vec![
            b"root".as_ref(),
            b"existing".as_ref(),
            b"but_no_subquery".as_ref(),
        ];
        let result = path_query.should_add_parent_tree_at_path(&path, grove_version);
        assert!(!result.unwrap());
    }

    #[test]
    fn test_should_add_parent_tree_at_path_version_gating() {
        // Test with latest version
        let grove_version = GroveVersion::latest();

        let mut query = Query::new();
        query.add_parent_tree_on_subquery = true;
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], query);

        let result = path_query.should_add_parent_tree_at_path(&[b"root".as_ref()], grove_version);
        assert!(result.is_ok());
        assert!(result.unwrap());

        // Test with mismatched path
        let result = path_query.should_add_parent_tree_at_path(&[], grove_version);
        assert!(result.is_ok());
        assert!(!result.unwrap());
    }

    #[test]
    fn path_query_terminal_keys_uses_versioned_terminal_keys() {
        let grove_version = GroveVersion::latest();
        let path_query = PathQuery::new_unsized(
            vec![b"root".to_vec()],
            Query::new_single_key(b"leaf".to_vec()),
        );

        let keys = path_query
            .terminal_keys(10, grove_version)
            .expect("terminal keys");

        assert_eq!(keys, vec![(vec![b"root".to_vec()], b"leaf".to_vec())]);
    }

    #[test]
    fn path_query_terminal_keys_conditionals_gate_at_v4() {
        use grovedb_version::version::{v3::GROVE_V3, v4::GROVE_V4};

        // Query selects only "queried"; a conditional branch exists for the
        // unqueried key "other". The legacy walk (v1-v3) emits a terminal key
        // for the unqueried conditional branch; the v4 walk only resolves
        // conditionals against keys the query actually selects (issue #689).
        let mut query = Query::new_single_key(b"queried".to_vec());
        query.add_conditional_subquery(
            QueryItem::Key(b"other".to_vec()),
            None,
            Some(Query::new_single_key(b"inner".to_vec())),
        );
        let path_query = PathQuery::new_unsized(vec![b"root".to_vec()], query);

        let legacy_keys = path_query
            .terminal_keys(10, &GROVE_V3)
            .expect("terminal keys under v3");
        assert_eq!(
            legacy_keys,
            vec![
                (vec![b"root".to_vec(), b"other".to_vec()], b"inner".to_vec()),
                (vec![b"root".to_vec()], b"queried".to_vec()),
            ]
        );

        let fixed_keys = path_query
            .terminal_keys(10, &GROVE_V4)
            .expect("terminal keys under v4");
        assert_eq!(
            fixed_keys,
            vec![(vec![b"root".to_vec()], b"queried".to_vec())]
        );
    }

    #[test]
    fn path_query_terminal_keys_unknown_version_errors() {
        let mut version = GroveVersion::latest().clone();
        version.grovedb_versions.path_query_methods.terminal_keys = 2;

        let path_query = PathQuery::new_unsized(
            vec![b"root".to_vec()],
            Query::new_single_key(b"leaf".to_vec()),
        );

        let err = path_query
            .terminal_keys(10, &version)
            .expect_err("unknown terminal_keys version must error");
        assert!(matches!(err, Error::VersionError(_)));
    }

    // ---------- SizedQuery / PathQuery AggregateCountOnRange validation ----------

    #[test]
    fn sized_query_validate_leaf_acor_rejects_limit_and_offset() {
        // Leaf shape (single AggregateCountOnRange item, no subqueries):
        // both SizedQuery::limit and SizedQuery::offset are rejected
        // because a leaf returns a single u64 and pagination would
        // silently change the answer.
        let mut sq = SizedQuery::new(
            Query::new_aggregate_count_on_range(QueryItem::Range(b"a".to_vec()..b"z".to_vec())),
            Some(10),
            None,
        );
        let err = sq
            .validate_aggregate_count_on_range()
            .expect_err("limit must fail");
        match err {
            Error::InvalidQuery(msg) => {
                assert!(msg.contains("leaf"), "unexpected message: {msg}");
                assert!(msg.contains("limit"), "unexpected message: {msg}");
            }
            _ => panic!("expected InvalidQuery"),
        }

        // Removing the limit but keeping offset should still fail.
        sq.limit = None;
        sq.offset = Some(5);
        let err = sq
            .validate_aggregate_count_on_range()
            .expect_err("offset must fail");
        match err {
            Error::InvalidQuery(msg) => {
                assert!(msg.contains("leaf"), "unexpected message: {msg}");
                assert!(msg.contains("offset"), "unexpected message: {msg}");
            }
            _ => panic!("expected InvalidQuery"),
        }
    }

    #[test]
    fn sized_query_validate_carrier_acor_accepts_limit_rejects_offset() {
        // Carrier shape (outer Key/Range items + AggregateCountOnRange
        // subquery): SizedQuery::limit is permitted (caps the outer
        // walk), but SizedQuery::offset is still rejected pending a
        // separate design pass.
        let mut carrier = Query::new();
        carrier.insert_key(b"k1".to_vec());
        carrier.default_subquery_branch = SubqueryBranch {
            subquery_path: None,
            subquery: Some(Box::new(Query::new_aggregate_count_on_range(
                QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
            ))),
        };
        let mut sq = SizedQuery::new(carrier, Some(20), None);

        // limit=Some(20) is now accepted on the carrier shape.
        let inner = sq
            .validate_aggregate_count_on_range()
            .expect("carrier with limit must validate");
        assert!(matches!(inner, QueryItem::Range(_)));

        // offset is still rejected, with a carrier-specific message.
        sq.limit = None;
        sq.offset = Some(3);
        let err = sq
            .validate_aggregate_count_on_range()
            .expect_err("carrier offset must fail");
        match err {
            Error::InvalidQuery(msg) => {
                assert!(msg.contains("carrier"), "unexpected message: {msg}");
                assert!(msg.contains("offset"), "unexpected message: {msg}");
            }
            _ => panic!("expected InvalidQuery"),
        }
    }

    #[test]
    fn sized_query_validate_leaf_asor_rejects_limit_and_offset() {
        // Leaf shape (single AggregateSumOnRange item, no subqueries):
        // both SizedQuery::limit and SizedQuery::offset are rejected
        // because a leaf returns a single i64 and pagination would
        // silently change the answer.
        let mut sq = SizedQuery::new(
            Query::new_aggregate_sum_on_range(QueryItem::Range(b"a".to_vec()..b"z".to_vec())),
            Some(10),
            None,
        );
        let err = sq
            .validate_aggregate_sum_on_range()
            .expect_err("limit must fail");
        match err {
            Error::InvalidQuery(msg) => {
                assert!(msg.contains("leaf"), "unexpected message: {msg}");
                assert!(msg.contains("limit"), "unexpected message: {msg}");
            }
            _ => panic!("expected InvalidQuery"),
        }

        sq.limit = None;
        sq.offset = Some(5);
        let err = sq
            .validate_aggregate_sum_on_range()
            .expect_err("offset must fail");
        match err {
            Error::InvalidQuery(msg) => {
                assert!(msg.contains("leaf"), "unexpected message: {msg}");
                assert!(msg.contains("offset"), "unexpected message: {msg}");
            }
            _ => panic!("expected InvalidQuery"),
        }
    }

    #[test]
    fn sized_query_validate_carrier_asor_accepts_limit_rejects_offset() {
        // Sum-side mirror of
        // `sized_query_validate_carrier_acor_accepts_limit_rejects_offset`.
        // Carrier shape (outer Key/Range items + AggregateSumOnRange
        // subquery): SizedQuery::limit is permitted (caps the outer
        // walk), but SizedQuery::offset is still rejected pending a
        // separate design pass.
        let mut carrier = Query::new();
        carrier.insert_key(b"k1".to_vec());
        carrier.default_subquery_branch = SubqueryBranch {
            subquery_path: None,
            subquery: Some(Box::new(Query::new_aggregate_sum_on_range(
                QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
            ))),
        };
        let mut sq = SizedQuery::new(carrier, Some(20), None);

        // limit=Some(20) is now accepted on the carrier shape.
        let inner = sq
            .validate_aggregate_sum_on_range()
            .expect("carrier with limit must validate");
        assert!(matches!(inner, QueryItem::Range(_)));

        // offset is still rejected, with a carrier-specific message.
        sq.limit = None;
        sq.offset = Some(3);
        let err = sq
            .validate_aggregate_sum_on_range()
            .expect_err("carrier offset must fail");
        match err {
            Error::InvalidQuery(msg) => {
                assert!(msg.contains("carrier"), "unexpected message: {msg}");
                assert!(msg.contains("offset"), "unexpected message: {msg}");
            }
            _ => panic!("expected InvalidQuery"),
        }
    }

    #[test]
    fn sized_query_validate_leaf_acasor_rejects_limit_and_offset() {
        // Combined-aggregate leaf shape: both SizedQuery::limit and
        // SizedQuery::offset are rejected — pagination would silently
        // change both the count and the sum.
        let mut sq = SizedQuery::new(
            Query::new_aggregate_count_and_sum_on_range(QueryItem::Range(
                b"a".to_vec()..b"z".to_vec(),
            )),
            Some(10),
            None,
        );
        let err = sq
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("limit must fail");
        match err {
            Error::InvalidQuery(msg) => {
                assert!(msg.contains("leaf"), "unexpected message: {msg}");
                assert!(msg.contains("limit"), "unexpected message: {msg}");
            }
            _ => panic!("expected InvalidQuery"),
        }

        sq.limit = None;
        sq.offset = Some(5);
        let err = sq
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("offset must fail");
        match err {
            Error::InvalidQuery(msg) => {
                assert!(msg.contains("leaf"), "unexpected message: {msg}");
                assert!(msg.contains("offset"), "unexpected message: {msg}");
            }
            _ => panic!("expected InvalidQuery"),
        }
    }

    #[test]
    fn sized_query_validate_carrier_acasor_accepts_limit_rejects_offset() {
        // Combined-side mirror of
        // `sized_query_validate_carrier_acor_accepts_limit_rejects_offset`.
        let mut carrier = Query::new();
        carrier.insert_key(b"k1".to_vec());
        carrier.default_subquery_branch = SubqueryBranch {
            subquery_path: None,
            subquery: Some(Box::new(Query::new_aggregate_count_and_sum_on_range(
                QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
            ))),
        };
        let mut sq = SizedQuery::new(carrier, Some(20), None);

        // limit=Some(20) is now accepted on the carrier shape.
        let inner = sq
            .validate_aggregate_count_and_sum_on_range()
            .expect("carrier with limit must validate");
        assert!(matches!(inner, QueryItem::Range(_)));

        // offset is still rejected, with a carrier-specific message.
        sq.limit = None;
        sq.offset = Some(3);
        let err = sq
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("carrier offset must fail");
        match err {
            Error::InvalidQuery(msg) => {
                assert!(msg.contains("carrier"), "unexpected message: {msg}");
                assert!(msg.contains("offset"), "unexpected message: {msg}");
            }
            _ => panic!("expected InvalidQuery"),
        }
    }

    #[test]
    fn sized_query_validate_acor_forwards_query_level_errors() {
        // SizedQuery validation should forward Query-level rejections (here:
        // inner Key) as InvalidQuery.
        let sq = SizedQuery::new(
            Query::new_aggregate_count_on_range(QueryItem::Key(b"k".to_vec())),
            None,
            None,
        );
        let err = sq
            .validate_aggregate_count_on_range()
            .expect_err("inner Key must fail");
        match err {
            Error::InvalidQuery(msg) => assert!(msg.contains("Key")),
            _ => panic!("expected InvalidQuery"),
        }
    }

    #[test]
    fn sized_query_validate_acor_happy_path() {
        let sq = SizedQuery::new(
            Query::new_aggregate_count_on_range(QueryItem::Range(b"a".to_vec()..b"z".to_vec())),
            None,
            None,
        );
        let inner = sq
            .validate_aggregate_count_on_range()
            .expect("happy path must validate");
        assert!(matches!(inner, QueryItem::Range(_)));
    }

    #[test]
    fn path_query_validate_acor_forwards_to_sized_query() {
        // PathQuery::validate_aggregate_count_on_range delegates to
        // SizedQuery::validate_aggregate_count_on_range — exercise both error
        // and happy paths through the public PathQuery surface.
        let pq = PathQuery::new_aggregate_count_on_range(
            vec![b"path".to_vec()],
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        );
        let inner = pq
            .validate_aggregate_count_on_range()
            .expect("happy path through PathQuery must validate");
        assert!(matches!(inner, QueryItem::Range(_)));

        // Forward limit rejection.
        let mut pq_bad = pq.clone();
        pq_bad.query.limit = Some(1);
        let err = pq_bad
            .validate_aggregate_count_on_range()
            .expect_err("limit must fail");
        assert!(matches!(err, Error::InvalidQuery(_)));
    }

    #[test]
    fn path_query_has_aggregate_count_on_range_recognizes_helper_constructor() {
        let pq = PathQuery::new_aggregate_count_on_range(
            vec![b"path".to_vec()],
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        );
        assert!(pq.has_aggregate_count_on_range());

        let pq_regular = PathQuery::new_single_key(vec![b"p".to_vec()], b"k".to_vec());
        assert!(!pq_regular.has_aggregate_count_on_range());
    }

    #[test]
    fn query_validation_error_to_static_str_projects_invalid_operation_and_catches_other_variants()
    {
        use grovedb_query::error::Error as QueryError;

        // The expected normal case: `InvalidOperation(&'static str)` is
        // projected through unchanged.
        let normal = QueryError::InvalidOperation("specific reason");
        assert_eq!(
            super::query_validation_error_to_static_str(normal),
            "specific reason"
        );

        // The defensive catch-all: any other QueryError variant gets the
        // generic fallback label. This branch shouldn't be reachable from
        // real `Query::validate_aggregate_count_on_range` results — it's
        // here to surface "an unrelated bug" rather than silently turning
        // into a useless empty string.
        let other = QueryError::NotSupported("anything not InvalidOperation".to_string());
        assert_eq!(
            super::query_validation_error_to_static_str(other),
            "AggregateCountOnRange query validation failed"
        );
    }
}
