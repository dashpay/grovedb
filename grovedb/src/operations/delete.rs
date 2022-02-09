use storage::rocksdb_storage::OptimisticTransactionDBTransaction;

use crate::{Element, Error, GroveDb};

impl GroveDb {
    pub fn delete_up_tree_while_empty<'a, P>(
        &mut self,
        path: P,
        key: &'a [u8],
        stop_path_height: Option<u16>,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<u16, Error>
    where
        P: IntoIterator<Item = &'a [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        let mut path_iter = path.into_iter();
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

    pub fn delete<'a, P>(
        &mut self,
        path: P,
        key: &'a [u8],
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<(), Error>
    where
        P: IntoIterator<Item = &'a [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        self.delete_internal(path, key, false, transaction)?;
        Ok(())
    }

    pub fn delete_if_empty_tree<'a, P>(
        &mut self,
        path: P,
        key: &'a [u8],
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<bool, Error>
    where
        P: IntoIterator<Item = &'a [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        self.delete_internal(path, key, true, transaction)
    }

    fn delete_internal<'a, P>(
        &mut self,
        path: P,
        key: &'a [u8],
        only_delete_tree_if_empty: bool,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<bool, Error>
    where
        P: IntoIterator<Item = &'a [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
        if transaction.is_none() && self.is_readonly {
            return Err(Error::DbIsInReadonlyMode);
        }
        let path_iter = path.into_iter();
        if path_iter.len() == 0 {
            // Attempt to delete a root tree leaf
            Err(Error::InvalidPath(
                "root tree leafs currently cannot be deleted",
            ))
        } else {
            let element = self.get_raw(path_iter.clone(), key.as_ref(), transaction)?;
            let subtrees = self.get_subtrees();
            let delete_element = || -> Result<(), Error> {
                // TODO: we shouldn't handle this context manually each time
                let mut parent_merk = subtrees.borrow_mut(path_iter.clone(), transaction)?;
                Element::delete(&mut parent_merk, &key, transaction)?;
                Ok(())
            };

            if let Element::Tree(_) = element {
                let subtree_merk_path = path_iter.clone().chain(std::iter::once(key));
                let subtrees_paths = self.find_subtrees(subtree_merk_path.clone(), transaction)?;
                let is_empty = subtrees
                    .borrow_mut(subtree_merk_path, transaction)?
                    .apply(|s| s.is_empty_tree(transaction));

                if only_delete_tree_if_empty && !is_empty {
                    return Ok(false);
                } else {
                    // TODO: dumb traversal should not be tolerated
                    for subtree_path in subtrees_paths {
                        let mut subtree = subtrees
                            .borrow_mut(subtree_path.iter().map(|x| x.as_slice()), transaction)?;
                        subtree.clear(transaction).map_err(|e| {
                            Error::CorruptedData(format!(
                                "unable to cleanup tree from storage: {}",
                                e
                            ))
                        })?;
                        if let Some(prefix) = subtree.get_prefix() {
                            self.get_subtrees()
                                .delete_temp_tree_with_prefix(prefix.to_vec(), transaction);
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
    pub(crate) fn find_subtrees<'a, P>(
        &self,
        path: P,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<Vec<Vec<Vec<u8>>>, Error>
    where
        P: IntoIterator<Item = &'a [u8]>,
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
        let subtrees = self.get_subtrees();

        while let Some(q) = queue.pop() {
            // Get the correct subtree with q_ref as path
            let path_iter = q.iter().map(|x| x.as_slice());
            let merk = subtrees.borrow_mut(path_iter.clone(), transaction)?;
            let mut raw_iter = Element::iterator(merk.raw_iter(transaction));
            while let Some((key, value)) = raw_iter.next()? {
                if let Element::Tree(_) = value {
                    let mut sub_path = q.clone();
                    sub_path.push(key.to_vec());
                    queue.push(sub_path.clone());
                    result.push(sub_path);
                }
            }
        }
        Ok(result)
    }
}
