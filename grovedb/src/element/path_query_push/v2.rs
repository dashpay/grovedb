//! V5 trusted descent with unbudgeted parent echoes and local count pages.

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

        if add_parent_tree_on_subquery && (subquery.is_some() || subquery_path.is_some()) {
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

            if let Some(global) = budget.global.as_mut() {
                *global = global.saturating_sub(child_consumed);
            }
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
            budget.rows = budget.rows.saturating_add(child_rows);

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
