//! `path_query_push` v2 (`GROVE_V5`+).
//!
//! The V5 trusted engine: [`super::v1`]'s accounting, plus what a read needs
//! to return the rows a V1 proof of the same query returns. Three changes over
//! v1:
//!
//! 1. **Local offsets are refused where the prover refuses them.** A subquery
//!    carrying a `Query::offset` anywhere needs Merk trees along its path, so
//!    an Item or a non-Merk tree as this element, or as a present component of
//!    its `subquery_path`, is refused. A missing component still reads as
//!    empty. A subquery whose own node carries the offset, directly under this
//!    element, also needs this element to be a provable count-bearing tree. The
//!    page itself is read by the child frame: `get_path_query_internal` hands a
//!    query node that carries an offset to the local page reader.
//! 2. **Raw reads echo the parent tree.** When the query sets
//!    `add_parent_tree_on_subquery` and the options keep
//!    `include_parent_tree_on_subquery`, a raw read returns this tree's own
//!    element ahead of the rows below it, as the proof does. An echo is not a
//!    row: it charges no budget, and since a global offset could not skip it,
//!    the two are refused together before the walk starts. Non-raw reads never
//!    echo, so the typed wrappers keep their V4 results.
//! 3. **Descents take a child's rows from its count, not its result list.** v1
//!    counts a child's rows as the length of what it returned, which from here
//!    on also holds the echoes of deeper parents, so the instance budget would
//!    be charged for them. v2 takes the rows the child frame charged
//!    (`QueryBudget::rows`, which excludes echoes) and adds them to this
//!    frame's own count, so its parent can do the same.
//!
//! The rest is v1's accounting unchanged: the global budget settles by
//! everything a child charged, the instance chain by rows only unless
//! empty-subtree charges are opted in, and the empty-subtree charge fires only
//! for a child that consumed nothing and skipped nothing (issue #690).

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
        local_offset_read::refuse_non_merk_on_offset_path,
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

/// `path_query_push` v2: see the module documentation.
pub(crate) fn path_query_push_v2(
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
        add_parent_tree_on_subquery,
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
    // Change 1: an offset below this element needs a Merk tree here, and an
    // offset on the subquery's own node needs a count-bearing one.
    if subquery
        .as_ref()
        .is_some_and(|query| query.has_instance_offset_anywhere())
        && (!element.is_any_tree() || element.uses_non_merk_data_storage())
    {
        return Err(Error::InvalidQuery(
            "local offsets require Merk tree intermediates along their selected path",
        ))
        .wrap_with_cost(cost);
    }
    if subquery
        .as_ref()
        .is_some_and(|query| query.offset.is_some())
        && subquery_path.as_ref().is_none_or(|path| path.is_empty())
        && !matches!(
            element.underlying(),
            Element::ProvableCountTree(..)
                | Element::ProvableCountSumTree(..)
                | Element::ProvableCountProvableSumTree(..)
        )
    {
        return Err(Error::InvalidQuery(
            "local offsets require a provable count-bearing host",
        ))
        .wrap_with_cost(cost);
    }
    if element.is_any_tree() {
        let mut path_vec = path.to_vec();
        let key = cost_return_on_error_no_add!(
            cost,
            key.ok_or(Error::MissingParameter(
                "the key must be provided when using a subquery path",
            ))
        );
        path_vec.push(key);

        // The prover refuses a non-Merk element anywhere on a local offset's
        // path, including a subquery_path's components above a range node.
        if let (Some(subquery), Some(subquery_path)) = (&subquery, &subquery_path)
            && subquery.has_instance_offset_anywhere()
            && !subquery_path.is_empty()
        {
            let mut offset_path = path_vec.clone();
            offset_path.extend(subquery_path.iter().map(|k| k.as_slice()));
            cost_return_on_error!(
                &mut cost,
                refuse_non_merk_on_offset_path(
                    storage,
                    &offset_path,
                    path_vec.len(),
                    tx.as_ref(),
                    grove_version
                )
            );
        }

        // Change 2: only raw reads echo, so non-raw reads keep their V4
        // results. The echo charges no budget.
        if allow_get_raw
            && query_options.include_parent_tree_on_subquery
            && add_parent_tree_on_subquery
            && (subquery.is_some() || subquery_path.is_some())
        {
            let echoed = element.clone();
            match result_type {
                QueryElementResultType => {
                    results.push(QueryResultElement::ElementResultItem(echoed))
                }
                QueryKeyElementPairResultType => results.push(
                    QueryResultElement::KeyElementPairResultItem((key.to_vec(), echoed)),
                ),
                QueryPathKeyElementTrioResultType => {
                    results.push(QueryResultElement::PathKeyElementTrioResultItem((
                        path.iter().map(|p| p.to_vec()).collect(),
                        key.to_vec(),
                        echoed,
                    )))
                }
            }
        }

        if let Some(subquery) = subquery {
            if let Some(subquery_path) = &subquery_path {
                path_vec.extend(subquery_path.iter().map(|k| k.as_slice()));
            }

            let inner_query = SizedQuery::new(subquery, budget.global, budget.offset);
            let path_vec_owned = path_vec.iter().map(|x| x.to_vec()).collect();
            let inner_path_query = PathQuery::new(path_vec_owned, inner_query);

            // Change 3: `child_rows` is what the child frame charged, which
            // excludes echoes; its result list includes them.
            let (mut sub_elements, skipped, child_consumed, child_rows) = cost_return_on_error!(
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

            // Global budget: settle by everything the child charged (rows
            // plus empty-subtree charges), matching the prover's
            // shared-counter accounting.
            if let Some(global) = budget.global.as_mut() {
                *global = global.saturating_sub(child_consumed);
            }
            // Instance chain: result rows only, unless empty-subtree charges
            // were opted in. The instance flag is subordinate to the governing
            // empty-range flag per the `QueryOptions` contract, and
            // `child_consumed` can carry the unconditional absent-terminal
            // global charges even when the governing flag is off.
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
            // Carried up so the enclosing frame reconciles from rows charged
            // too, not from a result list that holds echoes.
            budget.rows = budget.rows.saturating_add(child_rows);

            // The empty-subtree charge, restated over consumption: only a
            // child that charged nothing at all and skipped nothing (the
            // issue-#690 guard) eats a slot here.
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
                    // unconditionally here, and v1 and v2 keep that global
                    // charge (it bounds walks across many missing terminals)
                    // but class it with the empty-subtree charges for the
                    // instance chain: no row was produced, and per the
                    // `QueryOptions` contract the instance flag has no effect
                    // unless the governing empty-range charging flag is on
                    // too.
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
                        add_parent_tree_on_subquery,
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
                    add_parent_tree_on_subquery,
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
