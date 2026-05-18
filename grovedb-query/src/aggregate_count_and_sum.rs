//! `AggregateCountAndSumOnRange` Query helpers and validation.
//!
//! Dual-axis sibling of [`crate::aggregate_count`] and
//! [`crate::aggregate_sum`]. Owns the Query-level construction,
//! detection, and validation of `AggregateCountAndSumOnRange`
//! queries — which return BOTH a `u64` count AND a signed `i64` sum
//! over an inner range from a single proof against a
//! `ProvableCountProvableSumTree` host.
//!
//! They come in two shapes:
//!
//! - **Leaf** — a query whose single item is
//!   `AggregateCountAndSumOnRange(_)`. Produces a single
//!   `(u64, i64)` over the inner range.
//!
//! - **Carrier** — a query whose items are `Key(_)` / `Range*(_)` and
//!   whose `default_subquery_branch.subquery` resolves (after walking
//!   the optional `subquery_path`) to a valid leaf
//!   `AggregateCountAndSumOnRange`. Produces one `(u64, i64)` per
//!   matched outer key — the natural per-outer-key extension of the
//!   leaf shape.
//!
//! All combined-aggregate validation lives in this file so the much
//! larger `Query` core in `query.rs` stays focused on the
//! general-purpose query plumbing.

use crate::{error::Error, query::Query, query_item::QueryItem};

impl Query {
    /// Creates a combined aggregate-count-and-sum-on-range query that
    /// returns BOTH the `u64` count AND the signed `i64` sum of children
    /// matched by `range` from a single proof. Mirror of
    /// `Query::new_aggregate_count_on_range` / `new_aggregate_sum_on_range`
    /// for the new `AggregateCountAndSumOnRange` variant.
    ///
    /// This variant is only valid against `ProvableCountProvableSumTree`
    /// hosts — the single-axis hosts cannot host it because their node
    /// hashes don't bind both aggregates.
    ///
    /// `range` must be a true range variant; passing `Key`, `RangeFull`,
    /// or any aggregate variant is allowed at construction time but will
    /// be rejected by
    /// [`Self::validate_aggregate_count_and_sum_on_range`].
    pub fn new_aggregate_count_and_sum_on_range(range: QueryItem) -> Self {
        Self {
            items: vec![QueryItem::AggregateCountAndSumOnRange(Box::new(range))],
            left_to_right: true,
            ..Self::default()
        }
    }

    /// Returns `Some(...)` for any query containing an
    /// `AggregateCountAndSumOnRange` item, regardless of well-formedness.
    /// Mirror of [`Self::aggregate_count_on_range`] /
    /// [`Self::aggregate_sum_on_range`].
    pub fn aggregate_count_and_sum_on_range(&self) -> Option<&QueryItem> {
        self.items
            .iter()
            .find(|item| item.is_aggregate_count_and_sum_on_range())
    }

    /// Mirror of `Query::has_aggregate_count_on_range_anywhere` /
    /// `has_aggregate_sum_on_range_anywhere` for the combined variant.
    /// Used by the prover/verifier to validate at entry — if any
    /// `AggregateCountAndSumOnRange` is present anywhere, the query must
    /// satisfy [`Self::validate_aggregate_count_and_sum_on_range`].
    pub fn has_aggregate_count_and_sum_on_range_anywhere(&self) -> bool {
        if self.aggregate_count_and_sum_on_range().is_some() {
            return true;
        }
        if let Some(sub) = self.default_subquery_branch.subquery.as_deref()
            && sub.has_aggregate_count_and_sum_on_range_anywhere()
        {
            return true;
        }
        if let Some(branches) = &self.conditional_subquery_branches {
            for (selector, branch) in branches {
                // Same defense-in-depth as the sum side: the selector
                // itself is a `QueryItem` and could carry an
                // `AggregateCountAndSumOnRange` tag even though it
                // wouldn't be a meaningful matcher. Reject defensively
                // so a hidden ACASOR in a selector cannot slip past the
                // aggregate-shape check.
                if selector.is_aggregate_count_and_sum_on_range() {
                    return true;
                }
                if let Some(sub) = branch.subquery.as_deref()
                    && sub.has_aggregate_count_and_sum_on_range_anywhere()
                {
                    return true;
                }
            }
        }
        false
    }

