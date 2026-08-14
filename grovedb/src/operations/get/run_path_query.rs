//! The unified read entry point: one function that executes every
//! [`PathQuery`] shape.
//!
//! [`GroveDb::run_path_query`] classifies the query once
//! ([`PathQuery::classify`]) and routes it to the engine that already
//! serves that shape — the key-selection reader, the aggregate-on-range
//! readers (leaf and per-key carrier, on all three axes), the
//! indexed-axis primitives, or the budgeted sum reader. It returns a
//! [`PathQueryRun`] whose variant mirrors the shape, so a caller holding
//! an arbitrary `PathQuery` gets a typed answer without knowing in
//! advance which of the specialized entry points serves it.
//!
//! Every classified shape now has a reader behind it: the dispatch
//! returns no `NotSupported` of its own except the read-mode version
//! gate below.
//!
//! Read-mode shapes (axis and sum-budget reads) are gated on
//! `path_query_methods.unified_read_mode` — `0` before GROVE_V4 means
//! the whole vocabulary is rejected with `NotSupported`, mirroring the
//! fail-closed version-2 decode on older nodes. Key-selection and
//! aggregate shapes are served at every version, exactly as their
//! dedicated entry points serve them.
//!
//! Everything here is a **trusted read**: no result carries a
//! cryptographic guarantee. The proved counterparts are `prove_query`
//! (key selection, aggregates) and the indexed-axis proof family; the
//! unified proof dispatch arrives separately.

use grovedb_costs::{cost_return_on_error, CostResult, CostsExt};
use grovedb_merk::proofs::query::{AggregateFold, AxisTraversal, IndexAxis};
use grovedb_path::SubtreePath;
use grovedb_version::{
    check_grovedb_v0_with_cost, error::GroveVersionError, version::GroveVersion,
};

use crate::{
    element::aggregate_sum_query::AggregateSumQueryResult,
    operations::proof::indexed_axis::AxisEntries,
    query::{AggregateKind, PathQueryShape},
    query_result_type::{QueryResultElements, QueryResultType},
    AggregateSumPathQuery, Error, GroveDb, PathQuery, TransactionArg,
};

/// A single aggregate scalar read off an indexed tree's per-axis
/// secondary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AxisAggregateValue {
    /// How many entries the value band selected; each contributes 1.
    Population(u64),
    /// The signed sum of the selected entries' axis values.
    Total(i64),
}

/// The typed answer to [`GroveDb::run_path_query`] — one variant per
/// [`PathQueryShape`] family.
#[derive(Debug)]
pub enum PathQueryRun {
    /// Key-selection shapes (including count-offset pagination): the
    /// regular result set plus the number of elements skipped by the
    /// query's offset.
    Elements {
        /// The selected elements, in the requested result type.
        elements: QueryResultElements,
        /// Elements skipped by `SizedQuery::offset`.
        skipped: u16,
    },
    /// Leaf `AggregateCountOnRange`: one count.
    AggregateCount(u64),
    /// Leaf `AggregateSumOnRange`: one signed sum.
    AggregateSum(i64),
    /// Leaf `AggregateCountAndSumOnRange`: both, from one walk.
    AggregateCountAndSum {
        /// Count of matched children.
        count: u64,
        /// Signed sum of matched children.
        sum: i64,
    },
    /// Carrier `AggregateCountOnRange`: one count per matched outer key.
    AggregateCountPerKey(Vec<(Vec<u8>, u64)>),
    /// Carrier `AggregateSumOnRange`: one signed sum per matched outer
    /// key.
    AggregateSumPerKey(Vec<(Vec<u8>, i64)>),
    /// Carrier `AggregateCountAndSumOnRange`: both metrics per matched
    /// outer key, each from one walk over that key's leaf.
    AggregateCountAndSumPerKey(Vec<(Vec<u8>, u64, i64)>),
    /// Single-path axis read (`TopK` / `Bounded` traversals): the
    /// entries in walk order.
    AxisEntries(AxisEntries),
    /// Branched axis read: per branch key, in query order, the entries
    /// — or `None` when the branch key is absent at the branching
    /// level (mirroring the branched proof's authenticated-absence
    /// slots, minus the authentication).
    BranchedAxisEntries(Vec<(Vec<u8>, Option<AxisEntries>)>),
    /// `RankOfKey` traversal: the item's 0-based rank in the walk.
    AxisRank(u64),
    /// `AggregateOverValueRange` traversal: one scalar over the value range.
    AxisAggregate(AxisAggregateValue),
    /// Sum-budget read: the budgeted walk's matches and stop state.
    SumBudget(AggregateSumQueryResult),
}

