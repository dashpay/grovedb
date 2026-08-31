//! `PathQuery::merge` — **v1** (direction-aware; never shipped alone,
//! superseded by [`super::v2`] before `GROVE_V4` released).
//!
//! Every input must agree on `left_to_right` — a conflict is a typed
//! error instead of v0's silent drop — and the shared direction
//! propagates to the merged root through
//! [`Query::merge_multiple_directional`] and the final assignment
//! below, so sub-level inputs no longer lose it under a synthesized
//! root. Limits and offsets are still refused, exactly as in
//! [`super::v0`].
//!
//! Kept as its own dispatch arm (unlike the folded `path_query_push`
//! intermediate) because it is selectable data: the `merge` slot is
//! consulted by the verifier when re-deriving merged queries, and the
//! 0/1/2 numbering is already spelled out in the `GROVE_V4` version
//! table docs.

use grovedb_merk::proofs::query::{Query, QueryItem, SubqueryBranch};

use crate::{Error, PathQuery};

/// `merge` v1 — see the module documentation.
pub(super) fn merge_v1(path_queries: Vec<&PathQuery>) -> Result<PathQuery, Error> {
    // Direction agreement: merged queries feed proofs and the verifier
    // re-runs the same merge, so a conflict must be a typed error
    // rather than a silently different query.
    let shared_direction = path_queries[0].query.query.left_to_right;
    if path_queries
        .iter()
        .any(|path_query| path_query.query.query.left_to_right != shared_direction)
    {
        return Err(Error::NotSupported(
            "can not merge path queries with conflicting directions (left_to_right differs); \
             align the directions before merging"
                .to_string(),
        ));
    }

    let (common_path, next_index) = PathQuery::get_common_path(&path_queries);

    let mut queries_for_common_path_this_level: Vec<Query> = vec![];
    let mut queries_for_common_path_sub_level: Vec<SubqueryBranch> = vec![];

    // convert all the paths after the common path to queries
    path_queries.into_iter().try_for_each(|path_query| {
        if path_query.query.offset.is_some() {
            return Err(Error::NotSupported(
                "can not merge pathqueries with offsets".to_string(),
            ));
        }
        // Limits never merge here: a merged limit would silently mean
        // something different than either input asked for. (Merge v2
        // lifts them instead.)
        if path_query.query.limit.is_some() {
            return Err(Error::NotSupported(
                "can not merge pathqueries with limits, consider setting the limit after the \
                 merge"
                    .to_string(),
            ));
        }
        if path_query.has_instance_limits() {
            return Err(Error::NotSupported(
                "can not merge pathqueries carrying per-instance limits (Query::limit)".to_string(),
            ));
        }
        path_query
            .to_subquery_branch_with_offset_start_index(next_index)
            .and_then(|unsized_path_query| {
                if unsized_path_query.subquery_path.is_none() {
                    queries_for_common_path_this_level.push(*unsized_path_query.subquery.ok_or(
                        Error::CorruptedCodeExecution(
                            "subquery must exist when subquery_path is none in merge",
                        ),
                    )?);
                } else {
                    queries_for_common_path_sub_level.push(unsized_path_query);
                }
                Ok(())
            })
    })?;

    let mut merged_query = Query::merge_multiple_directional(queries_for_common_path_this_level)
        .map_err(|e| Error::NotSupported(e.to_string()))?;

    // add conditional subqueries
    for sub_path_query in queries_for_common_path_sub_level {
        let SubqueryBranch {
            subquery_path,
            subquery,
        } = sub_path_query;
        let mut subquery_path =
            subquery_path.ok_or(Error::CorruptedCodeExecution("subquery path must exist"))?;
        let key = subquery_path.remove(0); // must exist
        merged_query.insert_item(QueryItem::Key(key.clone()));
        let rest_of_path = if subquery_path.is_empty() {
            None
        } else {
            Some(subquery_path)
        };
        let subquery_branch = SubqueryBranch {
            subquery_path: rest_of_path,
            subquery,
        };
        // See v0: read modes are rejected in the prelude; propagate
        // rather than discard if one ever reaches here.
        merged_query
            .merge_conditional_boxed_subquery(QueryItem::Key(key), subquery_branch)
            .map_err(|e| Error::NotSupported(e.to_string()))?;
    }

    // The agreed direction travels to the merged root (it would
    // otherwise be lost whenever the inputs land at a sub level under
    // a synthesized root query).
    merged_query.left_to_right = shared_direction;

    Ok(PathQuery::new_unsized(common_path, merged_query))
}
