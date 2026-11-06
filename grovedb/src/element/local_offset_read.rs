//! Trusted local pages reuse the count-skip traversal and read the rows it
//! selects. Boundary paths, returned rows and the root of each collapsed
//! subtree are loaded; a collapsed subtree's contents are not.
//!
//! A page comes from one pinned storage view: the host element, its root and
//! every traversal fetch go through one raw iterator, which pins the committed
//! state at its creation plus a supplied transaction's own writes. Reading the
//! root key outside that view would let a commit that rotates the root leave
//! the key naming a demoted child, silently truncating the page.

use grovedb_costs::{cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt};
use grovedb_merk::{
    element::tree_type::ElementTreeTypeExtensions,
    proofs::query::count_offset::local_count_offset_page_from_view, tree_type::TreeType,
};
use grovedb_path::SubtreePath;
use grovedb_storage::{
    rocksdb_storage::{PrefixedRocksDbTransactionContext, RocksDbStorage},
    Storage, StorageContext,
};
use grovedb_version::version::GroveVersion;

use super::{
    path_query_push_args::PathQueryPushArgs, query_budget::QueryBudget, query_options::QueryOptions,
};
use crate::{
    operations::indexed_tree::snapshot_fetch_node,
    query_result_type::{QueryResultElements, QueryResultType},
    util::{validate_path_segment_lengths, TxRef},
    Element, Error, SizedQuery, Transaction, TransactionArg,
};

/// One raw iterator through a transaction: a pinned storage view.
type PinnedView<'db> = <PrefixedRocksDbTransactionContext<'db> as StorageContext<'db>>::RawIterator;

const NON_MERK_ON_OFFSET_PATH: &str =
    "local offsets require Merk tree intermediates along their selected path";

/// Walks `path` from component `from` and refuses a present element that is
/// not a Merk tree, as the prover's `validate_local_offset_host` refuses one
/// on a local offset's path. Returns whether a component is missing; the walk
/// stops there, since a missing page reads as empty.
pub(super) fn refuse_non_merk_on_offset_path(
    storage: &RocksDbStorage,
    path: &[&[u8]],
    from: usize,
    tx: &Transaction,
    v: &GroveVersion,
) -> CostResult<bool, Error> {
    let mut cost = Default::default();
    if from >= path.len() {
        return Ok(false).wrap_with_cost(cost);
    }
    let context = storage
        .get_transactional_storage_context(path[..from].into(), None, tx)
        .unwrap_add_cost(&mut cost);
    walk_offset_path(context.raw_iter(), path, from, v).add_cost(cost)
}

/// [`refuse_non_merk_on_offset_path`] through a view the caller already holds.
fn walk_offset_path(
    mut view: PinnedView<'_>,
    path: &[&[u8]],
    from: usize,
    v: &GroveVersion,
) -> CostResult<bool, Error> {
    let mut cost = Default::default();
    for depth in from..path.len() {
        let prefix = RocksDbStorage::build_prefix(path[..depth].into()).unwrap_add_cost(&mut cost);
        view = view.retarget(prefix);
        let node = cost_return_on_error!(&mut cost, snapshot_fetch_node(&mut view, path[depth], v));
        let Some(node) = node else {
            return Ok(true).wrap_with_cost(cost);
        };
        let element = cost_return_on_error_no_add!(
            cost,
            Element::deserialize(node.value_as_slice(), v).map_err(Error::from)
        );
        if !element.is_any_tree() || element.uses_non_merk_data_storage() {
            return Err(Error::InvalidQuery(NON_MERK_ON_OFFSET_PATH)).wrap_with_cost(cost);
        }
    }
    Ok(false).wrap_with_cost(cost)
}

/// A seam for landing a commit between reading the host element and loading
/// its root.
#[cfg(test)]
mod test_hooks {
    use std::cell::RefCell;

