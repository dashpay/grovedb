//! `AggregateCountOnRange` (ACOR) Query helpers and validation.
//!
//! This module owns the Query-level construction, detection, and validation
//! of `AggregateCountOnRange` queries. ACOR comes in two shapes:
//!
//! - **Leaf** — a query whose single item is `AggregateCountOnRange(_)`.
//!   Produces a single `u64` count over the inner range.
//!
//! - **Carrier** — a query whose items are `Key(_)` / `Range*(_)` and whose
//!   `default_subquery_branch.subquery` resolves (after walking the optional
//!   `subquery_path`) to a valid leaf ACOR. Produces one `u64` per matched
//!   outer key — the natural per-outer-key extension of the leaf shape.
//!
//! All ACOR validation lives in this file so the much larger `Query` core
//! in `query.rs` stays focused on the general-purpose query plumbing.

use crate::{error::Error, query::Query, query_item::QueryItem};

impl Query {
    /// Creates an aggregate-count-on-range query that counts the elements
    /// matched by `range`. The resulting query has `AggregateCountOnRange(range)`
    /// as its sole item, no subquery branches, and `left_to_right = true`
    /// (counting is direction-agnostic).
    ///
    /// `range` must be a true range variant (`Range`, `RangeInclusive`,
    /// `RangeFrom`, `RangeTo`, `RangeToInclusive`, `RangeAfter`, `RangeAfterTo`,
    /// or `RangeAfterToInclusive`). Passing `Key`, `RangeFull`, or another
    /// `AggregateCountOnRange` is allowed at construction time but will be
    /// rejected by [`Self::validate_aggregate_count_on_range`].
    pub fn new_aggregate_count_on_range(range: QueryItem) -> Self {
        Self {
            items: vec![QueryItem::AggregateCountOnRange(Box::new(range))],
            left_to_right: true,
            ..Self::default()
        }
    }

    /// If this query contains an `AggregateCountOnRange` item *anywhere* in
    /// its `items` vec, returns a reference to the first such item (whether
    /// the surrounding query is well-formed or not). Returns `None` only
    /// when no item is an `AggregateCountOnRange`.
    ///
    /// This is intentionally a **detection-only** helper: malformed queries
    /// like `items: [Key(...), AggregateCountOnRange(...)]` still report
    /// `Some(...)` here so callers don't accidentally route them through
    /// the regular-query path. Use
    /// [`Self::validate_aggregate_count_on_range`] when you also need to
    /// enforce the well-formedness rules (single item, allowed inner kind,
    /// no subqueries, etc.).
    pub fn aggregate_count_on_range(&self) -> Option<&QueryItem> {
        self.items
            .iter()
            .find(|item| item.is_aggregate_count_on_range())
    }

    /// Returns `true` if any item in this query — including items inside
    /// nested subquery branches — is an `AggregateCountOnRange`.
    ///
    /// `AggregateCountOnRange` is a *terminal* item: a well-formed query
    /// either contains exactly one `AggregateCountOnRange` at the top
    /// level and nothing else (leaf shape) or contains
    /// `Key`/`Range*` items at the top level with an ACOR nested in the
    /// `default_subquery_branch.subquery` (carrier shape). This recursive
    /// detector exists so the prover can validate up front: if any ACOR
    /// is present anywhere, the query as a whole must satisfy
    /// [`Self::validate_aggregate_count_on_range`] — otherwise a malformed
    /// shape could slip past a top-level-only check and be silently routed
    /// through the regular-proof path.
    pub fn has_aggregate_count_on_range_anywhere(&self) -> bool {
        if self.aggregate_count_on_range().is_some() {
            return true;
        }
        if let Some(sub) = self.default_subquery_branch.subquery.as_deref()
            && sub.has_aggregate_count_on_range_anywhere()
        {
            return true;
        }
        if let Some(branches) = &self.conditional_subquery_branches {
            for branch in branches.values() {
                if let Some(sub) = branch.subquery.as_deref()
                    && sub.has_aggregate_count_on_range_anywhere()
                {
                    return true;
                }
            }
        }
        false
    }

