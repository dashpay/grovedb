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

use grovedb_merk::proofs::query::{
    query_item::QueryItem, AxisQuery, AxisTraversal, ReadMode, SumBudgetRead as SumBudgetReadSpec,
};

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
    /// An axis-ordered read of the indexed tree the path names: the
    /// root query carries `ReadMode::Axis` and nothing else.
    AxisRead {
        /// The axis read to perform.
        axis: &'q AxisQuery,
    },
    /// The same axis read fanned over N sibling branch keys: root items
    /// are `Key`s selecting the branches, the default subquery branch's
    /// path is the shared suffix from each branch key to its indexed
    /// tree, and the branch terminal carries `ReadMode::Axis`.
    BranchedAxisRead {
        /// The `Key` items naming the branches.
        branch_items: &'q [QueryItem],
        /// The shared path from each branch key to its indexed tree.
        suffix: &'q [Vec<u8>],
        /// The axis read performed under every branch.
        axis: &'q AxisQuery,
    },
    /// A key-ordered read of the root items that stops on a running-sum
    /// budget (the read `AggregateSumPathQuery` serves): the root query
    /// carries `ReadMode::SumBudget`. Trusted reads only until the
    /// sum-budget proof shape lands.
    SumBudget {
        /// The stop condition.
        budget: &'q SumBudgetReadSpec,
        /// The key-ordered items the budget walk scans.
        items: &'q [QueryItem],
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

        // Read modes are checked first: a query carrying one anywhere is
        // one of the three read-mode shapes or malformed — it must never
        // fall through and be served as key selection.
        if query.has_read_mode_anywhere() {
            return self.classify_read_mode_shape();
        }

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

    /// Grammar for queries that carry a [`ReadMode`] anywhere. Strict
    /// v1 grammar — exactly three legal placements, everything else is
    /// a typed error naming the violated rule. Looser placements
    /// (conditional axis branches, range items over branch keys,
    /// heterogeneous per-branch reads) can be admitted later; loosening
    /// a grammar is additive, tightening one is a break.
    fn classify_read_mode_shape(&self) -> Result<PathQueryShape<'_>, Error> {
        let sized = &self.query;
        let query = &sized.query;

        // No pagination in any read-mode shape: axis traversals carry
        // their own caps (`k` / `limit`), and a sum budget is its own
        // stop condition.
        if sized.limit.is_some() {
            return Err(Error::InvalidQuery(
                "read-mode queries may not set SizedQuery::limit — the read mode carries its \
                 own entry caps",
            ));
        }
        if sized.offset.is_some() {
            return Err(Error::InvalidQuery(
                "read-mode queries may not set SizedQuery::offset — axis pagination is \
                 expressed in the traversal (TopK.offset)",
            ));
        }
        if query.add_parent_tree_on_subquery {
            return Err(Error::InvalidQuery(
                "read-mode queries may not set add_parent_tree_on_subquery",
            ));
        }

        match query.read_mode.as_deref() {
            // Shape: single-path axis read. The axis query is the whole
            // read; the node selects nothing by key.
            Some(ReadMode::Axis(axis)) => {
                if !query.items.is_empty() {
                    return Err(Error::InvalidQuery(
                        "an axis read carries no query items — the axis traversal is the \
                         whole read",
                    ));
                }
                if query.default_subquery_branch.subquery.is_some()
                    || query.default_subquery_branch.subquery_path.is_some()
                {
                    return Err(Error::InvalidQuery(
                        "an axis read carries no subquery branches — it is a terminal read \
                         of the indexed tree the path names",
                    ));
                }
                if has_conditional_branches(query) {
                    return Err(Error::InvalidQuery(
                        "an axis read carries no conditional subquery branches",
                    ));
                }
                if self.path.is_empty() {
                    return Err(Error::InvalidQuery(
                        "an axis read's path names the indexed tree and cannot be empty — \
                         the GroveDB root is always a NormalTree, never an indexed tree",
                    ));
                }
                axis.validate().map_err(read_mode_validation_error)?;
                Ok(PathQueryShape::AxisRead { axis })
            }
            // Shape: sum-budget read of this node's items.
            Some(ReadMode::SumBudget(budget)) => {
                if query.items.is_empty() {
                    return Err(Error::InvalidQuery(
                        "a sum-budget read needs at least one query item to walk",
                    ));
                }
                if query.items.iter().any(|item| {
                    matches!(
                        item,
                        QueryItem::AggregateCountOnRange(_)
                            | QueryItem::AggregateSumOnRange(_)
                            | QueryItem::AggregateCountAndSumOnRange(_)
                    )
                }) {
                    return Err(Error::InvalidQuery(
                        "a sum-budget read walks plain key/range items — aggregate wrappers \
                         have their own query shapes",
                    ));
                }
                if query.default_subquery_branch.subquery.is_some()
                    || query.default_subquery_branch.subquery_path.is_some()
                    || has_conditional_branches(query)
                {
                    return Err(Error::InvalidQuery(
                        "a sum-budget read carries no subquery branches — it walks one tree's \
                         items in key order",
                    ));
                }
                budget.validate().map_err(read_mode_validation_error)?;
                Ok(PathQueryShape::SumBudget {
                    budget,
                    items: &query.items,
                })
            }
            // The root has no read mode but something below does: only
            // the branched-axis grammar is legal — branch keys at the
            // root, one shared suffix, one axis terminal.
            None => {
                if has_conditional_branches(query) {
                    return Err(Error::InvalidQuery(
                        "read modes may not appear under conditional subquery branches — a \
                         branched axis read uses Key items and the default subquery branch",
                    ));
                }
                if query.items.is_empty()
                    || !query
                        .items
                        .iter()
                        .all(|item| matches!(item, QueryItem::Key(_)))
                {
                    return Err(Error::InvalidQuery(
                        "a branched axis read selects its branches with Key items only \
                         (at least one)",
                    ));
                }
                let branch = &query.default_subquery_branch;
                let Some(suffix) = branch.subquery_path.as_deref() else {
                    return Err(Error::InvalidQuery(
                        "a branched axis read requires a non-empty subquery_path — the \
                         shared suffix from each branch key to its indexed tree",
                    ));
                };
                if suffix.is_empty() || suffix.iter().any(|segment| segment.is_empty()) {
                    return Err(Error::InvalidQuery(
                        "a branched axis read's suffix must be non-empty and contain \
                         non-empty keys",
                    ));
                }
                let Some(inner) = branch.subquery.as_deref() else {
                    return Err(Error::InvalidQuery(
                        "a branched axis read requires the default subquery branch to carry \
                         the axis-read terminal",
                    ));
                };
                match inner.read_mode.as_deref() {
                    Some(ReadMode::Axis(axis)) => {
                        if !inner.items.is_empty()
                            || inner.default_subquery_branch.subquery.is_some()
                            || inner.default_subquery_branch.subquery_path.is_some()
                            || has_conditional_branches(inner)
                            || inner.add_parent_tree_on_subquery
                        {
                            return Err(Error::InvalidQuery(
                                "a branched axis read's terminal carries only the axis read — \
                                 no items, subquery branches, or parent-tree flag",
                            ));
                        }
                        axis.validate().map_err(read_mode_validation_error)?;
                        // A branched read answers with one entry list per
                        // branch, so its terminal must be an
                        // entry-listing traversal. Rank-of-key and
                        // range-aggregate produce a single scalar about
                        // one tree and have no per-branch list to fill;
                        // rejecting them here keeps the reader and the
                        // verifier from having to treat "impossible"
                        // shapes as internal errors.
                        if matches!(
                            axis.traversal,
                            AxisTraversal::RankOfKey { .. } | AxisTraversal::RangeAggregate { .. }
                        ) {
                            return Err(Error::InvalidQuery(
                                "a branched axis read serves entry-listing traversals \
                                 (RankedPage / Bounded) only; rank-of-key and range-aggregate \
                                 are single-path reads",
                            ));
                        }
                        Ok(PathQueryShape::BranchedAxisRead {
                            branch_items: &query.items,
                            suffix,
                            axis,
                        })
                    }
                    Some(ReadMode::SumBudget(_)) => Err(Error::InvalidQuery(
                        "a sum-budget read may only appear at the root query, not under \
                         branch keys",
                    )),
                    None => Err(Error::InvalidQuery(
                        "read modes may nest at most one level deep: branch keys at the \
                         root, one suffix, one axis-read terminal",
                    )),
                }
            }
        }
    }
}

