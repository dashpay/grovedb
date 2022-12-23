#[cfg(feature = "full")]
use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostContext, CostResult, CostsExt,
    OperationCost,
};

#[cfg(any(feature = "full", feature = "verify"))]
use merk::proofs::{Query};
#[cfg(feature = "full")]
use merk::proofs::{query::QueryItem};
#[cfg(feature = "full")]
use storage::{rocksdb_storage::RocksDbStorage, RawIterator, StorageContext};

#[cfg(any(feature = "full", feature = "verify"))]
use crate::{Element, SizedQuery};

#[cfg(feature = "full")]
use crate::{
    element::helpers::raw_decode,
    query_result_type::{
        KeyElementPair, QueryResultElement, QueryResultElements, QueryResultType,
        QueryResultType::QueryElementResultType,
    },
    util::{merk_optional_tx, storage_context_optional_tx},
    Error, PathQuery, TransactionArg,
};

#[cfg(feature = "full")]
pub struct PathQueryPushArgs<'db, 'ctx, 'a>
where
    'db: 'ctx,
{
    pub storage: &'db RocksDbStorage,
    pub transaction: TransactionArg<'db, 'ctx>,
    pub key: Option<&'a [u8]>,
    pub element: Element,
    pub path: &'a [&'a [u8]],
    pub subquery_key: Option<Vec<u8>>,
    pub subquery: Option<Query>,
    pub left_to_right: bool,
    pub allow_get_raw: bool,
    pub result_type: QueryResultType,
    pub results: &'a mut Vec<QueryResultElement>,
    pub limit: &'a mut Option<u16>,
    pub offset: &'a mut Option<u16>,
}

impl Element {
    #[cfg(feature = "full")]
    pub fn get_query(
        storage: &RocksDbStorage,
        merk_path: &[&[u8]],
        query: &Query,
        result_type: QueryResultType,
        transaction: TransactionArg,
    ) -> CostResult<QueryResultElements, Error> {
        let sized_query = SizedQuery::new(query.clone(), None, None);
        Element::get_sized_query(storage, merk_path, &sized_query, result_type, transaction)
            .map_ok(|(elements, _)| elements)
    }

    #[cfg(feature = "full")]
    pub fn get_query_values(
        storage: &RocksDbStorage,
        merk_path: &[&[u8]],
        query: &Query,
        transaction: TransactionArg,
    ) -> CostResult<Vec<Element>, Error> {
        Element::get_query(
            storage,
            merk_path,
            query,
            QueryElementResultType,
            transaction,
        )
        .flat_map_ok(|result_items| {
            let elements: Vec<Element> = result_items
                .elements
                .into_iter()
                .filter_map(|result_item| match result_item {
                    QueryResultElement::ElementResultItem(element) => Some(element),
                    QueryResultElement::KeyElementPairResultItem(_) => None,
                    QueryResultElement::PathKeyElementTrioResultItem(_) => None,
                })
                .collect();
            Ok(elements).wrap_with_cost(OperationCost::default())
        })
    }

    #[cfg(feature = "full")]
    pub fn get_query_apply_function(
        storage: &RocksDbStorage,
        path: &[&[u8]],
        sized_query: &SizedQuery,
        allow_get_raw: bool,
        result_type: QueryResultType,
        transaction: TransactionArg,
        add_element_function: fn(PathQueryPushArgs) -> CostResult<(), Error>,
    ) -> CostResult<(QueryResultElements, u16), Error> {
        let mut cost = OperationCost::default();

        let mut results = Vec::new();

        let mut limit = sized_query.limit;
        let original_offset = sized_query.offset;
        let mut offset = original_offset;

        if sized_query.query.left_to_right {
            for item in sized_query.query.iter() {
                cost_return_on_error!(
                    &mut cost,
                    Self::query_item(
                        storage,
                        item,
                        &mut results,
                        path,
                        sized_query,
                        transaction,
                        &mut limit,
                        &mut offset,
                        allow_get_raw,
                        result_type,
                        add_element_function,
                    )
                );
                if limit == Some(0) {
                    break;
                }
            }
        } else {
            for item in sized_query.query.rev_iter() {
                cost_return_on_error!(
                    &mut cost,
                    Self::query_item(
                        storage,
                        item,
                        &mut results,
                        path,
                        sized_query,
                        transaction,
                        &mut limit,
                        &mut offset,
                        allow_get_raw,
                        result_type,
                        add_element_function,
                    )
                );
                if limit == Some(0) {
                    break;
                }
            }
        }

        let skipped = if let Some(original_offset_unwrapped) = original_offset {
            original_offset_unwrapped - offset.unwrap()
        } else {
            0
        };
        Ok((QueryResultElements::from_elements(results), skipped)).wrap_with_cost(cost)
    }

