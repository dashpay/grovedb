//! Query shape classification.
//!
//! A [`PathQuery`] is one type, but the engine serves several distinct
//! *shapes* through it: plain key selection, the three aggregate-on-range
//! families (each in a leaf and a carrier form), and count-offset
//! pagination. Today each entry point re-discovers the shape by calling
//! the `has_*` / `validate_*` helpers in its own order; this module
//! gives that discovery a single name, [`PathQuery::classify`], so the
//! reader, the prover, and the verifier can share one decision instead
//! of three copies of it.
//!
//! Classification is **pure** (no database access — a proof verifier,
//! which holds only the query, classifies identically to the prover),
//! **total** (every `PathQuery` maps to exactly one shape or to a typed
//! error naming the violated rule), and mirrors the gate order of
//! `prove_query_non_serialized`: aggregate-count, then aggregate-sum,
//! then combined, then count-offset pagination, then plain key
//! selection. Because prover and verifier must agree on what a query
//! *means*, any change to this ordering or to the underlying validators
//! is consensus-relevant and belongs behind a grove-version gate.
//!
//! What classification deliberately does **not** decide:
//!
//! - Envelope eligibility. A shape like
//!   [`PathQueryShape::CountOffsetPaginated`] classifies the same for a
//!   V0 and a V1 proof; whether the envelope supports it is the
//!   envelope gate's decision (`apply_count_offset_envelope_gate`).
//! - Tree types. The check that a count-offset target really is a
//!   `ProvableCountTree` (or that an aggregate leaf sits on the right
//!   provable tree) requires opening the merk and stays at execution /
//!   verification time. Classification is purely syntactic.

use grovedb_merk::proofs::query::query_item::QueryItem;

use crate::{Error, PathQuery};

/// Which aggregate-on-range family an aggregate-shaped query belongs
/// to. The three families are structurally identical (leaf and carrier
/// forms, same placement rules) and differ only in what the wrapped
/// range aggregates to: a count (`u64`), a sum (`i64`), or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateKind {
    /// `QueryItem::AggregateCountOnRange`
    Count,
    /// `QueryItem::AggregateSumOnRange`
    Sum,
    /// `QueryItem::AggregateCountAndSumOnRange`
    CountAndSum,
}

/// The shape of a [`PathQuery`] — what kind of read it describes, as
/// opposed to *where* (the path) or *which keys* (the items).
///
/// Borrowed views into the query keep classification allocation-free;
/// the referenced items live exactly as long as the query does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathQueryShape<'q> {
    /// Plain key selection — everything the regular query/proof
    /// pipeline serves, including subquery descent, limits, and
    /// `offset == Some(0)` (which the engine treats as no offset).
    KeySelection,
    /// A non-zero `SizedQuery::offset` over a single range item with no
    /// subqueries: the offset-paginated read of a count-bearing
    /// provable tree. `inner` is the single range item being paginated.
    CountOffsetPaginated {
        /// The single range `QueryItem` the pagination walks.
        inner: &'q QueryItem,
    },
    /// A query whose single item is an aggregate wrapper — produces one
    /// scalar answer (`u64` / `i64` / `(u64, i64)` by `kind`) over
    /// `inner`.
    AggregateLeaf {
        /// Which aggregate family.
        kind: AggregateKind,
        /// The wrapped range the aggregate is computed over.
        inner: &'q QueryItem,
    },
    /// An outer fan-out of `Key`/`Range*` items whose default subquery
    /// resolves to an aggregate leaf — produces one scalar answer *per
    /// matched outer key*.
    AggregateCarrier {
        /// Which aggregate family.
        kind: AggregateKind,
        /// The wrapped range of the leaf aggregate inside the carrier.
        inner: &'q QueryItem,
    },
}

