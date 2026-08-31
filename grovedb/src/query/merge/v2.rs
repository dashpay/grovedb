//! `PathQuery::merge` — **v2** (`GROVE_V4`+).
//!
//! Carries [`super::v1`]'s direction rules (agreement required, shared
//! direction propagated) and additionally merges *limited* path queries
//! by **lifting**: an input's global `SizedQuery::limit` becomes its
//! merged branch's per-instance cap (`Query::limit`) — exact, because
//! the branch instance executes exactly once (its path is a concrete
//! key chain), so "at most N rows from this whole input" and "at most
//! N rows per instance" coincide — and authored per-instance limits
//! ride along on their branches.
//!
//! Budgets never blend, so limits merge only as **exclusive grafts**:
//!
//! - a limited input whose whole path is the common path lands *at the
//!   merged root*, where its query body would merge with the other
//!   inputs' — refused;
//! - two limited branches colliding on a key are refused;
//! - limit-free inputs keep the v1 merge behavior on the same code
//!   path.
//!
//! Offsets are still refused at every version.

use grovedb_merk::proofs::query::{Query, QueryItem, SubqueryBranch};

use crate::{operations::proof::util::hex_to_ascii, Error, PathQuery};

/// `merge` v2 — see the module documentation.
pub(super) fn merge_v2(path_queries: Vec<&PathQuery>) -> Result<PathQuery, Error> {
    // Direction agreement — see v1.
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
        let carries_limits = path_query.query.limit.is_some() || path_query.has_instance_limits();
        path_query
            .to_subquery_branch_with_offset_start_index(next_index)
            .and_then(|mut unsized_path_query| {
                if unsized_path_query.subquery_path.is_none() {
                    // The input lands at the merged root, where its
                    // query body merges with the other root-level
                    // inputs — budgets cannot blend, so limits are
                    // refused here even under v2.
                    if carries_limits {
                        return Err(Error::NotSupported(
                            "can not merge a limited path query that lands at the merged root: \
                             its budget would have to blend with the other queries' result \
                             sets; give it a longer path of its own or set the limit after the \
                             merge"
                                .to_string(),
                        ));
                    }
                    queries_for_common_path_this_level.push(*unsized_path_query.subquery.ok_or(
                        Error::CorruptedCodeExecution(
                            "subquery must exist when subquery_path is none in merge",
                        ),
                    )?);
                } else {
                    // The lift: an input's global budget becomes its
                    // branch-root query's per-instance cap. Exact,
                    // because the branch instance executes exactly once
                    // (its path is a concrete key chain), so "at most N
                    // rows from this whole input" and "at most N rows
                    // per instance" coincide.
                    if let Some(global) = path_query.query.limit {
                        let subquery = unsized_path_query.subquery.as_deref_mut().ok_or(
                            Error::CorruptedCodeExecution(
                                "subquery must exist on a sub-level merge branch",
                            ),
                        )?;
                        subquery.limit = Some(match subquery.limit {
                            Some(own) => own.min(global),
                            None => global,
                        });
                    }
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
        let limits_in_play = merged_query.has_instance_limit_anywhere()
            || subquery_branch
                .subquery
                .as_deref()
                .is_some_and(|subquery| subquery.has_instance_limit_anywhere());
        if limits_in_play {
            // Budgets never blend: a limit-carrying branch (lifted or
            // authored) merges only as an exclusive graft. Any overlap
            // with an existing conditional would need the two branches'
            // bodies — and budgets — merged, which is refused by
            // design.
            let collides = merged_query
                .conditional_subquery_branches
                .as_ref()
                .is_some_and(|branches| branches.keys().any(|item| item.contains(key.as_slice())));
            if collides {
                return Err(Error::NotSupported(format!(
                    "can not merge limited path queries whose branches collide at key {}; \
                     remove the limits or merge the colliding queries separately",
                    hex_to_ascii(&key),
                )));
            }
            merged_query.add_conditional_subquery(
                QueryItem::Key(key),
                subquery_branch.subquery_path,
                subquery_branch.subquery.map(|subquery| *subquery),
            );
        } else {
            // See v0: read modes are rejected in the prelude; propagate
            // rather than discard if one ever reaches here.
            merged_query
                .merge_conditional_boxed_subquery(QueryItem::Key(key), subquery_branch)
                .map_err(|e| Error::NotSupported(e.to_string()))?;
        }
    }

    // The agreed direction travels to the merged root — see v1.
    merged_query.left_to_right = shared_direction;

    Ok(PathQuery::new_unsized(common_path, merged_query))
}