    #[cfg(feature = "full")]
    // Returns a vector of elements excluding trees, and the number of skipped
    // elements
    pub fn get_path_query(
        storage: &RocksDbStorage,
        path_query: &PathQuery,
        result_type: QueryResultType,
        transaction: TransactionArg,
    ) -> CostResult<(QueryResultElements, u16), Error> {
        let path_slices = path_query
            .path
            .iter()
            .map(|x| x.as_slice())
            .collect::<Vec<_>>();
        Element::get_query_apply_function(
            storage,
            path_slices.as_slice(),
            &path_query.query,
            false,
            result_type,
            transaction,
            Element::path_query_push,
        )
    }

    #[cfg(feature = "full")]
    // Returns a vector of elements including trees, and the number of skipped
    // elements
    pub fn get_raw_path_query(
        storage: &RocksDbStorage,
        path_query: &PathQuery,
        result_type: QueryResultType,
        transaction: TransactionArg,
    ) -> CostResult<(QueryResultElements, u16), Error> {
        let path_slices = path_query
            .path
            .iter()
            .map(|x| x.as_slice())
            .collect::<Vec<_>>();
        Element::get_query_apply_function(
            storage,
            path_slices.as_slice(),
            &path_query.query,
            true,
            result_type,
            transaction,
            Element::path_query_push,
        )
    }

    #[cfg(feature = "full")]
    /// Returns a vector of elements, and the number of skipped elements
    pub fn get_sized_query(
        storage: &RocksDbStorage,
        path: &[&[u8]],
        sized_query: &SizedQuery,
        result_type: QueryResultType,
        transaction: TransactionArg,
    ) -> CostResult<(QueryResultElements, u16), Error> {
        Element::get_query_apply_function(
            storage,
            path,
            sized_query,
            false,
            result_type,
            transaction,
            Element::path_query_push,
        )
    }