impl PathQueryShape<'_> {
    /// Whether this is the plain key-selection shape.
    pub fn is_key_selection(&self) -> bool {
        matches!(self, PathQueryShape::KeySelection)
    }

    /// The aggregate family, if this is an aggregate shape (leaf or
    /// carrier).
    pub fn aggregate_kind(&self) -> Option<AggregateKind> {
        match self {
            PathQueryShape::AggregateLeaf { kind, .. }
            | PathQueryShape::AggregateCarrier { kind, .. } => Some(*kind),
            _ => None,
        }
    }
}

impl PathQuery {
    /// Classifies this query into the single [`PathQueryShape`] it
    /// describes, running the same validators the prove/verify entry
    /// points run today, in the same order.
    ///
    /// A malformed query — an aggregate item in a non-canonical
    /// position, a non-zero offset over anything but a single plain
    /// range, pagination on an aggregate — returns the exact error the
    /// corresponding validator returns, so callers that migrate from
    /// direct validator calls to `classify` keep their error surface.
    ///
    /// The aggregate families are checked before pagination because a
    /// well-formed aggregate query can never carry a non-zero offset
    /// (both the leaf and carrier size-constraint rules reject it), and
    /// `validate_count_offset_paginated` in turn rejects aggregate
    /// wrappers — the shapes are mutually exclusive, and this order
    /// reproduces which of the two rejection messages a mixed query
    /// gets from the prover today.
    pub fn classify(&self) -> Result<PathQueryShape<'_>, Error> {
        let query = &self.query.query;

        if query.has_aggregate_count_on_range_anywhere() {
            // Leaf-vs-carrier is decided exactly the way the validator
            // dispatcher decides it: owning an aggregate item at the top
            // level is the leaf shape, anything else that still carries
            // one somewhere must validate as a carrier.
            let is_leaf = query.aggregate_count_on_range().is_some();
            let inner = self.validate_aggregate_count_on_range()?;
            return Ok(if is_leaf {
                PathQueryShape::AggregateLeaf {
                    kind: AggregateKind::Count,
                    inner,
                }
            } else {
                PathQueryShape::AggregateCarrier {
                    kind: AggregateKind::Count,
                    inner,
                }
            });
        }

        if query.has_aggregate_sum_on_range_anywhere() {
            let is_leaf = query.aggregate_sum_on_range().is_some();
            let inner = self.validate_aggregate_sum_on_range()?;
            return Ok(if is_leaf {
                PathQueryShape::AggregateLeaf {
                    kind: AggregateKind::Sum,
                    inner,
                }
            } else {
                PathQueryShape::AggregateCarrier {
                    kind: AggregateKind::Sum,
                    inner,
                }
            });
        }

        if query.has_aggregate_count_and_sum_on_range_anywhere() {
            let is_leaf = query.aggregate_count_and_sum_on_range().is_some();
            let inner = self.validate_aggregate_count_and_sum_on_range()?;
            return Ok(if is_leaf {
                PathQueryShape::AggregateLeaf {
                    kind: AggregateKind::CountAndSum,
                    inner,
                }
            } else {
                PathQueryShape::AggregateCarrier {
                    kind: AggregateKind::CountAndSum,
                    inner,
                }
            });
        }

        if self.has_non_zero_offset() {
            let inner = self.validate_count_offset_paginated()?;
            return Ok(PathQueryShape::CountOffsetPaginated { inner });
        }