    /// Validates the Query-level constraints that apply when an
    /// `AggregateCountOnRange` is present. On success, returns a reference
    /// to the inner range `QueryItem` describing the keys being counted
    /// (the same item regardless of whether the surrounding query is the
    /// leaf shape or the carrier shape).
    ///
    /// Top-level dispatcher: classifies the query as either
    /// - **leaf** (the query owns an `AggregateCountOnRange` item directly —
    ///   the original single-`u64` shape), or
    /// - **carrier** (the query is an outer fan-out of `Key`/`Range` items
    ///   whose `default_subquery_branch.subquery` resolves to a leaf
    ///   `AggregateCountOnRange` — the per-outer-key shape)
    ///
    /// and forwards to the corresponding rule set. See
    /// [`Self::validate_leaf_aggregate_count_on_range`] and
    /// [`Self::validate_carrier_aggregate_count_on_range`] for the precise
    /// rules in each case.
    ///
    /// `SizedQuery::limit` / `SizedQuery::offset` checks live at the
    /// `PathQuery` / `SizedQuery` layer.
    pub fn validate_aggregate_count_on_range(&self) -> Result<&QueryItem, Error> {
        if self.aggregate_count_on_range().is_some() {
            // Owns an ACOR at this level → leaf shape.
            self.validate_leaf_aggregate_count_on_range()
        } else if self.has_aggregate_count_on_range_anywhere() {
            // Doesn't own an ACOR but a nested subquery does → carrier shape.
            self.validate_carrier_aggregate_count_on_range()
        } else {
            Err(Error::InvalidOperation(
                "validate_aggregate_count_on_range called on a query without an \
                 AggregateCountOnRange item",
            ))
        }
    }

    /// Validates the leaf shape: a query whose single item is
    /// `AggregateCountOnRange(_)` and whose surroundings carry no subquery
    /// branches. Returns a reference to the inner range `QueryItem`.
    ///
    /// Rules enforced (matching the constraints documented in the GroveDB
    /// book chapter "Aggregate Count Queries"):
    ///
    /// 1. The query must contain exactly one item.
    /// 2. That item must be `AggregateCountOnRange(_)`.
    /// 3. The inner item must not be `Key` (use `has_raw` / `get_raw` for
    ///    existence tests).
    /// 4. The inner item must not be `RangeFull` (read the parent
    ///    `Element::ProvableCountTree` / `Element::ProvableCountSumTree`
    ///    bytes directly for the unconditional total).
    /// 5. The inner item must not itself be `AggregateCountOnRange`.
    /// 6. `default_subquery_branch.subquery` and
    ///    `default_subquery_branch.subquery_path` must both be `None`.
    /// 7. `conditional_subquery_branches` must be `None` or empty.
    pub fn validate_leaf_aggregate_count_on_range(&self) -> Result<&QueryItem, Error> {
        if self.items.len() != 1 {
            return Err(Error::InvalidOperation(
                "AggregateCountOnRange must be the only item in the query",
            ));
        }
        let inner = match &self.items[0] {
            QueryItem::AggregateCountOnRange(inner) => inner.as_ref(),
            _ => {
                return Err(Error::InvalidOperation(
                    "validate_aggregate_count_on_range called on a query without an \
                     AggregateCountOnRange item",
                ));
            }
        };
        match inner {
            QueryItem::Key(_) => {
                return Err(Error::InvalidOperation(
                    "AggregateCountOnRange may not wrap Key — use has_raw / get_raw for \
                     existence tests",
                ));
            }
            QueryItem::RangeFull(_) => {
                return Err(Error::InvalidOperation(
                    "AggregateCountOnRange may not wrap RangeFull — read the parent \
                     ProvableCountTree element for the unconditional total",
                ));
            }
            QueryItem::AggregateCountOnRange(_) => {
                return Err(Error::InvalidOperation(
                    "AggregateCountOnRange may not wrap another AggregateCountOnRange",
                ));
            }
            _ => {}
        }
        if self.default_subquery_branch.subquery.is_some()
            || self.default_subquery_branch.subquery_path.is_some()
        {
            return Err(Error::InvalidOperation(
                "AggregateCountOnRange queries may not carry a default subquery branch",
            ));
        }
        if let Some(branches) = &self.conditional_subquery_branches
            && !branches.is_empty()
        {
            return Err(Error::InvalidOperation(
                "AggregateCountOnRange queries may not carry conditional subquery branches",
            ));
        }
        Ok(inner)
    }

