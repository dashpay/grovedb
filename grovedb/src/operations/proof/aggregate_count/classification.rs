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
//! Forthcoming aggregate variants (sum, average) will define their own
//! parallel classification types (`AggregateSumClassification`,
//! `AggregateAverageClassification`, …) in sibling modules — the
//! leaf-vs-carrier shape is a property of any aggregate-on-range query,
//! but each variant carries its own kind of inner descriptor.

use grovedb_query::QueryItem;

use crate::{Error, PathQuery};

/// Classification of an `AggregateCountOnRange` `PathQuery`. Encodes
/// either the leaf-only inner range (no carrier descent) or the
/// carrier outer items + leaf inner range + optional `subquery_path`
/// that the verifier must follow per outer key.
pub(super) struct AggregateCountClassification {
    /// The inner range that the leaf merk count proof must satisfy.
    pub(super) leaf_inner_range: QueryItem,
    /// Carrier outer items. `None` for leaf-only queries.
    pub(super) carrier_outer_items: Option<Vec<QueryItem>>,
    /// Carrier subquery_path (the keys between each outer match and the
    /// leaf merk). Empty `Vec` if no subquery_path was set. `None` for
    /// leaf-only queries.
    pub(super) carrier_subquery_path: Option<Vec<Vec<u8>>>,
    /// Whether the outer query is left-to-right. Affects which results
    /// the merk_proof returns when the outer items are ranges. Always
    /// `true` for leaf-only.
    pub(super) carrier_left_to_right: bool,
}

/// Classify an `AggregateCountOnRange` path query and validate it at
/// the PathQuery level — `SizedQuery::limit` / `offset` (which
/// aggregate-count explicitly forbids) are enforced for both shapes.
pub(super) fn classify_aggregate_count_path_query(
    path_query: &PathQuery,
) -> Result<AggregateCountClassification, Error> {
    let leaf_inner = path_query.validate_aggregate_count_on_range()?.clone();
    let q = &path_query.query.query;
    if q.aggregate_count_on_range().is_some() {
        // Leaf shape: top-level `AggregateCountOnRange` item. The
        // top-level `validate_aggregate_count_on_range` dispatcher above
        // routed through the leaf validator, so we already know
        // `leaf_inner` is the inner range of the top-level
        // `AggregateCountOnRange` item.
        return Ok(AggregateCountClassification {
            leaf_inner_range: leaf_inner,
            carrier_outer_items: None,
            carrier_subquery_path: None,
            carrier_left_to_right: true,
        });
    }
    // Carrier shape: validation above routed through the carrier
    // validator, so `leaf_inner` is the *subquery's* inner range. We
    // just need to extract the outer items and the optional
    // subquery_path.
    let outer_items = q.items.clone();
    let subquery_path = q
        .default_subquery_branch
        .subquery_path
        .clone()
        .unwrap_or_default();
    Ok(AggregateCountClassification {
        leaf_inner_range: leaf_inner,
        carrier_outer_items: Some(outer_items),
        carrier_subquery_path: Some(subquery_path),
        carrier_left_to_right: q.left_to_right,
    })
}