        Ok(PathQueryShape::KeySelection)
    }
}

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::{query::query_item::QueryItem, Query};

    use super::*;
    use crate::SizedQuery;

    fn path() -> Vec<Vec<u8>> {
        vec![b"tree".to_vec()]
    }

    fn range_item() -> QueryItem {
        QueryItem::Range(b"a".to_vec()..b"z".to_vec())
    }

    fn leaf_aggregate_query(kind: AggregateKind) -> Query {
        match kind {
            AggregateKind::Count => Query::new_aggregate_count_on_range(range_item()),
            AggregateKind::Sum => Query::new_aggregate_sum_on_range(range_item()),
            AggregateKind::CountAndSum => Query::new_aggregate_count_and_sum_on_range(range_item()),
        }
    }

    fn carrier_aggregate_query(kind: AggregateKind) -> Query {
        let mut carrier = Query::new();
        carrier.insert_key(b"outer".to_vec());
        carrier.set_subquery(leaf_aggregate_query(kind));
        carrier
    }

    // ---------- Plain shapes ----------

    #[test]
    fn plain_queries_classify_as_key_selection() {
        // Single key.
        let pq = PathQuery::new_single_key(path(), b"k".to_vec());
        assert!(matches!(
            pq.classify().expect("plain key query must classify"),
            PathQueryShape::KeySelection
        ));

        // Range + subquery descent + limit — still plain key selection.
        let mut q = Query::new_single_query_item(range_item());
        q.set_subquery(Query::new_single_key(b"inner".to_vec()));
        let pq = PathQuery::new(path(), SizedQuery::new(q, Some(10), None));
        assert!(pq
            .classify()
            .expect("subquery descent must classify")
            .is_key_selection());

        // Empty path (root merk) is fine for key selection.
        let pq = PathQuery::new_unsized(vec![], Query::new_single_query_item(range_item()));
        assert!(pq
            .classify()
            .expect("root-merk key query must classify")
            .is_key_selection());
    }

    #[test]
    fn offset_zero_classifies_as_key_selection() {
        // The engine treats `offset == Some(0)` as "no offset"; classify
        // must agree rather than routing it to the paginated shape.
        let pq = PathQuery::new(
            path(),
            SizedQuery::new(Query::new_single_query_item(range_item()), None, Some(0)),
        );
        assert!(pq
            .classify()
            .expect("offset 0 must classify")
            .is_key_selection());
    }

    // ---------- Count-offset pagination ----------

    #[test]
    fn non_zero_offset_over_single_range_classifies_as_count_offset_paginated() {
        let pq = PathQuery::new(
            path(),
            SizedQuery::new(Query::new_single_query_item(range_item()), None, Some(5)),
        );
        match pq.classify().expect("offset-paginated range must classify") {
            PathQueryShape::CountOffsetPaginated { inner } => {
                assert_eq!(inner, &range_item());
            }
            other => panic!("expected CountOffsetPaginated, got {other:?}"),
        }
    }

    #[test]
    fn count_offset_errors_match_the_validator() {
        // Every malformed offset-paginated query must produce the exact
        // error `validate_count_offset_paginated` produces, so entry
        // points migrating to classify keep their error surface.
        let malformed = [
            // Key item — matches at most one element, offset > 0 is
            // structurally empty.
            PathQuery::new(
                path(),
                SizedQuery::new(
                    Query::new_single_query_item(QueryItem::Key(b"k".to_vec())),
                    None,
                    Some(3),
                ),
            ),
            // Subquery present.
            {
                let mut q = Query::new_single_query_item(range_item());
                q.set_subquery(Query::new_single_key(b"inner".to_vec()));
                PathQuery::new(path(), SizedQuery::new(q, None, Some(3)))
            },
            // Two items.
            {
                let mut q = Query::new_single_query_item(range_item());
                q.insert_item(QueryItem::Range(b"0".to_vec()..b"5".to_vec()));
                PathQuery::new(path(), SizedQuery::new(q, None, Some(3)))
            },
            // Root merk target.
            PathQuery::new(
                vec![],
                SizedQuery::new(Query::new_single_query_item(range_item()), None, Some(3)),
            ),
        ];
        for pq in malformed {
            let classify_err = pq.classify().expect_err("malformed offset query must fail");
            let validator_err = pq
                .validate_count_offset_paginated()
                .expect_err("validator must also fail");
            assert_eq!(
                format!("{classify_err}"),
                format!("{validator_err}"),
                "classify must surface the validator's error for {pq}"
            );
        }
    }

    // ---------- Aggregate shapes ----------

    #[test]
    fn aggregate_leaves_classify_with_their_kind_and_inner() {
        for kind in [
            AggregateKind::Count,
            AggregateKind::Sum,
            AggregateKind::CountAndSum,
        ] {
            let pq = PathQuery::new_unsized(path(), leaf_aggregate_query(kind));
            match pq.classify().expect("leaf aggregate must classify") {
                PathQueryShape::AggregateLeaf { kind: got, inner } => {
                    assert_eq!(got, kind);
                    assert_eq!(inner, &range_item());
                }
                other => panic!("expected AggregateLeaf({kind:?}), got {other:?}"),
            }
        }
    }

    #[test]
    fn aggregate_carriers_classify_with_their_kind_and_inner() {
        for kind in [
            AggregateKind::Count,
            AggregateKind::Sum,
            AggregateKind::CountAndSum,
        ] {
            let pq = PathQuery::new_unsized(path(), carrier_aggregate_query(kind));
            match pq.classify().expect("carrier aggregate must classify") {
                PathQueryShape::AggregateCarrier { kind: got, inner } => {
                    assert_eq!(got, kind);
                    assert_eq!(inner, &range_item());
                    assert_eq!(pq.classify().unwrap().aggregate_kind(), Some(kind));
                }
                other => panic!("expected AggregateCarrier({kind:?}), got {other:?}"),
            }
        }
    }

    #[test]
    fn carrier_with_limit_is_allowed_leaf_with_limit_is_not() {
        // The size-constraint split: carriers may cap outer matches with
        // `limit`, leaves reject both limit and offset.
        let carrier = PathQuery::new(
            path(),
            SizedQuery::new(carrier_aggregate_query(AggregateKind::Sum), Some(4), None),
        );
        assert!(matches!(
            carrier
                .classify()
                .expect("carrier with limit must classify"),
            PathQueryShape::AggregateCarrier { .. }
        ));

        let leaf = PathQuery::new(
            path(),
            SizedQuery::new(leaf_aggregate_query(AggregateKind::Sum), Some(4), None),
        );
        leaf.classify()
            .expect_err("leaf with limit must be rejected");
    }

    #[test]
    fn aggregate_errors_match_the_validators() {
        // Malformed aggregate shapes must surface the exact validator
        // error. One representative per family and per failure class.
        let cases: Vec<(PathQuery, fn(&PathQuery) -> Result<&QueryItem, Error>)> = vec![
            // ACOR wrapping Key.
            (
                PathQuery::new_unsized(
                    path(),
                    Query::new_aggregate_count_on_range(QueryItem::Key(b"k".to_vec())),
                ),
                PathQuery::validate_aggregate_count_on_range,
            ),
            // ASOR leaf at the root merk (empty path).
            (
                PathQuery::new_unsized(vec![], leaf_aggregate_query(AggregateKind::Sum)),
                PathQuery::validate_aggregate_sum_on_range,
            ),
            // ACASOR with an extra item beside the aggregate. Pushed
            // directly: `insert_item` would collision-merge the key into
            // the aggregate wrapper and silently degrade it to a plain
            // range (a pre-existing `QueryItem` merge footgun).
            (
                {
                    let mut q = leaf_aggregate_query(AggregateKind::CountAndSum);
                    q.items.push(QueryItem::Key(b"extra".to_vec()));
                    PathQuery::new_unsized(path(), q)
                },
                PathQuery::validate_aggregate_count_and_sum_on_range,
            ),
            // Leaf ACOR with an offset (size-constraint rule).
            (
                PathQuery::new(
                    path(),
                    SizedQuery::new(leaf_aggregate_query(AggregateKind::Count), None, Some(2)),
                ),
                PathQuery::validate_aggregate_count_on_range,
            ),
            // Carrier ASOR with an offset (carriers reject offsets too).
            (
                PathQuery::new(
                    path(),
                    SizedQuery::new(carrier_aggregate_query(AggregateKind::Sum), None, Some(2)),
                ),
                PathQuery::validate_aggregate_sum_on_range,
            ),
        ];
        for (case_index, (pq, validator)) in cases.into_iter().enumerate() {
            let classify_err = match pq.classify() {
                Err(e) => e,
                Ok(shape) => panic!(
                    "case {case_index}: malformed aggregate must fail classification, got \
                     {shape:?} for {pq}"
                ),
            };
            let validator_err = validator(&pq).expect_err("validator must also fail");
            assert_eq!(
                format!("{classify_err}"),
                format!("{validator_err}"),
                "classify must surface the validator's error for {pq}"
            );
        }
    }

    #[test]
    fn mixed_aggregate_kinds_are_rejected() {
        // Two different aggregate wrappers in one query. The count gate
        // runs first (prover order), so the count validator's rejection
        // is the one surfaced.
        let mut q = Query::new();
        q.items
            .push(QueryItem::AggregateCountOnRange(Box::new(range_item())));
        q.items
            .push(QueryItem::AggregateSumOnRange(Box::new(range_item())));
        let pq = PathQuery::new_unsized(path(), q);
        let classify_err = pq.classify().expect_err("mixed aggregates must fail");
        let validator_err = pq
            .validate_aggregate_count_on_range()
            .expect_err("count validator fires first");
        assert_eq!(format!("{classify_err}"), format!("{validator_err}"));

        // A sum leaf whose default subquery hides a combined aggregate:
        // the sum gate fires first and rejects the subquery branch.
        let mut hidden = leaf_aggregate_query(AggregateKind::Sum);
        hidden.set_subquery(leaf_aggregate_query(AggregateKind::CountAndSum));
        let pq = PathQuery::new_unsized(path(), hidden);
        pq.classify()
            .expect_err("aggregate hidden beside another kind must fail");
    }

    // ---------- Totality ----------

    #[test]
    fn classify_is_total_over_a_shape_grid() {
        // Cross a grid of item sets × subquery presence × limit/offset ×
        // path emptiness and confirm classify never panics: every
        // combination either classifies or returns a typed error.
        let item_sets: Vec<Vec<QueryItem>> = vec![
            vec![],
            vec![QueryItem::Key(b"k".to_vec())],
            vec![range_item()],
            vec![QueryItem::RangeFull(..)],
            vec![QueryItem::Key(b"k".to_vec()), range_item()],
            vec![QueryItem::AggregateCountOnRange(Box::new(range_item()))],
            vec![QueryItem::AggregateSumOnRange(Box::new(range_item()))],
            vec![QueryItem::AggregateCountAndSumOnRange(Box::new(
                range_item(),
            ))],
            vec![
                QueryItem::AggregateCountOnRange(Box::new(range_item())),
                QueryItem::AggregateSumOnRange(Box::new(range_item())),
            ],
        ];
        let subqueries: Vec<Option<Query>> = vec![
            None,
            Some(Query::new_single_key(b"inner".to_vec())),
            Some(leaf_aggregate_query(AggregateKind::Count)),
            Some(leaf_aggregate_query(AggregateKind::Sum)),
        ];
        let limits = [None, Some(0u16), Some(7)];
        let offsets = [None, Some(0u16), Some(7)];
        let paths: Vec<Vec<Vec<u8>>> = vec![vec![], path()];

        let mut classified = 0usize;
        let mut rejected = 0usize;
        for items in &item_sets {
            for subquery in &subqueries {
                for &limit in &limits {
                    for &offset in &offsets {
                        for p in &paths {
                            let mut q = Query::new();
                            q.items = items.clone();
                            if let Some(sub) = subquery {
                                q.set_subquery(sub.clone());
                            }
                            let pq = PathQuery::new(p.clone(), SizedQuery::new(q, limit, offset));
                            match pq.classify() {
                                Ok(_) => classified += 1,
                                Err(Error::InvalidQuery(_)) => rejected += 1,
                                Err(other) => {
                                    panic!("classify must only fail with InvalidQuery, got {other:?} for {pq}")
                                }
                            }
                        }
                    }
                }
            }
        }
        // Sanity: the grid exercises both outcomes.
        assert!(classified > 0, "grid must contain classifiable queries");
        assert!(rejected > 0, "grid must contain rejected queries");
    }
}