    /// Validates the carrier shape: an outer query whose items are
    /// `Key`/`Range`-like (NOT `AggregateCountOnRange`), and whose
    /// `default_subquery_branch.subquery` resolves to a valid leaf ACOR
    /// query (possibly after walking a `subquery_path`).
    ///
    /// Returns a reference to the leaf's inner range `QueryItem` — the
    /// same kind of value [`Self::validate_leaf_aggregate_count_on_range`]
    /// returns for a leaf-shape query.
    ///
    /// Rules enforced:
    /// 1. Items must be non-empty.
    /// 2. Each item must be `Key(_)` or a `Range*(_)` variant — explicitly
    ///    NOT `AggregateCountOnRange` (those route through the leaf
    ///    validator) and NOT `RangeFull` (use a leaf ACOR on the parent
    ///    instead).
    /// 3. `default_subquery_branch.subquery` must be `Some(_)`. Its target
    ///    query must itself validate as a leaf ACOR query.
    /// 4. `default_subquery_branch.subquery_path` may be `Some(_)`
    ///    (typically names the path from each outer-key match to the leaf
    ///    subtree). When set, every element must be a non-empty key.
    /// 5. `conditional_subquery_branches` must be `None` or empty
    ///    (out of scope for the initial implementation).
    pub fn validate_carrier_aggregate_count_on_range(&self) -> Result<&QueryItem, Error> {
        if self.items.is_empty() {
            return Err(Error::InvalidOperation(
                "carrier AggregateCountOnRange query must have at least one outer item",
            ));
        }
        for item in &self.items {
            match item {
                QueryItem::Key(_)
                | QueryItem::Range(_)
                | QueryItem::RangeInclusive(_)
                | QueryItem::RangeFrom(_)
                | QueryItem::RangeTo(_)
                | QueryItem::RangeToInclusive(_)
                | QueryItem::RangeAfter(_)
                | QueryItem::RangeAfterTo(_)
                | QueryItem::RangeAfterToInclusive(_) => {}
                QueryItem::RangeFull(_) => {
                    return Err(Error::InvalidOperation(
                        "carrier AggregateCountOnRange query may not have a RangeFull outer item",
                    ));
                }
                QueryItem::AggregateCountOnRange(_) => {
                    return Err(Error::InvalidOperation(
                        "carrier AggregateCountOnRange query may not own an \
                         AggregateCountOnRange item — use the leaf shape instead",
                    ));
                }
            }
        }
        let subquery = match self.default_subquery_branch.subquery.as_deref() {
            Some(sub) => sub,
            None => {
                return Err(Error::InvalidOperation(
                    "carrier AggregateCountOnRange query must set \
                     default_subquery_branch.subquery to a leaf ACOR query",
                ));
            }
        };
        if let Some(path) = &self.default_subquery_branch.subquery_path
            && path.iter().any(|k| k.is_empty())
        {
            return Err(Error::InvalidOperation(
                "carrier AggregateCountOnRange query's subquery_path must contain non-empty keys",
            ));
        }
        if let Some(branches) = &self.conditional_subquery_branches
            && !branches.is_empty()
        {
            return Err(Error::InvalidOperation(
                "carrier AggregateCountOnRange query may not carry conditional subquery \
                 branches (out of scope for this feature)",
            ));
        }
        // The subquery must validate as a leaf ACOR (which is what the
        // proof descent will ultimately consume).
        subquery.validate_leaf_aggregate_count_on_range()
    }
}