    #[cfg(feature = "full")]
    fn path_query_push(args: PathQueryPushArgs) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        let PathQueryPushArgs {
            storage,
            transaction,
            key,
            element,
            path,
            subquery_key,
            subquery,
            left_to_right,
            allow_get_raw,
            result_type,
            results,
            limit,
            offset,
        } = args;
        if element.is_tree() {
            let mut path_vec = path.to_vec();
            let key = cost_return_on_error_no_add!(
                &cost,
                key.ok_or(Error::MissingParameter(
                    "the key must be provided when using a subquery key",
                ))
            );
            path_vec.push(key);

            if let Some(subquery) = subquery {
                if let Some(subquery_key) = &subquery_key {
                    path_vec.push(subquery_key.as_slice());
                }

                let inner_query = SizedQuery::new(subquery, *limit, *offset);
                let path_vec_owned = path_vec.iter().map(|x| x.to_vec()).collect();
                let inner_path_query = PathQuery::new(path_vec_owned, inner_query);

                let (mut sub_elements, skipped) = cost_return_on_error!(
                    &mut cost,
                    Element::get_path_query(storage, &inner_path_query, result_type, transaction)
                );

                if let Some(limit) = limit {
                    *limit -= sub_elements.len() as u16;
                }
                if let Some(offset) = offset {
                    *offset -= skipped;
                }
                results.append(&mut sub_elements.elements);
            } else if let Some(subquery_key) = subquery_key {
                if offset.unwrap_or(0) == 0 {
                    match result_type {
                        QueryResultType::QueryElementResultType => {
                            merk_optional_tx!(
                                &mut cost,
                                storage,
                                path_vec.iter().copied(),
                                transaction,
                                subtree,
                                {
                                    results.push(QueryResultElement::ElementResultItem(
                                        cost_return_on_error!(
                                            &mut cost,
                                            Element::get_with_absolute_refs(
                                                &subtree,
                                                path_vec.as_slice(),
                                                subquery_key.as_slice()
                                            )
                                        ),
                                    ));
                                }
                            );
                        }
                        QueryResultType::QueryKeyElementPairResultType => {
                            merk_optional_tx!(
                                &mut cost,
                                storage,
                                path_vec.iter().copied(),
                                transaction,
                                subtree,
                                {
                                    results.push(QueryResultElement::KeyElementPairResultItem((
                                        subquery_key.clone(),
                                        cost_return_on_error!(
                                            &mut cost,
                                            Element::get_with_absolute_refs(
                                                &subtree,
                                                path_vec.as_slice(),
                                                subquery_key.as_slice()
                                            )
                                        ),
                                    )));
                                }
                            );
                        }
                        QueryResultType::QueryPathKeyElementTrioResultType => {
                            let original_path_vec = path.iter().map(|a| a.to_vec()).collect();
                            merk_optional_tx!(
                                &mut cost,
                                storage,
                                path_vec.iter().copied(),
                                transaction,
                                subtree,
                                {
                                    results.push(QueryResultElement::PathKeyElementTrioResultItem(
                                        (
                                            original_path_vec,
                                            subquery_key.clone(),
                                            cost_return_on_error!(
                                                &mut cost,
                                                Element::get_with_absolute_refs(
                                                    &subtree,
                                                    path_vec.as_slice(),
                                                    subquery_key.as_slice()
                                                )
                                            ),
                                        ),
                                    ));
                                }
                            );
                        }
                    }
                    if let Some(limit) = limit {
                        *limit -= 1;
                    }
                } else if let Some(offset) = offset {
                    *offset -= 1;
                }
            } else if allow_get_raw {
                cost_return_on_error_no_add!(
                    &cost,
                    Element::basic_push(PathQueryPushArgs {
                        storage,
                        transaction,
                        key: Some(key),
                        element,
                        path,
                        subquery_key,
                        subquery,
                        left_to_right,
                        allow_get_raw,
                        result_type,
                        results,
                        limit,
                        offset,
                    })
                );
            } else {
                return Err(Error::InvalidPath(
                    "you must provide a subquery or a subquery_key when interacting with a Tree \
                     of trees"
                        .to_owned(),
                ))
                .wrap_with_cost(cost);
            }
        } else {
            cost_return_on_error_no_add!(
                &cost,
                Element::basic_push(PathQueryPushArgs {
                    storage,
                    transaction,
                    key,
                    element,
                    path,
                    subquery_key,
                    subquery,
                    left_to_right,
                    allow_get_raw,
                    result_type,
                    results,
                    limit,
                    offset,
                })
            );
        }
        Ok(()).wrap_with_cost(cost)
    }

    #[cfg(any(feature = "full", feature = "verify"))]
    pub fn subquery_paths_for_sized_query(
        sized_query: &SizedQuery,
        key: &[u8],
    ) -> (Option<Vec<u8>>, Option<Query>) {
        for (query_item, subquery_branch) in &sized_query.query.conditional_subquery_branches {
            if query_item.contains(key) {
                let subquery_key = subquery_branch.subquery_key.clone();
                let subquery = subquery_branch
                    .subquery
                    .as_ref()
                    .map(|query| *query.clone());
                return (subquery_key, subquery);
            }
        }
        let subquery_key = sized_query
            .query
            .default_subquery_branch
            .subquery_key
            .clone();
        let subquery = sized_query
            .query
            .default_subquery_branch
            .subquery
            .as_ref()
            .map(|query| *query.clone());
        (subquery_key, subquery)
    }

    #[cfg(feature = "full")]
    // TODO: refactor
    #[allow(clippy::too_many_arguments)]
    fn query_item(
        storage: &RocksDbStorage,
        item: &QueryItem,
        results: &mut Vec<QueryResultElement>,
        path: &[&[u8]],
        sized_query: &SizedQuery,
        transaction: TransactionArg,
        limit: &mut Option<u16>,
        offset: &mut Option<u16>,
        allow_get_raw: bool,
        result_type: QueryResultType,
        add_element_function: fn(PathQueryPushArgs) -> CostResult<(), Error>,
    ) -> CostResult<(), Error> {
        let mut cost = OperationCost::default();

        if !item.is_range() {
            // this is a query on a key
            if let QueryItem::Key(key) = item {
                let element_res = merk_optional_tx!(
                    &mut cost,
                    storage,
                    path.iter().copied(),
                    transaction,
                    subtree,
                    { Element::get(&subtree, key).unwrap_add_cost(&mut cost) }
                );
                match element_res {
                    Ok(element) => {
                        let (subquery_key, subquery) =
                            Self::subquery_paths_for_sized_query(sized_query, key);
                        add_element_function(PathQueryPushArgs {
                            storage,
                            transaction,
                            key: Some(key.as_slice()),
                            element,
                            path,
                            subquery_key,
                            subquery,
                            left_to_right: sized_query.query.left_to_right,
                            allow_get_raw,
                            result_type,
                            results,
                            limit,
                            offset,
                        })
                        .unwrap_add_cost(&mut cost)
                    }
                    Err(Error::PathKeyNotFound(_)) => Ok(()),
                    Err(e) => Err(e),
                }
            } else {
                Err(Error::InternalError(
                    "QueryItem must be a Key if not a range",
                ))
            }
        } else {
            // this is a query on a range
            storage_context_optional_tx!(storage, path.iter().copied(), transaction, ctx, {
                let ctx = ctx.unwrap_add_cost(&mut cost);
                let mut iter = ctx.raw_iter();

                item.seek_for_iter(&mut iter, sized_query.query.left_to_right)
                    .unwrap_add_cost(&mut cost);

                while item
                    .iter_is_valid_for_type(&iter, *limit, sized_query.query.left_to_right)
                    .unwrap_add_cost(&mut cost)
                {
                    let element = cost_return_on_error_no_add!(
                        &cost,
                        raw_decode(
                            iter.value()
                                .unwrap_add_cost(&mut cost)
                                .expect("if key exists then value should too")
                        )
                    );
                    let key = iter
                        .key()
                        .unwrap_add_cost(&mut cost)
                        .expect("key should exist");
                    let (subquery_key, subquery) =
                        Self::subquery_paths_for_sized_query(sized_query, key);
                    cost_return_on_error!(
                        &mut cost,
                        add_element_function(PathQueryPushArgs {
                            storage,
                            transaction,
                            key: Some(key),
                            element,
                            path,
                            subquery_key,
                            subquery,
                            left_to_right: sized_query.query.left_to_right,
                            allow_get_raw,
                            result_type,
                            results,
                            limit,
                            offset,
                        })
                    );
                    if sized_query.query.left_to_right {
                        iter.next().unwrap_add_cost(&mut cost);
                    } else {
                        iter.prev().unwrap_add_cost(&mut cost);
                    }
                    cost.seek_count += 1;
                }
                Ok(())
            })
        }
        .wrap_with_cost(cost)
    }

    #[cfg(feature = "full")]
    fn basic_push(args: PathQueryPushArgs) -> Result<(), Error> {
        let PathQueryPushArgs {
            path,
            key,
            element,
            result_type,
            results,
            limit,
            offset,
            ..
        } = args;

        let element = element.convert_if_reference_to_absolute_reference(path, key)?;

        if offset.unwrap_or(0) == 0 {
            match result_type {
                QueryResultType::QueryElementResultType => {
                    results.push(QueryResultElement::ElementResultItem(element));
                }
                QueryResultType::QueryKeyElementPairResultType => {
                    let key = key.ok_or(Error::CorruptedPath("basic push must have a key"))?;
                    results.push(QueryResultElement::KeyElementPairResultItem((
                        Vec::from(key),
                        element,
                    )));
                }
                QueryResultType::QueryPathKeyElementTrioResultType => {
                    let key = key.ok_or(Error::CorruptedPath("basic push must have a key"))?;
                    let path = path.iter().map(|a| a.to_vec()).collect();
                    results.push(QueryResultElement::PathKeyElementTrioResultItem((
                        path,
                        Vec::from(key),
                        element,
                    )));
                }
            }
            if let Some(limit) = limit {
                *limit -= 1;
            }
        } else if let Some(offset) = offset {
            *offset -= 1;
        }
        Ok(())
    }

    #[cfg(feature = "full")]
    pub fn iterator<I: RawIterator>(mut raw_iter: I) -> CostContext<ElementsIterator<I>> {
        let mut cost = OperationCost::default();
        raw_iter.seek_to_first().unwrap_add_cost(&mut cost);
        ElementsIterator::new(raw_iter).wrap_with_cost(cost)
    }
}

