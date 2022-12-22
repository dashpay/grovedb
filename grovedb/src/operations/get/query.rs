#[cfg(feature = "full")]
use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
#[cfg(feature = "full")]
use integer_encoding::VarInt;
use merk::proofs::query::QueryItem;

use crate::query_result_type::KeyOptionalElementPair;
#[cfg(feature = "full")]
use crate::{
    query_result_type::{QueryResultElement, QueryResultElements, QueryResultType},
    reference_path::ReferencePathType,
    Element, Error, GroveDb, PathQuery, TransactionArg,
};

#[cfg(feature = "full")]
impl GroveDb {
    pub fn query_many(
        &self,
        path_queries: &[&PathQuery],
        transaction: TransactionArg,
    ) -> CostResult<Vec<Vec<u8>>, Error> {
        let mut cost = OperationCost::default();

        let elements = cost_return_on_error!(
            &mut cost,
            self.query_many_raw(
                path_queries,
                QueryResultType::QueryElementResultType,
                transaction
            )
        );
        let results_wrapped = elements
            .into_iterator()
            .map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(Element::Reference(reference_path, ..)) => {
                    match reference_path {
                        ReferencePathType::AbsolutePathReference(absolute_path) => {
                            // While `map` on iterator is lazy, we should accumulate costs even if
                            // `collect` will end in `Err`, so we'll use
                            // external costs accumulator instead of
                            // returning costs from `map` call.
                            let maybe_item = self
                                .follow_reference(absolute_path, transaction)
                                .unwrap_add_cost(&mut cost)?;

                            match maybe_item {
                                Element::Item(item, _) => Ok(item),
                                Element::SumItem(value, _) => Ok(value.encode_var_vec()),
                                _ => {
                                    Err(Error::InvalidQuery("the reference must result in an item"))
                                }
                            }
                        }
                        _ => Err(Error::CorruptedCodeExecution(
                            "reference after query must have absolute paths",
                        )),
                    }
                }
                _ => Err(Error::InvalidQuery(
                    "path_queries can only refer to references",
                )),
            })
            .collect::<Result<Vec<Vec<u8>>, Error>>();

        results_wrapped.wrap_with_cost(cost)
    }

    pub fn query_many_raw(
        &self,
        path_queries: &[&PathQuery],
        result_type: QueryResultType,
        transaction: TransactionArg,
    ) -> CostResult<QueryResultElements, Error>
