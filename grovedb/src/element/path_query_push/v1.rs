//! `path_query_push` — **v1** (`GROVE_V4`+).
//!
//! The per-instance-limit engine. Three changes over [`super::v0`]:
//!
//! 1. **Per-instance limits are served.** A subquery descent hands the
//!    child frame the parent's remaining instance budget
//!    (`budget.instance`); the child frame `min`-combines it with the
//!    child query's own `Query::limit`, so each execution instance of a
//!    query node gets a fresh cap while every enclosing cap keeps
//!    bounding it ("top k per parent").
//! 2. **Descents reconcile by consumed budget, not returned rows.** The
//!    child frame reports everything it charged against the global
//!    budget — rows plus empty-subtree charges — and the parent's
//!    global budget is settled from that. v0 subtracts returned rows
//!    only, silently dropping empty-subtree charges made below the
//!    first nesting level; the prover threads one shared counter and
//!    keeps them, so v1 aligns the read path with proof accounting.
//!    The instance chain is settled from rows only — per-instance caps
//!    bound result rows, not traversal work — unless
//!    `decrease_instance_limits_on_range_with_no_sub_elements` opts
//!    empty-subtree charges in.
//! 3. **The empty-subtree charge fires on zero consumption, and only
//!    when nothing was skipped.** v0 charges whenever the child
//!    returned no rows — even when offset skips consumed real matches
//!    (issue #690). v1 charges only when the child *consumed* nothing
//!    and skipped nothing: offset-emptied subqueries are pagination,
//!    not absence, and a child that burned budget on its own empty
//!    subtrees already moved the counters — exactly the prover's
//!    "has a result at this level" test.

use grovedb_costs::{
    cost_return_on_error, cost_return_on_error_into, cost_return_on_error_no_add, CostResult,
    CostsExt, OperationCost,
};
use grovedb_element::Element;
use grovedb_merk::element::get::ElementFetchFromStorageExtensions;
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

use crate::{
    element::{
        path_query_push_args::PathQueryPushArgs,
        query::{get_path_query_internal, ElementQueryExtensions},
        query_options::QueryOptions,
    },
    query_result_type::{
        QueryResultElement,
        QueryResultType::{
            QueryElementResultType, QueryKeyElementPairResultType,
            QueryPathKeyElementTrioResultType,
        },
    },
    Error, PathQuery, SizedQuery,
};