    /// Validates the Query-level constraints that apply when an
    /// `AggregateCountAndSumOnRange` is present. On success, returns a
    /// reference to the inner range `QueryItem` describing the keys
    /// being aggregated (the same item regardless of whether the
    /// surrounding query is the leaf shape or the carrier shape).
    ///
    /// Top-level dispatcher: classifies the query as either
    /// - **leaf** (the query owns an `AggregateCountAndSumOnRange` item
    ///   directly — the original single-`(u64, i64)` shape), or
    /// - **carrier** (the query is an outer fan-out of `Key`/`Range`
    ///   items whose `default_subquery_branch.subquery` resolves to a
    ///   leaf `AggregateCountAndSumOnRange` — the per-outer-key shape)
    ///
    /// and forwards to the corresponding rule set. See
    /// [`Self::validate_leaf_aggregate_count_and_sum_on_range`] and
    /// [`Self::validate_carrier_aggregate_count_and_sum_on_range`].
    ///
    /// `SizedQuery::limit` / `SizedQuery::offset` checks live at the
    /// `PathQuery` / `SizedQuery` layer.
    pub fn validate_aggregate_count_and_sum_on_range(&self) -> Result<&QueryItem, Error> {
        if self.aggregate_count_and_sum_on_range().is_some() {
            // Owns an ACASOR at this level → leaf shape.
            self.validate_leaf_aggregate_count_and_sum_on_range()
        } else if self.has_aggregate_count_and_sum_on_range_anywhere() {
            // Doesn't own an ACASOR but a nested subquery does → carrier shape.
            self.validate_carrier_aggregate_count_and_sum_on_range()
        } else {
            Err(Error::InvalidOperation(
                "validate_aggregate_count_and_sum_on_range called on a query without an \
                 AggregateCountAndSumOnRange item",
            ))
        }
    }