#[cfg(test)]
mod tests {
    use indexmap::IndexMap;

    use crate::{query_item::QueryItem, Query, SubqueryBranch};

    // ---------- Leaf-ACOR validation tests ----------
    //
    // These hit each numbered rule in
    // `Query::validate_leaf_aggregate_count_on_range` independently. The
    // happy path is also covered to ensure the success arm returns the
    // inner range.

    fn make_acor_query(inner: QueryItem) -> Query {
        Query::new_aggregate_count_on_range(inner)
    }

    #[test]
    fn validate_acor_happy_path_returns_inner() {
        let q = make_acor_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        let inner = q
            .validate_aggregate_count_on_range()
            .expect("happy path should validate");
        match inner {
            QueryItem::Range(r) => {
                assert_eq!(r.start, b"a".to_vec());
                assert_eq!(r.end, b"z".to_vec());
            }
            _ => panic!("expected inner Range"),
        }
    }

    #[test]
    fn validate_acor_rejects_extra_items() {
        let mut q = make_acor_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        q.items.push(QueryItem::Key(b"extra".to_vec()));
        let err = q
            .validate_aggregate_count_on_range()
            .expect_err("two-item query must fail");
        assert!(matches!(err, crate::error::Error::InvalidOperation(_)));
    }

    #[test]
    fn validate_acor_rejects_non_acor_only_item() {
        // A query with one item that isn't AggregateCountOnRange triggers the
        // "validate called on a query without an AggregateCountOnRange item"
        // branch.
        let q = Query::new_single_query_item(QueryItem::Key(b"k".to_vec()));
        let err = q
            .validate_aggregate_count_on_range()
            .expect_err("non-ACOR-only item must fail");
        assert!(matches!(err, crate::error::Error::InvalidOperation(_)));
    }