/// `path_query_push` v1 — see the module documentation.
pub(crate) fn path_query_push_v1(
    args: PathQueryPushArgs,
    grove_version: &GroveVersion,
) -> CostResult<(), Error> {
    use crate::util::{compat, TxRef};

    let mut cost = OperationCost::default();

    let PathQueryPushArgs {
        storage,
        transaction,
        key,
        element,
        path,
        subquery_path,
        subquery,
        left_to_right,
        query_options,
        result_type,
        results,
        budget,
    } = args;

    let tx = TxRef::new(storage, transaction);

    let QueryOptions {
        allow_get_raw,
        allow_cache,
        decrease_limit_on_range_with_no_sub_elements,
        decrease_instance_limits_on_range_with_no_sub_elements,
        ..
    } = query_options;
    if element.is_any_tree() {
        let mut path_vec = path.to_vec();
        let key = cost_return_on_error_no_add!(
            cost,
            key.ok_or(Error::MissingParameter(
                "the key must be provided when using a subquery path",
            ))
        );
        path_vec.push(key);

        if let Some(subquery) = subquery {
            if let Some(subquery_path) = &subquery_path {
                path_vec.extend(subquery_path.iter().map(|k| k.as_slice()));
            }

            let inner_query = SizedQuery::new(subquery, budget.global, budget.offset);
            let path_vec_owned = path_vec.iter().map(|x| x.to_vec()).collect();
            let inner_path_query = PathQuery::new(path_vec_owned, inner_query);

            let (mut sub_elements, skipped, child_consumed) = cost_return_on_error!(
                &mut cost,
                get_path_query_internal(
                    storage,
                    &inner_path_query,
                    budget.instance,
                    query_options,
                    result_type,
                    transaction,
                    grove_version,
                )
            );

            let child_rows = sub_elements.len().min(u16::MAX as usize) as u16;

            // Global budget: settle by everything the child charged —
            // rows plus empty-subtree charges — matching the prover's
            // shared-counter accounting.
            if let Some(global) = budget.global.as_mut() {
                *global = global.saturating_sub(child_consumed);
            }
            // Instance chain: result rows only, unless empty-subtree
            // charges were opted in — the instance flag is subordinate
            // to the governing empty-range flag per the `QueryOptions`
            // contract, and `child_consumed` can carry the
            // unconditional absent-terminal global charges even when
            // the governing flag is off.
            let instance_charge = if decrease_limit_on_range_with_no_sub_elements
                && decrease_instance_limits_on_range_with_no_sub_elements
            {
                child_consumed
            } else {
                child_rows
            };
            if let Some(instance) = budget.instance.as_mut() {
                *instance = instance.saturating_sub(instance_charge);
            }
            budget.consumed = budget.consumed.saturating_add(child_consumed);

            // The empty-subtree charge, restated over consumption: only
            // a child that charged nothing at all and skipped nothing
            // (the issue-#690 guard) eats a slot here.
            if child_consumed == 0 && skipped == 0 && decrease_limit_on_range_with_no_sub_elements {
                budget.charge_empty_subtree(decrease_instance_limits_on_range_with_no_sub_elements);
            }
            if let Some(offset) = budget.offset.as_mut() {
                *offset = offset.saturating_sub(skipped);
            }
            results.append(&mut sub_elements.elements);
        } else if let Some(subquery_path) = subquery_path {
            if budget.offset.unwrap_or(0) == 0 {
                let Some((subquery_path_last_key, subquery_path_front_keys)) =
                    &subquery_path.split_last()
                else {
                    return Err(Error::CorruptedCodeExecution(
                        "subquery_paths can not be empty",
                    ))
                    .wrap_with_cost(cost);
                };
                path_vec.extend(subquery_path_front_keys.iter().map(|k| k.as_slice()));

                let subtree_path: SubtreePath<_> = path_vec.as_slice().into();
                let subtree = cost_return_on_error!(
                    &mut cost,
                    compat::merk_optional_tx(
                        storage,
                        subtree_path,
                        tx.as_ref(),
                        None,
                        grove_version
                    )
                );

                let maybe_element = cost_return_on_error_into!(
                    &mut cost,
                    Element::get_optional_with_absolute_refs(
                        &subtree,
                        path_vec.as_slice(),
                        subquery_path_last_key.as_slice(),
                        allow_cache,
                        grove_version,
                    )
                );

                if let Some(element) = maybe_element {
                    match result_type {
                        QueryElementResultType => {
                            results.push(QueryResultElement::ElementResultItem(element));
                        }
                        QueryKeyElementPairResultType => {
                            results.push(QueryResultElement::KeyElementPairResultItem((
                                subquery_path_last_key.to_vec(),
                                element,
                            )));
                        }
                        QueryPathKeyElementTrioResultType => {
                            results.push(QueryResultElement::PathKeyElementTrioResultItem((
                                path_vec.iter().map(|p| p.to_vec()).collect(),
                                subquery_path_last_key.to_vec(),
                                element,
                            )));
                        }
                    }
                    budget.charge_row();
                } else {
                    // The terminal was absent. v0 charges the limit
                    // unconditionally here and v1 keeps that global
                    // charge (it bounds walks across many missing
                    // terminals), but classes it with the empty-subtree
                    // charges for the instance chain: no row was
                    // produced, and per the `QueryOptions` contract the
                    // instance flag is subordinate — it has no effect
                    // unless the governing empty-range charging flag is
                    // on too.
                    budget.charge_empty_subtree(
                        decrease_limit_on_range_with_no_sub_elements
                            && decrease_instance_limits_on_range_with_no_sub_elements,
                    );
                }
            } else if let Some(offset) = budget.offset.as_mut() {
                *offset = offset.saturating_sub(1);
            }
        } else if allow_get_raw {
            cost_return_on_error_no_add!(
                cost,
                Element::basic_push(
                    PathQueryPushArgs {
                        storage,
                        transaction,
                        key: Some(key),
                        element,
                        path,
                        subquery_path,
                        subquery,
                        left_to_right,
                        query_options,
                        result_type,
                        results,
                        budget,
                    },
                    grove_version
                )
            );
        } else {
            return Err(Error::InvalidPath(
                "you must provide a subquery or a subquery_path when interacting with a Tree of \
                 trees"
                    .to_owned(),
            ))
            .wrap_with_cost(cost);
        }
    } else {
        cost_return_on_error_no_add!(
            cost,
            Element::basic_push(
                PathQueryPushArgs {
                    storage,
                    transaction,
                    key,
                    element,
                    path,
                    subquery_path,
                    subquery,
                    left_to_right,
                    query_options,
                    result_type,
                    results,
                    budget,
                },
                grove_version
            )
        );
    }
    Ok(()).wrap_with_cost(cost)
}
