//! `PathQuery::merge` — **v1** (`GROVE_V4`+).
//!
//! Direction-aware and limit-lifting. Every input must agree on
//! `left_to_right` — a conflict is a typed error instead of v0's
//! silent drop — and the shared direction propagates to the merged
//! root, so sub-level inputs no longer lose it under a synthesized
//! root query. In addition, v1 merges *limited* path queries by
//! **lifting**: an input's global `SizedQuery::limit` becomes its
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
//! - limit-free inputs merge exactly as v0 does, modulo the direction
//!   rules above, on the same code path.
//!
//! Offsets are still refused at every version.
//!
//! (An intermediate carrying only the direction rules was once gated
//! here for `GROVE_V4`; since no grove version ever shipped it, it was
//! folded into this v1 rather than kept as a dead dispatch arm —
//! matching the `element.path_query_push` fold.)

use grovedb_merk::proofs::query::{Query, QueryItem, SubqueryBranch};

use crate::{operations::proof::util::hex_to_ascii, Error, PathQuery};

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
        let carries_limits = path_query.query.limit.is_some() || path_query.has_instance_limits();
        path_query
            .to_subquery_branch_with_offset_start_index(next_index)
            .and_then(|mut unsized_path_query| {
                if unsized_path_query.subquery_path.is_none() {
                    // The input lands at the merged root, where its
                    // query body merges with the other root-level
                    // inputs — budgets cannot blend, so limits are
                    // refused here.
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

    // Add conditional subqueries in a canonical order: every
    // limit-free branch first (through the full merge machinery, which
    // is still limit-free at that point), then the limit-carrying
    // branches as exclusive grafts. Without the partition, acceptance
    // would depend on input order — a disjoint limited branch
    // processed early would force a later limit-free overlap onto the
    // graft path and refuse it. The partition is deterministic (stable
    // over input order), and the verifier re-runs the same merge, so
    // both sides derive the identical query.
    let carries_limits = |branch: &SubqueryBranch| {
        branch
            .subquery
            .as_deref()
            .is_some_and(|subquery| subquery.has_instance_limit_anywhere())
    };
    let (limited_branches, unlimited_branches): (Vec<SubqueryBranch>, Vec<SubqueryBranch>) =
        queries_for_common_path_sub_level
            .into_iter()
            .partition(&carries_limits);

    for sub_path_query in unlimited_branches {
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
        // rather than discard if one ever reaches here. The merged
        // query is still limit-free in this phase, so the machinery's
        // blanket instance-limit gate cannot misfire.
        merged_query
            .merge_conditional_boxed_subquery(QueryItem::Key(key), subquery_branch)
            .map_err(|e| Error::NotSupported(e.to_string()))?;
    }

    for sub_path_query in limited_branches {
        let SubqueryBranch {
            subquery_path,
            subquery,
        } = sub_path_query;
        let mut subquery_path =
            subquery_path.ok_or(Error::CorruptedCodeExecution("subquery path must exist"))?;
        let key = subquery_path.remove(0); // must exist

        // Budgets never blend: a limit-carrying branch merges only as
        // an exclusive graft. Overlap with the merged root's own
        // selection (assessed BEFORE this branch's item is inserted —
        // the graft's own `Key` would otherwise always match) or with
        // any already-merged conditional would need the two sides'
        // bodies — and budgets — merged, which is refused by design.
        // Only the actually colliding structures are consulted, so a
        // disjoint limited branch cannot change another branch's
        // acceptance.
        let collides = merged_query
            .items
            .iter()
            .any(|item| item.contains(key.as_slice()))
            || merged_query
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
        merged_query.insert_item(QueryItem::Key(key.clone()));
        let rest_of_path = if subquery_path.is_empty() {
            None
        } else {
            Some(subquery_path)
        };
        merged_query.add_conditional_subquery(
            QueryItem::Key(key),
            rest_of_path,
            subquery.map(|subquery| *subquery),
        );
    }

    // The agreed direction travels to the merged root (it would
    // otherwise be lost whenever the inputs land at a sub level under
    // a synthesized root query).
    merged_query.left_to_right = shared_direction;

    Ok(PathQuery::new_unsized(common_path, merged_query))
}
