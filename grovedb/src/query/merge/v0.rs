//! `PathQuery::merge` — **v0** (legacy, `GROVE_V1`..`GROVE_V3`).
//!
//! Frozen behavior: input directions are silently dropped — sub-level
//! inputs end up under a synthesized root query whose direction is the
//! default, and this-level bodies keep the first query's direction
//! through [`Query::merge_multiple`] — and any input carrying a
//! `SizedQuery::limit`, a `SizedQuery::offset`, or a per-instance
//! `Query::limit` is refused. `GROVE_V1`..`GROVE_V3` are live in
//! production and must keep deriving these exact merged queries.

use grovedb_merk::proofs::query::{Query, QueryItem, SubqueryBranch};

use crate::{Error, PathQuery};

/// `merge` v0 — see the module documentation.
pub(super) fn merge_v0(path_queries: Vec<&PathQuery>) -> Result<PathQuery, Error> {
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

    let mut merged_query = Query::merge_multiple(queries_for_common_path_this_level)
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
        // The read-mode gate in `PathQuery::merge`'s prelude already
        // rejected any input carrying one, so this cannot fire today —
        // propagate rather than discard, so a future path that reaches
        // here with a read mode surfaces it instead of silently
        // dropping the mode.
        merged_query
            .merge_conditional_boxed_subquery(QueryItem::Key(key), subquery_branch)
            .map_err(|e| Error::NotSupported(e.to_string()))?;
    }

    Ok(PathQuery::new_unsized(common_path, merged_query))
}
