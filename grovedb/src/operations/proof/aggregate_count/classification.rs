//! Classification of an `AggregateCountOnRange` `PathQuery` into either
//! the **leaf** shape (single `AggregateCountOnRange(_)` item) or the
//! **carrier** shape (outer `Key`/`Range*` items routing to a leaf
//! aggregate-count subquery).
//!
//! The classification is consumed by the per-key traversal in
//! [`super::per_key`] to decide whether to terminate the path walk at a
//! single count proof or to fan out across the carrier's matched outer
//! keys.
//!
//! Both the struct and the bulk of the classify logic live in the
//! shared [`super::super::aggregate_common`] module — this file just
//! re-exports them under the count-axis names and supplies the
//! count-specific validate + is-leaf-shape callbacks.

use crate::{Error, PathQuery};

/// Type alias for the shared classification descriptor, kept under the
/// count-side name so existing callers in `per_key.rs` / `mod.rs` need
/// no changes.
pub(super) type AggregateCountClassification =
    super::super::aggregate_common::AggregateClassification;

/// Classify an `AggregateCountOnRange` path query. Thin wrapper over
/// [`super::super::aggregate_common::classify_aggregate_path_query`]
/// that supplies the count-side validator and "owns an
/// AggregateCountOnRange item at top level" predicate.
pub(super) fn classify_aggregate_count_path_query(
    path_query: &PathQuery,
) -> Result<AggregateCountClassification, Error> {
    super::super::aggregate_common::classify_aggregate_path_query(
        path_query,
        |pq| pq.validate_aggregate_count_on_range(),
        |q| q.aggregate_count_on_range().is_some(),
    )
}
