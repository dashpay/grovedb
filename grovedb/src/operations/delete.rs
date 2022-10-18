use std::collections::{BTreeSet, HashMap};

use costs::{
    cost_return_on_error, cost_return_on_error_no_add, CostResult, CostsExt, OperationCost,
};
use merk::Merk;
use storage::{
    rocksdb_storage::{PrefixedRocksDbStorageContext, PrefixedRocksDbTransactionContext},
    Storage, StorageContext,
};

use crate::{
    batch::{GroveDbOp, KeyInfo, KeyInfoPath, Op},
    util::{
        merk_optional_tx, storage_context_optional_tx, storage_context_with_parent_optional_tx,
    },
    Element, Error, GroveDb, Transaction, TransactionArg,
};

impl GroveDb {
    /// Delete up tree while empty will delete nodes while they are empty up a
    /// tree.
    pub fn delete_up_tree_while_empty<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        stop_path_height: Option<u16>,
        validate: bool,
        transaction: TransactionArg,
    ) -> CostResult<u16, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();
        let mut batch_operations: Vec<GroveDbOp> = Vec::new();
        let path_iter = path.into_iter();
        let path_len = path_iter.len();
        let maybe_ops = cost_return_on_error!(
            &mut cost,
            self.add_delete_operations_for_delete_up_tree_while_empty(
                path_iter,
                key,
                stop_path_height,
                validate,
                &mut batch_operations,
                transaction
            )
        );

        let ops = cost_return_on_error_no_add!(
            &cost,
            if let Some(stop_path_height) = stop_path_height {
                maybe_ops.ok_or(Error::DeleteUpTreeStopHeightMoreThanInitialPathSize(
                    format!(
                        "stop path height {} more than path size of {}",
                        stop_path_height, path_len
                    ),
                ))
            } else {
                maybe_ops.ok_or(Error::CorruptedCodeExecution(
                    "stop path height not set, but still not deleting element",
                ))
            }
        );
        let ops_len = ops.len();
        dbg!(&ops);
        self.apply_batch(ops, None, transaction)
            .map_ok(|_| ops_len as u16)
    }

    pub fn delete_operations_for_delete_up_tree_while_empty<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        stop_path_height: Option<u16>,
        validate: bool,
        mut current_batch_operations: Vec<GroveDbOp>,
        transaction: TransactionArg,
    ) -> CostResult<Option<Vec<GroveDbOp>>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        self.add_delete_operations_for_delete_up_tree_while_empty(
            path,
            key,
            stop_path_height,
            validate,
            &mut current_batch_operations,
            transaction,
        )
    }

    pub fn add_delete_operations_for_delete_up_tree_while_empty<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        stop_path_height: Option<u16>,
        validate: bool,
        current_batch_operations: &mut Vec<GroveDbOp>,
        transaction: TransactionArg,
    ) -> CostResult<Option<Vec<GroveDbOp>>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let mut path_iter = path.into_iter();
        if let Some(stop_path_height) = stop_path_height {
            if stop_path_height == path_iter.clone().len() as u16 {
                return Ok(None).wrap_with_cost(cost);
            }
        }
        if validate {
            cost_return_on_error!(
                &mut cost,
                self.check_subtree_exists_path_not_found(path_iter.clone(), transaction)
            );
        }
        if let Some(delete_operation_this_level) = cost_return_on_error!(
            &mut cost,
            self.delete_operation_for_delete_internal(
                path_iter.clone(),
                key,
                true,
                validate,
                current_batch_operations,
                transaction,
            )
        ) {
            let mut delete_operations = vec![delete_operation_this_level.clone()];
            if let Some(last) = path_iter.next_back() {
                current_batch_operations.push(delete_operation_this_level);
                if let Some(mut delete_operations_upper_level) = cost_return_on_error!(
                    &mut cost,
                    self.add_delete_operations_for_delete_up_tree_while_empty(
                        path_iter,
                        last,
                        stop_path_height,
                        validate,
                        current_batch_operations,
                        transaction,
                    )
                ) {
                    delete_operations.append(&mut delete_operations_upper_level);
                }
            }
            Ok(Some(delete_operations)).wrap_with_cost(cost)
        } else {
            Ok(None).wrap_with_cost(cost)
        }
    }

    pub fn delete<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        transaction: TransactionArg,
    ) -> CostResult<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        self.delete_internal(path, key, false, transaction)
            .map_ok(|_| ())
    }

    pub fn delete_if_empty_tree<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        transaction: TransactionArg,
    ) -> CostResult<bool, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        self.delete_internal(path, key, true, transaction)
    }

    pub fn delete_operation_for_delete_internal<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        only_delete_tree_if_empty: bool,
        validate: bool,
        current_batch_operations: &[GroveDbOp],
        transaction: TransactionArg,
    ) -> CostResult<Option<GroveDbOp>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let path_iter = path.into_iter();
        if path_iter.len() == 0 {
            // Attempt to delete a root tree leaf
            Err(Error::InvalidPath(
                "root tree leaves currently cannot be deleted".to_owned(),
            ))
            .wrap_with_cost(cost)
        } else {
            if validate {
                cost_return_on_error!(
                    &mut cost,
                    self.check_subtree_exists_path_not_found(path_iter.clone(), transaction)
                );
            }
            let element = cost_return_on_error!(
                &mut cost,
                self.get_raw(path_iter.clone(), key.as_ref(), transaction)
            );

            if let Element::Tree(..) = element {
                let subtree_merk_path = path_iter.clone().chain(std::iter::once(key));
                let subtree_merk_path_vec = subtree_merk_path
                    .clone()
                    .map(|x| x.to_vec())
                    .collect::<Vec<Vec<u8>>>();
                // TODO: may be a bug
                let _subtrees_paths = cost_return_on_error!(
                    &mut cost,
                    self.find_subtrees(subtree_merk_path.clone(), transaction)
                );
                let batch_deleted_keys = current_batch_operations
                    .iter()
                    .filter_map(|op| match op.op {
                        Op::Delete => {
                            // todo: to_path clones (best to figure out how to compare without
                            // cloning)
                            if op.path.to_path() == subtree_merk_path_vec {
                                Some(op.key.as_slice())
                            } else {
                                None
                            }
                        }
                        _ => None,
                    })
                    .collect::<BTreeSet<&[u8]>>();
                let mut is_empty = merk_optional_tx!(
                    &mut cost,
                    self.db,
                    subtree_merk_path,
                    transaction,
                    subtree,
                    {
                        subtree
                            .is_empty_tree_except(batch_deleted_keys)
                            .unwrap_add_cost(&mut cost)
                    }
                );

                // If there is any current batch operation that is inserting something in this
                // tree then it is not empty either
                is_empty &= !current_batch_operations.iter().any(|op| match op.op {
                    Op::Delete => false,
                    // todo: fix for to_path (it clones)
                    _ => op.path.to_path() == subtree_merk_path_vec,
                });

                let result = if only_delete_tree_if_empty && !is_empty {
                    Ok(None)
                } else if is_empty {
                    Ok(Some(GroveDbOp::delete_run_op(
                        path_iter.map(|x| x.to_vec()).collect(),
                        key.to_vec(),
                    )))
                } else {
                    Err(Error::NotSupported(
                        "deletion operation for non empty tree not currently supported",
                    ))
                };
                result.wrap_with_cost(cost)
            } else {
                Ok(Some(GroveDbOp::delete_run_op(
                    path_iter.map(|x| x.to_vec()).collect(),
                    key.to_vec(),
                )))
                .wrap_with_cost(cost)
            }
        }
    }

    pub fn worst_case_delete_operation_for_delete_internal<'p, 'db, S: Storage<'db>, P>(
        &self,
        path: &KeyInfoPath,
        key: &KeyInfo,
        validate: bool,
        max_element_size: u32,
    ) -> CostResult<Option<GroveDbOp>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        if path.len() == 0 {
            // Attempt to delete a root tree leaf
            Err(Error::InvalidPath(
                "root tree leaves currently cannot be deleted".to_owned(),
            ))
            .wrap_with_cost(cost)
        } else {
            if validate {
                GroveDb::add_worst_case_get_merk::<S>(&mut cost, path);
            }
            GroveDb::add_worst_case_get_raw_cost::<S>(&mut cost, path, key, max_element_size);
            Ok(Some(GroveDbOp::delete_worst_case_op(
                path.clone(),
                key.clone(),
            )))
            .wrap_with_cost(cost)
        }
    }

    fn delete_internal<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        only_delete_tree_if_empty: bool,
        transaction: TransactionArg,
    ) -> CostResult<bool, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        if let Some(transaction) = transaction {
            self.delete_internal_on_transaction(path, key, only_delete_tree_if_empty, transaction)
        } else {
            self.delete_internal_without_transaction(path, key, only_delete_tree_if_empty)
        }
    }

    fn delete_internal_on_transaction<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        only_delete_tree_if_empty: bool,
        transaction: &Transaction,
    ) -> CostResult<bool, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let path_iter = path.into_iter();
        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw(path_iter.clone(), key.as_ref(), Some(transaction))
        );
        let mut merk_cache: HashMap<Vec<Vec<u8>>, Merk<PrefixedRocksDbTransactionContext>> =
            HashMap::default();
        let mut subtree_to_delete_from: Merk<PrefixedRocksDbTransactionContext> = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(path_iter.clone(), transaction)
        );
        if let Element::Tree(..) = element {
            let subtree_merk_path = path_iter.clone().chain(std::iter::once(key));
            let subtrees_paths = cost_return_on_error!(
                &mut cost,
                self.find_subtrees(subtree_merk_path.clone(), Some(transaction))
            );

            let subtree_of_tree_we_are_deleting: Merk<PrefixedRocksDbTransactionContext> = cost_return_on_error!(
                &mut cost,
                self.open_transactional_merk_at_path(path_iter.clone(), transaction)
            );
            let is_empty = subtree_of_tree_we_are_deleting
                .is_empty_tree()
                .unwrap_add_cost(&mut cost);

            if only_delete_tree_if_empty && !is_empty {
                return Ok(false).wrap_with_cost(cost);
            } else {
                if !is_empty {
                    // TODO: dumb traversal should not be tolerated
                    for subtree_path in subtrees_paths {
                        let mut inner_subtree_to_delete_from: Merk<
                            PrefixedRocksDbTransactionContext,
                        > = cost_return_on_error!(
                            &mut cost,
                            self.open_transactional_merk_at_path(
                                subtree_path.iter().map(|x| x.as_slice()),
                                transaction
                            )
                        );
                        cost_return_on_error!(
                            &mut cost,
                            inner_subtree_to_delete_from.clear().map_err(|e| {
                                Error::CorruptedData(format!(
                                    "unable to cleanup tree from storage_cost: {}",
                                    e
                                ))
                            })
                        );
                    }
                }
                cost_return_on_error!(
                    &mut cost,
                    Element::delete(&mut subtree_to_delete_from, &key)
                );
            }
        } else {
            cost_return_on_error!(
                &mut cost,
                Element::delete(&mut subtree_to_delete_from, &key)
            );
        }
        merk_cache.insert(
            path_iter.clone().map(|k| k.to_vec()).collect(),
            subtree_to_delete_from,
        );
        cost_return_on_error!(
            &mut cost,
            self.propagate_changes_with_transaction(merk_cache, path_iter, transaction)
        );

        Ok(true).wrap_with_cost(cost)
    }

    fn delete_internal_without_transaction<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        only_delete_tree_if_empty: bool,
    ) -> CostResult<bool, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut cost = OperationCost::default();

        let path_iter = path.into_iter();
        let element = cost_return_on_error!(
            &mut cost,
            self.get_raw(path_iter.clone(), key.as_ref(), None)
        );
        let mut merk_cache: HashMap<Vec<Vec<u8>>, Merk<PrefixedRocksDbStorageContext>> =
            HashMap::default();
        let mut subtree_to_delete_from: Merk<PrefixedRocksDbStorageContext> = cost_return_on_error!(
            &mut cost,
            self.open_non_transactional_merk_at_path(path_iter.clone())
        );
        if let Element::Tree(..) = element {
            let subtree_merk_path = path_iter.clone().chain(std::iter::once(key));
            let subtrees_paths = cost_return_on_error!(
                &mut cost,
                self.find_subtrees(subtree_merk_path.clone(), None)
            );

            let subtree_of_tree_we_are_deleting: Merk<PrefixedRocksDbStorageContext> = cost_return_on_error!(
                &mut cost,
                self.open_non_transactional_merk_at_path(path_iter.clone())
            );
            let is_empty = subtree_of_tree_we_are_deleting
                .is_empty_tree()
                .unwrap_add_cost(&mut cost);

            if only_delete_tree_if_empty && !is_empty {
                return Ok(false).wrap_with_cost(cost);
            } else {
                if !is_empty {
                    // TODO: dumb traversal should not be tolerated
                    for subtree_path in subtrees_paths.into_iter().rev() {
                        let mut inner_subtree_to_delete_from: Merk<PrefixedRocksDbStorageContext> = cost_return_on_error!(
                            &mut cost,
                            self.open_non_transactional_merk_at_path(
                                subtree_path.iter().map(|x| x.as_slice())
                            )
                        );
                        cost_return_on_error!(
                            &mut cost,
                            inner_subtree_to_delete_from.clear().map_err(|e| {
                                Error::CorruptedData(format!(
                                    "unable to cleanup tree from storage_cost: {}",
                                    e
                                ))
                            })
                        );
                    }
                }
                cost_return_on_error!(
                    &mut cost,
                    Element::delete(&mut subtree_to_delete_from, &key)
                );
            }
        } else {
            cost_return_on_error!(
                &mut cost,
                Element::delete(&mut subtree_to_delete_from, &key)
            );
        }
        merk_cache.insert(
            path_iter.clone().map(|k| k.to_vec()).collect(),
            subtree_to_delete_from,
        );
        cost_return_on_error!(
            &mut cost,
            self.propagate_changes_without_transaction(merk_cache, path_iter)
        );

        Ok(true).wrap_with_cost(cost)
    }

    // TODO: dumb traversal should not be tolerated
    /// Finds keys which are trees for a given subtree recursively.
    /// One element means a key of a `merk`, n > 1 elements mean relative path
    /// for a deeply nested subtree.
    pub(crate) fn find_subtrees<'p, P>(
        &self,
        path: P,
        transaction: TransactionArg,
    ) -> CostResult<Vec<Vec<Vec<u8>>>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
    {
        let mut cost = OperationCost::default();

        // TODO: remove conversion to vec;
        // However, it's not easy for a reason:
        // new keys to enqueue are taken from raw iterator which returns Vec<u8>;
        // changing that to slice is hard as cursor should be moved for next iteration
        // which requires exclusive (&mut) reference, also there is no guarantee that
        // slice which points into storage_cost internals will remain valid if raw
        // iterator got altered so why that reference should be exclusive;

        let mut queue: Vec<Vec<Vec<u8>>> = vec![path.into_iter().map(|x| x.to_vec()).collect()];
        let mut result: Vec<Vec<Vec<u8>>> = queue.clone();

        while let Some(q) = queue.pop() {
            // Get the correct subtree with q_ref as path
            let path_iter = q.iter().map(|x| x.as_slice());
            storage_context_optional_tx!(self.db, path_iter.clone(), transaction, storage, {
                let storage = storage.unwrap_add_cost(&mut cost);
                let mut raw_iter = Element::iterator(storage.raw_iter()).unwrap_add_cost(&mut cost);
                while let Some((key, value)) = cost_return_on_error!(&mut cost, raw_iter.next()) {
                    if let Element::Tree(..) = value {
                        let mut sub_path = q.clone();
                        sub_path.push(key.to_vec());
                        queue.push(sub_path.clone());
                        result.push(sub_path);
                    }
                }
            })
        }
        Ok(result).wrap_with_cost(cost)
    }

    pub fn worst_case_deletion_cost<'p, P>(
        &self,
        _path: P,
        key: &'p [u8],
        max_element_size: u32,
    ) -> OperationCost
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let mut cost = OperationCost::default();
        GroveDb::add_worst_case_delete_cost(
            &mut cost,
            // path,
            key.len() as u32,
            max_element_size,
        );
        cost
    }
}
