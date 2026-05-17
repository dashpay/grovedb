//! `AggregateSumOnRange` Query helpers and validation.
//!
//! Sum-side mirror of [`crate::aggregate_count`]. Owns the Query-level
//! construction, detection, and validation of `AggregateSumOnRange`
//! queries — which sum the children matched by an inner range against
//! a `ProvableSumTree` (or `ProvableCountProvableSumTree`) host.
//!
//! They come in two shapes:
//!
//! - **Leaf** — a query whose single item is `AggregateSumOnRange(_)`.
//!   Produces a single `i64` sum over the inner range.
//!
//! - **Carrier** — a query whose items are `Key(_)` / `Range*(_)` and
//!   whose `default_subquery_branch.subquery` resolves (after walking
//!   the optional `subquery_path`) to a valid leaf
//!   `AggregateSumOnRange`. Produces one `i64` per matched outer
//!   key — the natural per-outer-key extension of the leaf shape.
//!
//! All sum-side validation lives in this file so the much larger
//! `Query` core in `query.rs` stays focused on the general-purpose
//! query plumbing.

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
    /// `AggregateSumOnRange` is present. On success, returns a reference
    /// to the inner range `QueryItem` describing the keys being summed
    /// (the same item regardless of whether the surrounding query is the
    /// leaf shape or the carrier shape).
    ///
    /// Top-level dispatcher: classifies the query as either
    /// - **leaf** (the query owns an `AggregateSumOnRange` item directly —
    ///   the original single-`i64` shape), or
    /// - **carrier** (the query is an outer fan-out of `Key`/`Range` items
    ///   whose `default_subquery_branch.subquery` resolves to a leaf
    ///   `AggregateSumOnRange` — the per-outer-key shape)
    ///
    /// and forwards to the corresponding rule set. See
    /// [`Self::validate_leaf_aggregate_sum_on_range`] and
    /// [`Self::validate_carrier_aggregate_sum_on_range`] for the precise
    /// rules in each case.
    ///
    /// `SizedQuery::limit` / `SizedQuery::offset` checks live at the
    /// `PathQuery` / `SizedQuery` layer.
    pub fn validate_aggregate_sum_on_range(&self) -> Result<&QueryItem, Error> {
        if self.aggregate_sum_on_range().is_some() {
            // Owns an aggregate-sum at this level → leaf shape.
            self.validate_leaf_aggregate_sum_on_range()
        } else if self.has_aggregate_sum_on_range_anywhere() {
            // Doesn't own an aggregate-sum but a nested subquery does → carrier shape.
            self.validate_carrier_aggregate_sum_on_range()
        } else {
            Err(Error::InvalidOperation(
                "validate_aggregate_sum_on_range called on a query without an \
                 AggregateSumOnRange item",
            ))
        }
    }

    /// Validates the leaf shape: a query whose single item is
    /// `AggregateSumOnRange(_)` and whose surroundings carry no subquery
    /// branches. Returns a reference to the inner range `QueryItem`.
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
    pub fn validate_leaf_aggregate_sum_on_range(&self) -> Result<&QueryItem, Error> {
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

    /// Validates the carrier shape: an outer query whose items are
    /// `Key`/`Range`-like (NOT `AggregateSumOnRange`), and whose
    /// `default_subquery_branch.subquery` resolves to a valid leaf `AggregateSumOnRange`
    /// query (possibly after walking a `subquery_path`).
    ///
    /// Returns a reference to the leaf's inner range `QueryItem` — the
    /// same kind of value [`Self::validate_leaf_aggregate_sum_on_range`]
    /// returns for a leaf-shape query.
    ///
    /// Rules enforced:
    /// 1. Items must be non-empty.
    /// 2. Each item must be `Key(_)` or a `Range*(_)` variant — explicitly
    ///    NOT `AggregateSumOnRange` (those route through the leaf
    ///    validator) and NOT `RangeFull` (use a leaf `AggregateSumOnRange` on the parent
    ///    instead).
    /// 3. `default_subquery_branch.subquery` must be `Some(_)`. Its target
    ///    query must itself validate as a leaf `AggregateSumOnRange` query.
    /// 4. `default_subquery_branch.subquery_path` may be `Some(_)`
    ///    (typically names the path from each outer-key match to the leaf
    ///    subtree). When set, every element must be a non-empty key.
    /// 5. `conditional_subquery_branches` must be `None` or empty
    ///    (out of scope for the initial implementation).
    pub fn validate_carrier_aggregate_sum_on_range(&self) -> Result<&QueryItem, Error> {
        if self.items.is_empty() {
            return Err(Error::InvalidOperation(
                "carrier AggregateSumOnRange query must have at least one outer item",
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
                        "carrier AggregateSumOnRange query may not have a RangeFull outer item",
                    ));
                }
                QueryItem::AggregateSumOnRange(_) => {
                    return Err(Error::InvalidOperation(
                        "carrier AggregateSumOnRange query may not own an \
                         AggregateSumOnRange item — use the leaf shape instead",
                    ));
                }
                QueryItem::AggregateCountOnRange(_) => {
                    return Err(Error::InvalidOperation(
                        "carrier AggregateSumOnRange query may not own an \
                         AggregateCountOnRange item — the two aggregate variants are orthogonal",
                    ));
                }
                QueryItem::AggregateCountAndSumOnRange(_) => {
                    return Err(Error::InvalidOperation(
                        "carrier AggregateSumOnRange query may not own an \
                         AggregateCountAndSumOnRange item — the aggregate variants are \
                         orthogonal",
                    ));
                }
            }
        }
        let subquery = match self.default_subquery_branch.subquery.as_deref() {
            Some(sub) => sub,
            None => {
                return Err(Error::InvalidOperation(
                    "carrier AggregateSumOnRange query must set \
                     default_subquery_branch.subquery to a leaf `AggregateSumOnRange` query",
                ));
            }
        };
        if let Some(path) = &self.default_subquery_branch.subquery_path
            && path.iter().any(|k| k.is_empty())
        {
            return Err(Error::InvalidOperation(
                "carrier AggregateSumOnRange query's subquery_path must contain non-empty keys",
            ));
        }
        if let Some(branches) = &self.conditional_subquery_branches
            && !branches.is_empty()
        {
            return Err(Error::InvalidOperation(
                "carrier AggregateSumOnRange query may not carry conditional subquery \
                 branches (out of scope for this feature)",
            ));
        }
        // The subquery must validate as a leaf `AggregateSumOnRange` (which is what the
        // proof descent will ultimately consume).
        subquery.validate_leaf_aggregate_sum_on_range()
    }
}

