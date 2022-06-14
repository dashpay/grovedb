use std::collections::BTreeSet;

use storage::StorageContext;

use crate::{
    batch::{GroveDbOp, Op},
    util::{merk_optional_tx, storage_context_optional_tx},
    Element, Error, GroveDb, TransactionArg,
};

impl GroveDb {
    pub fn delete_up_tree_while_empty<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        stop_path_height: Option<u16>,
        transaction: TransactionArg,
    ) -> Result<u16, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut path_iter = path.into_iter();
        self.check_subtree_exists_path_not_found(path_iter.clone(), transaction)?;
        if let Some(stop_path_height) = stop_path_height {
            if stop_path_height == path_iter.clone().len() as u16 {
                return Ok(0);
            }
        }
        if !self.delete_internal(path_iter.clone(), key, true, transaction)? {
            return Ok(0);
        }
        let mut delete_count: u16 = 1;
        if let Some(last) = path_iter.next_back() {
            let deleted_parent =
                self.delete_up_tree_while_empty(path_iter, last, stop_path_height, transaction)?;
            delete_count += deleted_parent;
        }
        Ok(delete_count)
    }

    pub fn delete_operations_for_delete_up_tree_while_empty<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        stop_path_height: Option<u16>,
        validate: bool,
        current_batch_operations: &Vec<GroveDbOp>,
        transaction: TransactionArg,
    ) -> Result<Option<Vec<GroveDbOp>>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut current_batch_operations = current_batch_operations.clone();
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
    ) -> Result<Option<Vec<GroveDbOp>>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut path_iter = path.into_iter();
        if let Some(stop_path_height) = stop_path_height {
            if stop_path_height == path_iter.clone().len() as u16 {
                return Ok(None);
            }
        }
        if validate {
            self.check_subtree_exists_path_not_found(path_iter.clone(), transaction)?;
        }
        if let Some(delete_operation_this_level) = self.delete_operation_for_delete_internal(
            path_iter.clone(),
            key,
            true,
            validate,
            current_batch_operations,
            transaction,
        )? {
            let mut delete_operations = vec![delete_operation_this_level.clone()];
            if let Some(last) = path_iter.next_back() {
                current_batch_operations.push(delete_operation_this_level);
                if let Some(mut delete_operations_upper_level) = self
                    .add_delete_operations_for_delete_up_tree_while_empty(
                        path_iter,
                        last,
                        stop_path_height,
                        validate,
                        current_batch_operations,
                        transaction,
                    )?
                {
                    delete_operations.append(&mut delete_operations_upper_level);
                }
            }
            Ok(Some(delete_operations))
        } else {
            Ok(None)
        }
    }

    pub fn delete<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        transaction: TransactionArg,
    ) -> Result<(), Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        self.delete_internal(path, key, false, transaction)?;
        Ok(())
    }

    pub fn delete_if_empty_tree<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        transaction: TransactionArg,
    ) -> Result<bool, Error>
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
        current_batch_operations: &Vec<GroveDbOp>,
        transaction: TransactionArg,
    ) -> Result<Option<GroveDbOp>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let path_iter = path.into_iter();
        if path_iter.len() == 0 {
            // Attempt to delete a root tree leaf
            Err(Error::InvalidPath(
                "root tree leaves currently cannot be deleted",
            ))
        } else {
            if validate {
                self.check_subtree_exists_path_not_found(path_iter.clone(), transaction)?;
            }
            let element = self.get_raw(path_iter.clone(), key.as_ref(), transaction)?;

            if let Element::Tree(..) = element {
                let subtree_merk_path = path_iter.clone().chain(std::iter::once(key));
                let subtree_merk_path_vec = subtree_merk_path
                    .clone()
                    .map(|x| x.to_vec())
                    .collect::<Vec<Vec<u8>>>();
                let subtrees_paths = self.find_subtrees(subtree_merk_path.clone(), transaction)?;
                let batch_deleted_keys = current_batch_operations
                    .iter()
                    .filter_map(|op| match op.op {
                        Op::Insert { .. } => None,
                        Op::Delete => {
                            if op.path == subtree_merk_path_vec {
                                Some(op.key.as_slice())
                            } else {
                                None
                            }
                        }
                    })
                    .collect::<BTreeSet<&[u8]>>();
                let mut is_empty =
                    merk_optional_tx!(self.db, subtree_merk_path, transaction, subtree, {
                        subtree.is_empty_tree_except(batch_deleted_keys)
                    });

                // If there is any current batch operation that is inserting something in this
                // tree then it is not empty either
                is_empty &= current_batch_operations.iter().any(|op| match op.op {
                    Op::Insert { .. } => op.path == subtree_merk_path_vec,
                    Op::Delete => false,
                });

                if only_delete_tree_if_empty && !is_empty {
                    Ok(None)
                } else {
                    if is_empty {
                        Ok(Some(GroveDbOp::delete(
                            path_iter.map(|x| x.to_vec()).collect(),
                            key.to_vec(),
                        )))
                    } else {
                        Err(Error::NotSupported(
                            "deletion operation for non empty tree not currently supported",
                        ))
                    }
                }
            } else {
                Ok(Some(GroveDbOp::delete(
                    path_iter.map(|x| x.to_vec()).collect(),
                    key.to_vec(),
                )))
            }
        }
    }

    fn delete_internal<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        only_delete_tree_if_empty: bool,
        transaction: TransactionArg,
    ) -> Result<bool, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let path_iter = path.into_iter();
        if path_iter.len() == 0 {
            // Attempt to delete a root tree leaf
            Err(Error::InvalidPath(
                "root tree leaves currently cannot be deleted",
            ))
        } else {
            self.check_subtree_exists_path_not_found(path_iter.clone(), transaction)?;
            let element = self.get_raw(path_iter.clone(), key.as_ref(), transaction)?;
            let delete_element = || -> Result<(), Error> {
                merk_optional_tx!(self.db, path_iter.clone(), transaction, mut parent_merk, {
                    Element::delete(&mut parent_merk, &key)?;
                    Ok(())
                })
            };

            if let Element::Tree(..) = element {
                let subtree_merk_path = path_iter.clone().chain(std::iter::once(key));
                let subtrees_paths = self.find_subtrees(subtree_merk_path.clone(), transaction)?;
                let is_empty =
                    merk_optional_tx!(self.db, subtree_merk_path, transaction, subtree, {
                        subtree.is_empty_tree()
                    });

                if only_delete_tree_if_empty && !is_empty {
                    return Ok(false);
                } else {
                    // TODO: dumb traversal should not be tolerated
                    if !is_empty {
                        for subtree_path in subtrees_paths {
                            merk_optional_tx!(
                                self.db,
                                subtree_path.iter().map(|x| x.as_slice()),
                                transaction,
                                mut subtree,
                                {
                                    subtree.clear().map_err(|e| {
                                        Error::CorruptedData(format!(
                                            "unable to cleanup tree from storage: {}",
                                            e
                                        ))
                                    })?;
                                }
                            );
                        }
                    }
                    delete_element()?;
                }
            } else {
                delete_element()?;
            }
            self.propagate_changes(path_iter, transaction)?;
            Ok(true)
        }
    }

    // TODO: dumb traversal should not be tolerated
    /// Finds keys which are trees for a given subtree recursively.
    /// One element means a key of a `merk`, n > 1 elements mean relative path
    /// for a deeply nested subtree.
    pub(crate) fn find_subtrees<'p, P>(
        &self,
        path: P,
        transaction: TransactionArg,
    ) -> Result<Vec<Vec<Vec<u8>>>, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
    {
        // TODO: remove conversion to vec;
        // However, it's not easy for a reason:
        // new keys to enqueue are taken from raw iterator which returns Vec<u8>;
        // changing that to slice is hard as cursor should be moved for next iteration
        // which requires exclusive (&mut) reference, also there is no guarantee that
        // slice which points into storage internals will remain valid if raw iterator
        // got altered so why that reference should be exclusive;

        let mut queue: Vec<Vec<Vec<u8>>> =
            vec![path.into_iter().map(|x| x.as_ref().to_vec()).collect()];
        let mut result: Vec<Vec<Vec<u8>>> = queue.clone();

        while let Some(q) = queue.pop() {
            // Get the correct subtree with q_ref as path
            let path_iter = q.iter().map(|x| x.as_slice());
            storage_context_optional_tx!(self.db, path_iter.clone(), transaction, storage, {
                let mut raw_iter = Element::iterator(storage.raw_iter());
                while let Some((key, value)) = raw_iter.next()? {
                    if let Element::Tree(..) = value {
                        let mut sub_path = q.clone();
                        sub_path.push(key.to_vec());
                        queue.push(sub_path.clone());
                        result.push(sub_path);
                    }
                }
            })
        }
        Ok(result)
    }
}
