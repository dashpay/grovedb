use std::collections::HashSet;

use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostContext, CostsExt, OperationCost,
};
use storage::StorageContext;

use crate::{
    util::{merk_optional_tx, meta_storage_context_optional_tx, storage_context_optional_tx},
    Element, Error, GroveDb, PathQuery, TransactionArg,
};

/// Limit of possible indirections
pub const MAX_REFERENCE_HOPS: usize = 10;

impl GroveDb {
    pub fn get<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        transaction: TransactionArg,
    ) -> CostContext<Result<Element, Error>>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        match cost_return_on_error!(&mut cost, self.get_raw(path, key, transaction)) {
            Element::Reference(reference_path, _) => self
                .follow_reference(reference_path, transaction)
                .add_cost(cost),
            other => Ok(other).wrap_with_cost(cost),
        }
    }

    pub fn follow_reference(
        &self,
        mut path: Vec<Vec<u8>>,
        transaction: TransactionArg,
    ) -> CostContext<Result<Element, Error>> {
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
                )
            } else {
                return Err(Error::CorruptedPath("empty path")).wrap_with_cost(cost);
            }
            visited.insert(path);
            match current_element {
                Element::Reference(reference_path, _) => path = reference_path,
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
    ) -> CostContext<Result<Element, Error>>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let path_iter = path.into_iter();
        if path_iter.len() == 0 {
            cost_return_on_error!(
                &mut cost,
                self.check_subtree_exists_path_not_found([key], transaction)
            );
            merk_optional_tx!(&mut cost, self.db, [key], transaction, subtree, {
                subtree
                    .root_hash()
                    .map(Element::new_tree)
                    .map(Ok)
                    .add_cost(cost)
            })
        } else {
            cost_return_on_error!(
                &mut cost,
                self.check_subtree_exists_path_not_found(path_iter.clone(), transaction)
            );
            merk_optional_tx!(&mut cost, self.db, path_iter, transaction, subtree, {
                Element::get(&subtree, key).add_cost(cost)
            })
        }
    }

    /// Does tree element exist without following references
    pub fn has_raw<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        transaction: TransactionArg,
    ) -> CostContext<Result<bool, Error>>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let path_iter = path.into_iter();

        if path_iter.len() == 0 {
            // Root tree's items are serialized into meta storage and cannot be checked
            // easily; Knowing that root tree's leafs are subtrees only, we can
            // check them using roots storage.
            storage_context_optional_tx!(self.db, [key], transaction, storage, {
                storage
                    .get_root(merk::ROOT_KEY_KEY)
                    .wrap_with_cost(Default::default())
                    .map_err(|e| e.into())
                    .flat_map_ok(|root| {
                        root.map(|r| {
                            Ok(true).wrap_with_cost(OperationCost {
                                seek_count: 1,
                                loaded_bytes: r.len(),
                                ..Default::default()
                            })
                        })
                        .unwrap_or(Ok(false).wrap_with_cost(OperationCost {
                            seek_count: 1,
                            ..Default::default()
                        }))
                    })
            })
        } else {
            // Merk's items should be written into data storage and checked accordingly
            storage_context_optional_tx!(self.db, path_iter, transaction, storage, {
                storage
                    .get(key)
                    .wrap_with_cost(Default::default())
                    .map_err(|e| e.into())
                    .flat_map_ok(|root| {
                        root.map(|r| {
                            Ok(true).wrap_with_cost(OperationCost {
                                seek_count: 1,
                                loaded_bytes: r.len(),
                                ..Default::default()
                            })
                        })
                        .unwrap_or(Ok(false).wrap_with_cost(OperationCost {
                            seek_count: 1,
                            ..Default::default()
                        }))
                    })
            })
        }
    }

    pub fn query_many(
        &self,
        path_queries: &[&PathQuery],
        transaction: TransactionArg,
    ) -> CostContext<Result<Vec<Vec<u8>>, Error>> {
        let mut cost = OperationCost::default();

        let elements =
            cost_return_on_error!(&mut cost, self.query_many_raw(path_queries, transaction));
        let results_wrapped = elements
            .into_iter()
            .map(|(_, element)| match element {
                Element::Reference(reference_path, _) => {
                    let maybe_item = self
                        .follow_reference(reference_path, transaction)
                        .unwrap_add_cost(&mut cost)?;
                    if let Element::Item(item, _) = maybe_item {
                        Ok(item)
                    } else {
                        Err(Error::InvalidQuery("the reference must result in an item"))
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
        transaction: TransactionArg,
    ) -> CostContext<Result<Vec<(Vec<u8>, Element)>, Error>> {
        let mut cost = OperationCost::default();

        let query = cost_return_on_error!(&mut cost, PathQuery::merge(path_queries.to_vec()));
        let (result, _) = cost_return_on_error!(&mut cost, self.query_raw(&query, transaction));
        Ok(result).wrap_with_cost(cost)
    }

    pub fn get_proved_path_query(
        &self,
        path_query: &PathQuery,
        transaction: TransactionArg,
    ) -> CostContext<Result<Vec<u8>, Error>> {
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
    ) -> CostContext<Result<(Vec<Vec<u8>>, u16), Error>> {
        let mut cost = OperationCost::default();

        let (elements, skipped) =
            cost_return_on_error!(&mut cost, self.query_raw(path_query, transaction));

        let results_wrapped = elements
            .into_iter()
            .map(|(_, element)| match element {
                Element::Reference(reference_path, _) => {
                    // While `map` on iterator is lazy, we should accumulate costs even if `collect`
                    // will end in `Err`, so we'll use external costs accumulator instead of
                    // returning costs from `map` call.
                    let maybe_item = self
                        .follow_reference(reference_path, transaction)
                        .unwrap_add_cost(&mut cost)?;

                    if let Element::Item(item, _) = maybe_item {
                        Ok(item)
                    } else {
                        Err(Error::InvalidQuery("the reference must result in an item"))
                    }
                }
                Element::Item(item, _) => Ok(item),
                Element::Tree(..) => Err(Error::InvalidQuery(
                    "path_queries can only refer to items and references",
                )),
            })
            .collect::<Result<Vec<Vec<u8>>, Error>>();

        let results = cost_return_on_error_no_add!(&cost, results_wrapped);
        Ok((results, skipped)).wrap_with_cost(cost)
    }

    pub fn query_raw(
        &self,
        path_query: &PathQuery,
        transaction: TransactionArg,
    ) -> CostContext<Result<(Vec<(Vec<u8>, Element)>, u16), Error>> {
        let path_slices = path_query
            .path
            .iter()
            .map(|x| x.as_slice())
            .collect::<Vec<_>>();
        Element::get_path_query(&self.db, &path_slices, path_query, transaction)
    }

    fn check_subtree_exists<'p, P>(
        &self,
        path: P,
        transaction: TransactionArg,
        error: Error,
    ) -> CostContext<Result<(), Error>>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let mut path_iter = path.into_iter();
        if path_iter.len() == 0 {
            return Ok(()).wrap_with_cost(cost);
        }
        if path_iter.len() == 1 {
            meta_storage_context_optional_tx!(self.db, transaction, meta_storage, {
                let root_leaf_keys = cost_return_on_error!(
                    &mut cost,
                    Self::get_root_leaf_keys_internal(&meta_storage)
                );
                if !root_leaf_keys.contains_key(path_iter.next().expect("must contain an item")) {
                    return Err(error).wrap_with_cost(cost);
                }
            });
        } else {
            let mut parent_iter = path_iter;
            let parent_key = parent_iter.next_back().expect("path is not empty");
            merk_optional_tx!(&mut cost, self.db, parent_iter, transaction, parent, {
                match Element::get(&parent, parent_key).unwrap_add_cost(&mut cost) {
                    Ok(Element::Tree(..)) => {}
                    Ok(_) | Err(Error::PathKeyNotFound(_)) => {
                        return Err(error).wrap_with_cost(cost)
                    }
                    Err(e) => return Err(e).wrap_with_cost(cost),
                }
            });
        }
        Ok(()).wrap_with_cost(cost)
    }

    pub fn check_subtree_exists_path_not_found<'p, P>(
        &self,
        path: P,
        transaction: TransactionArg,
    ) -> CostContext<Result<(), Error>>
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
    ) -> CostContext<Result<(), Error>>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        self.check_subtree_exists(
            path,
            transaction,
            Error::InvalidPath("subtree doesn't exist"),
        )
    }
}