where {
        let mut cost = OperationCost::default();

        let query = cost_return_on_error!(&mut cost, PathQuery::merge(path_queries.to_vec()));
        let (result, _) =
            cost_return_on_error!(&mut cost, self.query_raw(&query, result_type, transaction));
        Ok(result).wrap_with_cost(cost)
    }

    pub fn get_proved_path_query(
        &self,
        path_query: &PathQuery,
        transaction: TransactionArg,
    ) -> CostResult<Vec<u8>, Error> {
        if transaction.is_some() {
            Err(Error::NotSupported(
                "transactions are not currently supported",
            ))
            .wrap_with_cost(Default::default())
        } else {
            self.prove_query(path_query)
        }
    }

    pub fn query(
        &self,
        path_query: &PathQuery,
        transaction: TransactionArg,
    ) -> CostResult<(Vec<Vec<u8>>, u16), Error> {
        let mut cost = OperationCost::default();

        let (elements, skipped) = cost_return_on_error!(
            &mut cost,
            self.query_raw(
                path_query,
                QueryResultType::QueryElementResultType,
                transaction
            )
        );

        let results_wrapped = elements
            .into_iterator()
            .map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(element) => {
                    match element {
                        Element::Reference(reference_path, ..) => {
                            match reference_path {
                                ReferencePathType::AbsolutePathReference(absolute_path) => {
                                    // While `map` on iterator is lazy, we should accumulate costs
                                    // even if `collect` will
                                    // end in `Err`, so we'll use
                                    // external costs accumulator instead of
                                    // returning costs from `map` call.
                                    let maybe_item = self
                                        .follow_reference(absolute_path, transaction)
                                        .unwrap_add_cost(&mut cost)?;

                                    match maybe_item {
                                        Element::Item(item, _) => Ok(item),
                                        Element::SumItem(item, _) => Ok(item.encode_var_vec()),
                                        _ => Err(Error::InvalidQuery(
                                            "the reference must result in an item",
                                        )),
                                    }
                                }
                                _ => Err(Error::CorruptedCodeExecution(
                                    "reference after query must have absolute paths",
                                )),
                            }
                        }
                        Element::Item(item, _) => Ok(item),
                        Element::SumItem(item, _) => Ok(item.encode_var_vec()),
                        Element::Tree(..) | Element::SumTree(..) => Err(Error::InvalidQuery(
                            "path_queries can only refer to items and references",
                        )),
                    }
                }
                _ => Err(Error::CorruptedCodeExecution(
                    "query returned incorrect result type",
                )),
            })
            .collect::<Result<Vec<Vec<u8>>, Error>>();

        let results = cost_return_on_error_no_add!(&cost, results_wrapped);
        Ok((results, skipped)).wrap_with_cost(cost)
    }

    pub fn query_sums(
        &self,
        path_query: &PathQuery,
        transaction: TransactionArg,
    ) -> CostResult<(Vec<i64>, u16), Error> {
        let mut cost = OperationCost::default();

        let (elements, skipped) = cost_return_on_error!(
            &mut cost,
            self.query_raw(
                path_query,
                QueryResultType::QueryElementResultType,
                transaction
            )
        );

        let results_wrapped = elements
            .into_iterator()
            .map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(element) => {
                    match element {
                        Element::Reference(reference_path, ..) => {
                            match reference_path {
                                ReferencePathType::AbsolutePathReference(absolute_path) => {
                                    // While `map` on iterator is lazy, we should accumulate costs
                                    // even if `collect` will
                                    // end in `Err`, so we'll use
                                    // external costs accumulator instead of
                                    // returning costs from `map` call.
                                    let maybe_item = self
                                        .follow_reference(absolute_path, transaction)
                                        .unwrap_add_cost(&mut cost)?;

                                    if let Element::SumItem(item, _) = maybe_item {
                                        Ok(item)
                                    } else {
                                        Err(Error::InvalidQuery(
                                            "the reference must result in a sum item",
                                        ))
                                    }
                                }
                                _ => Err(Error::CorruptedCodeExecution(
                                    "reference after query must have absolute paths",
                                )),
                            }
                        }
                        Element::SumItem(item, _) => Ok(item),
                        Element::Tree(..) | Element::SumTree(..) | Element::Item(..) => {
                            Err(Error::InvalidQuery(
                                "path_queries over sum items can only refer to sum items and \
                                 references",
                            ))
                        }
                    }
                }
                _ => Err(Error::CorruptedCodeExecution(
                    "query returned incorrect result type",
                )),
            })
            .collect::<Result<Vec<i64>, Error>>();

        let results = cost_return_on_error_no_add!(&cost, results_wrapped);
        Ok((results, skipped)).wrap_with_cost(cost)
    }

    pub fn query_raw(
        &self,
        path_query: &PathQuery,
        result_type: QueryResultType,
        transaction: TransactionArg,
    ) -> CostResult<(QueryResultElements, u16), Error> {
        Element::get_raw_path_query(&self.db, path_query, result_type, transaction)
    }

    pub fn query_raw_keys_optional(
        &self,
        path_query: &PathQuery,
        transaction: TransactionArg,
    ) -> CostResult<Vec<KeyOptionalElementPair>, Error> {
        if path_query.query.query.has_subquery() {
            return Err(Error::NotSupported(
                "subqueries are not supported in query_raw_keys_optional",
            ))
            .wrap_with_cost(OperationCost::default());
        }
        if path_query.query.limit.is_some() {
            return Err(Error::NotSupported(
                "limits are not supported in query_raw_keys_optional",
            ))
            .wrap_with_cost(OperationCost::default());
        }
        if path_query.query.offset.is_some() {
            return Err(Error::NotSupported(
                "offsets are not supported in query_raw_keys_optional",
            ))
            .wrap_with_cost(OperationCost::default());
        }
        if path_query.query.query.has_only_keys() {
            return Err(Error::NotSupported(
                "ranges are not supported in query_raw_keys_optional",
            ))
            .wrap_with_cost(OperationCost::default());
        }
        let mut cost = OperationCost::default();

        let (elements, _) = cost_return_on_error!(
            &mut cost,
            self.query_raw(
                path_query,
                QueryResultType::QueryKeyElementPairResultType,
                transaction
            )
        );

        let mut elements_map = elements.to_key_elements_hash_map();

        Ok(path_query
            .query
            .query
            .items
            .iter()
            .filter_map(|item| {
                if let QueryItem::Key(key) = item {
                    Some((key.clone(), elements_map.remove(key)))
                } else {
                    None
                }
            })
            .collect())
        .wrap_with_cost(cost)
    }
}

#[cfg(feature = "full")]
#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use costs::{
        storage_cost::{removal::StorageRemovedBytes::NoStorageRemoval, StorageCost},
        OperationCost,
    };
    use merk::proofs::Query;
    use pretty_assertions::assert_eq;

    use crate::{
        operations::insert::InsertOptions,
        tests::{make_empty_grovedb, make_test_grovedb, TEST_LEAF},
        Element, Error, PathQuery, SizedQuery,
    };

    fn test_query_raw_keys_options() {
        let db = make_test_grovedb();

        db.insert(
            [TEST_LEAF],
            b"1",
            Element::new_item(b"hello".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF],
            b"2",
            Element::new_item(b"hello too".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("should insert subtree successfully");
        db.insert(
            [TEST_LEAF],
            b"5",
            Element::new_item(b"bye".to_vec()),
            None,
            None,
        )
        .unwrap()
        .expect("should insert subtree successfully");

        let mut query = Query::new();
        query.insert_key(b"1".to_vec());
        query.insert_key(b"2".to_vec());
        query.insert_key(b"3".to_vec());
        let path_query =
            PathQuery::new(vec![TEST_LEAF.to_vec()], SizedQuery::new(query, None, None));
        let raw_result = db
            .query_raw_keys_optional(&path_query, None)
            .unwrap()
            .expect("should get successfully");

        let raw_result: HashMap<_, _> = raw_result.into_iter().collect();

        assert_eq!(raw_result.len(), 3);
        assert_eq!(raw_result.get(b"4".to_vec().as_slice()), None);
        assert_eq!(raw_result.get(b"3".to_vec().as_slice()), Some(&None));
        assert_eq!(
            raw_result.get(b"5".to_vec().as_slice()),
            Some(&Some(Element::new_item(b"bye".to_vec())))
        )
    }
}
