//! Module for retrieving subtrees
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use merk::Merk;
use storage::{
    rocksdb_storage::{OptimisticTransactionDBTransaction, PrefixedRocksDbStorage},
    RawIterator,
};

use crate::{Element, Error, GroveDb};

// TODO: should take temp_root_leaf_keys also
pub struct Subtrees<'a> {
    pub root_leaf_keys: &'a HashMap<Vec<u8>, usize>,
    pub temp_subtrees: &'a RefCell<HashMap<Vec<u8>, Merk<PrefixedRocksDbStorage>>>,
    pub deleted_subtrees: &'a RefCell<HashSet<Vec<u8>>>,
    pub storage: Rc<storage::rocksdb_storage::OptimisticTransactionDB>,
}

impl Subtrees<'_> {
    pub fn insert_temp_tree<'a, P>(
        &self,
        path: P,
        merk: Merk<PrefixedRocksDbStorage>,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Option<Merk<PrefixedRocksDbStorage>>
    where
        P: IntoIterator<Item = &'a [u8]>,
    {
        match transaction {
            None => None,
            Some(_) => {
                let prefix = GroveDb::compress_subtree_key(path, None);
                self.insert_temp_tree_with_prefix(prefix, merk, transaction)
            }
        }
    }

    pub fn insert_temp_tree_with_prefix(
        &self,
        prefix: Vec<u8>,
        merk: Merk<PrefixedRocksDbStorage>,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Option<Merk<PrefixedRocksDbStorage>> {
        match transaction {
            None => None,
            Some(_) => {
                // Removed subtree could be inserted again in a scope of a transaction that's
                // why we need to stop treating it as deleted
                self.deleted_subtrees.borrow_mut().remove(prefix.as_slice());
                self.temp_subtrees.borrow_mut().insert(prefix, merk)
            }
        }
    }

    pub fn delete_temp_tree_with_prefix<T>(&self, prefix: Vec<u8>, transaction: Option<T>) {
        if transaction.is_some() {
            self.deleted_subtrees.borrow_mut().insert(prefix);
        }
    }

    pub fn get<'a, P>(
        &self,
        path: P,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<(Merk<PrefixedRocksDbStorage>, Option<Vec<u8>>), Error>
    where
        P: IntoIterator<Item = &'a [u8]>,
        <P as IntoIterator>::IntoIter: Clone + DoubleEndedIterator,
    {
        let merk;
        let mut prefix: Option<Vec<u8>> = None;
        match transaction {
            None => {
                merk = self.get_subtree_without_transaction(path)?;
            }
            Some(_) => {
                let path_iter = path.into_iter();
                let tree_prefix = GroveDb::compress_subtree_key(path_iter.clone(), None);
                if self.deleted_subtrees.borrow().contains(&tree_prefix) {
                    return Err(Error::InvalidPath("no subtree found under that path"));
                }
                if self.temp_subtrees.borrow().contains_key(&tree_prefix) {
                    // get the merk out
                    merk = self
                        .temp_subtrees
                        .borrow_mut()
                        .remove(&tree_prefix)
                        .expect("confirmed it's in the hashmap");
                    prefix = Some(tree_prefix);
                } else {
                    // merk is not in the hash map get it without transaction
                    merk = self.get_subtree_without_transaction(path_iter)?;
                }
            }
        }
        Ok((merk, prefix))
    }

    pub fn get_subtree_without_transaction<'a, P>(
        &self,
        path: P,
    ) -> Result<Merk<PrefixedRocksDbStorage>, Error>
    where
        P: IntoIterator<Item = &'a [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + Clone,
    {
        let mut path_iter = path.into_iter();
        let (subtree, has_keys) = self.get_subtree_with_key_info(path_iter.clone(), None)?;
        if !has_keys {
            // if the subtree has no keys, it's either empty or invalid
            // we can confirm that it's an empty tree by checking if it was inserted into
            // the parent tree
            let key = path_iter
                .next_back()
                .ok_or(Error::InvalidPath("empty path"))?;

            // if parent path is empty, we are dealing with root leaf node
            // we can confirm validity of a root leaf node by checking root_leaf_keys
            let mut parent_path = path_iter.peekable();
            if parent_path.peek().is_none() {
                return if self.root_leaf_keys.contains_key(key.as_ref()) {
                    Ok(subtree)
                } else {
                    Err(Error::InvalidPath("no subtree found for root path"))
                };
            }

            // Non root leaf nodes, get parent tree and confirm child validity
            let (parent_tree, has_keys) = self.get_subtree_with_key_info(parent_path, None)?;
            if !has_keys {
                // parent tree can't be empty, hence invalid path
                Err(Error::InvalidPath(
                    "no subtree found as parent in path is empty",
                ))
            } else {
                // Check that it contains the child as an empty tree
                let elem = Element::get(&parent_tree, key).map_err(|_| {
                    Error::InvalidPath("no subtree found as parent does not contain child")
                })?;
                match elem {
                    Element::Tree(_) => Ok(subtree),
                    _ => Err(Error::InvalidPath(
                        "no subtree found as path refers to an element or reference",
                    )),
                }
            }
        } else {
            Ok(subtree)
        }
    }

    fn get_subtree_with_key_info<'a, P>(
        &self,
        path: P,
        key: Option<&'a [u8]>,
    ) -> Result<(Merk<PrefixedRocksDbStorage>, bool), Error>
    where
        P: IntoIterator<Item = &'a [u8]>,
    {
        let subtree_prefix = GroveDb::compress_subtree_key(path, key);
        let merk = Merk::open(PrefixedRocksDbStorage::new(
            self.storage.clone(),
            subtree_prefix,
        )?)
        .map_err(|_| Error::InvalidPath("no subtree found under that path"))?;
        let has_keys = !merk.is_empty_tree(None);
        Ok((merk, has_keys))
    }
}