#[cfg(feature = "full")]
#[cfg(test)]
mod tests {
    use merk::{proofs::Query, Merk};
    use storage::rocksdb_storage::PrefixedRocksDbStorageContext;

    use crate::{
        element::*,
        query_result_type::{
            KeyElementPair, QueryResultElement, QueryResultElements,
            QueryResultType::{QueryKeyElementPairResultType, QueryPathKeyElementTrioResultType},
        },
        tests::{make_test_grovedb, TEST_LEAF},
        SizedQuery,
    };

    #[test]
    fn test_get_query() {
        let db = make_test_grovedb();

        db.insert(
            [TEST_LEAF],
            b"d",
            Element::new_item(b"ayyd".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"c",
            Element::new_item(b"ayyc".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"a",
            Element::new_item(b"ayya".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"b",
            Element::new_item(b"ayyb".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");

        // Test queries by key
        let mut query = Query::new();
        query.insert_key(b"c".to_vec());
        query.insert_key(b"a".to_vec());

        assert_eq!(
            Element::get_query_values(&db.db, &[TEST_LEAF], &query, None)
                .unwrap()
                .expect("expected successful get_query"),
            vec![
                Element::new_item(b"ayya".to_vec()),
                Element::new_item(b"ayyc".to_vec())
            ]
        );

        // Test range query
        let mut query = Query::new();
        query.insert_range(b"b".to_vec()..b"d".to_vec());
        query.insert_range(b"a".to_vec()..b"c".to_vec());
        assert_eq!(
            Element::get_query_values(&db.db, &[TEST_LEAF], &query, None)
                .unwrap()
                .expect("expected successful get_query"),
            vec![
                Element::new_item(b"ayya".to_vec()),
                Element::new_item(b"ayyb".to_vec()),
                Element::new_item(b"ayyc".to_vec())
            ]
        );

        // Test range inclusive query
        let mut query = Query::new();
        query.insert_range_inclusive(b"b".to_vec()..=b"d".to_vec());
        query.insert_range(b"b".to_vec()..b"c".to_vec());
        assert_eq!(
            Element::get_query_values(&db.db, &[TEST_LEAF], &query, None)
                .unwrap()
                .expect("expected successful get_query"),
            vec![
                Element::new_item(b"ayyb".to_vec()),
                Element::new_item(b"ayyc".to_vec()),
                Element::new_item(b"ayyd".to_vec())
            ]
        );

        // Test overlaps
        let mut query = Query::new();
        query.insert_key(b"a".to_vec());
        query.insert_range(b"b".to_vec()..b"d".to_vec());
        query.insert_range(b"a".to_vec()..b"c".to_vec());
        assert_eq!(
            Element::get_query_values(&db.db, &[TEST_LEAF], &query, None)
                .unwrap()
                .expect("expected successful get_query"),
            vec![
                Element::new_item(b"ayya".to_vec()),
                Element::new_item(b"ayyb".to_vec()),
                Element::new_item(b"ayyc".to_vec())
            ]
        );
    }

    #[test]
    fn test_get_query_with_path() {
        let db = make_test_grovedb();

        db.insert(
            [TEST_LEAF],
            b"d",
            Element::new_item(b"ayyd".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"c",
            Element::new_item(b"ayyc".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"a",
            Element::new_item(b"ayya".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"b",
            Element::new_item(b"ayyb".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");

        // Test queries by key
        let mut query = Query::new();
        query.insert_key(b"c".to_vec());
        query.insert_key(b"a".to_vec());
        assert_eq!(
            Element::get_query(
                &db.db,
                &[TEST_LEAF],
                &query,
                QueryPathKeyElementTrioResultType,
                None
            )
            .unwrap()
            .expect("expected successful get_query")
            .to_path_key_elements(),
            vec![
                (
                    vec![TEST_LEAF.to_vec()],
                    b"a".to_vec(),
                    Element::new_item(b"ayya".to_vec())
                ),
                (
                    vec![TEST_LEAF.to_vec()],
                    b"c".to_vec(),
                    Element::new_item(b"ayyc".to_vec())
                )
            ]
        );
    }

    #[test]
    fn test_get_range_query() {
        let db = make_test_grovedb();

        let storage = &db.db;
        let mut merk = db
            .open_non_transactional_merk_at_path([TEST_LEAF])
            .unwrap()
            .expect("cannot open Merk"); // TODO implement costs

        Element::new_item(b"ayyd".to_vec())
            .insert(&mut merk, b"d", None)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyc".to_vec())
            .insert(&mut merk, b"c", None)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayya".to_vec())
            .insert(&mut merk, b"a", None)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyb".to_vec())
            .insert(&mut merk, b"b", None)
            .unwrap()
            .expect("expected successful insertion");

        // Test range inclusive query
        let mut query = Query::new();
        query.insert_range(b"a".to_vec()..b"d".to_vec());

        let ascending_query = SizedQuery::new(query.clone(), None, None);
        let (elements, skipped) = Element::get_sized_query(
            &storage,
            &[TEST_LEAF],
            &ascending_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");

        let elements: Vec<KeyElementPair> = elements
            .into_iterator()
            .filter_map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(_element) => None,
                QueryResultElement::KeyElementPairResultItem(key_element_pair) => {
                    Some(key_element_pair)
                }
                QueryResultElement::PathKeyElementTrioResultItem(_) => None,
            })
            .collect();
        assert_eq!(
            elements,
            vec![
                (b"a".to_vec(), Element::new_item(b"ayya".to_vec())),
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec())),
                (b"c".to_vec(), Element::new_item(b"ayyc".to_vec())),
            ]
        );
        assert_eq!(skipped, 0);

        query.left_to_right = false;

        let backwards_query = SizedQuery::new(query.clone(), None, None);
        let (elements, skipped) = Element::get_sized_query(
            &storage,
            &[TEST_LEAF],
            &backwards_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");

        let elements: Vec<KeyElementPair> = elements
            .into_iterator()
            .filter_map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(_element) => None,
                QueryResultElement::KeyElementPairResultItem(key_element_pair) => {
                    Some(key_element_pair)
                }
                QueryResultElement::PathKeyElementTrioResultItem(_) => None,
            })
            .collect();
        assert_eq!(
            elements,
            vec![
                (b"c".to_vec(), Element::new_item(b"ayyc".to_vec())),
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec())),
                (b"a".to_vec(), Element::new_item(b"ayya".to_vec())),
            ]
        );
        assert_eq!(skipped, 0);
    }

    #[test]
    fn test_get_range_inclusive_query() {
        let db = make_test_grovedb();

        let storage = &db.db;
        let mut merk: Merk<PrefixedRocksDbStorageContext> = db
            .open_non_transactional_merk_at_path([TEST_LEAF])
            .unwrap()
            .expect("cannot open Merk");

        Element::new_item(b"ayyd".to_vec())
            .insert(&mut merk, b"d", None)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyc".to_vec())
            .insert(&mut merk, b"c", None)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayya".to_vec())
            .insert(&mut merk, b"a", None)
            .unwrap()
            .expect("expected successful insertion");
        Element::new_item(b"ayyb".to_vec())
            .insert(&mut merk, b"b", None)
            .unwrap()
            .expect("expected successful insertion");

        // Test range inclusive query
        let mut query = Query::new_with_direction(true);
        query.insert_range_inclusive(b"a".to_vec()..=b"d".to_vec());

        let ascending_query = SizedQuery::new(query.clone(), None, None);
        fn check_elements_no_skipped(
            (elements, skipped): (QueryResultElements, u16),
            reverse: bool,
        ) {
            let mut expected = vec![
                (b"a".to_vec(), Element::new_item(b"ayya".to_vec())),
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec())),
                (b"c".to_vec(), Element::new_item(b"ayyc".to_vec())),
                (b"d".to_vec(), Element::new_item(b"ayyd".to_vec())),
            ];
            if reverse {
                expected.reverse();
            }
            assert_eq!(elements.to_key_elements(), expected);
            assert_eq!(skipped, 0);
        }

        check_elements_no_skipped(
            Element::get_sized_query(
                &storage,
                &[TEST_LEAF],
                &ascending_query,
                QueryKeyElementPairResultType,
                None,
            )
            .unwrap()
            .expect("expected successful get_query"),
            false,
        );

        query.left_to_right = false;

        let backwards_query = SizedQuery::new(query.clone(), None, None);
        check_elements_no_skipped(
            Element::get_sized_query(
                &storage,
                &[TEST_LEAF],
                &backwards_query,
                QueryKeyElementPairResultType,
                None,
            )
            .unwrap()
            .expect("expected successful get_query"),
            true,
        );

        // Test range inclusive query
        let mut query = Query::new_with_direction(false);
        query.insert_range_inclusive(b"b".to_vec()..=b"d".to_vec());
        query.insert_range(b"a".to_vec()..b"c".to_vec());

        let backwards_query = SizedQuery::new(query.clone(), None, None);
        check_elements_no_skipped(
            Element::get_sized_query(
                &storage,
                &[TEST_LEAF],
                &backwards_query,
                QueryKeyElementPairResultType,
                None,
            )
            .unwrap()
            .expect("expected successful get_query"),
            true,
        );
    }

    #[test]
    fn test_get_limit_query() {
        let db = make_test_grovedb();

        db.insert(
            [TEST_LEAF],
            b"d",
            Element::new_item(b"ayyd".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"c",
            Element::new_item(b"ayyc".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"a",
            Element::new_item(b"ayya".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");
        db.insert(
            [TEST_LEAF],
            b"b",
            Element::new_item(b"ayyb".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("cannot insert element");

        // Test queries by key
        let mut query = Query::new_with_direction(true);
        query.insert_key(b"c".to_vec());
        query.insert_key(b"a".to_vec());

        // since these are just keys a backwards query will keep same order
        let backwards_query = SizedQuery::new(query.clone(), None, None);
        let (elements, skipped) = Element::get_sized_query(
            &db.db,
            &[TEST_LEAF],
            &backwards_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![
                (b"a".to_vec(), Element::new_item(b"ayya".to_vec())),
                (b"c".to_vec(), Element::new_item(b"ayyc".to_vec())),
            ]
        );
        assert_eq!(skipped, 0);

        // Test queries by key
        let mut query = Query::new_with_direction(false);
        query.insert_key(b"c".to_vec());
        query.insert_key(b"a".to_vec());

        // since these are just keys a backwards query will keep same order
        let backwards_query = SizedQuery::new(query.clone(), None, None);
        let (elements, skipped) = Element::get_sized_query(
            &db.db,
            &[TEST_LEAF],
            &backwards_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![
                (b"c".to_vec(), Element::new_item(b"ayyc".to_vec())),
                (b"a".to_vec(), Element::new_item(b"ayya".to_vec())),
            ]
        );
        assert_eq!(skipped, 0);

        // The limit will mean we will only get back 1 item
        let limit_query = SizedQuery::new(query.clone(), Some(1), None);
        let (elements, skipped) = Element::get_sized_query(
            &db.db,
            &[TEST_LEAF],
            &limit_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![(b"c".to_vec(), Element::new_item(b"ayyc".to_vec())),]
        );
        assert_eq!(skipped, 0);

        // Test range query
        let mut query = Query::new_with_direction(true);
        query.insert_range(b"b".to_vec()..b"d".to_vec());
        query.insert_range(b"a".to_vec()..b"c".to_vec());
        let limit_query = SizedQuery::new(query.clone(), Some(2), None);
        let (elements, skipped) = Element::get_sized_query(
            &db.db,
            &[TEST_LEAF],
            &limit_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![
                (b"a".to_vec(), Element::new_item(b"ayya".to_vec())),
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec()))
            ]
        );
        assert_eq!(skipped, 0);

        let limit_offset_query = SizedQuery::new(query.clone(), Some(2), Some(1));
        let (elements, skipped) = Element::get_sized_query(
            &db.db,
            &[TEST_LEAF],
            &limit_offset_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec())),
                (b"c".to_vec(), Element::new_item(b"ayyc".to_vec()))
            ]
        );
        assert_eq!(skipped, 1);

        // Test range query
        let mut query = Query::new_with_direction(false);
        query.insert_range(b"b".to_vec()..b"d".to_vec());
        query.insert_range(b"a".to_vec()..b"c".to_vec());

        let limit_offset_backwards_query = SizedQuery::new(query.clone(), Some(2), Some(1));
        let (elements, skipped) = Element::get_sized_query(
            &db.db,
            &[TEST_LEAF],
            &limit_offset_backwards_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec())),
                (b"a".to_vec(), Element::new_item(b"ayya".to_vec()))
            ]
        );
        assert_eq!(skipped, 1);

        // Test range inclusive query
        let mut query = Query::new_with_direction(true);
        query.insert_range_inclusive(b"b".to_vec()..=b"d".to_vec());
        query.insert_range(b"b".to_vec()..b"c".to_vec());
        let limit_full_query = SizedQuery::new(query.clone(), Some(5), Some(0));
        let (elements, skipped) = Element::get_sized_query(
            &db.db,
            &[TEST_LEAF],
            &limit_full_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec())),
                (b"c".to_vec(), Element::new_item(b"ayyc".to_vec())),
                (b"d".to_vec(), Element::new_item(b"ayyd".to_vec())),
            ]
        );
        assert_eq!(skipped, 0);

        let mut query = Query::new_with_direction(false);
        query.insert_range_inclusive(b"b".to_vec()..=b"d".to_vec());
        query.insert_range(b"b".to_vec()..b"c".to_vec());

        let limit_offset_backwards_query = SizedQuery::new(query.clone(), Some(2), Some(1));
        let (elements, skipped) = Element::get_sized_query(
            &db.db,
            &[TEST_LEAF],
            &limit_offset_backwards_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![
                (b"c".to_vec(), Element::new_item(b"ayyc".to_vec())),
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec())),
            ]
        );
        assert_eq!(skipped, 1);

        // Test overlaps
        let mut query = Query::new_with_direction(false);
        query.insert_key(b"a".to_vec());
        query.insert_range(b"b".to_vec()..b"d".to_vec());
        query.insert_range(b"b".to_vec()..b"c".to_vec());
        let limit_backwards_query = SizedQuery::new(query.clone(), Some(2), Some(1));
        let (elements, skipped) = Element::get_sized_query(
            &db.db,
            &[TEST_LEAF],
            &limit_backwards_query,
            QueryKeyElementPairResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_query");
        assert_eq!(
            elements.to_key_elements(),
            vec![
                (b"b".to_vec(), Element::new_item(b"ayyb".to_vec())),
                (b"a".to_vec(), Element::new_item(b"ayya".to_vec())),
            ]
        );
        assert_eq!(skipped, 1);
    }
}

