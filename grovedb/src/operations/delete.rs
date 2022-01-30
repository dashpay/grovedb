use storage::rocksdb_storage::OptimisticTransactionDBTransaction;

use crate::{Element, Error, GroveDb};

impl GroveDb {

    pub fn delete_up_tree_while_empty(
        &mut self,
        path: &[&[u8]],
        key: Vec<u8>,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<u16, Error> {
        let deleted = self.delete_internal(path, key, false, transaction)?;
        if !deleted {
            return Ok(0 as u16);
        }
        let mut delete_count: u16 = 1;
        if let Some((key, rest_path)) = path.split_last() {
            let deleted_parent = self.delete_up_tree_while_empty(rest_path, key.to_vec(), transaction)?;
            delete_count += deleted_parent;
        }
        Ok(delete_count)
    }

    pub fn delete(
        &mut self,
        path: &[&[u8]],
        key: Vec<u8>,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<(), Error> {
        self.delete_internal(path, key, false, transaction)?;
        Ok(())
    }

    pub fn delete_if_empty_tree(
        &mut self,
        path: &[&[u8]],
        key: Vec<u8>,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<bool, Error> {
        self.delete_internal(path, key, true, transaction)
    }

    fn delete_internal(
        &mut self,
        path: &[&[u8]],
        key: Vec<u8>,
        only_delete_if_empty_tree: bool,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<bool, Error> {
        if transaction.is_none() && self.is_readonly {
            return Err(Error::DbIsInReadonlyMode);
        }
        if path.is_empty() {
            // Attempt to delete a root tree leaf
            Err(Error::InvalidPath(
                "root tree leafs currently cannot be deleted",
            ))
        } else {
            let element = self.get_raw(path, &key, transaction)?;
            let subtrees = self.get_subtrees();
            let (mut merk, prefix) = subtrees.get(path, transaction)?;

            if let Element::Tree(_) = element {
                if merk.is_empty_tree() {
                    Element::delete(&mut merk, key.clone(), transaction)?;
                } else if only_delete_if_empty_tree {
                    return Ok(false);
                } else {
                    Element::delete(&mut merk, key.clone(), transaction)?;

                    // we need to add the merk trees into the hashmap because we will use them for querying data
                    if let Some(prefix) = prefix {
                        subtrees
                            .insert_temp_tree_with_prefix(prefix, merk, transaction);
                    } else {
                        subtrees
                            .insert_temp_tree(path, merk, transaction);
                    }

                    // TODO: dumb traversal should not be tolerated
                    let mut concat_path: Vec<Vec<u8>> = path.iter().map(|x| x.to_vec()).collect();
                    concat_path.push(key);
                    let subtrees_paths = self.find_subtrees(concat_path, transaction)?;

                    for subtree_path in subtrees_paths {
                        // TODO: eventually we need to do something about this nested slices
                        let subtree_path_ref: Vec<&[u8]> =
                            subtree_path.iter().map(|x| x.as_slice()).collect();
                        let mut subtree = subtrees.get_subtree_without_transaction(subtree_path_ref.as_slice())?;
                        subtree.clear(transaction).map_err(|e| {
                            Error::CorruptedData(format!("unable to cleanup tree from storage: {}", e))
                        })?;
                    }
                }
            } else {
                Element::delete(&mut merk, key.clone(), transaction)?;
            }

            self.propagate_changes(path, transaction)?;
            Ok(true)
        }
    }

    // TODO: dumb traversal should not be tolerated
    /// Finds keys which are trees for a given subtree recursively.
    /// One element means a key of a `merk`, n > 1 elements mean relative path
    /// for a deeply nested subtree.
    pub(crate) fn find_subtrees(
        &self,
        path: Vec<Vec<u8>>,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<Vec<Vec<Vec<u8>>>, Error> {
        let mut queue: Vec<Vec<Vec<u8>>> = vec![path.clone()];
        let mut result: Vec<Vec<Vec<u8>>> = vec![path];

        while let Some(q) = queue.pop() {
            // TODO: eventually we need to do something about this nested slices
            let q_ref: Vec<&[u8]> = q.iter().map(|x| x.as_slice()).collect();
            // Get the correct subtree with q_ref as path
            let (merk, _) = self.get_subtrees().get(&q_ref, transaction)?;
            let mut iter = Element::iterator(merk.raw_iter());
            // let mut iter = self.elements_iterator(&q_ref, transaction)?;
            while let Some((key, value)) = iter.next()? {
                if let Element::Tree(_) = value {
                    let mut sub_path = q.clone();
                    sub_path.push(key);
                    queue.push(sub_path.clone());
                    result.push(sub_path);
                }
            }
        }
        Ok(result)
    }
}