/// Whether the query has a non-empty conditional-subquery-branch map.
fn has_conditional_branches(query: &grovedb_merk::proofs::Query) -> bool {
    query
        .conditional_subquery_branches
        .as_ref()
        .is_some_and(|branches| !branches.is_empty())
}

/// Projects the vocabulary crate's validation error (always
/// `InvalidOperation(&'static str)`) into this crate's `InvalidQuery`,
/// preserving the message. Shared with the axis-bounds lowering, which
/// runs the same validation on the prover/verifier side.
pub(crate) fn read_mode_validation_error(e: grovedb_query::error::Error) -> Error {
    match e {
        grovedb_query::error::Error::InvalidOperation(msg) => Error::InvalidQuery(msg),
        _ => Error::InvalidQuery("read-mode validation failed"),
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

    // ---------- Read-mode shapes ----------

    use grovedb_merk::proofs::query::{AxisQuery, IndexAxis, ReadMode};

    #[test]
    fn axis_constructors_classify_as_axis_read() {
        let queries = [
            PathQuery::new_axis_top_k(path(), IndexAxis::Count, 5, 0, true),
            PathQuery::new_axis_bounded(path(), IndexAxis::Sum, -10, 10, 3, false),
            PathQuery::new_axis_rank_of_key(path(), IndexAxis::Avg, b"alice".to_vec(), true),
            PathQuery::new_axis_range_aggregate(path(), IndexAxis::Sum, 0, 100),
        ];
        for pq in queries {
            match pq.classify().expect("axis constructor must classify") {
                PathQueryShape::AxisRead { axis } => {
                    axis.validate().expect("constructed axis must validate");
                }
                other => panic!("expected AxisRead, got {other:?} for {pq}"),
            }
        }
    }

    #[test]
    fn branched_axis_constructor_classifies_with_its_parts() {
        let pq = PathQuery::new_branched_axis(
            vec![b"contracts".to_vec()],
            vec![b"alice".to_vec(), b"bob".to_vec()],
            vec![b"scores".to_vec()],
            AxisQuery::top_k(IndexAxis::Count, 3, 0, true),
        );
        match pq.classify().expect("branched constructor must classify") {
            PathQueryShape::BranchedAxisRead {
                branch_items,
                suffix,
                axis,
            } => {
                assert_eq!(branch_items.len(), 2);
                assert_eq!(suffix, &[b"scores".to_vec()]);
                assert_eq!(axis.axis, IndexAxis::Count);
            }
            other => panic!("expected BranchedAxisRead, got {other:?}"),
        }
    }

    #[test]
    fn sum_budget_constructor_classifies_with_its_parts() {
        let pq = PathQuery::new_sum_budget(path(), vec![range_item()], true, 500, Some(20));
        match pq.classify().expect("sum-budget constructor must classify") {
            PathQueryShape::SumBudget { budget, items } => {
                assert_eq!(budget.sum_limit, 500);
                assert_eq!(budget.max_items_checked, Some(20));
                assert_eq!(items.len(), 1);
            }
            other => panic!("expected SumBudget, got {other:?}"),
        }
    }

    #[test]
    fn read_mode_grammar_rejections_name_the_violated_rule() {
        use grovedb_merk::proofs::query::SumBudgetRead;

        fn axis_node() -> Query {
            let mut q = Query::new();
            q.read_mode = Some(Box::new(ReadMode::Axis(AxisQuery::top_k(
                IndexAxis::Count,
                1,
                0,
                true,
            ))));
            q
        }

        let cases: Vec<(&str, PathQuery)> = vec![
            ("axis read carries items", {
                let mut q = axis_node();
                q.items.push(QueryItem::Key(b"k".to_vec()));
                PathQuery::new_unsized(path(), q)
            }),
            ("axis read carries a subquery", {
                let mut q = axis_node();
                q.set_subquery(Query::new_single_key(b"x".to_vec()));
                PathQuery::new_unsized(path(), q)
            }),
            ("axis read at the root merk", {
                PathQuery::new_unsized(vec![], axis_node())
            }),
            ("axis read with a limit", {
                PathQuery::new(path(), SizedQuery::new(axis_node(), Some(1), None))
            }),
            ("axis read with an offset", {
                PathQuery::new(path(), SizedQuery::new(axis_node(), None, Some(1)))
            }),
            ("axis read with parent-tree flag", {
                let mut q = axis_node();
                q.add_parent_tree_on_subquery = true;
                PathQuery::new_unsized(path(), q)
            }),
            ("invalid axis payload (k = 0)", {
                let mut q = Query::new();
                q.read_mode = Some(Box::new(ReadMode::Axis(AxisQuery::top_k(
                    IndexAxis::Count,
                    0,
                    0,
                    true,
                ))));
                PathQuery::new_unsized(path(), q)
            }),
            ("range aggregate on the Avg axis", {
                PathQuery::new_axis_range_aggregate(path(), IndexAxis::Avg, 0, 10)
            }),
            ("branched: range item selecting branches", {
                let mut q = Query::new_single_query_item(range_item());
                q.set_subquery_path(vec![b"s".to_vec()]);
                q.set_subquery(axis_node());
                PathQuery::new_unsized(path(), q)
            }),
            ("branched: missing suffix", {
                let mut q = Query::new_single_key(b"b".to_vec());
                q.set_subquery(axis_node());
                PathQuery::new_unsized(path(), q)
            }),
            ("branched: empty suffix segment", {
                let mut q = Query::new_single_key(b"b".to_vec());
                q.set_subquery_path(vec![b"".to_vec()]);
                q.set_subquery(axis_node());
                PathQuery::new_unsized(path(), q)
            }),
            ("branched: terminal carries items", {
                let mut terminal = axis_node();
                terminal.items.push(QueryItem::Key(b"k".to_vec()));
                let mut q = Query::new_single_key(b"b".to_vec());
                q.set_subquery_path(vec![b"s".to_vec()]);
                q.set_subquery(terminal);
                PathQuery::new_unsized(path(), q)
            }),
            ("sum budget below the root", {
                let mut terminal = Query::new_single_query_item(range_item());
                terminal.read_mode = Some(Box::new(ReadMode::SumBudget(SumBudgetRead {
                    sum_limit: 1,
                    max_items_checked: None,
                })));
                let mut q = Query::new_single_key(b"b".to_vec());
                q.set_subquery_path(vec![b"s".to_vec()]);
                q.set_subquery(terminal);
                PathQuery::new_unsized(path(), q)
            }),
            ("read mode two levels deep", {
                let mut middle = Query::new_single_key(b"m".to_vec());
                middle.set_subquery_path(vec![b"s".to_vec()]);
                middle.set_subquery(axis_node());
                let mut q = Query::new_single_key(b"b".to_vec());
                q.set_subquery_path(vec![b"t".to_vec()]);
                q.set_subquery(middle);
                PathQuery::new_unsized(path(), q)
            }),
            ("read mode under a conditional branch", {
                let mut q = Query::new_single_key(b"b".to_vec());
                q.add_conditional_subquery(QueryItem::Key(b"b".to_vec()), None, Some(axis_node()));
                PathQuery::new_unsized(path(), q)
            }),
            ("sum budget without items", {
                PathQuery::new_sum_budget(path(), vec![], true, 1, None)
            }),
            ("sum budget over an aggregate item", {
                PathQuery::new_sum_budget(
                    path(),
                    vec![QueryItem::AggregateSumOnRange(Box::new(range_item()))],
                    true,
                    1,
                    None,
                )
            }),
            ("sum budget of zero", {
                PathQuery::new_sum_budget(path(), vec![range_item()], true, 0, None)
            }),
        ];
        for (label, pq) in cases {
            match pq.classify() {
                Err(Error::InvalidQuery(_)) => {}
                Err(other) => {
                    panic!("case {label:?}: expected InvalidQuery, got {other:?}")
                }
                Ok(shape) => panic!("case {label:?}: must be rejected, classified as {shape:?}"),
            }
        }
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
        let axis_terminal = {
            let mut q = Query::new();
            q.read_mode = Some(Box::new(ReadMode::Axis(AxisQuery::top_k(
                IndexAxis::Sum,
                2,
                0,
                true,
            ))));
            q
        };
        let subqueries: Vec<Option<Query>> = vec![
            None,
            Some(Query::new_single_key(b"inner".to_vec())),
            Some(leaf_aggregate_query(AggregateKind::Count)),
            Some(leaf_aggregate_query(AggregateKind::Sum)),
            Some(axis_terminal.clone()),
        ];
        let read_modes: Vec<Option<Box<ReadMode>>> = vec![
            None,
            Some(Box::new(ReadMode::Axis(AxisQuery::top_k(
                IndexAxis::Count,
                1,
                0,
                false,
            )))),
            Some(Box::new(ReadMode::SumBudget(
                grovedb_merk::proofs::query::SumBudgetRead {
                    sum_limit: 10,
                    max_items_checked: None,
                },
            ))),
        ];
        let limits = [None, Some(0u16), Some(7)];
        let offsets = [None, Some(0u16), Some(7)];
        let paths: Vec<Vec<Vec<u8>>> = vec![vec![], path()];

        let mut classified = 0usize;
        let mut rejected = 0usize;
        for items in &item_sets {
            for subquery in &subqueries {
                for read_mode in &read_modes {
                    for &limit in &limits {
                        for &offset in &offsets {
                            for p in &paths {
                                let mut q = Query::new();
                                q.items = items.clone();
                                if let Some(sub) = subquery {
                                    q.set_subquery(sub.clone());
                                    if matches!(sub.read_mode.as_deref(), Some(ReadMode::Axis(_))) {
                                        q.set_subquery_path(vec![b"suffix".to_vec()]);
                                    }
                                }
                                q.read_mode = read_mode.clone();
                                let pq =
                                    PathQuery::new(p.clone(), SizedQuery::new(q, limit, offset));
                                match pq.classify() {
                                    Ok(_) => classified += 1,
                                    Err(Error::InvalidQuery(_)) => rejected += 1,
                                    Err(other) => {
                                        panic!(
                                            "classify must only fail with InvalidQuery, got \
                                             {other:?} for {pq}"
                                        )
                                    }
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
