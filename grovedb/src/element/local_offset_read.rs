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
    let merk = cost_return_on_error!(
        &mut cost,
        compat::merk_optional_tx(storage, path.into(), tx.as_ref(), None, v)
    );
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
