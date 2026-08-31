//! `path_query_push` — **v0** (legacy, `GROVE_V1`..`GROVE_V3`).
//!
//! Frozen accounting: an empty inner subquery result consumes one outer
//! limit slot whenever `decrease_limit_on_range_with_no_sub_elements` is
//! set, even when the emptiness was caused by `offset` skipping rows that
//! did match (issue #690). `GROVE_V1`..`GROVE_V3` are live in production
//! and must keep returning these result sets; see the module documentation
//! and [`super::v1`] for the fixed accounting.

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
        path_query_push_args::PathQueryPushArgs, query::ElementQueryExtensions,
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

/// `path_query_push` v0 — see the module documentation.
pub(crate) fn path_query_push_v0(
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

            let (mut sub_elements, skipped) = cost_return_on_error!(
                &mut cost,
                Element::get_path_query(
                    storage,
                    &inner_path_query,
                    query_options,
                    result_type,
                    transaction,
                    grove_version,
                )
            );

            if let Some(limit) = budget.global.as_mut() {
                // v0: decrement even when the emptiness was caused by offset
                // skipping rows that matched — the issue #690 behaviour,
                // frozen for GROVE_V1..GROVE_V3. [`super::v1`] (GROVE_V4+)
                // adds a `skipped == 0` guard here; that guard is the only
                // difference between the two files.
                if sub_elements.is_empty() && decrease_limit_on_range_with_no_sub_elements {
                    *limit = limit.saturating_sub(1);
                } else {
                    *limit = limit.saturating_sub(sub_elements.len().min(u16::MAX as usize) as u16);
                }
            }
            if let Some(offset) = budget.offset.as_mut() {
                *offset = offset.saturating_sub(skipped);
            }
            results.append(&mut sub_elements.elements);
        } else if let Some(subquery_path) = subquery_path {
            if budget.offset.unwrap_or(0) == 0 {
                if let Some((subquery_path_last_key, subquery_path_front_keys)) =
                    &subquery_path.split_last()
                {
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

                    match result_type {
                        QueryElementResultType => {
                            if let Some(element) = cost_return_on_error_into!(
                                &mut cost,
                                Element::get_optional_with_absolute_refs(
                                    &subtree,
                                    path_vec.as_slice(),
                                    subquery_path_last_key.as_slice(),
                                    allow_cache,
                                    grove_version,
                                )
                            ) {
                                results.push(QueryResultElement::ElementResultItem(element));
                            }
                        }
                        QueryKeyElementPairResultType => {
                            if let Some(element) = cost_return_on_error_into!(
                                &mut cost,
                                Element::get_optional_with_absolute_refs(
                                    &subtree,
                                    path_vec.as_slice(),
                                    subquery_path_last_key.as_slice(),
                                    allow_cache,
                                    grove_version,
                                )
                            ) {
                                results.push(QueryResultElement::KeyElementPairResultItem((
                                    subquery_path_last_key.to_vec(),
                                    element,
                                )));
                            }
                        }
                        QueryPathKeyElementTrioResultType => {
                            if let Some(element) = cost_return_on_error_into!(
                                &mut cost,
                                Element::get_optional_with_absolute_refs(
                                    &subtree,
                                    path_vec.as_slice(),
                                    subquery_path_last_key.as_slice(),
                                    allow_cache,
                                    grove_version,
                                )
                            ) {
                                results.push(QueryResultElement::PathKeyElementTrioResultItem((
                                    path_vec.iter().map(|p| p.to_vec()).collect(),
                                    subquery_path_last_key.to_vec(),
                                    element,
                                )));
                            }
                        }
                    }
                } else {
                    return Err(Error::CorruptedCodeExecution(
                        "subquery_paths can not be empty",
                    ))
                    .wrap_with_cost(cost);
                };

                if let Some(limit) = budget.global.as_mut() {
                    *limit = limit.saturating_sub(1);
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
