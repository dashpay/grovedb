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
//! - an input whose whole path is the common path lands *at the merged
//!   root*, where its query body merges with the other inputs': a
//!   budget at that body (a global limit, or a cap on the body itself)
//!   is refused, and caps on its own branches are accepted only when
//!   it is the sole body at the root (so nothing else selects rows
//!   under those branches);
//! - a limited branch colliding on a key already owned by another
//!   grafted branch **descends** into it and grafts where the two
//!   actually diverge (the same merge, one level down — see
//!   [`graft_below`]); it is refused only when the bodies would meet,
//!   or when the key is selected by a range or by the merged root's own
//!   items, where nothing below can be told apart;
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
        path_query
            .to_subquery_branch_with_offset_start_index(next_index)
            .and_then(|mut unsized_path_query| {
                if unsized_path_query.subquery_path.is_none() {
                    // The input lands at the merged root, where its
                    // query body merges with the other root-level
                    // inputs'. A budget AT that body — a global limit,
                    // which has nowhere to lift, or a cap on the body
                    // itself — would have to blend, so it is refused
                    // here. Caps on the body's own branches are
                    // exclusive to those branches and ride along, as
                    // long as nothing else lands at the root beside it
                    // (checked below, once every input is placed).
                    let root_body =
                        *unsized_path_query
                            .subquery
                            .ok_or(Error::CorruptedCodeExecution(
                                "subquery must exist when subquery_path is none in merge",
                            ))?;
                    if path_query.query.limit.is_some() || root_body.limit.is_some() {
                        return Err(Error::NotSupported(
                            "can not merge a limited path query that lands at the merged root: \
                             its budget would have to blend with the other queries' result \
                             sets; give it a longer path of its own or set the limit after the \
                             merge"
                                .to_string(),
                        ));
                    }
                    queries_for_common_path_this_level.push(root_body);
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

    // Bodies meeting at the merged root become one query, so a cap on
    // any of their branches would govern rows the other bodies select
    // under the same keys: several root-landers merge only when none
    // carries a cap anywhere. (A lone root-lander keeps its branch caps
    // — this is how a branch that was itself merged from several inputs
    // comes back through `graft_below`.)
    if queries_for_common_path_this_level.len() > 1
        && queries_for_common_path_this_level
            .iter()
            .any(|query| query.has_instance_limit_anywhere())
    {
        return Err(Error::NotSupported(
            "can not merge limited path queries whose bodies meet at the merged root: a \
             branch cap would govern rows the other bodies select; give the limited query a \
             longer path of its own or merge it separately"
                .to_string(),
        ));
    }

    // A lone root-lander IS the root body — no merge to run, and none
    // of the merge machinery's gates to trip over the branch caps it
    // may legitimately carry.
    let mut merged_query = match queries_for_common_path_this_level.len() {
        0 => Query::new(),
        1 => queries_for_common_path_this_level.remove(0),
        _ => Query::merge_multiple_directional(queries_for_common_path_this_level)
            .map_err(|e| Error::NotSupported(e.to_string()))?,
    };

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
        let rest_of_path = if subquery_path.is_empty() {
            None
        } else {
            Some(subquery_path)
        };

        // A branch grafted earlier — limit-free through the machinery,
        // or an exclusive limited graft — may already own this exact
        // key. Budgets still never blend: the two are merged one level
        // down, where they either diverge onto exclusive keys of their
        // own or are refused because their bodies would meet.
        let owned_by_exact_key = merged_query
            .conditional_subquery_branches
            .as_ref()
            .is_some_and(|branches| branches.contains_key(&QueryItem::Key(key.clone())));
        let selected_by_a_range = merged_query
            .items
            .iter()
            .any(|item| item.contains(key.as_slice()) && *item != QueryItem::Key(key.clone()));
        if owned_by_exact_key && !selected_by_a_range {
            let branches = merged_query.conditional_subquery_branches.as_mut().ok_or(
                Error::CorruptedCodeExecution(
                    "conditional branches must exist when one owns the key",
                ),
            )?;
            let existing = branches.get(&QueryItem::Key(key.clone())).cloned().ok_or(
                Error::CorruptedCodeExecution("the owning conditional branch must exist"),
            )?;
            let grafted = graft_below(
                &common_path,
                &key,
                existing,
                SubqueryBranch {
                    subquery_path: rest_of_path,
                    subquery,
                },
            )?;
            branches.insert(QueryItem::Key(key), grafted);
            continue;
        }

        // Budgets never blend: a limit-carrying branch merges only as
        // an exclusive graft. Overlap with the merged root's own
        // selection (assessed BEFORE this branch's item is inserted —
        // the graft's own `Key` would otherwise always match) or with
        // a conditional that selects the key by a range would need the
        // two sides' bodies — and budgets — merged, which is refused by
        // design. Only the actually colliding structures are consulted,
        // so a disjoint limited branch cannot change another branch's
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

/// Merge a limited branch into the branch that already owns its first
/// key, one level down.
///
/// Both branches are rebuilt as path queries rooted at `common_path +
/// [key]` and run through the same merge: their remaining paths either
/// diverge — each lands on an exclusive key of its own, exactly as it
/// would have had the common path been one level deeper — or they do
/// not, and the merge refuses them there (a body landing at that root,
/// or a further collision that never diverges) with the root-landing
/// and collision rules above. The result is the owning branch's
/// replacement: its subquery path is whatever the two still share below
/// `key`, and its subquery is the merged body. Lifted limits are already
/// per-instance caps on the leaf bodies, so the descent carries no
/// global limit to lift twice.
fn graft_below(
    common_path: &[Vec<u8>],
    key: &[u8],
    existing: SubqueryBranch,
    incoming: SubqueryBranch,
) -> Result<SubqueryBranch, Error> {
    let base: Vec<Vec<u8>> = common_path
        .iter()
        .cloned()
        .chain(std::iter::once(key.to_vec()))
        .collect();
    let as_path_query = |branch: SubqueryBranch| -> Result<PathQuery, Error> {
        let SubqueryBranch {
            subquery_path,
            subquery,
        } = branch;
        let query = subquery.ok_or(Error::NotSupported(
            "can not descend into a conditional branch that carries no subquery".to_string(),
        ))?;
        let path: Vec<Vec<u8>> = base
            .iter()
            .cloned()
            .chain(subquery_path.unwrap_or_default())
            .collect();
        Ok(PathQuery::new_unsized(path, *query))
    };
    let existing = as_path_query(existing)?;
    let incoming = as_path_query(incoming)?;
    let merged = merge_v1(vec![&existing, &incoming]).map_err(|error| match error {
        Error::NotSupported(message) => Error::NotSupported(format!(
            "can not merge limited path queries whose branches collide at key {} and do \
             not diverge below it: {}",
            hex_to_ascii(key),
            message
        )),
        other => other,
    })?;
    let PathQuery { path, query } = merged;
    let below = path[base.len()..].to_vec();
    Ok(SubqueryBranch {
        subquery_path: if below.is_empty() { None } else { Some(below) },
        subquery: Some(Box::new(query.query)),
    })
}