#[cfg(feature = "full")]
pub struct ElementsIterator<I: RawIterator> {
    raw_iter: I,
}

#[cfg(feature = "full")]
impl<I: RawIterator> ElementsIterator<I> {
    pub fn new(raw_iter: I) -> Self {
        ElementsIterator { raw_iter }
    }

    pub fn next_element(&mut self) -> CostResult<Option<KeyElementPair>, Error> {
        let mut cost = OperationCost::default();

        Ok(if self.raw_iter.valid().unwrap_add_cost(&mut cost) {
            if let Some((key, value)) = self
                .raw_iter
                .key()
                .unwrap_add_cost(&mut cost)
                .zip(self.raw_iter.value().unwrap_add_cost(&mut cost))
            {
                let element = cost_return_on_error_no_add!(&cost, raw_decode(value));
                let key_vec = key.to_vec();
                self.raw_iter.next().unwrap_add_cost(&mut cost);
                Some((key_vec, element))
            } else {
                None
            }
        } else {
            None
        })
        .wrap_with_cost(cost)
    }

    pub fn fast_forward(&mut self, key: &[u8]) -> Result<(), Error> {
        while self.raw_iter.valid().unwrap() {
            if self.raw_iter.key().unwrap().unwrap() == key {
                break;
            } else {
                self.raw_iter.next().unwrap();
            }
        }
        Ok(())
    }
}
