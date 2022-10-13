use std::collections::HashSet;

use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use merk::Merk;
use storage::{
    rocksdb_storage::{
        PrefixedRocksDbStorageContext, PrefixedRocksDbTransactionContext, RocksDbStorage,
    },
    StorageContext,
};

use crate::{
    batch::{KeyInfo, KeyInfoPath},
    query_result_type::{QueryResultElement, QueryResultElements, QueryResultType},
    reference_path::{
        path_from_reference_path_type, path_from_reference_qualified_path_type, ReferencePathType,
    },
    util::{merk_optional_tx, root_merk_optional_tx, storage_context_optional_tx},
    Element, Error, GroveDb, PathQuery, Transaction, TransactionArg,
};

/// Limit of possible indirections
pub const MAX_REFERENCE_HOPS: usize = 10;

impl GroveDb {
    pub fn get<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        transaction: TransactionArg,
    ) -> CostResult<Element, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let path_iter = path.into_iter();

        match cost_return_on_error!(&mut cost, self.get_raw(path_iter.clone(), key, transaction)) {
            Element::Reference(reference_path, ..) => {
                let path = cost_return_on_error!(
                    &mut cost,
                    path_from_reference_path_type(reference_path, path_iter, Some(key))
                        .wrap_with_cost(OperationCost::default())
                );
                self.follow_reference(path, transaction).add_cost(cost)
            }
            other => Ok(other).wrap_with_cost(cost),
        }
    }

    pub fn follow_reference(
        &self,
        mut path: Vec<Vec<u8>>,
        transaction: TransactionArg,
    ) -> CostResult<Element, Error> {
        let mut cost = OperationCost::default();

        let mut hops_left = MAX_REFERENCE_HOPS;
        let mut current_element;
        let mut visited = HashSet::new();

        while hops_left > 0 {
            if visited.contains(&path) {
                return Err(Error::CyclicReference).wrap_with_cost(cost);
            }
            if let Some((key, path_slice)) = path.split_last() {
                current_element = cost_return_on_error!(
                    &mut cost,
                    self.get_raw(path_slice.iter().map(|x| x.as_slice()), key, transaction)
                        .map_err(|e| match e {
                            Error::PathKeyNotFound(p) => {
                                Error::CorruptedReferencePathKeyNotFound(p)
                            }
                            Error::PathNotFound(p) => {
                                Error::CorruptedReferencePathNotFound(p)
                            }
                            _ => e,
                        })
                )
            } else {
                return Err(Error::CorruptedPath("empty path")).wrap_with_cost(cost);
            }
            visited.insert(path.clone());
            match current_element {
                Element::Reference(reference_path, ..) => {
                    path = cost_return_on_error!(
                        &mut cost,
                        path_from_reference_qualified_path_type(reference_path, &path)
                            .wrap_with_cost(OperationCost::default())
                    )
                }
                other => return Ok(other).wrap_with_cost(cost),
            }
            hops_left -= 1;
        }
        Err(Error::ReferenceLimit).wrap_with_cost(cost)
    }

    /// Get tree item without following references
    pub fn get_raw<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        transaction: TransactionArg,
    ) -> CostResult<Element, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        if let Some(transaction) = transaction {
            self.get_raw_on_transaction(path, key, transaction)
        } else {
            self.get_raw_without_transaction(path, key)
        }
    }

    /// Get tree item without following references
    pub fn get_raw_on_transaction<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        transaction: &Transaction,
    ) -> CostResult<Element, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let mut path_iter = path.into_iter();
        let mut merk_to_get_from: Merk<PrefixedRocksDbTransactionContext> = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(path_iter.clone(), transaction)
        );

        Element::get(&merk_to_get_from, key).add_cost(cost)
    }

    /// Get tree item without following references
    pub fn get_raw_without_transaction<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
    ) -> CostResult<Element, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let mut path_iter = path.into_iter();
        let mut merk_to_get_from: Merk<PrefixedRocksDbStorageContext> = cost_return_on_error!(
            &mut cost,
            self.open_non_transactional_merk_at_path(path_iter.clone())
        );

        Element::get(&merk_to_get_from, key).add_cost(cost)
    }

    /// Does tree element exist without following references
    pub fn has_raw<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        transaction: TransactionArg,
    ) -> CostResult<bool, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let path_iter = path.into_iter();

        // Merk's items should be written into data storage_cost and checked accordingly
        storage_context_optional_tx!(self.db, path_iter, transaction, storage, {
            storage.flat_map(|s| s.get(key).map_err(|e| e.into()).map_ok(|x| x.is_some()))
        })
    }

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
            .into_iter()
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

                            if let Element::Item(item, _) = maybe_item {
                                Ok(item)
                            } else {
                                Err(Error::InvalidQuery("the reference must result in an item"))
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
            .into_iter()
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

                                    if let Element::Item(item, _) = maybe_item {
                                        Ok(item)
                                    } else {
                                        Err(Error::InvalidQuery(
                                            "the reference must result in an item",
                                        ))
                                    }
                                }
                                _ => Err(Error::CorruptedCodeExecution(
                                    "reference after query must have absolute paths",
                                )),
                            }
                        }
                        Element::Item(item, _) => Ok(item),
                        Element::Tree(..) => Err(Error::InvalidQuery(
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

    pub fn query_raw(
        &self,
        path_query: &PathQuery,
        result_type: QueryResultType,
        transaction: TransactionArg,
    ) -> CostResult<(QueryResultElements, u16), Error> {
        Element::get_raw_path_query(&self.db, path_query, result_type, transaction)
    }

    fn check_subtree_exists<'p, P>(
        &self,
        path: P,
        transaction: TransactionArg,
        error: Error,
    ) -> CostResult<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let path_iter = path.into_iter();
        if path_iter.len() == 0 {
            return Ok(()).wrap_with_cost(cost);
        }

        let mut parent_iter = path_iter;
        let parent_key = parent_iter.next_back().expect("path is not empty");
        let element =
                if let Some(transaction) = transaction {
            let mut merk_to_get_from: Merk<PrefixedRocksDbTransactionContext> = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(parent_iter, transaction)
        );

            Element::get(&merk_to_get_from, parent_key)
        } else {
            let mut merk_to_get_from: Merk<PrefixedRocksDbStorageContext> = cost_return_on_error!(
            &mut cost,
            self.open_non_transactional_merk_at_path(parent_iter)
        );

            Element::get(&merk_to_get_from, parent_key)
        }.unwrap_add_cost(&mut cost);
        match element {
            Ok(Element::Tree(..)) => { Ok(()).wrap_with_cost(cost) }
            Ok(_) | Err(Error::PathKeyNotFound(_)) => Err(error).wrap_with_cost(cost),
            Err(e) => Err(e).wrap_with_cost(cost),
        }
    }

    pub fn check_subtree_exists_path_not_found<'p, P>(
        &self,
        path: P,
        transaction: TransactionArg,
    ) -> CostResult<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        self.check_subtree_exists(
            path,
            transaction,
            Error::PathNotFound("subtree doesn't exist"),
        )
    }

    pub fn check_subtree_exists_invalid_path<'p, P>(
        &self,
        path: P,
        transaction: TransactionArg,
    ) -> CostResult<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        self.check_subtree_exists(
            path,
            transaction,
            Error::InvalidPath("subtree doesn't exist".to_owned()),
        )
    }

    pub fn worst_case_for_has_raw(
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_worst_case_has_raw_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            max_element_size,
        );
        cost
    }

    pub fn worst_case_for_get_raw(
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_worst_case_get_raw_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            max_element_size,
        );
        cost
    }

    pub fn worst_case_for_get(
        path: &KeyInfoPath,
        key: &KeyInfo,
        max_element_size: u32,
        max_references_sizes: Vec<u32>,
    ) -> OperationCost {
        let mut cost = OperationCost::default();
        GroveDb::add_worst_case_get_cost::<RocksDbStorage>(
            &mut cost,
            path,
            key,
            max_element_size,
            max_references_sizes,
        );
        cost
    }
}
