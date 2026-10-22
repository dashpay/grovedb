//! Trusted local pages reuse the unencoded count-skip traversal. Only boundary
//! paths and returned rows are visited; skipped subtrees stay collapsed.

use grovedb_costs::{cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt};
use grovedb_merk::proofs::{Node, Op};
use grovedb_storage::rocksdb_storage::RocksDbStorage;
use grovedb_version::version::GroveVersion;

use super::{
    path_query_push_args::PathQueryPushArgs, query_budget::QueryBudget, query_options::QueryOptions,
};
use crate::{
    query_result_type::{QueryResultElements, QueryResultType},
    util::{compat, TxRef},
    Element, Error, SizedQuery, TransactionArg,
};

#[allow(clippy::too_many_arguments)]
pub(super) fn read_local_count_page(
    storage: &RocksDbStorage,
    path: &[&[u8]],
    query: &SizedQuery,
    offset: u16,
    budget: &mut QueryBudget,
    options: QueryOptions,
    result_type: QueryResultType,
    transaction: TransactionArg,
    add_element_function: fn(PathQueryPushArgs, &GroveVersion) -> CostResult<(), Error>,
    v: &GroveVersion,
) -> CostResult<(QueryResultElements, u16, u16, u16), Error> {
    let mut cost = Default::default();
    let tx = TxRef::new(storage, transaction);
    let merk = match compat::merk_optional_tx(storage, path.into(), tx.as_ref(), None, v)
        .unwrap_add_cost(&mut cost)
    {
        Ok(merk) => merk,
        // Direct range reads treat a missing storage layer as empty under
        // either strict-path option. Intermediate descents enforce that option.
        Err(Error::PathParentLayerNotFound(_)) => {
            if options.decrease_limit_on_range_with_no_sub_elements {
                budget.charge_empty_subtree(
                    options.decrease_instance_limits_on_range_with_no_sub_elements,
                );
            }
            return Ok((
                QueryResultElements::from_elements(Vec::new()),
                0,
                budget.consumed,
                budget.rows,
            ))
            .wrap_with_cost(cost);
        }
        Err(error) => return Err(error).wrap_with_cost(cost),
    };
    let page = cost_return_on_error!(
        &mut cost,
        merk.prove_local_count_offset_on_range(
            &query.query.items[0],
            u64::from(offset),
            budget.effective_limit().map(u64::from),
            query.query.left_to_right,
            v
        )
        .map_err(Error::MerkError)
    );
    let mut results = Vec::new();
    for op in page.ops {
        let node = match op {
            Op::Push(node) | Op::PushInverted(node) => node,
            _ => continue,
        };
        let (key, value) = match node {
            Node::KV(key, value)
            | Node::KVCount(key, value, _)
            | Node::KVSum(key, value, _)
            | Node::KVCountSum(key, value, ..)
            | Node::KVValueHash(key, value, ..)
            | Node::KVValueHashFeatureType(key, value, ..) => (key, value),
            _ => continue,
        };
        let element = cost_return_on_error_no_add!(
            cost,
            Element::deserialize(&value, v).map_err(Error::from)
        );
        if element.is_non_empty_tree() {
            return Err(Error::NotSupported(
                "local count-unit pages cannot return nonempty trees".to_string(),
            ))
            .wrap_with_cost(cost);
        }
        cost_return_on_error!(
            &mut cost,
            add_element_function(
                PathQueryPushArgs {
                    storage,
                    transaction,
                    key: Some(&key),
                    element,
                    path,
                    subquery_path: None,
                    subquery: None,
                    left_to_right: query.query.left_to_right,
                    add_parent_tree_on_subquery: false,
                    query_options: options,
                    result_type,
                    results: &mut results,
                    budget,
                },
                v
            )
        );
    }
    if budget.rows == 0 && options.decrease_limit_on_range_with_no_sub_elements {
        budget.charge_empty_subtree(options.decrease_instance_limits_on_range_with_no_sub_elements);
    }
    // Local skips never contribute to the global SizedQuery skipped counter.
    Ok((
        QueryResultElements::from_elements(results),
        0,
        budget.consumed,
        budget.rows,
    ))
    .wrap_with_cost(cost)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{element::query::ElementQueryExtensions, Query};

    #[test]
    fn missing_local_page_charges_only_the_configured_empty_page_budgets() {
        let db = crate::tests::make_empty_grovedb();
        let v = GroveVersion::latest();
        let mut query = Query::new_range_full();
        query.offset = Some(1);
        query.limit = Some(1);
        let sized = SizedQuery::new(query, Some(2), None);
        for strict in [false, true] {
            for charge_empty in [false, true] {
                for charge_instance in [false, true] {
                    let mut budget = QueryBudget::new(Some(2), Some(1), None);
                    let page = read_local_count_page(
                        &db.db,
                        &[b"missing".as_slice(), b"doc"],
                        &sized,
                        1,
                        &mut budget,
                        QueryOptions {
                            allow_get_raw: true,
                            error_if_intermediate_path_tree_not_present: strict,
                            decrease_limit_on_range_with_no_sub_elements: charge_empty,
                            decrease_instance_limits_on_range_with_no_sub_elements: charge_instance,
                            ..QueryOptions::default()
                        },
                        QueryResultType::QueryElementResultType,
                        None,
                        Element::path_query_push,
                        v,
                    );
                    assert!(page.cost.seek_count > 0, "opening costs must be retained");
                    let (rows, skipped, consumed, row_count) = page.unwrap().unwrap();
                    assert!(rows.is_empty());
                    assert_eq!((skipped, row_count), (0, 0));
                    assert_eq!(consumed, u16::from(charge_empty));
                    assert_eq!(budget.global, Some(2 - u16::from(charge_empty)));
                    assert_eq!(
                        budget.instance,
                        Some(1 - u16::from(charge_empty && charge_instance))
                    );
                }
            }
        }
    }
}
