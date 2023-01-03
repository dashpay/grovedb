// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Delete operations and costs

#[cfg(feature = "full")]
mod average_case;
#[cfg(feature = "full")]
mod delete_up_tree;
#[cfg(feature = "full")]
mod worst_case;

#[cfg(feature = "full")]
use std::collections::{BTreeSet, HashMap};

#[cfg(feature = "full")]
use costs::{
    cost_return_on_error, cost_return_on_error_no_add,
    storage_cost::removal::{StorageRemovedBytes, StorageRemovedBytes::BasicStorageRemoval},
    CostResult, CostsExt, OperationCost,
};
#[cfg(feature = "full")]
pub use delete_up_tree::DeleteUpTreeOptions;
#[cfg(feature = "full")]
use merk::{Error as MerkError, Merk, MerkOptions};
#[cfg(feature = "full")]
use storage::{
    rocksdb_storage::{
        PrefixedRocksDbBatchTransactionContext, PrefixedRocksDbStorageContext,
        PrefixedRocksDbTransactionContext,
    },
    Storage, StorageBatch, StorageContext,
};

#[cfg(feature = "full")]
use crate::{
    batch::{GroveDbOp, Op},
    util::{
        merk_optional_tx, storage_context_optional_tx, storage_context_with_parent_optional_tx,
    },
    Element, ElementFlags, Error, GroveDb, Transaction, TransactionArg,
};

#[cfg(feature = "full")]
#[derive(Clone)]
/// Delete options
pub struct DeleteOptions {
    pub allow_deleting_non_empty_trees: bool,
    pub deleting_non_empty_trees_returns_error: bool,
    pub base_root_storage_is_free: bool,
    pub validate_tree_at_path_exists: bool,
}

#[cfg(feature = "full")]
impl Default for DeleteOptions {
    fn default() -> Self {
        DeleteOptions {
            allow_deleting_non_empty_trees: false,
            deleting_non_empty_trees_returns_error: true,
            base_root_storage_is_free: true,
            validate_tree_at_path_exists: false,
        }
    }
}

#[cfg(feature = "full")]
impl DeleteOptions {
    fn as_merk_options(&self) -> MerkOptions {
        MerkOptions {
            base_root_storage_is_free: self.base_root_storage_is_free,
        }
    }
}

#[cfg(feature = "full")]
/// 0 represents key size, 1 represents element size
type EstimatedKeyAndElementSize = (u32, u32);