    /// Validates the leaf shape: a query whose single item is
    /// `AggregateCountAndSumOnRange(_)` and whose surroundings carry no
    /// subquery branches. Returns a reference to the inner range
    /// `QueryItem`.
    ///
    /// Rules enforced:
    ///
    /// 1. The query must contain exactly one item.
    /// 2. That item must be `AggregateCountAndSumOnRange(_)`.
    /// 3. The inner item must not be `Key` (use `has_raw` / `get_raw` for
    ///    existence tests).
    /// 4. The inner item must not be `RangeFull` (read the parent
    ///    `Element::ProvableCountProvableSumTree` bytes directly for the
    ///    unconditional totals).
    /// 5. The inner item must not be any aggregate variant
    ///    (`AggregateCountOnRange`, `AggregateSumOnRange`, or another
    ///    `AggregateCountAndSumOnRange`) — the three are orthogonal.
    /// 6. `default_subquery_branch.subquery` and
    ///    `default_subquery_branch.subquery_path` must both be `None`.
    /// 7. `conditional_subquery_branches` must be `None` or empty.
    pub fn validate_leaf_aggregate_count_and_sum_on_range(&self) -> Result<&QueryItem, Error> {
        if self.items.len() != 1 {
            return Err(Error::InvalidOperation(
                "AggregateCountAndSumOnRange must be the only item in the query",
            ));
        }
        let inner = match &self.items[0] {
            QueryItem::AggregateCountAndSumOnRange(inner) => inner.as_ref(),
            _ => {
                return Err(Error::InvalidOperation(
                    "validate_aggregate_count_and_sum_on_range called on a query without an \
                     AggregateCountAndSumOnRange item",
                ));
            }
        };
        match inner {
            QueryItem::Key(_) => {
                return Err(Error::InvalidOperation(
                    "AggregateCountAndSumOnRange may not wrap Key — use has_raw / get_raw for \
                     existence tests",
                ));
            }
            QueryItem::RangeFull(_) => {
                return Err(Error::InvalidOperation(
                    "AggregateCountAndSumOnRange may not wrap RangeFull — read the parent \
                     ProvableCountProvableSumTree element for the unconditional totals",
                ));
            }
            QueryItem::AggregateCountAndSumOnRange(_) => {
                return Err(Error::InvalidOperation(
                    "AggregateCountAndSumOnRange may not wrap another \
                     AggregateCountAndSumOnRange",
                ));
            }
            QueryItem::AggregateCountOnRange(_) => {
                return Err(Error::InvalidOperation(
                    "AggregateCountAndSumOnRange may not wrap AggregateCountOnRange — the \
                     aggregate variants are orthogonal",
                ));
            }
            QueryItem::AggregateSumOnRange(_) => {
                return Err(Error::InvalidOperation(
                    "AggregateCountAndSumOnRange may not wrap AggregateSumOnRange — the \
                     aggregate variants are orthogonal",
                ));
            }
            _ => {}
        }
        if self.default_subquery_branch.subquery.is_some()
            || self.default_subquery_branch.subquery_path.is_some()
        {
            return Err(Error::InvalidOperation(
                "AggregateCountAndSumOnRange queries may not carry a default subquery branch",
            ));
        }
        if let Some(branches) = &self.conditional_subquery_branches
            && !branches.is_empty()
        {
            return Err(Error::InvalidOperation(
                "AggregateCountAndSumOnRange queries may not carry conditional subquery \
                 branches",
            ));
        }
        Ok(inner)
    }

    /// Validates the carrier shape: an outer query whose items are
    /// `Key`/`Range`-like (NOT `AggregateCountAndSumOnRange`), and whose
    /// `default_subquery_branch.subquery` resolves to a valid leaf
    /// `AggregateCountAndSumOnRange` query (possibly after walking a
    /// `subquery_path`).
    ///
    /// Returns a reference to the leaf's inner range `QueryItem`.
    ///
    /// Rules enforced:
    /// 1. Items must be non-empty.
    /// 2. Each item must be `Key(_)` or a `Range*(_)` variant — explicitly
    ///    NOT `AggregateCountAndSumOnRange` (those route through the leaf
    ///    validator) and NOT `RangeFull` (use a leaf
    ///    `AggregateCountAndSumOnRange` on the parent instead).
    /// 3. `default_subquery_branch.subquery` must be `Some(_)`. Its target
    ///    query must itself validate as a leaf `AggregateCountAndSumOnRange`
    ///    query.
    /// 4. `default_subquery_branch.subquery_path` may be `Some(_)` (typically
    ///    names the path from each outer-key match to the leaf subtree).
    ///    When set, every element must be a non-empty key.
    /// 5. `conditional_subquery_branches` must be `None` or empty
    ///    (out of scope for the initial implementation).
    pub fn validate_carrier_aggregate_count_and_sum_on_range(&self) -> Result<&QueryItem, Error> {
        if self.items.is_empty() {
            return Err(Error::InvalidOperation(
                "carrier AggregateCountAndSumOnRange query must have at least one outer item",
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
                        "carrier AggregateCountAndSumOnRange query may not have a RangeFull \
                         outer item",
                    ));
                }
                QueryItem::AggregateCountAndSumOnRange(_) => {
                    return Err(Error::InvalidOperation(
                        "carrier AggregateCountAndSumOnRange query may not own an \
                         AggregateCountAndSumOnRange item — use the leaf shape instead",
                    ));
                }
                QueryItem::AggregateCountOnRange(_) => {
                    return Err(Error::InvalidOperation(
                        "carrier AggregateCountAndSumOnRange query may not own an \
                         AggregateCountOnRange item — the aggregate variants are orthogonal",
                    ));
                }
                QueryItem::AggregateSumOnRange(_) => {
                    return Err(Error::InvalidOperation(
                        "carrier AggregateCountAndSumOnRange query may not own an \
                         AggregateSumOnRange item — the aggregate variants are orthogonal",
                    ));
                }
            }
        }
        let subquery = match self.default_subquery_branch.subquery.as_deref() {
            Some(sub) => sub,
            None => {
                return Err(Error::InvalidOperation(
                    "carrier AggregateCountAndSumOnRange query must set \
                     default_subquery_branch.subquery to a leaf \
                     `AggregateCountAndSumOnRange` query",
                ));
            }
        };
        if let Some(path) = &self.default_subquery_branch.subquery_path
            && path.iter().any(|k| k.is_empty())
        {
            return Err(Error::InvalidOperation(
                "carrier AggregateCountAndSumOnRange query's subquery_path must contain \
                 non-empty keys",
            ));
        }
        if let Some(branches) = &self.conditional_subquery_branches
            && !branches.is_empty()
        {
            return Err(Error::InvalidOperation(
                "carrier AggregateCountAndSumOnRange query may not carry conditional \
                 subquery branches (out of scope for this feature)",
            ));
        }
        // The subquery must validate as a leaf `AggregateCountAndSumOnRange`.
        subquery.validate_leaf_aggregate_count_and_sum_on_range()
    }
}

