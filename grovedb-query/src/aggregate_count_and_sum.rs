//! `AggregateCountAndSumOnRange` Query helpers and validation.
//!
//! Dual-axis sibling of [`crate::aggregate_count`] and
//! [`crate::aggregate_sum`]. Owns the Query-level construction,
//! detection, and validation of `AggregateCountAndSumOnRange`
//! queries — which return BOTH a `u64` count AND a signed `i64` sum
//! over an inner range from a single proof against a
//! `ProvableCountProvableSumTree` host.
//!
//! Leaf-only at the Query layer (no carrier shape today). All
//! combined-aggregate validation lives in this file so the much larger
//! `Query` core in `query.rs` stays focused on the general-purpose
//! query plumbing.

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
    /// `AggregateCountAndSumOnRange` is present. Mirror of
    /// `Query::validate_aggregate_count_on_range` /
    /// `validate_aggregate_sum_on_range` for the dual-axis
    /// `ProvableCountProvableSumTree` host.
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
    ///
    /// `SizedQuery::limit` / `SizedQuery::offset` checks live at the
    /// `PathQuery` / `SizedQuery` layer.
    pub fn validate_aggregate_count_and_sum_on_range(&self) -> Result<&QueryItem, Error> {
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
}

#[cfg(test)]
mod tests {
    use crate::{query_item::QueryItem, Query};

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
}