    #[test]
    fn validate_acor_rejects_inner_key() {
        let q = make_acor_query(QueryItem::Key(b"k".to_vec()));
        let err = q
            .validate_aggregate_count_on_range()
            .expect_err("inner Key must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(msg.contains("Key")),
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_acor_rejects_inner_range_full() {
        let q = make_acor_query(QueryItem::RangeFull(std::ops::RangeFull));
        let err = q
            .validate_aggregate_count_on_range()
            .expect_err("inner RangeFull must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(msg.contains("RangeFull")),
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_acor_rejects_nested_acor() {
        // AggregateCountOnRange wrapping another AggregateCountOnRange.
        let inner_acor = QueryItem::AggregateCountOnRange(Box::new(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        let q = make_acor_query(inner_acor);
        let err = q
            .validate_aggregate_count_on_range()
            .expect_err("nested ACOR must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("AggregateCountOnRange"))
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_acor_rejects_default_subquery_branch() {
        let mut q = make_acor_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        q.default_subquery_branch = SubqueryBranch {
            subquery_path: None,
            subquery: Some(Box::new(Query::new())),
        };
        let err = q
            .validate_aggregate_count_on_range()
            .expect_err("default subquery branch must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(msg.contains("subquery")),
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_acor_rejects_default_subquery_path() {
        let mut q = make_acor_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        q.default_subquery_branch = SubqueryBranch {
            subquery_path: Some(vec![b"x".to_vec()]),
            subquery: None,
        };
        let err = q
            .validate_aggregate_count_on_range()
            .expect_err("subquery_path must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(msg.contains("subquery")),
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_acor_rejects_conditional_subquery_branches() {
        let mut q = make_acor_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        let mut branches = IndexMap::new();
        branches.insert(
            QueryItem::Key(b"k".to_vec()),
            SubqueryBranch {
                subquery_path: None,
                subquery: Some(Box::new(Query::new())),
            },
        );
        q.conditional_subquery_branches = Some(branches);
        let err = q
            .validate_aggregate_count_on_range()
            .expect_err("conditional branches must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("conditional"));
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_acor_accepts_empty_conditional_branches_map() {
        // An empty `Some(IndexMap::new())` is treated as "no branches" by the
        // validator (the rule enforces non-empty rejection only).
        let mut q = make_acor_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        q.conditional_subquery_branches = Some(IndexMap::new());
        let inner = q
            .validate_aggregate_count_on_range()
            .expect("empty conditional map must validate");
        assert!(matches!(inner, QueryItem::Range(_)));
    }

    #[test]
    fn aggregate_count_on_range_helper_detects_acor_anywhere_in_items() {
        // Well-formed shape — single ACOR item.
        let q = make_acor_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        assert!(q.aggregate_count_on_range().is_some());

        // Two items including ACOR → still detected, so the routing layer
        // can hand the malformed query to validate_aggregate_count_on_range
        // for a precise error rather than silently treating it as a regular
        // query.
        let mut q2 = q.clone();
        q2.items.push(QueryItem::Key(b"x".to_vec()));
        assert!(
            q2.aggregate_count_on_range().is_some(),
            "ACOR + extra item must still be detected as ACOR-bearing"
        );

        // ACOR not at index 0 — also detected.
        let mut q3 = Query::new_single_query_item(QueryItem::Key(b"x".to_vec()));
        q3.items.push(QueryItem::AggregateCountOnRange(Box::new(
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        )));
        assert!(q3.aggregate_count_on_range().is_some());

        // No ACOR anywhere → None.
        let q4 = Query::new_single_query_item(QueryItem::Key(b"x".to_vec()));
        assert!(q4.aggregate_count_on_range().is_none());

        // Empty items → None.
        let q5 = Query::new();
        assert!(q5.aggregate_count_on_range().is_none());
    }

    #[test]
    fn has_aggregate_count_on_range_anywhere_walks_subqueries() {
        // No ACOR anywhere → false.
        let plain = Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        assert!(!plain.has_aggregate_count_on_range_anywhere());

        // Top-level ACOR → true (covered by `aggregate_count_on_range` too).
        let top = make_acor_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        assert!(top.has_aggregate_count_on_range_anywhere());

        // ACOR hidden inside `default_subquery_branch.subquery` — the
        // top-level-only `aggregate_count_on_range` would miss it, but the
        // recursive helper finds it. This is the surface that the
        // prove_query entry-point gate uses to refuse to run any
        // ACOR-bearing query that isn't a canonical leaf-or-carrier shape.
        let inner_acor = make_acor_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        let mut hidden =
            Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        hidden.set_subquery(inner_acor);
        assert!(hidden.aggregate_count_on_range().is_none());
        assert!(
            hidden.has_aggregate_count_on_range_anywhere(),
            "ACOR hidden in default subquery branch must be detected"
        );

        // ACOR hidden in a conditional subquery branch.
        let inner_acor2 = make_acor_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        let mut conditional =
            Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        conditional.add_conditional_subquery(
            QueryItem::Key(b"k".to_vec()),
            None,
            Some(inner_acor2),
        );
        assert!(
            conditional.has_aggregate_count_on_range_anywhere(),
            "ACOR hidden in conditional subquery branch must be detected"
        );
    }

    // ---------- Carrier ACOR validation tests ----------
    //
    // The carrier shape is an outer query with `Key`/`Range*` items whose
    // `default_subquery_branch.subquery` resolves to a leaf ACOR query.
    // It is the multi-outer-key extension of the leaf shape, returning one
    // count per outer key. These tests verify the
    // `validate_carrier_aggregate_count_on_range` rules and the dispatcher
    // behavior of the top-level `validate_aggregate_count_on_range`.

    fn make_leaf_acor_subquery() -> Query {
        make_acor_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()))
    }

    #[test]
    fn validate_carrier_acor_happy_path_keys_outer_with_subquery_path() {
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"brand_000".to_vec()));
        carrier.items.push(QueryItem::Key(b"brand_001".to_vec()));
        carrier.set_subquery_path(vec![b"color".to_vec()]);
        carrier.set_subquery(make_leaf_acor_subquery());
        // Top-level dispatcher accepts the carrier and returns the leaf's
        // inner range.
        let inner = carrier
            .validate_aggregate_count_on_range()
            .expect("carrier should validate");
        assert!(matches!(inner, QueryItem::Range(_)));
        // And the dedicated carrier validator agrees.
        carrier
            .validate_carrier_aggregate_count_on_range()
            .expect("carrier validator should accept");
        // Leaf validator must reject (carrier-level items aren't ACOR).
        assert!(carrier.validate_leaf_aggregate_count_on_range().is_err());
    }

    #[test]
    fn validate_carrier_acor_happy_path_no_subquery_path() {
        // subquery_path is optional — the leaf ACOR may be directly under
        // each outer match.
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"a".to_vec()));
        carrier.set_subquery(make_leaf_acor_subquery());
        carrier
            .validate_aggregate_count_on_range()
            .expect("carrier without subquery_path should validate");
    }

    #[test]
    fn validate_carrier_acor_rejects_acor_at_both_levels() {
        // Carrier itself owns an ACOR AND its subquery is also an ACOR.
        // The top-level dispatcher routes to the LEAF validator first
        // (because aggregate_count_on_range() returns Some at carrier
        // level), so the leaf's "single item" rule catches the
        // ACOR-in-subquery shape via the items-len check or the
        // no-subquery rule. Either way the error fires.
        let mut q = make_acor_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        q.set_subquery(make_leaf_acor_subquery());
        let err = q
            .validate_aggregate_count_on_range()
            .expect_err("ACOR at both levels must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(
                    msg.contains("AggregateCountOnRange") || msg.contains("subquery"),
                    "unexpected message: {msg}"
                );
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_acor_rejects_range_full_outer() {
        let mut carrier = Query::new();
        carrier
            .items
            .push(QueryItem::RangeFull(std::ops::RangeFull));
        carrier.set_subquery(make_leaf_acor_subquery());
        let err = carrier
            .validate_aggregate_count_on_range()
            .expect_err("RangeFull outer must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("RangeFull"), "unexpected message: {msg}");
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_acor_rejects_acor_outer_item() {
        // Both a Key and an AggregateCountOnRange item at the carrier
        // level. The leaf validator's items-len check fires first (since
        // there's an ACOR item in items, aggregate_count_on_range()
        // returns Some, and len != 1).
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        carrier
            .items
            .push(QueryItem::AggregateCountOnRange(Box::new(
                QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
            )));
        carrier.set_subquery(make_leaf_acor_subquery());
        let err = carrier
            .validate_aggregate_count_on_range()
            .expect_err("ACOR + Key outer items must fail");
        assert!(matches!(err, crate::error::Error::InvalidOperation(_)));
    }

    #[test]
    fn validate_carrier_acor_rejects_carrier_with_missing_subquery() {
        // Outer items present but no subquery → not a carrier (and not a
        // leaf), so the top-level dispatcher routes to the
        // "not an ACOR query" error.
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        let err = carrier
            .validate_aggregate_count_on_range()
            .expect_err("carrier without subquery must fail");
        assert!(matches!(err, crate::error::Error::InvalidOperation(_)));
    }

    #[test]
    fn validate_carrier_acor_rejects_non_acor_subquery() {
        // Outer Keys + subquery that is NOT an ACOR (just a regular range
        // query) → not a valid carrier ACOR. The top-level dispatcher
        // sees `has_aggregate_count_on_range_anywhere() == false`, so it
        // surfaces the "not an ACOR query" error rather than the carrier
        // validator's "subquery must validate as leaf ACOR" error.
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        let regular_sub =
            Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        carrier.set_subquery(regular_sub);
        let err = carrier
            .validate_aggregate_count_on_range()
            .expect_err("non-ACOR subquery must fail");
        assert!(matches!(err, crate::error::Error::InvalidOperation(_)));
    }

    #[test]
    fn validate_carrier_acor_rejects_conditional_branches() {
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        carrier.set_subquery(make_leaf_acor_subquery());
        carrier.add_conditional_subquery(
            QueryItem::Key(b"k".to_vec()),
            None,
            Some(make_leaf_acor_subquery()),
        );
        let err = carrier
            .validate_aggregate_count_on_range()
            .expect_err("carrier conditional branches must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("conditional"), "unexpected message: {msg}")
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_acor_rejects_empty_outer_items() {
        // Empty items + leaf ACOR subquery → not a valid carrier.
        // (Empty outer means no outer key to iterate; doesn't make sense.)
        let mut carrier = Query::new();
        carrier.set_subquery(make_leaf_acor_subquery());
        let err = carrier
            .validate_carrier_aggregate_count_on_range()
            .expect_err("empty outer items must fail");
        assert!(matches!(err, crate::error::Error::InvalidOperation(_)));
    }

    #[test]
    fn validate_carrier_acor_rejects_carrier_subquery_with_invalid_inner() {
        // The carrier validator delegates to the leaf validator for the
        // subquery, so a malformed leaf ACOR (e.g. wrapping `Key`) is
        // surfaced via the carrier path. Pin the exact rejection message
        // so a refactor that re-routes the rejection through a different
        // arm doesn't silently accept the malformed shape.
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        carrier.set_subquery(make_acor_query(QueryItem::Key(b"k".to_vec())));
        let err = carrier
            .validate_aggregate_count_on_range()
            .expect_err("malformed inner Key in subquery ACOR must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(
                msg.contains("may not wrap Key"),
                "unexpected message: {msg}"
            ),
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_acor_rejects_empty_subquery_path_element() {
        // A carrier's subquery_path may not contain empty keys — those
        // would point at "no key" in the intermediate descent, which the
        // merk single-key prover can't satisfy.
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        carrier.set_subquery_path(vec![b"".to_vec()]);
        carrier.set_subquery(make_leaf_acor_subquery());
        let err = carrier
            .validate_aggregate_count_on_range()
            .expect_err("empty subquery_path key must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("non-empty keys"), "unexpected message: {msg}")
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_acor_accepts_range_outer_items() {
        // A carrier may use Range outer items (the spec leaves room for
        // this). Verify the validator agrees for every Range* variant
        // the rule whitelists.
        for outer in [
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
            QueryItem::RangeInclusive(b"a".to_vec()..=b"z".to_vec()),
            QueryItem::RangeFrom(b"a".to_vec()..),
            QueryItem::RangeTo(..b"z".to_vec()),
            QueryItem::RangeToInclusive(..=b"z".to_vec()),
            QueryItem::RangeAfter(b"a".to_vec()..),
            QueryItem::RangeAfterTo(b"a".to_vec()..b"z".to_vec()),
            QueryItem::RangeAfterToInclusive(b"a".to_vec()..=b"z".to_vec()),
        ] {
            let mut carrier = Query::new();
            carrier.items.push(outer);
            carrier.set_subquery(make_leaf_acor_subquery());
            carrier
                .validate_aggregate_count_on_range()
                .expect("carrier with Range* outer should validate");
        }
    }

    #[test]
    fn validate_acor_dispatcher_rejects_non_acor_query() {
        // The top-level dispatcher returns the "not an ACOR" error when
        // neither shape matches.
        let q = Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        let err = q
            .validate_aggregate_count_on_range()
            .expect_err("non-ACOR query must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(msg.contains(
                "validate_aggregate_count_on_range called on a query \
                              without an AggregateCountOnRange item"
            )),
            _ => panic!("expected InvalidOperation"),
        }
    }
}