#[cfg(test)]
mod tests {
    use indexmap::IndexMap;

    use crate::{query_item::QueryItem, Query, SubqueryBranch};

    // ---------- AggregateCountAndSumOnRange (combined) validator tests ----------
    //
    // These hit each numbered rule in
    // `Query::validate_aggregate_count_and_sum_on_range` independently and
    // confirm the happy path returns the inner range.

    fn make_combined_query(inner: QueryItem) -> Query {
        Query::new_aggregate_count_and_sum_on_range(inner)
    }

    #[test]
    fn validate_combined_happy_path_returns_inner() {
        let q = make_combined_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        let inner = q
            .validate_aggregate_count_and_sum_on_range()
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
    fn validate_combined_rejects_extra_items() {
        let mut q = make_combined_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        q.items.push(QueryItem::Key(b"extra".to_vec()));
        let err = q
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("two-item query must fail");
        assert!(matches!(err, crate::error::Error::InvalidOperation(_)));
    }

    #[test]
    fn validate_combined_rejects_inner_key() {
        let q = make_combined_query(QueryItem::Key(b"k".to_vec()));
        let err = q
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("inner Key must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(msg.contains("Key")),
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_combined_rejects_inner_range_full() {
        let q = make_combined_query(QueryItem::RangeFull(std::ops::RangeFull));
        let err = q
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("inner RangeFull must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(msg.contains("RangeFull")),
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_combined_rejects_nested_aggregates() {
        // Combined wrapping combined.
        let q1 = make_combined_query(QueryItem::AggregateCountAndSumOnRange(Box::new(
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        )));
        let err = q1
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("nested combined must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("AggregateCountAndSumOnRange"));
            }
            _ => panic!("expected InvalidOperation"),
        }

        // Combined wrapping count.
        let q2 = make_combined_query(QueryItem::AggregateCountOnRange(Box::new(
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        )));
        let err = q2
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("combined wrapping count must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("AggregateCountOnRange"));
            }
            _ => panic!("expected InvalidOperation"),
        }

        // Combined wrapping sum.
        let q3 = make_combined_query(QueryItem::AggregateSumOnRange(Box::new(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        ))));
        let err = q3
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("combined wrapping sum must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("AggregateSumOnRange"));
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_combined_rejects_subquery_branch() {
        let mut q = make_combined_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        q.set_subquery(Query::new());
        let err = q
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("subquery must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(msg.contains("subquery")),
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_combined_rejects_conditional_subquery_branches() {
        let mut q = make_combined_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        q.add_conditional_subquery(QueryItem::Key(b"k".to_vec()), None, Some(Query::new()));
        let err = q
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("conditional branches must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("conditional"));
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn has_aggregate_count_and_sum_on_range_anywhere_walks_subqueries() {
        // No combined anywhere → false.
        let plain = Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        assert!(!plain.has_aggregate_count_and_sum_on_range_anywhere());

        // Top-level → true.
        let top = make_combined_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        assert!(top.has_aggregate_count_and_sum_on_range_anywhere());

        // Hidden inside default subquery branch.
        let inner = make_combined_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        let mut hidden =
            Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        hidden.set_subquery(inner);
        assert!(hidden.aggregate_count_and_sum_on_range().is_none());
        assert!(hidden.has_aggregate_count_and_sum_on_range_anywhere());

        // Hidden inside a conditional subquery's subquery.
        let inner2 = make_combined_query(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        let mut conditional =
            Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        conditional.add_conditional_subquery(QueryItem::Key(b"k".to_vec()), None, Some(inner2));
        assert!(conditional.has_aggregate_count_and_sum_on_range_anywhere());

        // Combined appearing as the SELECTOR of a conditional branch.
        let mut selector =
            Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        selector.add_conditional_subquery(
            QueryItem::AggregateCountAndSumOnRange(Box::new(QueryItem::Range(
                b"a".to_vec()..b"z".to_vec(),
            ))),
            None,
            None,
        );
        assert!(selector.has_aggregate_count_and_sum_on_range_anywhere());
    }

    // ---------- Cross-aggregate dispatch (combined side) ----------

    #[test]
    fn validate_combined_dispatch_returns_a_known_error_for_non_combined_query() {
        // A query with no ACASOR item routes through the
        // `validate_aggregate_count_and_sum_on_range` Err arm — pin
        // the exact rejection so a refactor that changed the message
        // would be caught.
        let q = Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        let err = q
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("non-combined query must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(
                msg.contains("AggregateCountAndSumOnRange"),
                "unexpected message: {msg}"
            ),
            _ => panic!("expected InvalidOperation"),
        }
    }

    // ---------- Carrier combined-aggregate validation tests ----------
    //
    // The carrier shape is an outer query with `Key`/`Range*` items whose
    // `default_subquery_branch.subquery` resolves to a leaf
    // `AggregateCountAndSumOnRange` query. It is the multi-outer-key
    // extension of the leaf shape, returning one `(u64, i64)` per outer
    // key. These tests mirror the ACOR/ASOR carrier validator tests.

    fn make_leaf_combined_subquery() -> Query {
        Query::new_aggregate_count_and_sum_on_range(QueryItem::Range(b"a".to_vec()..b"z".to_vec()))
    }

    #[test]
    fn validate_carrier_combined_happy_path_keys_outer_with_subquery_path() {
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"brand_000".to_vec()));
        carrier.items.push(QueryItem::Key(b"brand_001".to_vec()));
        carrier.set_subquery_path(vec![b"color".to_vec()]);
        carrier.set_subquery(make_leaf_combined_subquery());
        let inner = carrier
            .validate_aggregate_count_and_sum_on_range()
            .expect("carrier should validate");
        assert!(matches!(inner, QueryItem::Range(_)));
        carrier
            .validate_carrier_aggregate_count_and_sum_on_range()
            .expect("carrier validator should accept");
        // Leaf validator must reject (carrier-level items aren't ACASOR).
        assert!(carrier
            .validate_leaf_aggregate_count_and_sum_on_range()
            .is_err());
    }

    #[test]
    fn validate_carrier_combined_happy_path_no_subquery_path() {
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"a".to_vec()));
        carrier.set_subquery(make_leaf_combined_subquery());
        carrier
            .validate_aggregate_count_and_sum_on_range()
            .expect("carrier without subquery_path should validate");
    }

    #[test]
    fn validate_carrier_combined_rejects_combined_at_both_levels() {
        let mut q = Query::new_aggregate_count_and_sum_on_range(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        ));
        q.set_subquery(make_leaf_combined_subquery());
        let err = q
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("ACASOR at both levels must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(
                    msg.contains("AggregateCountAndSumOnRange") || msg.contains("subquery"),
                    "unexpected message: {msg}"
                );
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_combined_rejects_range_full_outer() {
        let mut carrier = Query::new();
        carrier
            .items
            .push(QueryItem::RangeFull(std::ops::RangeFull));
        carrier.set_subquery(make_leaf_combined_subquery());
        let err = carrier
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("RangeFull outer must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("RangeFull"), "unexpected message: {msg}");
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_combined_rejects_combined_outer_item() {
        // Both Key and ACASOR at the carrier level. The leaf validator's
        // items-len check fires first.
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        carrier
            .items
            .push(QueryItem::AggregateCountAndSumOnRange(Box::new(
                QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
            )));
        carrier.set_subquery(make_leaf_combined_subquery());
        let err = carrier
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("ACASOR + Key outer items must fail");
        assert!(matches!(err, crate::error::Error::InvalidOperation(_)));
    }

    #[test]
    fn validate_carrier_combined_rejects_carrier_with_missing_subquery() {
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        let err = carrier
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("carrier without subquery must fail");
        assert!(matches!(err, crate::error::Error::InvalidOperation(_)));
    }

    #[test]
    fn validate_carrier_combined_rejects_non_combined_subquery() {
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        let regular_sub =
            Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        carrier.set_subquery(regular_sub);
        let err = carrier
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("non-ACASOR subquery must fail");
        assert!(matches!(err, crate::error::Error::InvalidOperation(_)));
    }

    #[test]
    fn validate_carrier_combined_rejects_conditional_branches() {
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        carrier.set_subquery(make_leaf_combined_subquery());
        carrier.add_conditional_subquery(
            QueryItem::Key(b"k".to_vec()),
            None,
            Some(make_leaf_combined_subquery()),
        );
        let err = carrier
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("carrier conditional branches must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("conditional"), "unexpected message: {msg}")
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_combined_rejects_empty_outer_items() {
        let mut carrier = Query::new();
        carrier.set_subquery(make_leaf_combined_subquery());
        let err = carrier
            .validate_carrier_aggregate_count_and_sum_on_range()
            .expect_err("empty outer items must fail");
        assert!(matches!(err, crate::error::Error::InvalidOperation(_)));
    }

    #[test]
    fn validate_carrier_combined_rejects_nested_carrier() {
        let mut inner_carrier = Query::new();
        inner_carrier
            .items
            .push(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        inner_carrier.set_subquery(make_leaf_combined_subquery());

        let mut outer_carrier = Query::new();
        outer_carrier
            .items
            .push(QueryItem::Range(b"A".to_vec()..b"Z".to_vec()));
        outer_carrier.set_subquery(inner_carrier);

        let err = outer_carrier
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("nested carrier (Range x Range x ACASOR) must be rejected");
        assert!(matches!(err, crate::error::Error::InvalidOperation(_)));
    }

    #[test]
    fn validate_carrier_combined_rejects_carrier_subquery_with_invalid_inner() {
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        carrier.set_subquery(Query::new_aggregate_count_and_sum_on_range(QueryItem::Key(
            b"k".to_vec(),
        )));
        let err = carrier
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("malformed inner Key in subquery ACASOR must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(
                msg.contains("may not wrap Key"),
                "unexpected message: {msg}"
            ),
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_combined_rejects_empty_subquery_path_element() {
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        carrier.set_subquery_path(vec![b"".to_vec()]);
        carrier.set_subquery(make_leaf_combined_subquery());
        let err = carrier
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("empty subquery_path key must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("non-empty keys"), "unexpected message: {msg}")
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_combined_accepts_range_outer_items() {
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
            carrier.set_subquery(make_leaf_combined_subquery());
            carrier
                .validate_aggregate_count_and_sum_on_range()
                .expect("carrier with Range* outer should validate");
        }
    }

    #[test]
    fn validate_carrier_combined_direct_rejects_missing_subquery() {
        let mut carrier = Query::new();
        carrier.insert_key(b"k".to_vec());
        let err = carrier
            .validate_carrier_aggregate_count_and_sum_on_range()
            .expect_err("missing subquery must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("must set"), "unexpected message: {msg}")
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_combined_direct_rejects_combined_outer_item() {
        let mut carrier = Query::new();
        carrier
            .items
            .push(QueryItem::AggregateCountAndSumOnRange(Box::new(
                QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
            )));
        carrier.set_subquery(make_leaf_combined_subquery());
        let err = carrier
            .validate_carrier_aggregate_count_and_sum_on_range()
            .expect_err("ACASOR outer item via direct carrier validator must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(
                msg.contains("may not own an") || msg.contains("AggregateCountAndSumOnRange"),
                "unexpected message: {msg}"
            ),
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_combined_direct_rejects_range_full_outer() {
        let mut carrier = Query::new();
        carrier
            .items
            .push(QueryItem::RangeFull(std::ops::RangeFull));
        carrier.set_subquery(make_leaf_combined_subquery());
        let err = carrier
            .validate_carrier_aggregate_count_and_sum_on_range()
            .expect_err("RangeFull outer via direct carrier validator must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("RangeFull"), "unexpected message: {msg}")
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_combined_direct_rejects_count_outer_item() {
        let mut carrier = Query::new();
        carrier
            .items
            .push(QueryItem::AggregateCountOnRange(Box::new(
                QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
            )));
        carrier.set_subquery(make_leaf_combined_subquery());
        let err = carrier
            .validate_carrier_aggregate_count_and_sum_on_range()
            .expect_err("AggregateCountOnRange outer item must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(
                msg.contains("AggregateCountOnRange"),
                "unexpected message: {msg}"
            ),
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_combined_direct_rejects_sum_outer_item() {
        let mut carrier = Query::new();
        carrier
            .items
            .push(QueryItem::AggregateSumOnRange(Box::new(QueryItem::Range(
                b"a".to_vec()..b"z".to_vec(),
            ))));
        carrier.set_subquery(make_leaf_combined_subquery());
        let err = carrier
            .validate_carrier_aggregate_count_and_sum_on_range()
            .expect_err("AggregateSumOnRange outer item must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(
                msg.contains("AggregateSumOnRange"),
                "unexpected message: {msg}"
            ),
            _ => panic!("expected InvalidOperation"),
        }
    }

    // ---------- Leaf rules accessible via the dispatcher ----------

    #[test]
    fn validate_leaf_combined_accepts_empty_conditional_branches_map() {
        let mut q = Query::new_aggregate_count_and_sum_on_range(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        ));
        q.conditional_subquery_branches = Some(IndexMap::new());
        let inner = q
            .validate_aggregate_count_and_sum_on_range()
            .expect("empty conditional map must validate");
        assert!(matches!(inner, QueryItem::Range(_)));
    }

    #[test]
    fn validate_leaf_combined_rejects_default_subquery_branch() {
        let mut q = Query::new_aggregate_count_and_sum_on_range(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        ));
        q.default_subquery_branch = SubqueryBranch {
            subquery_path: None,
            subquery: Some(Box::new(Query::new())),
        };
        let err = q
            .validate_aggregate_count_and_sum_on_range()
            .expect_err("default subquery branch must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(msg.contains("subquery")),
            _ => panic!("expected InvalidOperation"),
        }
    }
}
