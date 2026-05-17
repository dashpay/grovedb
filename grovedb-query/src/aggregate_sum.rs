//! `AggregateSumOnRange` Query helpers and validation.
//!
//! Sum-side mirror of [`crate::aggregate_count`]. Owns the Query-level
//! construction, detection, and validation of `AggregateSumOnRange`
//! queries — which sum the children matched by an inner range against
//! a `ProvableSumTree` host.
//!
//! Unlike the count side, the sum variant is leaf-only at the Query
//! layer (no carrier shape today). All sum-side validation lives in
//! this file so the much larger `Query` core in `query.rs` stays
//! focused on the general-purpose query plumbing.

use crate::{error::Error, query::Query, query_item::QueryItem};

impl Query {
    /// Creates an aggregate-sum-on-range query that sums the children matched
    /// by `range`. Mirrors `Query::new_aggregate_count_on_range` for
    /// `ProvableSumTree` instead of `ProvableCountTree`.
    ///
    /// `range` must be a true range variant; passing `Key`, `RangeFull`,
    /// another `AggregateSumOnRange`, or an `AggregateCountOnRange` is
    /// allowed at construction time but will be rejected by
    /// [`Self::validate_aggregate_sum_on_range`].
    pub fn new_aggregate_sum_on_range(range: QueryItem) -> Self {
        Self {
            items: vec![QueryItem::AggregateSumOnRange(Box::new(range))],
            left_to_right: true,
            ..Self::default()
        }
    }

    /// Returns `Some(...)` for any query containing an
    /// `AggregateSumOnRange` item, regardless of well-formedness.
    pub fn aggregate_sum_on_range(&self) -> Option<&QueryItem> {
        self.items
            .iter()
            .find(|item| item.is_aggregate_sum_on_range())
    }

    /// Mirror of `Query::has_aggregate_count_on_range_anywhere` for
    /// `AggregateSumOnRange`. Used by the prover/verifier to validate at
    /// entry — if any ASOR is present anywhere, the query must satisfy
    /// [`Self::validate_aggregate_sum_on_range`].
    pub fn has_aggregate_sum_on_range_anywhere(&self) -> bool {
        if self.aggregate_sum_on_range().is_some() {
            return true;
        }
        if let Some(sub) = self.default_subquery_branch.subquery.as_deref()
            && sub.has_aggregate_sum_on_range_anywhere()
        {
            return true;
        }
        if let Some(branches) = &self.conditional_subquery_branches {
            for (selector, branch) in branches {
                // The selector is itself a `QueryItem` and could carry an
                // `AggregateSumOnRange` tag (the type permits it even
                // though it would not be a meaningful conditional
                // matcher). Reject defensively so a hidden ASOR in a
                // selector cannot slip past the aggregate-shape check.
                if selector.is_aggregate_sum_on_range() {
                    return true;
                }
                if let Some(sub) = branch.subquery.as_deref()
                    && sub.has_aggregate_sum_on_range_anywhere()
                {
                    return true;
                }
            }
        }
        false
    }

    /// Validates the Query-level constraints that apply when an
    /// `AggregateSumOnRange` is present. Mirror of
    /// `Query::validate_aggregate_count_on_range` (in the
    /// `grovedb-query::aggregate_count` module) for `ProvableSumTree`.
    ///
    /// Rules enforced:
    ///
    /// 1. The query must contain exactly one item.
    /// 2. That item must be `AggregateSumOnRange(_)`.
    /// 3. The inner item must not be `Key` (use `has_raw` / `get_raw` for
    ///    existence tests).
    /// 4. The inner item must not be `RangeFull` (read the parent
    ///    `Element::ProvableSumTree` bytes directly for the unconditional
    ///    total).
    /// 5. The inner item must not itself be `AggregateSumOnRange`.
    /// 6. The inner item must not be `AggregateCountOnRange` (the two
    ///    aggregate variants are orthogonal).
    /// 7. `default_subquery_branch.subquery` and
    ///    `default_subquery_branch.subquery_path` must both be `None`.
    /// 8. `conditional_subquery_branches` must be `None` or empty.
    ///
    /// `SizedQuery::limit` / `SizedQuery::offset` checks live at the
    /// `PathQuery` / `SizedQuery` layer.
    pub fn validate_aggregate_sum_on_range(&self) -> Result<&QueryItem, Error> {
        if self.items.len() != 1 {
            return Err(Error::InvalidOperation(
                "AggregateSumOnRange must be the only item in the query",
            ));
        }
        let inner = match &self.items[0] {
            QueryItem::AggregateSumOnRange(inner) => inner.as_ref(),
            _ => {
                return Err(Error::InvalidOperation(
                    "validate_aggregate_sum_on_range called on a query without an \
                     AggregateSumOnRange item",
                ));
            }
        };
        match inner {
            QueryItem::Key(_) => {
                return Err(Error::InvalidOperation(
                    "AggregateSumOnRange may not wrap Key — use has_raw / get_raw for \
                     existence tests",
                ));
            }
            QueryItem::RangeFull(_) => {
                return Err(Error::InvalidOperation(
                    "AggregateSumOnRange may not wrap RangeFull — read the parent \
                     ProvableSumTree element for the unconditional total",
                ));
            }
            QueryItem::AggregateSumOnRange(_) => {
                return Err(Error::InvalidOperation(
                    "AggregateSumOnRange may not wrap another AggregateSumOnRange",
                ));
            }
            QueryItem::AggregateCountOnRange(_) => {
                return Err(Error::InvalidOperation(
                    "AggregateSumOnRange may not wrap AggregateCountOnRange — the two are \
                     orthogonal aggregate queries",
                ));
            }
            QueryItem::AggregateCountAndSumOnRange(_) => {
                return Err(Error::InvalidOperation(
                    "AggregateSumOnRange may not wrap AggregateCountAndSumOnRange — the \
                     aggregate variants are orthogonal",
                ));
            }
            _ => {}
        }
        if self.default_subquery_branch.subquery.is_some()
            || self.default_subquery_branch.subquery_path.is_some()
        {
            return Err(Error::InvalidOperation(
                "AggregateSumOnRange queries may not carry a default subquery branch",
            ));
        }
        if let Some(branches) = &self.conditional_subquery_branches
            && !branches.is_empty()
        {
            return Err(Error::InvalidOperation(
                "AggregateSumOnRange queries may not carry conditional subquery branches",
            ));
        }
        Ok(inner)
    }
}