#[cfg(test)]
mod tests {
    use indexmap::IndexMap;

    use crate::{query_item::QueryItem, Query, SubqueryBranch};

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

    // ---------- Carrier aggregate-sum validation tests ----------
    //
    // The carrier shape is an outer query with `Key`/`Range*` items whose
    // `default_subquery_branch.subquery` resolves to a leaf
    // `AggregateSumOnRange` query. It is the multi-outer-key extension of
    // the leaf shape, returning one signed sum per outer key. These tests
    // mirror the ACOR carrier validator tests in `aggregate_count.rs`.

    fn make_leaf_aggregate_sum_subquery() -> Query {
        Query::new_aggregate_sum_on_range(QueryItem::Range(b"a".to_vec()..b"z".to_vec()))
    }

    #[test]
    fn validate_carrier_aggregate_sum_happy_path_keys_outer_with_subquery_path() {
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"brand_000".to_vec()));
        carrier.items.push(QueryItem::Key(b"brand_001".to_vec()));
        carrier.set_subquery_path(vec![b"color".to_vec()]);
        carrier.set_subquery(make_leaf_aggregate_sum_subquery());
        // Top-level dispatcher accepts the carrier and returns the leaf's
        // inner range.
        let inner = carrier
            .validate_aggregate_sum_on_range()
            .expect("carrier should validate");
        assert!(matches!(inner, QueryItem::Range(_)));
        // And the dedicated carrier validator agrees.
        carrier
            .validate_carrier_aggregate_sum_on_range()
            .expect("carrier validator should accept");
        // Leaf validator must reject (carrier-level items aren't aggregate-sum).
        assert!(carrier.validate_leaf_aggregate_sum_on_range().is_err());
    }

    #[test]
    fn validate_carrier_aggregate_sum_happy_path_no_subquery_path() {
        // subquery_path is optional — the leaf `AggregateSumOnRange` may
        // be directly under each outer match.
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"a".to_vec()));
        carrier.set_subquery(make_leaf_aggregate_sum_subquery());
        carrier
            .validate_aggregate_sum_on_range()
            .expect("carrier without subquery_path should validate");
    }

    #[test]
    fn validate_carrier_aggregate_sum_rejects_aggregate_sum_at_both_levels() {
        // Carrier itself owns an aggregate-sum AND its subquery is also
        // an aggregate-sum. The top-level dispatcher routes to the LEAF
        // validator first (because aggregate_sum_on_range() returns Some
        // at carrier level), so the leaf's "single item" / "no subquery"
        // rule catches the malformed shape.
        let mut q =
            Query::new_aggregate_sum_on_range(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        q.set_subquery(make_leaf_aggregate_sum_subquery());
        let err = q
            .validate_aggregate_sum_on_range()
            .expect_err("aggregate-sum at both levels must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(
                    msg.contains("AggregateSumOnRange") || msg.contains("subquery"),
                    "unexpected message: {msg}"
                );
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_aggregate_sum_rejects_range_full_outer() {
        let mut carrier = Query::new();
        carrier
            .items
            .push(QueryItem::RangeFull(std::ops::RangeFull));
        carrier.set_subquery(make_leaf_aggregate_sum_subquery());
        let err = carrier
            .validate_aggregate_sum_on_range()
            .expect_err("RangeFull outer must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("RangeFull"), "unexpected message: {msg}");
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_aggregate_sum_rejects_aggregate_sum_outer_item() {
        // Both a Key and an AggregateSumOnRange item at the carrier
        // level. The leaf validator's items-len check fires first (since
        // there's an aggregate-sum item in items, aggregate_sum_on_range()
        // returns Some, and len != 1).
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        carrier
            .items
            .push(QueryItem::AggregateSumOnRange(Box::new(QueryItem::Range(
                b"a".to_vec()..b"z".to_vec(),
            ))));
        carrier.set_subquery(make_leaf_aggregate_sum_subquery());
        let err = carrier
            .validate_aggregate_sum_on_range()
            .expect_err("aggregate-sum + Key outer items must fail");
        assert!(matches!(err, crate::error::Error::InvalidOperation(_)));
    }

    #[test]
    fn validate_carrier_aggregate_sum_rejects_carrier_with_missing_subquery() {
        // Outer items present but no subquery → not a carrier (and not a
        // leaf), so the top-level dispatcher routes to the
        // "not an aggregate-sum query" error.
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        let err = carrier
            .validate_aggregate_sum_on_range()
            .expect_err("carrier without subquery must fail");
        assert!(matches!(err, crate::error::Error::InvalidOperation(_)));
    }

    #[test]
    fn validate_carrier_aggregate_sum_rejects_non_aggregate_sum_subquery() {
        // Outer Keys + subquery that is NOT an aggregate-sum (just a
        // regular range query) → not a valid carrier aggregate-sum.
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        let regular_sub =
            Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        carrier.set_subquery(regular_sub);
        let err = carrier
            .validate_aggregate_sum_on_range()
            .expect_err("non-aggregate-sum subquery must fail");
        assert!(matches!(err, crate::error::Error::InvalidOperation(_)));
    }

    #[test]
    fn validate_carrier_aggregate_sum_rejects_conditional_branches() {
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        carrier.set_subquery(make_leaf_aggregate_sum_subquery());
        carrier.add_conditional_subquery(
            QueryItem::Key(b"k".to_vec()),
            None,
            Some(make_leaf_aggregate_sum_subquery()),
        );
        let err = carrier
            .validate_aggregate_sum_on_range()
            .expect_err("carrier conditional branches must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("conditional"), "unexpected message: {msg}")
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_aggregate_sum_rejects_empty_outer_items() {
        // Empty items + leaf `AggregateSumOnRange` subquery → not a valid
        // carrier. (Empty outer means no outer key to iterate; doesn't
        // make sense.)
        let mut carrier = Query::new();
        carrier.set_subquery(make_leaf_aggregate_sum_subquery());
        let err = carrier
            .validate_carrier_aggregate_sum_on_range()
            .expect_err("empty outer items must fail");
        assert!(matches!(err, crate::error::Error::InvalidOperation(_)));
    }

    #[test]
    fn validate_carrier_aggregate_sum_rejects_nested_carrier() {
        // Out of scope: a "Range × Range × AggregateSumOnRange" shape —
        // i.e. an outer carrier whose subquery is itself another carrier.
        // The carrier validator delegates to the *leaf* validator for the
        // subquery, so a carrier-of-carrier subquery fails the leaf rules.
        let mut inner_carrier = Query::new();
        inner_carrier
            .items
            .push(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        inner_carrier.set_subquery(make_leaf_aggregate_sum_subquery());

        let mut outer_carrier = Query::new();
        outer_carrier
            .items
            .push(QueryItem::Range(b"A".to_vec()..b"Z".to_vec()));
        outer_carrier.set_subquery(inner_carrier);

        let err = outer_carrier
            .validate_aggregate_sum_on_range()
            .expect_err("nested carrier (Range x Range x ASOR) must be rejected");
        assert!(matches!(err, crate::error::Error::InvalidOperation(_)));
    }

    #[test]
    fn validate_carrier_aggregate_sum_rejects_carrier_subquery_with_invalid_inner() {
        // The carrier validator delegates to the leaf validator for the
        // subquery, so a malformed leaf `AggregateSumOnRange` (e.g.
        // wrapping `Key`) is surfaced via the carrier path.
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        carrier.set_subquery(Query::new_aggregate_sum_on_range(QueryItem::Key(
            b"k".to_vec(),
        )));
        let err = carrier
            .validate_aggregate_sum_on_range()
            .expect_err("malformed inner Key in subquery aggregate-sum must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(
                msg.contains("may not wrap Key"),
                "unexpected message: {msg}"
            ),
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_aggregate_sum_rejects_empty_subquery_path_element() {
        // A carrier's subquery_path may not contain empty keys — those
        // would point at "no key" in the intermediate descent, which the
        // merk single-key prover can't satisfy.
        let mut carrier = Query::new();
        carrier.items.push(QueryItem::Key(b"k".to_vec()));
        carrier.set_subquery_path(vec![b"".to_vec()]);
        carrier.set_subquery(make_leaf_aggregate_sum_subquery());
        let err = carrier
            .validate_aggregate_sum_on_range()
            .expect_err("empty subquery_path key must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("non-empty keys"), "unexpected message: {msg}")
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_aggregate_sum_accepts_range_outer_items() {
        // A carrier may use Range outer items. Verify the validator
        // agrees for every Range* variant the rule whitelists.
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
            carrier.set_subquery(make_leaf_aggregate_sum_subquery());
            carrier
                .validate_aggregate_sum_on_range()
                .expect("carrier with Range* outer should validate");
        }
    }

    #[test]
    fn validate_carrier_aggregate_sum_direct_rejects_missing_subquery() {
        // Carrier-shaped items but no subquery — the carrier validator's
        // "subquery must be Some" branch fires.
        let mut carrier = Query::new();
        carrier.insert_key(b"k".to_vec());
        let err = carrier
            .validate_carrier_aggregate_sum_on_range()
            .expect_err("missing subquery must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("must set"), "unexpected message: {msg}")
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_aggregate_sum_direct_rejects_aggregate_sum_outer_item() {
        // aggregate-sum appears in outer items + a leaf subquery is set.
        // The top-level dispatcher routes to the leaf validator; calling
        // the carrier validator directly is the only way to hit the
        // carrier-side rule.
        let mut carrier = Query::new();
        carrier
            .items
            .push(QueryItem::AggregateSumOnRange(Box::new(QueryItem::Range(
                b"a".to_vec()..b"z".to_vec(),
            ))));
        carrier.set_subquery(make_leaf_aggregate_sum_subquery());
        let err = carrier
            .validate_carrier_aggregate_sum_on_range()
            .expect_err("aggregate-sum outer item via direct carrier validator must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(
                msg.contains("may not own an") || msg.contains("AggregateSumOnRange"),
                "unexpected message: {msg}"
            ),
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_aggregate_sum_direct_rejects_range_full_outer() {
        // RangeFull outer + leaf subquery — exercise the carrier
        // validator's `RangeFull` arm directly.
        let mut carrier = Query::new();
        carrier
            .items
            .push(QueryItem::RangeFull(std::ops::RangeFull));
        carrier.set_subquery(make_leaf_aggregate_sum_subquery());
        let err = carrier
            .validate_carrier_aggregate_sum_on_range()
            .expect_err("RangeFull outer via direct carrier validator must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => {
                assert!(msg.contains("RangeFull"), "unexpected message: {msg}")
            }
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_carrier_aggregate_sum_direct_rejects_aggregate_count_outer_item() {
        // ACOR in outer items + leaf subquery — exercise the
        // carrier-validator's `QueryItem::AggregateCountOnRange(_)` arm.
        let mut carrier = Query::new();
        carrier
            .items
            .push(QueryItem::AggregateCountOnRange(Box::new(
                QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
            )));
        carrier.set_subquery(make_leaf_aggregate_sum_subquery());
        let err = carrier
            .validate_carrier_aggregate_sum_on_range()
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
    fn validate_carrier_aggregate_sum_direct_rejects_aggregate_count_and_sum_outer_item() {
        // ACASOR in outer items + leaf subquery — exercise the
        // carrier-validator's `QueryItem::AggregateCountAndSumOnRange(_)`
        // arm.
        let mut carrier = Query::new();
        carrier
            .items
            .push(QueryItem::AggregateCountAndSumOnRange(Box::new(
                QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
            )));
        carrier.set_subquery(make_leaf_aggregate_sum_subquery());
        let err = carrier
            .validate_carrier_aggregate_sum_on_range()
            .expect_err("AggregateCountAndSumOnRange outer item must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(
                msg.contains("AggregateCountAndSumOnRange"),
                "unexpected message: {msg}"
            ),
            _ => panic!("expected InvalidOperation"),
        }
    }

    #[test]
    fn validate_aggregate_sum_dispatcher_rejects_non_aggregate_sum_query() {
        // The top-level dispatcher returns the "not an aggregate-sum"
        // error when neither shape matches.
        let q = Query::new_single_query_item(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        let err = q
            .validate_aggregate_sum_on_range()
            .expect_err("non-aggregate-sum query must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(msg.contains(
                "validate_aggregate_sum_on_range called on a query \
                              without an AggregateSumOnRange item"
            )),
            _ => panic!("expected InvalidOperation"),
        }
    }

    // ---------- Leaf rules accessible via the dispatcher ----------
    //
    // Pin a couple of cases that exercise the leaf branch of the
    // dispatcher (so the previously-existing leaf behavior is preserved
    // verbatim after the split).

    #[test]
    fn validate_leaf_aggregate_sum_accepts_empty_conditional_branches_map() {
        // An empty `Some(IndexMap::new())` is treated as "no branches"
        // by the leaf validator (the rule enforces non-empty rejection
        // only).
        let mut q =
            Query::new_aggregate_sum_on_range(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        q.conditional_subquery_branches = Some(IndexMap::new());
        let inner = q
            .validate_aggregate_sum_on_range()
            .expect("empty conditional map must validate");
        assert!(matches!(inner, QueryItem::Range(_)));
    }

    #[test]
    fn validate_leaf_aggregate_sum_rejects_default_subquery_branch() {
        let mut q =
            Query::new_aggregate_sum_on_range(QueryItem::Range(b"a".to_vec()..b"z".to_vec()));
        q.default_subquery_branch = SubqueryBranch {
            subquery_path: None,
            subquery: Some(Box::new(Query::new())),
        };
        let err = q
            .validate_aggregate_sum_on_range()
            .expect_err("default subquery branch must fail");
        match err {
            crate::error::Error::InvalidOperation(msg) => assert!(msg.contains("subquery")),
            _ => panic!("expected InvalidOperation"),
        }
    }
}
