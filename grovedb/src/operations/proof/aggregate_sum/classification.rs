//! Classification of an `AggregateSumOnRange` `PathQuery` into either
//! the **leaf** shape (single `AggregateSumOnRange(_)` item) or the
//! **carrier** shape (outer `Key`/`Range*` items routing to a leaf
//! aggregate-sum subquery).
//!
//! Sum-side mirror of
//! [`crate::operations::proof::aggregate_count::classification`].
//! The classification is consumed by the per-key traversal in
//! [`super::per_key`] to decide whether to terminate the path walk at a
//! single sum proof or to fan out across the carrier's matched outer
//! keys.
//!
//! Both the struct and the bulk of the classify logic live in the
//! shared [`super::super::aggregate_common`] module — this file just
//! re-exports them under the sum-axis names and supplies the
//! sum-specific validate + is-leaf-shape callbacks.

use crate::{Error, PathQuery};

/// Type alias for the shared classification descriptor, kept under the
/// sum-side name so existing callers in `per_key.rs` / `mod.rs` need
/// no changes.
pub(super) type AggregateSumClassification =
    super::super::aggregate_common::AggregateClassification;

/// Classify an `AggregateSumOnRange` path query. Thin wrapper over
/// [`super::super::aggregate_common::classify_aggregate_path_query`]
/// that supplies the sum-side validator and "owns an
/// AggregateSumOnRange item at top level" predicate.
pub(super) fn classify_aggregate_sum_path_query(
    path_query: &PathQuery,
) -> Result<AggregateSumClassification, Error> {
    super::super::aggregate_common::classify_aggregate_path_query(
        path_query,
        |pq| pq.validate_aggregate_sum_on_range(),
        |q| q.aggregate_sum_on_range().is_some(),
    )
}