#[cfg(test)]
mod tests {
    use crate::{query_item::QueryItem, Query};

    /// Sum-side mirror of `has_aggregate_count_on_range_anywhere_walks_subqueries`,
    /// with one extra case: an `AggregateSumOnRange` tag appearing as the
    /// *selector* (map key) of a conditional subquery branch. The selector
    /// is itself a `QueryItem` and the type permits ASOR there even though
    /// it would never be a meaningful matcher; the walker must surface it
    /// so the prove_query entry-point gate can reject the malformed shape.
    #[test]
    fn has_aggregate_sum_on_range_anywhere_walks_subqueries_and_selectors() {
        // No ASOR anywhere → false.
        let plain = Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        assert!(!plain.has_aggregate_sum_on_range_anywhere());

        // Top-level ASOR → true.
        let top = Query::new_aggregate_sum_on_range(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        assert!(top.has_aggregate_sum_on_range_anywhere());

        // ASOR hidden inside default_subquery_branch.subquery.
        let inner =
            Query::new_aggregate_sum_on_range(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        let mut hidden =
            Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        hidden.set_subquery(inner);
        assert!(hidden.aggregate_sum_on_range().is_none());
        assert!(
            hidden.has_aggregate_sum_on_range_anywhere(),
            "ASOR hidden in default subquery branch must be detected"
        );

        // ASOR hidden inside a conditional subquery branch's subquery.
        let inner2 =
            Query::new_aggregate_sum_on_range(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        let mut conditional =
            Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        conditional.add_conditional_subquery(QueryItem::Key(b"k".to_vec()), None, Some(inner2));
        assert!(
            conditional.has_aggregate_sum_on_range_anywhere(),
            "ASOR hidden in conditional subquery branch must be detected"
        );

        // ASOR appearing as the SELECTOR of a conditional branch. The
        // selector itself is a `QueryItem` and could carry an ASOR tag —
        // pre-fix this slipped past the walker because the iteration
        // looked only at `branch.subquery` and ignored the map key.
        let mut selector =
            Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        selector.add_conditional_subquery(
            QueryItem::AggregateSumOnRange(Box::new(QueryItem::Range(
                b"a".to_vec()..b"z".to_vec(),
            ))),
            None,
            None,
        );
        assert!(
            selector.has_aggregate_sum_on_range_anywhere(),
            "ASOR appearing as a conditional-branch selector must be detected"
        );
    }

    // ---------- Cross-aggregate orthogonality (sum side) ----------
    //
    // Pins the rejection arm that surfaces the rule "the three aggregate
    // variants are orthogonal — none of them may wrap any of the others
    // as their inner item". The matching arms for ACOR live in
    // `aggregate_count.rs`; ACASOR's symmetric arms live in
    // `aggregate_count_and_sum.rs`.

    #[test]
    fn validate_aggregate_sum_rejects_inner_aggregate_count_and_sum() {
        // ASOR wrapping ACASOR — exercises the
        // `QueryItem::AggregateCountAndSumOnRange(_)` arm inside
        // `validate_aggregate_sum_on_range`.
        let inner_combined = QueryItem::AggregateCountAndSumOnRange(Box::new(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        let q = Query::new_aggregate_sum_on_range(inner_combined);
        let err = q
            .validate_aggregate_sum_on_range()
            .expect_err("inner AggregateCountAndSumOnRange must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(
                    msg.contains("AggregateCountAndSumOnRange"),
                    "unexpected message: {msg}"
                );
            }
            _ => panic!("expected InvalidOperation"),
        }
    }
}