#[cfg(feature = "full")]
impl GroveDb {
    /// Delete element in GroveDb
    pub fn delete<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        options: Option<DeleteOptions>,
        transaction: TransactionArg,
    ) -> CostResult<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let options = options.unwrap_or_default();
        self.delete_internal(
            path,
            key,
            &options,
            transaction,
            &mut |_, removed_key_bytes, removed_value_bytes| {
                Ok((
                    BasicStorageRemoval(removed_key_bytes),
                    BasicStorageRemoval(removed_value_bytes),
                ))
            },
        )
        .map_ok(|_| ())
    }

    /// Delete element with sectional storage function
    pub fn delete_with_sectional_storage_function<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        options: Option<DeleteOptions>,
        transaction: TransactionArg,
        split_removal_bytes_function: &mut impl FnMut(
            &mut ElementFlags,
            u32, // key removed bytes
            u32, // value removed bytes
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
    ) -> CostResult<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let options = options.unwrap_or_default();
        self.delete_internal(
            path,
            key,
            &options,
            transaction,
            &mut |value, removed_key_bytes, removed_value_bytes| {
                let mut element = Element::deserialize(value.as_slice())
                    .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
                let maybe_flags = element.get_flags_mut();
                match maybe_flags {
                    None => Ok((
                        BasicStorageRemoval(removed_key_bytes),
                        BasicStorageRemoval(removed_value_bytes),
                    )),
                    Some(flags) => (split_removal_bytes_function)(
                        flags,
                        removed_key_bytes,
                        removed_value_bytes,
                    )
                    .map_err(|e| MerkError::ClientCorruptionError(e.to_string())),
                }
            },
        )
        .map_ok(|_| ())
    }

    /// Delete if an empty tree
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
        self.delete_if_empty_tree_with_sectional_storage_function(
            path,
            key,
            transaction,
            &mut |_, removed_key_bytes, removed_value_bytes| {
                Ok((
                    BasicStorageRemoval(removed_key_bytes),
                    (BasicStorageRemoval(removed_value_bytes)),
                ))
            },
        )
    }

    /// Delete if an empty tree with section storage function
    pub fn delete_if_empty_tree_with_sectional_storage_function<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        transaction: TransactionArg,
        split_removal_bytes_function: &mut impl FnMut(
            &mut ElementFlags,
            u32, // key removed bytes
            u32, // value removed bytes
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            Error,
        >,
    ) -> CostResult<bool, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let options = DeleteOptions {
            allow_deleting_non_empty_trees: false,
            deleting_non_empty_trees_returns_error: false,
            ..Default::default()
        };
        self.delete_internal(
            path,
            key,
            &options,
            transaction,
            &mut |value, removed_key_bytes, removed_value_bytes| {
                let mut element = Element::deserialize(value.as_slice())
                    .map_err(|e| MerkError::ClientCorruptionError(e.to_string()))?;
                let maybe_flags = element.get_flags_mut();
                match maybe_flags {
                    None => Ok((
                        BasicStorageRemoval(removed_key_bytes),
                        BasicStorageRemoval(removed_value_bytes),
                    )),
                    Some(flags) => (split_removal_bytes_function)(
                        flags,
                        removed_key_bytes,
                        removed_value_bytes,
                    )
                    .map_err(|e| MerkError::ClientCorruptionError(e.to_string())),
                }
            },
        )
    }

    /// Delete operation for delete internal
    pub fn delete_operation_for_delete_internal<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        options: &DeleteOptions,
        is_known_to_be_subtree_with_sum: Option<(bool, bool)>,
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
            if options.validate_tree_at_path_exists {
                cost_return_on_error!(
                    &mut cost,
                    self.check_subtree_exists_path_not_found(path_iter.clone(), transaction)
                );
            }
            let (is_subtree, is_subtree_with_sum) = match is_known_to_be_subtree_with_sum {
                None => {
                    let element = cost_return_on_error!(
                        &mut cost,
                        self.get_raw(path_iter.clone(), key.as_ref(), transaction)
                    );
                    match element {
                        Element::Tree(..) => (true, false),
                        Element::SumTree(..) => (true, true),
                        _ => (false, false),
                    }
                }
                Some(x) => x,
            };

            if is_subtree {
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
                        Op::Delete | Op::DeleteTree | Op::DeleteSumTree => {
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
                    Op::Delete | Op::DeleteTree | Op::DeleteSumTree => false,
                    // todo: fix for to_path (it clones)
                    _ => op.path.to_path() == subtree_merk_path_vec,
                });

                let result = if !options.allow_deleting_non_empty_trees && !is_empty {
                    if options.deleting_non_empty_trees_returns_error {
                        Err(Error::DeletingNonEmptyTree(
                            "trying to do a delete operation for a non empty tree, but options \
                             not allowing this",
                        ))
                    } else {
                        Ok(None)
                    }
                } else if is_empty {
                    Ok(Some(GroveDbOp::delete_tree_op(
                        path_iter.map(|x| x.to_vec()).collect(),
                        key.to_vec(),
                        is_subtree_with_sum,
                    )))
                } else {
                    Err(Error::NotSupported(
                        "deletion operation for non empty tree not currently supported",
                    ))
                };
                result.wrap_with_cost(cost)
            } else {
                Ok(Some(GroveDbOp::delete_op(
                    path_iter.map(|x| x.to_vec()).collect(),
                    key.to_vec(),
                )))
                .wrap_with_cost(cost)
            }
        }
    }

    fn delete_internal<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        options: &DeleteOptions,
        transaction: TransactionArg,
        sectioned_removal: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            MerkError,
        >,
    ) -> CostResult<bool, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        if let Some(transaction) = transaction {
            self.delete_internal_on_transaction(path, key, options, transaction, sectioned_removal)
        } else {
            self.delete_internal_without_transaction(path, key, options, sectioned_removal)
        }
    }

    fn delete_internal_on_transaction<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        options: &DeleteOptions,
        transaction: &Transaction,
        sectioned_removal: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            MerkError,
        >,
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
        let mut subtree_to_delete_from = cost_return_on_error!(
            &mut cost,
            self.open_transactional_merk_at_path(path_iter.clone(), transaction)
        );
        let uses_sum_tree = subtree_to_delete_from.is_sum_tree;
        if element.is_tree() {
            let subtree_merk_path = path_iter.clone().chain(std::iter::once(key));

            let subtree_of_tree_we_are_deleting = cost_return_on_error!(
                &mut cost,
                self.open_transactional_merk_at_path(subtree_merk_path.clone(), transaction)
            );
            let is_empty = subtree_of_tree_we_are_deleting
                .is_empty_tree()
                .unwrap_add_cost(&mut cost);

            if !options.allow_deleting_non_empty_trees && !is_empty {
                return if options.deleting_non_empty_trees_returns_error {
                    Err(Error::DeletingNonEmptyTree(
                        "trying to do a delete operation for a non empty tree, but options not \
                         allowing this",
                    ))
                    .wrap_with_cost(cost)
                } else {
                    Ok(false).wrap_with_cost(cost)
                };
            } else if !is_empty {
                let storage_batch = StorageBatch::new();
                let subtrees_paths = cost_return_on_error!(
                    &mut cost,
                    self.find_subtrees(subtree_merk_path, Some(transaction))
                );
                for subtree_path in subtrees_paths {
                    let mut storage = self
                        .db
                        .get_batch_transactional_storage_context(
                            subtree_path.iter().map(|x| x.as_slice()),
                            &storage_batch,
                            transaction,
                        )
                        .unwrap_add_cost(&mut cost);

                    cost_return_on_error!(
                        &mut cost,
                        storage.clear().map_err(|e| {
                            Error::CorruptedData(format!(
                                "unable to cleanup tree from storage: {}",
                                e
                            ))
                        })
                    );
                }
                // todo: verify why we need to open the same? merk again
                let storage = self
                    .db
                    .get_batch_transactional_storage_context(
                        path_iter.clone(),
                        &storage_batch,
                        transaction,
                    )
                    .unwrap_add_cost(&mut cost);

                let mut merk_to_delete_tree_from = cost_return_on_error!(
                    &mut cost,
                    Merk::open_layered_with_root_key(
                        storage,
                        subtree_to_delete_from.root_key(),
                        element.is_sum_tree()
                    )
                    .map_err(|_| {
                        Error::CorruptedData("cannot open a subtree with given root key".to_owned())
                    })
                );
                // We are deleting a tree, a tree uses 3 bytes
                cost_return_on_error!(
                    &mut cost,
                    Element::delete_with_sectioned_removal_bytes(
                        &mut merk_to_delete_tree_from,
                        key,
                        Some(options.as_merk_options()),
                        true,
                        uses_sum_tree,
                        sectioned_removal
                    )
                );
                let mut merk_cache: HashMap<
                    Vec<Vec<u8>>,
                    Merk<PrefixedRocksDbBatchTransactionContext>,
                > = HashMap::default();
                merk_cache.insert(
                    path_iter.clone().map(|k| k.to_vec()).collect(),
                    merk_to_delete_tree_from,
                );
                cost_return_on_error!(
                    &mut cost,
                    self.propagate_changes_with_batch_transaction(
                        &storage_batch,
                        merk_cache,
                        path_iter,
                        transaction
                    )
                );
                cost_return_on_error_no_add!(
                    &cost,
                    self.db
                        .commit_multi_context_batch(storage_batch, Some(transaction))
                        .unwrap_add_cost(&mut cost)
                        .map_err(|e| e.into())
                );
            } else {
                // We are deleting a tree, a tree uses 3 bytes
                cost_return_on_error!(
                    &mut cost,
                    Element::delete_with_sectioned_removal_bytes(
                        &mut subtree_to_delete_from,
                        key,
                        Some(options.as_merk_options()),
                        true,
                        uses_sum_tree,
                        sectioned_removal
                    )
                );
                let mut merk_cache: HashMap<Vec<Vec<u8>>, Merk<PrefixedRocksDbTransactionContext>> =
                    HashMap::default();
                merk_cache.insert(
                    path_iter.clone().map(|k| k.to_vec()).collect(),
                    subtree_to_delete_from,
                );
                cost_return_on_error!(
                    &mut cost,
                    self.propagate_changes_with_transaction(merk_cache, path_iter, transaction)
                );
            }
        } else {
            cost_return_on_error!(
                &mut cost,
                Element::delete_with_sectioned_removal_bytes(
                    &mut subtree_to_delete_from,
                    key,
                    Some(options.as_merk_options()),
                    false,
                    uses_sum_tree,
                    sectioned_removal,
                )
            );
            let mut merk_cache: HashMap<Vec<Vec<u8>>, Merk<PrefixedRocksDbTransactionContext>> =
                HashMap::default();
            merk_cache.insert(
                path_iter.clone().map(|k| k.to_vec()).collect(),
                subtree_to_delete_from,
            );
            cost_return_on_error!(
                &mut cost,
                self.propagate_changes_with_transaction(merk_cache, path_iter, transaction)
            );
        }

        Ok(true).wrap_with_cost(cost)
    }

    fn delete_internal_without_transaction<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        options: &DeleteOptions,
        sectioned_removal: &mut impl FnMut(
            &Vec<u8>,
            u32,
            u32,
        ) -> Result<
            (StorageRemovedBytes, StorageRemovedBytes),
            MerkError,
        >,
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
        let uses_sum_tree = subtree_to_delete_from.is_sum_tree;
        if element.is_tree() {
            let subtree_merk_path = path_iter.clone().chain(std::iter::once(key));
            let subtree_of_tree_we_are_deleting = cost_return_on_error!(
                &mut cost,
                self.open_non_transactional_merk_at_path(subtree_merk_path.clone())
            );
            let is_empty = subtree_of_tree_we_are_deleting
                .is_empty_tree()
                .unwrap_add_cost(&mut cost);

            if !options.allow_deleting_non_empty_trees && !is_empty {
                return if options.deleting_non_empty_trees_returns_error {
                    Err(Error::DeletingNonEmptyTree(
                        "trying to do a delete operation for a non empty tree, but options not \
                         allowing this",
                    ))
                    .wrap_with_cost(cost)
                } else {
                    Ok(false).wrap_with_cost(cost)
                };
            } else {
                if !is_empty {
                    let subtrees_paths = cost_return_on_error!(
                        &mut cost,
                        self.find_subtrees(subtree_merk_path, None)
                    );
                    // TODO: dumb traversal should not be tolerated
                    for subtree_path in subtrees_paths.into_iter().rev() {
                        let mut inner_subtree_to_delete_from = cost_return_on_error!(
                            &mut cost,
                            self.open_non_transactional_merk_at_path(
                                subtree_path.iter().map(|x| x.as_slice())
                            )
                        );
                        cost_return_on_error!(
                            &mut cost,
                            inner_subtree_to_delete_from.clear().map_err(|e| {
                                Error::CorruptedData(format!(
                                    "unable to cleanup tree from storage: {}",
                                    e
                                ))
                            })
                        );
                    }
                }
                cost_return_on_error!(
                    &mut cost,
                    Element::delete_with_sectioned_removal_bytes(
                        &mut subtree_to_delete_from,
                        key,
                        Some(options.as_merk_options()),
                        true,
                        uses_sum_tree,
                        sectioned_removal,
                    )
                );
            }
        } else {
            cost_return_on_error!(
                &mut cost,
                Element::delete_with_sectioned_removal_bytes(
                    &mut subtree_to_delete_from,
                    key,
                    Some(options.as_merk_options()),
                    false,
                    uses_sum_tree,
                    sectioned_removal,
                )
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
        // slice which points into storage internals will remain valid if raw
        // iterator got altered so why that reference should be exclusive;

        let mut queue: Vec<Vec<Vec<u8>>> = vec![path.into_iter().map(|x| x.to_vec()).collect()];
        let mut result: Vec<Vec<Vec<u8>>> = queue.clone();

        while let Some(q) = queue.pop() {
            // Get the correct subtree with q_ref as path
            let path_iter = q.iter().map(|x| x.as_slice());
            storage_context_optional_tx!(self.db, path_iter.clone(), transaction, storage, {
                let storage = storage.unwrap_add_cost(&mut cost);
                let mut raw_iter = Element::iterator(storage.raw_iter()).unwrap_add_cost(&mut cost);
                while let Some((key, value)) =
                    cost_return_on_error!(&mut cost, raw_iter.next_element())
                {
                    if value.is_tree() {
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
}

#[cfg(feature = "full")]
#[cfg(test)]
mod tests {
    use costs::{
        storage_cost::{removal::StorageRemovedBytes::BasicStorageRemoval, StorageCost},
        OperationCost,
    };
    use pretty_assertions::assert_eq;

    use crate::{
        operations::delete::{delete_up_tree::DeleteUpTreeOptions, DeleteOptions},
        tests::{make_empty_grovedb, make_test_grovedb, ANOTHER_TEST_LEAF, TEST_LEAF},
        Element, Error,
    };

    #[test]
    fn test_empty_subtree_deletion_without_transaction() {
        let _element = Element::new_item(b"ayy".to_vec());
        let db = make_test_grovedb();
        // Insert some nested subtrees
        db.insert([TEST_LEAF], b"key1", Element::empty_tree(), None, None)
            .unwrap()
            .expect("successful subtree 1 insert");
        db.insert([TEST_LEAF], b"key4", Element::empty_tree(), None, None)
            .unwrap()
            .expect("successful subtree 3 insert");

        let root_hash = db.root_hash(None).unwrap().unwrap();
        db.delete([TEST_LEAF], b"key1", None, None)
            .unwrap()
            .expect("unable to delete subtree");
        assert!(matches!(
            db.get([TEST_LEAF, b"key1", b"key2"], b"key3", None)
                .unwrap(),
            Err(Error::PathParentLayerNotFound(_))
        ));
        // assert_eq!(db.subtrees.len().unwrap(), 3); // TEST_LEAF, ANOTHER_TEST_LEAF
        // TEST_LEAF.key4 stay
        assert!(db.get([], TEST_LEAF, None).unwrap().is_ok());
        assert!(db.get([], ANOTHER_TEST_LEAF, None).unwrap().is_ok());
        assert!(db.get([TEST_LEAF], b"key4", None).unwrap().is_ok());
        assert_ne!(root_hash, db.root_hash(None).unwrap().unwrap());
    }

    #[test]
    fn test_empty_subtree_deletion_with_transaction() {
        let _element = Element::new_item(b"ayy".to_vec());

        let db = make_test_grovedb();
        let transaction = db.start_transaction();

        // Insert some nested subtrees
        db.insert(
            [TEST_LEAF],
            b"key1",
            Element::empty_tree(),
            None,
            Some(&transaction),
        )
        .unwrap()
        .expect("successful subtree 1 insert");
        db.insert(
            [TEST_LEAF],
            b"key4",
            Element::empty_tree(),
            None,
            Some(&transaction),
        )
        .unwrap()
        .expect("successful subtree 3 insert");

        db.delete([TEST_LEAF], b"key1", None, Some(&transaction))
            .unwrap()
            .expect("unable to delete subtree");
        assert!(matches!(
            db.get([TEST_LEAF, b"key1", b"key2"], b"key3", Some(&transaction))
                .unwrap(),
            Err(Error::PathParentLayerNotFound(_))
        ));
        transaction.commit().expect("cannot commit transaction");
        assert!(matches!(
            db.get([TEST_LEAF], b"key1", None).unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
        assert!(matches!(db.get([TEST_LEAF], b"key4", None).unwrap(), Ok(_)));
    }

    #[test]
    fn test_subtree_deletion_if_empty_with_transaction() {
        let element = Element::new_item(b"value".to_vec());
        let db = make_test_grovedb();

        let transaction = db.start_transaction();

        // Insert some nested subtrees
        db.insert(
            [TEST_LEAF],
            b"level1-A",
            Element::empty_tree(),
            None,
            Some(&transaction),
        )
        .unwrap()
        .expect("successful subtree insert A on level 1");
        db.insert(
            [TEST_LEAF, b"level1-A"],
            b"level2-A",
            Element::empty_tree(),
            None,
            Some(&transaction),
        )
        .unwrap()
        .expect("successful subtree insert A on level 2");
        db.insert(
            [TEST_LEAF, b"level1-A"],
            b"level2-B",
            Element::empty_tree(),
            None,
            Some(&transaction),
        )
        .unwrap()
        .expect("successful subtree insert B on level 2");
        // Insert an element into subtree
        db.insert(
            [TEST_LEAF, b"level1-A", b"level2-A"],
            b"level3-A",
            element,
            None,
            Some(&transaction),
        )
        .unwrap()
        .expect("successful value insert");
        db.insert(
            [TEST_LEAF],
            b"level1-B",
            Element::empty_tree(),
            None,
            Some(&transaction),
        )
        .unwrap()
        .expect("successful subtree insert B on level 1");

        db.commit_transaction(transaction)
            .unwrap()
            .expect("cannot commit changes");

        // Currently we have:
        // Level 1:            A
        //                    / \
        // Level 2:          A   B
        //                   |
        // Level 3:          A: value

        let transaction = db.start_transaction();

        let deleted = db
            .delete_if_empty_tree([TEST_LEAF], b"level1-A", Some(&transaction))
            .unwrap()
            .expect("unable to delete subtree");
        assert!(!deleted);

        let deleted = db
            .delete_up_tree_while_empty(
                [TEST_LEAF, b"level1-A", b"level2-A"],
                b"level3-A",
                &DeleteUpTreeOptions {
                    stop_path_height: Some(0),
                    ..Default::default()
                },
                Some(&transaction),
            )
            .unwrap()
            .expect("unable to delete subtree");
        assert_eq!(deleted, 2);

        assert!(matches!(
            db.get(
                [TEST_LEAF, b"level1-A", b"level2-A"],
                b"level3-A",
                Some(&transaction)
            )
            .unwrap(),
            Err(Error::PathParentLayerNotFound(_))
        ));

        assert!(matches!(
            db.get([TEST_LEAF, b"level1-A"], b"level2-A", Some(&transaction))
                .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));

        assert!(matches!(
            db.get([TEST_LEAF], b"level1-A", Some(&transaction))
                .unwrap(),
            Ok(Element::Tree(..)),
        ));
    }

    #[test]
    fn test_subtree_deletion_if_empty_without_transaction() {
        let element = Element::new_item(b"value".to_vec());
        let db = make_test_grovedb();

        // Insert some nested subtrees
        db.insert([TEST_LEAF], b"level1-A", Element::empty_tree(), None, None)
            .unwrap()
            .expect("successful subtree insert A on level 1");
        db.insert(
            [TEST_LEAF, b"level1-A"],
            b"level2-A",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert A on level 2");
        db.insert(
            [TEST_LEAF, b"level1-A"],
            b"level2-B",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert B on level 2");
        // Insert an element into subtree
        db.insert(
            [TEST_LEAF, b"level1-A", b"level2-A"],
            b"level3-A",
            element,
            None,
            None,
        )
        .unwrap()
        .expect("successful value insert");
        db.insert([TEST_LEAF], b"level1-B", Element::empty_tree(), None, None)
            .unwrap()
            .expect("successful subtree insert B on level 1");

        // Currently we have:
        // Level 1:            A
        //                    / \
        // Level 2:          A   B
        //                   |
        // Level 3:          A: value

        let deleted = db
            .delete_if_empty_tree([TEST_LEAF], b"level1-A", None)
            .unwrap()
            .expect("unable to delete subtree");
        assert!(!deleted);

        let deleted = db
            .delete_up_tree_while_empty(
                [TEST_LEAF, b"level1-A", b"level2-A"],
                b"level3-A",
                &DeleteUpTreeOptions {
                    stop_path_height: Some(0),
                    ..Default::default()
                },
                None,
            )
            .unwrap()
            .expect("unable to delete subtree");
        assert_eq!(deleted, 2);

        assert!(matches!(
            db.get([TEST_LEAF, b"level1-A", b"level2-A"], b"level3-A", None,)
                .unwrap(),
            Err(Error::PathParentLayerNotFound(_))
        ));

        assert!(matches!(
            db.get([TEST_LEAF, b"level1-A"], b"level2-A", None).unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));

        assert!(matches!(
            db.get([TEST_LEAF], b"level1-A", None).unwrap(),
            Ok(Element::Tree(..)),
        ));
    }

    #[test]
    fn test_recurring_deletion_through_subtrees_with_transaction() {
        let element = Element::new_item(b"ayy".to_vec());

        let db = make_test_grovedb();
        let transaction = db.start_transaction();

        // Insert some nested subtrees
        db.insert(
            [TEST_LEAF],
            b"key1",
            Element::empty_tree(),
            None,
            Some(&transaction),
        )
        .unwrap()
        .expect("successful subtree 1 insert");
        db.insert(
            [TEST_LEAF, b"key1"],
            b"key2",
            Element::empty_tree(),
            None,
            Some(&transaction),
        )
        .unwrap()
        .expect("successful subtree 2 insert");

        // Insert an element into subtree
        db.insert(
            [TEST_LEAF, b"key1", b"key2"],
            b"key3",
            element,
            None,
            Some(&transaction),
        )
        .unwrap()
        .expect("successful value insert");
        db.insert(
            [TEST_LEAF],
            b"key4",
            Element::empty_tree(),
            None,
            Some(&transaction),
        )
        .unwrap()
        .expect("successful subtree 3 insert");

        db.delete(
            [TEST_LEAF],
            b"key1",
            Some(DeleteOptions {
                allow_deleting_non_empty_trees: true,
                deleting_non_empty_trees_returns_error: false,
                ..Default::default()
            }),
            Some(&transaction),
        )
        .unwrap()
        .expect("unable to delete subtree");
        assert!(matches!(
            db.get([TEST_LEAF, b"key1", b"key2"], b"key3", Some(&transaction))
                .unwrap(),
            Err(Error::PathParentLayerNotFound(_))
        ));
        transaction.commit().expect("cannot commit transaction");
        assert!(matches!(
            db.get([TEST_LEAF], b"key1", None).unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
        db.get([TEST_LEAF], b"key4", None)
            .unwrap()
            .expect("expected to get key4");
    }

    #[test]
    fn test_recurring_deletion_through_subtrees_without_transaction() {
        let element = Element::new_item(b"ayy".to_vec());

        let db = make_test_grovedb();

        // Insert some nested subtrees
        db.insert([TEST_LEAF], b"key1", Element::empty_tree(), None, None)
            .unwrap()
            .expect("successful subtree 1 insert");
        db.insert(
            [TEST_LEAF, b"key1"],
            b"key2",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree 2 insert");

        // Insert an element into subtree
        db.insert([TEST_LEAF, b"key1", b"key2"], b"key3", element, None, None)
            .unwrap()
            .expect("successful value insert");
        db.insert([TEST_LEAF], b"key4", Element::empty_tree(), None, None)
            .unwrap()
            .expect("successful subtree 3 insert");

        db.delete(
            [TEST_LEAF],
            b"key1",
            Some(DeleteOptions {
                allow_deleting_non_empty_trees: true,
                deleting_non_empty_trees_returns_error: false,
                ..Default::default()
            }),
            None,
        )
        .unwrap()
        .expect("unable to delete subtree");
        assert!(matches!(
            db.get([TEST_LEAF, b"key1", b"key2"], b"key3", None)
                .unwrap(),
            Err(Error::PathParentLayerNotFound(_))
        ));
        assert!(matches!(
            db.get([TEST_LEAF], b"key1", None).unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
        assert!(matches!(db.get([TEST_LEAF], b"key4", None).unwrap(), Ok(_)));
    }

    #[test]
    fn test_item_deletion() {
        let db = make_test_grovedb();
        let element = Element::new_item(b"ayy".to_vec());
        db.insert([TEST_LEAF], b"key", element, None, None)
            .unwrap()
            .expect("successful insert");
        let root_hash = db.root_hash(None).unwrap().unwrap();
        assert!(db.delete([TEST_LEAF], b"key", None, None).unwrap().is_ok());
        assert!(matches!(
            db.get([TEST_LEAF], b"key", None).unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
        assert_ne!(root_hash, db.root_hash(None).unwrap().unwrap());
    }

    #[test]
    fn test_one_delete_tree_item_cost() {
        let db = make_empty_grovedb();
        let tx = db.start_transaction();

        db.insert(
            vec![],
            b"key1",
            Element::new_item(b"cat".to_vec()),
            None,
            Some(&tx),
        )
        .cost_as_result()
        .expect("expected to insert");

        let cost = db
            .delete(vec![], b"key1", None, Some(&tx))
            .cost_as_result()
            .expect("expected to delete");
        // Explanation for 147 storage removed bytes

        // Key -> 37 bytes
        // 32 bytes for the key prefix
        // 4 bytes for the key
        // 1 byte for key_size (required space for 36)

        // Value -> 72
        //   1 for the flag option (but no flags)
        //   1 for the enum type item
        //   3 for "cat"
        //   1 for cat length
        //   1 for Basic Merk
        // 32 for node hash
        // 32 for value hash (trees have this for free)
        // 1 byte for the value_size (required space for 70)

        // Parent Hook -> 40
        // Key Bytes 4
        // Hash Size 32
        // Key Length 1
        // Child Heights 2
        // Sum 1

        // Total 37 + 72 + 40 = 149

        // Hash node calls
        // everything is empty, so no need for hashes?
        assert_eq!(
            cost,
            OperationCost {
                seek_count: 6, // todo: verify this
                storage_cost: StorageCost {
                    added_bytes: 0,
                    replaced_bytes: 0,
                    removed_bytes: BasicStorageRemoval(149)
                },
                storage_loaded_bytes: 154, // todo: verify this
                hash_node_calls: 0,
            }
        );
    }
}