impl GroveDb {
    /// Execute any [`PathQuery`] shape as a trusted read and return the
    /// shape's typed answer. See the module docs for routing and
    /// gating; see [`PathQuery::classify`] for the shape grammar.
    #[allow(clippy::too_many_arguments)]
    pub fn run_path_query(
        &self,
        path_query: &PathQuery,
        allow_cache: bool,
        decrease_limit_on_range_with_no_sub_elements: bool,
        error_if_intermediate_path_tree_not_present: bool,
        result_type: QueryResultType,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<PathQueryRun, Error> {
        check_grovedb_v0_with_cost!(
            "run_path_query",
            grove_version
                .grovedb_versions
                .operations
                .query
                .run_path_query
        );
        let mut cost = Default::default();

        let shape = match path_query.classify() {
            Ok(shape) => shape,
            Err(e) => return Err(e).wrap_with_cost(cost),
        };

        // Read-mode shapes are gated on `unified_read_mode`; the
        // key-selection and aggregate shapes below are served at every
        // version, exactly as their dedicated entry points serve them.
        if matches!(
            shape,
            PathQueryShape::AxisRead { .. }
                | PathQueryShape::BranchedAxisRead { .. }
                | PathQueryShape::SumBudget { .. }
        ) {
            match grove_version
                .grovedb_versions
                .path_query_methods
                .unified_read_mode
            {
                0 => {
                    return Err(Error::NotSupported(
                        "read-mode (axis / sum-budget) path queries are not served at this \
                         grove version"
                            .to_string(),
                    ))
                    .wrap_with_cost(cost);
                }
                1 => {}
                received => {
                    return Err(Error::VersionError(
                        GroveVersionError::UnknownVersionMismatch {
                            method: "run_path_query (unified_read_mode)".to_string(),
                            known_versions: vec![0, 1],
                            received,
                        },
                    ))
                    .wrap_with_cost(cost);
                }
            }
        }

        match shape {
            PathQueryShape::KeySelection | PathQueryShape::CountOffsetPaginated { .. } => {
                let (elements, skipped) = cost_return_on_error!(
                    &mut cost,
                    self.query_raw(
                        path_query,
                        allow_cache,
                        decrease_limit_on_range_with_no_sub_elements,
                        error_if_intermediate_path_tree_not_present,
                        result_type,
                        transaction,
                        grove_version,
                    )
                );
                Ok(PathQueryRun::Elements { elements, skipped }).wrap_with_cost(cost)
            }
            PathQueryShape::AggregateLeaf { kind, .. } => match kind {
                AggregateKind::Count => {
                    let count = cost_return_on_error!(
                        &mut cost,
                        self.query_aggregate_count(path_query, transaction, grove_version)
                    );
                    Ok(PathQueryRun::AggregateCount(count)).wrap_with_cost(cost)
                }
                AggregateKind::Sum => {
                    let sum = cost_return_on_error!(
                        &mut cost,
                        self.query_aggregate_sum(path_query, transaction, grove_version)
                    );
                    Ok(PathQueryRun::AggregateSum(sum)).wrap_with_cost(cost)
                }
                AggregateKind::CountAndSum => {
                    let (count, sum) = cost_return_on_error!(
                        &mut cost,
                        self.query_aggregate_count_and_sum(path_query, transaction, grove_version)
                    );
                    Ok(PathQueryRun::AggregateCountAndSum { count, sum }).wrap_with_cost(cost)
                }
            },
            PathQueryShape::AggregateCarrier { kind, .. } => match kind {
                AggregateKind::Count => {
                    let per_key = cost_return_on_error!(
                        &mut cost,
                        self.query_aggregate_count_per_key(path_query, transaction, grove_version)
                    );
                    Ok(PathQueryRun::AggregateCountPerKey(per_key)).wrap_with_cost(cost)
                }
                AggregateKind::Sum => {
                    let per_key = cost_return_on_error!(
                        &mut cost,
                        self.query_aggregate_sum_per_key(path_query, transaction, grove_version)
                    );
                    Ok(PathQueryRun::AggregateSumPerKey(per_key)).wrap_with_cost(cost)
                }
                AggregateKind::CountAndSum => {
                    let per_key = cost_return_on_error!(
                        &mut cost,
                        self.query_aggregate_count_and_sum_per_key(
                            path_query,
                            transaction,
                            grove_version
                        )
                    );
                    Ok(PathQueryRun::AggregateCountAndSumPerKey(per_key)).wrap_with_cost(cost)
                }
            },
            PathQueryShape::AxisRead { axis } => {
                let path_refs: Vec<&[u8]> = path_query
                    .path
                    .iter()
                    .map(|segment| segment.as_slice())
                    .collect();
                self.run_axis_read(path_refs.as_slice(), axis, transaction, grove_version)
                    .add_cost(cost)
            }
            PathQueryShape::BranchedAxisRead {
                branch_items,
                suffix,
                axis,
            } => {
                // Hoisted: the branching prefix is the same for every
                // branch key, so it is built once rather than per branch.
                let prefix_refs: Vec<&[u8]> = path_query
                    .path
                    .iter()
                    .map(|segment| segment.as_slice())
                    .collect();
                let mut branches = Vec::with_capacity(branch_items.len());
                for item in branch_items {
                    let grovedb_merk::proofs::query::query_item::QueryItem::Key(branch_key) = item
                    else {
                        // classify guarantees Key items only.
                        return Err(Error::CorruptedCodeExecution(
                            "branched axis read classified with a non-Key branch item",
                        ))
                        .wrap_with_cost(cost);
                    };
                    // Mirror the branched proof's absence slots: a
                    // branch key — or any suffix segment under it —
                    // missing yields None rather than an error, so
                    // partially-populated branch sets read exactly the
                    // way they prove (the proof authenticates the
                    // absence at whichever level the chain breaks).
                    //
                    // Seeded from the hoisted `prefix_refs`: the prefix
                    // is built once, and only the per-branch descent
                    // pushes onto its own copy.
                    let mut resolved: Vec<&[u8]> = prefix_refs.clone();
                    let mut chain_broken = false;
                    for segment in std::iter::once(branch_key.as_slice())
                        .chain(suffix.iter().map(|segment| segment.as_slice()))
                    {
                        let present = cost_return_on_error!(
                            &mut cost,
                            self.get_raw_optional(
                                SubtreePath::from(resolved.as_slice()),
                                segment,
                                transaction,
                                grove_version,
                            )
                        );
                        if present.is_none() {
                            chain_broken = true;
                            break;
                        }
                        resolved.push(segment);
                    }
                    if chain_broken {
                        branches.push((branch_key.clone(), None));
                        continue;
                    }
                    let full_path = resolved;
                    let run = cost_return_on_error!(
                        &mut cost,
                        self.run_axis_read(full_path.as_slice(), axis, transaction, grove_version)
                    );
                    let PathQueryRun::AxisEntries(entries) = run else {
                        return Err(Error::CorruptedCodeExecution(
                            "branched axis read requires an entry-listing traversal",
                        ))
                        .wrap_with_cost(cost);
                    };
                    branches.push((branch_key.clone(), Some(entries)));
                }
                Ok(PathQueryRun::BranchedAxisEntries(branches)).wrap_with_cost(cost)
            }
            PathQueryShape::SumBudget { budget, items } => {
                use grovedb_merk::proofs::query::AggregateSumQuery;
                let aggregate_sum_path_query = AggregateSumPathQuery {
                    path: path_query.path.clone(),
                    aggregate_sum_query: AggregateSumQuery {
                        items: items.to_vec(),
                        left_to_right: path_query.query.query.left_to_right,
                        sum_limit: budget.sum_limit,
                        limit_of_items_to_check: budget.match_limit,
                    },
                };
                // The unified sum-budget read uses the PROVABLE fold
                // semantics — skip non-sum elements, skip references —
                // so the trusted read and the sum-budget proof replay
                // agree over any state. (The legacy AggregateSumPathQuery
                // surface keeps its configurable options.)
                let result = cost_return_on_error!(
                    &mut cost,
                    self.query_aggregate_sums_with_options(
                        &aggregate_sum_path_query,
                        crate::element::aggregate_sum_query::AggregateSumQueryOptions {
                            allow_cache,
                            error_if_intermediate_path_tree_not_present,
                            error_if_non_sum_item_found: false,
                            ignore_references: true,
                        },
                        transaction,
                        grove_version,
                    )
                );
                Ok(PathQueryRun::SumBudget(result)).wrap_with_cost(cost)
            }
        }
    }

    /// Single-path axis read: route one validated
    /// [`AxisQuery`](grovedb_merk::proofs::query::AxisQuery) to the
    /// indexed-tree primitive that serves its `(axis, traversal)` pair.
    fn run_axis_read(
        &self,
        path: &[&[u8]],
        axis_query: &grovedb_merk::proofs::query::AxisQuery,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<PathQueryRun, Error> {
        let mut cost = Default::default();
        let axis = axis_query.axis;
        let descending = axis_query.descending;

        match &axis_query.traversal {
            AxisTraversal::RankedPage { k, offset } => {
                let entries = cost_return_on_error!(
                    &mut cost,
                    self.axis_top_k_paginated_entries(
                        path,
                        axis,
                        *k,
                        *offset,
                        descending,
                        transaction,
                        grove_version
                    )
                );
                Ok(PathQueryRun::AxisEntries(entries)).wrap_with_cost(cost)
            }
            AxisTraversal::Bounded { lo, hi, limit } => {
                let entries = cost_return_on_error!(
                    &mut cost,
                    self.axis_bounded_entries(
                        path,
                        axis,
                        *lo,
                        *hi,
                        *limit,
                        descending,
                        transaction,
                        grove_version
                    )
                );
                Ok(PathQueryRun::AxisEntries(entries)).wrap_with_cost(cost)
            }
            AxisTraversal::RankOfKey { key } => {
                let rank = cost_return_on_error!(
                    &mut cost,
                    self.compute_indexed_axis_rank_of_key(
                        path,
                        axis,
                        key,
                        descending,
                        transaction,
                        grove_version
                    )
                );
                Ok(PathQueryRun::AxisRank(rank)).wrap_with_cost(cost)
            }
            AxisTraversal::AggregateOverValueRange { lo, hi, fold } => match (axis, fold) {
                (IndexAxis::Count, AggregateFold::Population) => {
                    let (lo_count, hi_count) = clamp_count_bounds(*lo, *hi);
                    let value = cost_return_on_error!(
                        &mut cost,
                        self.indexed_count_aggregate_over_value_range(
                            path,
                            lo_count,
                            hi_count,
                            transaction,
                            grove_version
                        )
                    );
                    Ok(PathQueryRun::AxisAggregate(AxisAggregateValue::Population(
                        value,
                    )))
                    .wrap_with_cost(cost)
                }
                (IndexAxis::Sum, AggregateFold::Total) => {
                    let (lo_sum, hi_sum) = clamp_sum_bounds(*lo, *hi);
                    let value = cost_return_on_error!(
                        &mut cost,
                        self.indexed_sum_aggregate_over_value_range(
                            path,
                            lo_sum,
                            hi_sum,
                            transaction,
                            grove_version
                        )
                    );
                    Ok(PathQueryRun::AxisAggregate(AxisAggregateValue::Total(
                        value,
                    )))
                    .wrap_with_cost(cost)
                }
                (IndexAxis::Sum, AggregateFold::Population) => {
                    let (lo_sum, hi_sum) = clamp_sum_bounds(*lo, *hi);
                    let value = cost_return_on_error!(
                        &mut cost,
                        self.indexed_sum_population_over_value_range(
                            path,
                            lo_sum,
                            hi_sum,
                            transaction,
                            grove_version
                        )
                    );
                    Ok(PathQueryRun::AxisAggregate(AxisAggregateValue::Population(
                        value,
                    )))
                    .wrap_with_cost(cost)
                }
                (IndexAxis::Count, AggregateFold::Total) => {
                    let (lo_count, hi_count) = clamp_count_bounds(*lo, *hi);
                    let value = cost_return_on_error!(
                        &mut cost,
                        self.indexed_count_total_over_value_range(
                            path,
                            lo_count,
                            hi_count,
                            transaction,
                            grove_version
                        )
                    );
                    Ok(PathQueryRun::AxisAggregate(AxisAggregateValue::Total(
                        value,
                    )))
                    .wrap_with_cost(cost)
                }
                // classify rejects value-range aggregates on the Avg axis.
                (IndexAxis::Avg, _) => Err(Error::CorruptedCodeExecution(
                    "value-range aggregate on the Avg axis survived classification",
                ))
                .wrap_with_cost(cost),
            },
        }
    }

    /// TopK dispatch across the three axes, normalizing into
    /// [`AxisEntries`].
    #[allow(clippy::too_many_arguments)]
    fn axis_top_k_paginated_entries(
        &self,
        path: &[&[u8]],
        axis: IndexAxis,
        k: u16,
        offset: u64,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<AxisEntries, Error> {
        let mut cost = Default::default();
        let entries = match axis {
            IndexAxis::Count => AxisEntries::Count(cost_return_on_error!(
                &mut cost,
                self.indexed_count_top_k_paginated(
                    path,
                    k,
                    offset,
                    descending,
                    transaction,
                    grove_version
                )
            )),
            IndexAxis::Sum => AxisEntries::Sum(cost_return_on_error!(
                &mut cost,
                self.indexed_sum_top_k_paginated(
                    path,
                    k,
                    offset,
                    descending,
                    transaction,
                    grove_version
                )
            )),
            IndexAxis::Avg => AxisEntries::Avg(cost_return_on_error!(
                &mut cost,
                self.indexed_avg_top_k_paginated(
                    path,
                    k,
                    offset,
                    descending,
                    transaction,
                    grove_version
                )
            )),
        };
        Ok(entries).wrap_with_cost(cost)
    }

    /// Bounded dispatch across the three axes, clamping the `i128`
    /// bounds into each axis's own domain (classification already
    /// rejected wholly-out-of-domain ranges).
    #[allow(clippy::too_many_arguments)]
    fn axis_bounded_entries(
        &self,
        path: &[&[u8]],
        axis: IndexAxis,
        lo: i128,
        hi: i128,
        limit: u16,
        descending: bool,
        transaction: TransactionArg,
        grove_version: &GroveVersion,
    ) -> CostResult<AxisEntries, Error> {
        let mut cost = Default::default();
        let entries = match axis {
            IndexAxis::Count => {
                let (lo_count, hi_count) = clamp_count_bounds(lo, hi);
                AxisEntries::Count(cost_return_on_error!(
                    &mut cost,
                    self.indexed_count_range(
                        path,
                        lo_count,
                        hi_count,
                        descending,
                        limit,
                        transaction,
                        grove_version
                    )
                ))
            }
            IndexAxis::Sum => {
                let (lo_sum, hi_sum) = clamp_sum_bounds(lo, hi);
                AxisEntries::Sum(cost_return_on_error!(
                    &mut cost,
                    self.indexed_sum_range(
                        path,
                        lo_sum,
                        hi_sum,
                        descending,
                        limit,
                        transaction,
                        grove_version
                    )
                ))
            }
            IndexAxis::Avg => AxisEntries::Avg(cost_return_on_error!(
                &mut cost,
                self.indexed_avg_range(path, lo, hi, descending, limit, transaction, grove_version)
            )),
        };
        Ok(entries).wrap_with_cost(cost)
    }
}

/// Clamp inclusive `i128` bounds into the count axis's `u64` domain.
/// Callers have already rejected wholly-out-of-domain ranges, so the
/// clamped pair still satisfies `lo <= hi`.
fn clamp_count_bounds(lo: i128, hi: i128) -> (u64, u64) {
    (
        lo.clamp(0, u64::MAX as i128) as u64,
        hi.clamp(0, u64::MAX as i128) as u64,
    )
}

/// Clamp inclusive `i128` bounds into the sum axis's `i64` domain.
fn clamp_sum_bounds(lo: i128, hi: i128) -> (i64, i64) {
    (
        lo.clamp(i64::MIN as i128, i64::MAX as i128) as i64,
        hi.clamp(i64::MIN as i128, i64::MAX as i128) as i64,
    )
}
