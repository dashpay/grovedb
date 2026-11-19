//! The canonical encoding of GroveDB's query types.
//!
//! [`Read::PathQuery`](super::op::Read::PathQuery) and
//! [`Read::SumBudget`](super::op::Read::SumBudget) carry GroveDB's own query
//! types, so a stream can say anything the query grammar allows. Their bytes
//! in a stream digest are written here, field by field in the conventions of
//! [`Op::encode`](super::op::Op::encode), and not taken from GroveDB's wire
//! encoding: a digest then changes only when a query does. Every `match`
//! names its variants, so a variant added to GroveDB stops this file
//! compiling until it has an encoding.

use grovedb::{
    AggregateSumPathQuery, AxisProjection, AxisQuery, AxisTraversal, PathQuery, Query, QueryItem,
    SubqueryBranch,
};
use grovedb_merk::proofs::query::{AggregateFold, IndexAxis, ReadMode};

use super::op::{put_bytes, put_len, put_option, put_path};

pub(crate) fn put_path_query(out: &mut Vec<u8>, path_query: &PathQuery) {
    put_path(out, &path_query.path);
    put_query(out, &path_query.query.query);
    put_option(out, path_query.query.limit, put_u16);
    put_option(out, path_query.query.offset, put_u16);
}

pub(crate) fn put_sum_path_query(out: &mut Vec<u8>, path_query: &AggregateSumPathQuery) {
    let query = &path_query.aggregate_sum_query;
    put_path(out, &path_query.path);
    put_items(out, &query.items);
    out.push(u8::from(query.left_to_right));
    out.extend_from_slice(&query.sum_limit.to_le_bytes());
    put_option(out, query.limit_of_items_to_check, put_u16);
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_items(out: &mut Vec<u8>, items: &[QueryItem]) {
    put_len(out, items.len());
    for item in items {
        put_item(out, item);
    }
}

fn put_query(out: &mut Vec<u8>, query: &Query) {
    let Query {
        items,
        default_subquery_branch,
        conditional_subquery_branches,
        left_to_right,
        add_parent_tree_on_subquery,
        read_mode,
        limit,
        offset,
    } = query;
    put_items(out, items);
    put_branch(out, default_subquery_branch);
    put_option(
        out,
        conditional_subquery_branches.as_ref(),
        |out, branches| {
            put_len(out, branches.len());
            for (item, branch) in branches {
                put_item(out, item);
                put_branch(out, branch);
            }
        },
    );
    out.push(u8::from(*left_to_right));
    out.push(u8::from(*add_parent_tree_on_subquery));
    put_option(out, read_mode.as_deref(), put_read_mode);
    put_option(out, *limit, put_u16);
    put_option(out, *offset, put_u16);
}

fn put_branch(out: &mut Vec<u8>, branch: &SubqueryBranch) {
    let SubqueryBranch {
        subquery_path,
        subquery,
    } = branch;
    put_option(out, subquery_path.as_deref(), put_path);
    put_option(out, subquery.as_deref(), put_query);
}

fn put_item(out: &mut Vec<u8>, item: &QueryItem) {
    match item {
        QueryItem::Key(key) => {
            out.push(0);
            put_bytes(out, key);
        }
        QueryItem::Range(range) => put_bounds(out, 1, &[&range.start, &range.end]),
        QueryItem::RangeInclusive(range) => put_bounds(out, 2, &[range.start(), range.end()]),
        QueryItem::RangeFull(..) => out.push(3),
        QueryItem::RangeFrom(range) => put_bounds(out, 4, &[&range.start]),
        QueryItem::RangeTo(range) => put_bounds(out, 5, &[&range.end]),
        QueryItem::RangeToInclusive(range) => put_bounds(out, 6, &[&range.end]),
        QueryItem::RangeAfter(range) => put_bounds(out, 7, &[&range.start]),
        QueryItem::RangeAfterTo(range) => put_bounds(out, 8, &[&range.start, &range.end]),
        QueryItem::RangeAfterToInclusive(range) => {
            put_bounds(out, 9, &[range.start(), range.end()])
        }
        QueryItem::AggregateCountOnRange(inner) => {
            out.push(10);
            put_item(out, inner);
        }
        QueryItem::AggregateSumOnRange(inner) => {
            out.push(11);
            put_item(out, inner);
        }
        QueryItem::AggregateCountAndSumOnRange(inner) => {
            out.push(12);
            put_item(out, inner);
        }
    }
}

fn put_bounds(out: &mut Vec<u8>, tag: u8, bounds: &[&Vec<u8>]) {
    out.push(tag);
    for bound in bounds {
        put_bytes(out, bound);
    }
}

fn put_read_mode(out: &mut Vec<u8>, read_mode: &ReadMode) {
    match read_mode {
        ReadMode::Axis(axis_query) => {
            out.push(0);
            put_axis_query(out, axis_query);
        }
        ReadMode::SumBudget(budget) => {
            out.push(1);
            out.extend_from_slice(&budget.sum_limit.to_le_bytes());
            put_option(out, budget.match_limit, put_u16);
        }
    }
}

fn put_axis_query(out: &mut Vec<u8>, axis_query: &AxisQuery) {
    let AxisQuery {
        axis,
        traversal,
        descending,
        projection,
    } = axis_query;
    out.push(match axis {
        IndexAxis::Count => 0,
        IndexAxis::Sum => 1,
        IndexAxis::Avg => 2,
        IndexAxis::BigSum => 3,
    });
    match traversal {
        AxisTraversal::RankedPage { k, offset } => {
            out.push(0);
            put_u16(out, *k);
            out.extend_from_slice(&offset.to_le_bytes());
        }
        AxisTraversal::Bounded { lo, hi, limit } => {
            out.push(1);
            out.extend_from_slice(&lo.to_le_bytes());
            out.extend_from_slice(&hi.to_le_bytes());
            put_u16(out, *limit);
        }
        AxisTraversal::RankOfKey { key } => {
            out.push(2);
            put_bytes(out, key);
        }
        AxisTraversal::AggregateOverValueRange { lo, hi, fold } => {
            out.push(3);
            out.extend_from_slice(&lo.to_le_bytes());
            out.extend_from_slice(&hi.to_le_bytes());
            out.push(match fold {
                AggregateFold::Population => 0,
                AggregateFold::Total => 1,
            });
        }
    }
    out.push(u8::from(*descending));
    out.push(match projection {
        AxisProjection::Entries => 0,
        AxisProjection::Keys => 1,
    });
}
