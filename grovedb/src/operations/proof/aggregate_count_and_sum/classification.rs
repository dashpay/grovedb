//! Classification of an `AggregateCountAndSumOnRange` `PathQuery`
//! into either the **leaf** shape (single
//! `AggregateCountAndSumOnRange(_)` item) or the **carrier** shape
//! (outer `Key`/`Range*` items routing to a leaf combined-aggregate
//! subquery).
//!
//! Combined-side mirror of
//! [`crate::operations::proof::aggregate_count::classification`] and
//! [`crate::operations::proof::aggregate_sum::classification`].

use grovedb_query::QueryItem;

use crate::{Error, PathQuery};

/// Classification of an `AggregateCountAndSumOnRange` `PathQuery`.
/// Encodes either the leaf-only inner range (no carrier descent) or
/// the carrier outer items + leaf inner range + optional
/// `subquery_path`.
pub(super) struct AggregateCountAndSumClassification {
    /// The inner range that the leaf merk combined-aggregate proof
    /// must satisfy.
    pub(super) leaf_inner_range: QueryItem,
    /// Carrier outer items. `None` for leaf-only queries.
    pub(super) carrier_outer_items: Option<Vec<QueryItem>>,
    /// Carrier subquery_path (the keys between each outer match and
    /// the leaf merk). Empty `Vec` if no subquery_path was set.
    /// `None` for leaf-only queries.
    pub(super) carrier_subquery_path: Option<Vec<Vec<u8>>>,
    /// Whether the outer query is left-to-right. Affects which
    /// results the merk_proof returns when the outer items are
    /// ranges. Always `true` for leaf-only.
    pub(super) carrier_left_to_right: bool,
}

/// Classify an `AggregateCountAndSumOnRange` path query and validate
/// it at the PathQuery level. The shape-specific pagination rules
/// are enforced through
/// [`PathQuery::validate_aggregate_count_and_sum_on_range`]: leaf
/// queries reject both `SizedQuery::limit` and `SizedQuery::offset`;
/// carrier queries accept `SizedQuery::limit` (caps the outer walk;
/// threaded into the proof verifier via `path_query.query.limit`) but
/// still reject `SizedQuery::offset`.
pub(super) fn classify_aggregate_count_and_sum_path_query(
    path_query: &PathQuery,
) -> Result<AggregateCountAndSumClassification, Error> {
    let leaf_inner = path_query
        .validate_aggregate_count_and_sum_on_range()?
        .clone();
    let q = &path_query.query.query;
    if q.aggregate_count_and_sum_on_range().is_some() {
        // Leaf shape: top-level `AggregateCountAndSumOnRange` item.
        return Ok(AggregateCountAndSumClassification {
            leaf_inner_range: leaf_inner,
            carrier_outer_items: None,
            carrier_subquery_path: None,
            carrier_left_to_right: true,
        });
    }
    // Carrier shape: validation above routed through the carrier
    // validator, so `leaf_inner` is the *subquery's* inner range.
    let outer_items = q.items.clone();
    let subquery_path = q
        .default_subquery_branch
        .subquery_path
        .clone()
        .unwrap_or_default();
    Ok(AggregateCountAndSumClassification {
        leaf_inner_range: leaf_inner,
        carrier_outer_items: Some(outer_items),
        carrier_subquery_path: Some(subquery_path),
        carrier_left_to_right: q.left_to_right,
    })
}
