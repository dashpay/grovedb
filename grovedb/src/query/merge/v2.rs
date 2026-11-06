//! Exclusive branch grafts with per-instance count-unit offsets.
//!
//! Direction agreement, global limit lifting, canonical branch ordering, and
//! exclusive grafts follow v1. See [`super::v1`] for the rationale for those
//! unchanged steps. This version additionally carries authored branch-local
//! `Query::offset`s. A nonzero global offset is still refused, as in v1: it
//! skips rows, while a branch offset skips committed count units, so lifting
//! one into the other could return a different page than the input alone. A
//! global zero skip remains a no-op. A global limit lifts only onto a body
//! without subqueries: it is also charged for an empty subtree, which a branch
//! cap is not. A node that echoes its parent trees cannot be shared with other
//! inputs at any depth, since its echo would cover their keys too, so an input
//! with an echo merges like one with a budget: only as an exclusive graft, which
//! applies these rules again at every node where the inputs meet.

use grovedb_merk::proofs::query::{Query, QueryItem, SubqueryBranch};

use crate::{
    operations::proof::util::hex_to_ascii, query::local_offset::has_parent_echo, Error, PathQuery,
};

pub(super) fn merge_v2(path_queries: Vec<&PathQuery>) -> Result<PathQuery, Error> {
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

    path_queries.into_iter().try_for_each(|path_query| {
        path_query
            .to_subquery_branch_with_offset_start_index(next_index)
            .and_then(|mut unsized_path_query| {
                if path_query.query.offset.is_some_and(|offset| offset != 0) {
                    return Err(Error::NotSupported(
                        "can not merge a path query with a global offset: it skips rows, while \
                         a merged branch can only skip count units; set Query::offset on the \
                         query instead"
                            .to_string(),
                    ));
                }
                if unsized_path_query.subquery_path.is_none() {
                    let root_body =
                        *unsized_path_query
                            .subquery
                            .ok_or(Error::CorruptedCodeExecution(
                                "subquery must exist when subquery_path is none in merge",
                            ))?;
                    if path_query.query.limit.is_some()
                        || root_body.limit.is_some()
                        || root_body.offset.is_some()
                    {
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
                    if let Some(global) = path_query.query.limit {
                        let subquery = unsized_path_query.subquery.as_deref_mut().ok_or(
                            Error::CorruptedCodeExecution(
                                "subquery must exist on a sub-level merge branch",
                            ),
                        )?;
                        // A global limit is also charged for an empty subtree,
                        // a branch cap for rows only, so a cap would serve a
                        // descending body more rows than the input alone.
                        if subquery.has_subquery() {
                            return Err(Error::NotSupported(
                                "can not merge a path query with a global limit whose body has \
                                 subqueries: a branch cap would not charge its empty subtrees; \
                                 set Query::limit on the query instead"
                                    .to_string(),
                            ));
                        }
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

    // A node's parent echo covers every key it selects with a subquery, so an
    // echoing root body would echo the keys other inputs graft into its node.
    // Bodies meeting here also merge whatever they select below, so an echo
    // further down would cover the other bodies' keys in the same way.
    if (queries_for_common_path_this_level
        .iter()
        .any(|query| query.add_parent_tree_on_subquery)
        && (!queries_for_common_path_sub_level.is_empty()
            || queries_for_common_path_this_level
                .iter()
                .any(|query| !query.add_parent_tree_on_subquery)))
        || (queries_for_common_path_this_level.len() > 1
            && queries_for_common_path_this_level
                .iter()
                .any(echoes_below_its_own_node))
    {
        return Err(Error::NotSupported(
            "can not merge a path query that echoes its parent trees with other queries \
             meeting at the same node: the echo would cover their keys too; merge it \
             separately"
                .to_string(),
        ));
    }

    if queries_for_common_path_this_level.len() > 1
        && queries_for_common_path_this_level.iter().any(|query| {
            query.has_instance_limit_anywhere() || query.has_instance_offset_anywhere()
        })
    {
        return Err(Error::NotSupported(
            "can not merge limited path queries whose bodies meet at the merged root: a \
             branch cap would govern rows the other bodies select; give the limited query a \
             longer path of its own or merge it separately"
                .to_string(),
        ));
    }

    let mut merged_query = match queries_for_common_path_this_level.len() {
        0 => Query::new(),
        1 => queries_for_common_path_this_level.remove(0),
        _ => Query::merge_multiple_directional(queries_for_common_path_this_level)
            .map_err(|e| Error::NotSupported(e.to_string()))?,
    };

    // A budget belongs to one input's rows and an echo to one input's keys, and
    // a free merge below the merged root would blend either with another input's.
    let stays_exclusive = |query: &Query| {
        query.has_instance_limit_anywhere()
            || query.has_instance_offset_anywhere()
            || has_parent_echo(query)
    };
    let branch_stays_exclusive =
        |branch: &SubqueryBranch| branch.subquery.as_deref().is_some_and(stays_exclusive);
    let (exclusive_branches, free_branches): (Vec<SubqueryBranch>, Vec<SubqueryBranch>) =
        queries_for_common_path_sub_level
            .into_iter()
            .partition(&branch_stays_exclusive);

    for sub_path_query in free_branches.into_iter().chain(exclusive_branches) {
        let use_free_merge =
            !(branch_stays_exclusive(&sub_path_query) || stays_exclusive(&merged_query));
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
        if use_free_merge {
            merged_query.insert_item(QueryItem::Key(key.clone()));
            merged_query
                .merge_conditional_boxed_subquery(
                    QueryItem::Key(key),
                    SubqueryBranch {
                        subquery_path: rest_of_path,
                        subquery,
                    },
                )
                .map_err(|e| Error::NotSupported(e.to_string()))?;
            continue;
        }

        let exact_key = QueryItem::Key(key.clone());
        let selected_by_exact_key = merged_query.items.contains(&exact_key);
        let owned_by_exact_key = merged_query
            .conditional_subquery_branches
            .as_ref()
            .and_then(|branches| branches.keys().find(|item| item.contains(key.as_slice())))
            .is_some_and(|item| *item == exact_key);
        let selected_by_a_range = merged_query
            .items
            .iter()
            .any(|item| item.contains(key.as_slice()) && *item != exact_key);
        if selected_by_exact_key && owned_by_exact_key && !selected_by_a_range {
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
                "can not merge path queries with limits, offsets or parent echoes whose \
                 branches collide at key {}; remove them or merge the colliding queries \
                 separately",
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

    merged_query.left_to_right = shared_direction;

    Ok(PathQuery::new_unsized(common_path, merged_query))
}

/// Whether a node below `query`'s own echoes its parent trees.
fn echoes_below_its_own_node(query: &Query) -> bool {
    let echoes = |branch: &SubqueryBranch| branch.subquery.as_deref().is_some_and(has_parent_echo);
    echoes(&query.default_subquery_branch)
        || query
            .conditional_subquery_branches
            .as_ref()
            .is_some_and(|branches| branches.values().any(echoes))
}

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
    let merged = merge_v2(vec![&existing, &incoming]).map_err(|error| match error {
        Error::NotSupported(message) => Error::NotSupported(format!(
            "can not merge path queries with limits, offsets or parent echoes whose \
             branches collide at key {} and do not diverge below it: {}",
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