    thread_local! {
        pub(super) static AFTER_HOST_ELEMENT: RefCell<Option<Box<dyn FnOnce()>>> =
            const { RefCell::new(None) };
    }
}

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
    // The same refusal the prover makes (`reject_unserved_local_offsets`).
    if path.is_empty() {
        return Err(Error::InvalidQuery(
            "the GroveDB root cannot host a local count-unit offset",
        ))
        .wrap_with_cost(cost);
    }
    let tx = TxRef::new(storage, transaction);
    let subtree_path: SubtreePath<_> = path.into();
    cost_return_on_error_no_add!(cost, validate_path_segment_lengths(&subtree_path));
    let Some((host_key, parent_path)) = path.split_last() else {
        return Err(Error::CorruptedCodeExecution(
            "the root path was refused above",
        ))
        .wrap_with_cost(cost);
    };
    let parent_context = storage
        .get_transactional_storage_context(parent_path.into(), None, tx.as_ref())
        .unwrap_add_cost(&mut cost);
    let mut view = parent_context.raw_iter();
    let host = cost_return_on_error!(&mut cost, snapshot_fetch_node(&mut view, host_key, v));
    let Some(host) = host else {
        // Direct range reads treat a missing storage layer as empty under
        // either strict-path option. Intermediate descents enforce that option.
        // Absence below a non-tree ancestor is refused, as the prover refuses
        // it; a genuinely missing component is an empty page.
        cost_return_on_error!(&mut cost, walk_offset_path(view, path, 0, v));
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
    };
    let element = cost_return_on_error_no_add!(
        cost,
        Element::deserialize(host.value_as_slice(), v).map_err(Error::from)
    );
    if !element.is_any_tree() || element.uses_non_merk_data_storage() {
        return Err(Error::InvalidQuery(NON_MERK_ON_OFFSET_PATH)).wrap_with_cost(cost);
    }
    let Some((root_key, tree_type)) = element.root_key_and_tree_type_owned() else {
        return Err(Error::CorruptedPath(
            "cannot open a subtree as parent exists but is not a tree".to_string(),
        ))
        .wrap_with_cost(cost);
    };
    if !matches!(
        tree_type,
        TreeType::ProvableCountTree
            | TreeType::ProvableCountSumTree
            | TreeType::ProvableCountProvableSumTree
    ) {
        return Err(Error::InvalidQuery(
            "local count-unit offsets require a provable count-bearing host",
        ))
        .wrap_with_cost(cost);
    }
    #[cfg(test)]
    if let Some(hook) = test_hooks::AFTER_HOST_ELEMENT.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
    let host_prefix = RocksDbStorage::build_prefix(subtree_path).unwrap_add_cost(&mut cost);
    let rows = cost_return_on_error!(
        &mut cost,
        local_count_offset_page_from_view(
            view.retarget(host_prefix),
            root_key,
            tree_type,
            &query.query.items[0],
            u64::from(offset),
            budget.effective_limit().map(u64::from),
            query.query.left_to_right,
            v
        )
        .map_err(Error::MerkError)
    );
    let mut results = Vec::new();
    for (key, value) in rows {
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

    #[test]
    fn root_ordinary_and_item_hosts_are_invalid_queries() {
        let v = GroveVersion::latest();
        let db = crate::tests::make_empty_grovedb();
        db.insert(
            &[] as &[&[u8]],
            b"tree",
            Element::empty_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .unwrap();
        db.insert(
            &[] as &[&[u8]],
            b"item",
            Element::new_item(vec![1]),
            None,
            None,
            v,
        )
        .unwrap()
        .unwrap();
        let mut query = Query::new_range_full();
        query.offset = Some(1);
        let sized = SizedQuery::new(query, None, None);
        let hosts: [&[&[u8]]; 3] = [&[], &[b"tree".as_slice()], &[b"item".as_slice()]];
        for path in hosts {
            let mut budget = QueryBudget::new(None, None, None);
            let error = read_local_count_page(
                &db.db,
                path,
                &sized,
                1,
                &mut budget,
                QueryOptions {
                    allow_get_raw: true,
                    ..QueryOptions::default()
                },
                QueryResultType::QueryElementResultType,
                None,
                Element::path_query_push,
                v,
            )
            .unwrap()
            .unwrap_err();
            assert!(
                matches!(error, Error::InvalidQuery(_)),
                "{path:?}: {error:?}"
            );
        }
    }

    /// `[b"host"]` is a provable count tree holding the given Item keys.
    fn count_host(keys: &[&[u8]]) -> std::sync::Arc<crate::tests::TempGroveDb> {
        let v = GroveVersion::latest();
        let db = crate::tests::make_empty_grovedb();
        db.insert(
            &[] as &[&[u8]],
            b"host",
            Element::empty_provable_count_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .unwrap();
        for key in keys {
            db.insert(
                &[b"host".as_slice()],
                key,
                Element::new_item(key.to_vec()),
                None,
                None,
                v,
            )
            .unwrap()
            .unwrap();
        }
        std::sync::Arc::new(db)
    }

    /// The keys of `[b"host"]`'s ascending page at offset 1, limit 1.
    fn second_row(db: &crate::tests::TempGroveDb, transaction: TransactionArg) -> Vec<Vec<u8>> {
        let mut query = Query::new_range_full();
        query.offset = Some(1);
        query.limit = Some(1);
        let sized = SizedQuery::new(query, None, None);
        let mut budget = QueryBudget::new(None, Some(1), None);
        let (rows, ..) = read_local_count_page(
            &db.db,
            &[b"host".as_slice()],
            &sized,
            1,
            &mut budget,
            QueryOptions {
                allow_get_raw: true,
                ..QueryOptions::default()
            },
            QueryResultType::QueryKeyElementPairResultType,
            transaction,
            Element::path_query_push,
            GroveVersion::latest(),
        )
        .unwrap()
        .unwrap();
        rows.to_key_elements()
            .into_iter()
            .map(|(key, _)| key)
            .collect()
    }

    /// Inserting c rotates the root from a to b, demoting a to a childless node
    /// of count 1. Had the page loaded the root key it read before that commit
    /// from the newer state, the skip would consume a and return nothing.
    #[test]
    fn a_root_rotation_after_the_host_element_read_cannot_truncate_a_page() {
        for supplied_transaction in [false, true] {
            let db = count_host(&[b"a", b"b"]);
            let writer = db.clone();
            test_hooks::AFTER_HOST_ELEMENT.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    writer
                        .insert(
                            &[b"host".as_slice()],
                            b"c",
                            Element::new_item(b"c".to_vec()),
                            None,
                            None,
                            GroveVersion::latest(),
                        )
                        .unwrap()
                        .unwrap();
                }));
            });
            let transaction = db.start_transaction();
            let supplied = supplied_transaction.then_some(&transaction);
            assert_eq!(
                second_row(&db, supplied),
                vec![b"b".to_vec()],
                "supplied transaction: {supplied_transaction}"
            );
            // The commit landed, so the next page reads the rotated tree.
            assert_eq!(second_row(&db, None), vec![b"b".to_vec()]);
        }
    }

    #[test]
    fn a_page_sees_a_supplied_transactions_pending_writes() {
        let v = GroveVersion::latest();
        let db = count_host(&[b"a"]);
        let transaction = db.start_transaction();
        db.insert(
            &[b"host".as_slice()],
            b"b",
            Element::new_item(b"b".to_vec()),
            None,
            Some(&transaction),
            v,
        )
        .unwrap()
        .unwrap();
        assert_eq!(second_row(&db, Some(&transaction)), vec![b"b".to_vec()]);
        assert!(second_row(&db, None).is_empty(), "b is not committed");
    }

    #[test]
    fn present_non_merk_ancestors_are_not_empty_local_pages() {
        let v = GroveVersion::latest();
        for element in [Element::new_item(vec![1]), Element::empty_mmr_tree()] {
            let db = crate::tests::make_empty_grovedb();
            db.insert(&[] as &[&[u8]], b"blocker", element, None, None, v)
                .unwrap()
                .unwrap();
            let mut query = Query::new_range_full();
            query.offset = Some(1);
            let sized = SizedQuery::new(query, None, None);
            for strict in [false, true] {
                let mut budget = QueryBudget::new(None, None, None);
                let error = read_local_count_page(
                    &db.db,
                    &[b"blocker".as_slice(), b"doc"],
                    &sized,
                    1,
                    &mut budget,
                    QueryOptions {
                        allow_get_raw: true,
                        error_if_intermediate_path_tree_not_present: strict,
                        ..QueryOptions::default()
                    },
                    QueryResultType::QueryElementResultType,
                    None,
                    Element::path_query_push,
                    v,
                )
                .unwrap()
                .unwrap_err();
                assert!(matches!(error, Error::InvalidQuery(_)));
                assert_eq!(budget.consumed, 0);
            }
        }
    }
}
