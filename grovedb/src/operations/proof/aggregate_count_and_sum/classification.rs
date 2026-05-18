//! Classification of an `AggregateCountAndSumOnRange` `PathQuery`
//! into either the **leaf** shape (single
//! `AggregateCountAndSumOnRange(_)` item) or the **carrier** shape
//! (outer `Key`/`Range*` items routing to a leaf combined-aggregate
//! subquery).
//!
//! Combined-side mirror of
//! [`crate::operations::proof::aggregate_count::classification`] and
//! [`crate::operations::proof::aggregate_sum::classification`]. Both
//! the struct and the bulk of the classify logic live in the shared
//! [`super::super::aggregate_common`] module — this file just
//! re-exports them under the combined-axis names and supplies the
//! combined-specific validate + is-leaf-shape callbacks.

use crate::{Error, PathQuery};

/// Type alias for the shared classification descriptor, kept under the
/// combined-side name so existing callers in `per_key.rs` / `mod.rs`
/// need no changes.
pub(super) type AggregateCountAndSumClassification =
    super::super::aggregate_common::AggregateClassification;

/// Classify an `AggregateCountAndSumOnRange` path query. Thin wrapper
/// over [`super::super::aggregate_common::classify_aggregate_path_query`]
/// that supplies the combined-side validator and "owns an
/// AggregateCountAndSumOnRange item at top level" predicate.
pub(super) fn classify_aggregate_count_and_sum_path_query(
    path_query: &PathQuery,
) -> Result<AggregateCountAndSumClassification, Error> {
    super::super::aggregate_common::classify_aggregate_path_query(
        path_query,
        |pq| pq.validate_aggregate_count_and_sum_on_range(),
        |q| q.aggregate_count_and_sum_on_range().is_some(),
    )
}
