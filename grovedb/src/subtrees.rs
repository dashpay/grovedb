//! Module for retrieving subtrees
use std::{collections::HashMap, rc::Rc, cell::RefCell};

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
    pub storage: Rc<storage::rocksdb_storage::OptimisticTransactionDB>,
}

impl Subtrees<'_> {
    pub fn insert_temp_tree(self, path: &[&[u8]], merk: Merk<PrefixedRocksDbStorage>, transaction: Option<&OptimisticTransactionDBTransaction>) -> Option<Merk<PrefixedRocksDbStorage>> {
        match transaction{
            None => None,
            Some(_) => {
                let prefix = GroveDb::compress_subtree_key(path, None);
                self.temp_subtrees.borrow_mut().insert(prefix, merk)
            }
        }
    }

    pub fn insert_temp_tree_with_prefix(self, prefix: Vec<u8>, merk: Merk<PrefixedRocksDbStorage>, transaction: Option<&OptimisticTransactionDBTransaction>) -> Option<Merk<PrefixedRocksDbStorage>> {
        match transaction{
            None => None,
            Some(_) => {
                self.temp_subtrees.borrow_mut().insert(prefix, merk)
            }
        }
    }

    pub fn get(
        &self,
        path: &[&[u8]],
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<Merk<PrefixedRocksDbStorage>, Error> {
        let merk;
        match transaction {
            None => {
                merk = self.get_subtree_without_transaction(path)?;
            },
            Some(_) => {
                let prefix = &GroveDb::compress_subtree_key(path, None);
                if self.temp_subtrees.borrow().contains_key(prefix) {
                    // get the merk out
                    merk = self.temp_subtrees.borrow_mut().remove(prefix).expect("confirmed it's in the hashmap");
                } else {
                    // merk is not in the hash map get it without transaction
                    merk = self.get_subtree_without_transaction(path)?;
                }
            }
        }
        Ok(merk)
    }

    pub fn get_subtree_without_transaction(
        &self,
        path: &[&[u8]],
    ) -> Result<Merk<PrefixedRocksDbStorage>, Error> {
        let subtree_prefix = GroveDb::compress_subtree_key(path, None);
        let (subtree, has_keys) = self.get_subtree_with_key_info(path, None)?;
        if !has_keys {
            // if the subtree has no keys, it's either empty or invalid
            // we can confirm that it's an empty tree by checking if it was inserted into
            // the parent tree
            let (key, parent_path) = path.split_last().ok_or(Error::InvalidPath("empty path"))?;

            // if parent path is empty, we are dealing with root leaf node
            // we can confirm validity of a root leaf node by checking root_leaf_keys
            if parent_path.is_empty() {
                // dbg!("parent path is empty, checking the root tree");
                let root_key = path[0].to_vec();
                return if self.root_leaf_keys.contains_key(&root_key) {
                    Ok(subtree)
                } else {
                    Err(Error::InvalidPath("no subtree found under that path"))
                };
            }

            // Non root leaf nodes, get parent tree and confirm child validity
            let (parent_tree, has_keys) = self.get_subtree_with_key_info(parent_path, None)?;
            if !has_keys {
                // parent tree can't be empty, hence invalid path
                Err(Error::InvalidPath("no subtree found under that path"))
            } else {
                // Check that it contains the child as an empty tree
                let elem = Element::get(&parent_tree, key)
                    .map_err(|_| Error::InvalidPath("no subtree found under that path"))?;
                match elem {
                    Element::Tree(_) => Ok(subtree),
                    _ => Err(Error::InvalidPath("no subtree found under that path")),
                }
            }
        } else {
            Ok(subtree)
        }
    }

    // pub fn get_subtree_with_transaction(
    //     &self,
    //     path: &[&[u8]],
    // ) -> Result<&Merk<PrefixedRocksDbStorage>, Error> {
    //     let subtree_prefix = GroveDb::compress_subtree_key(path, None);
    //     if let Some(merk) = self.temp_subtrees.borrow().get(&subtree_prefix) {
    //         Ok(merk)
    //     } else {
    //         Err(Error::InvalidPath("no subtree found under that path"))
    //         // dbg!("Getting subtree without transaction");
    //         // // if the subtree doesn't exist in temp_subtrees,
    //         // // check if it was created before the transaction was started
    //         // let merk = self
    //         //     .get_subtree_without_transaction(path)
    //         //     .map_err(|_| Error::InvalidPath("no subtree found under that path"))?;
    //         // Ok(merk)
    //     }
    // }

    fn get_subtree_with_key_info(
        &self,
        path: &[&[u8]],
        key: Option<&[u8]>,
    ) -> Result<(Merk<PrefixedRocksDbStorage>, bool), Error> {
        let subtree_prefix = GroveDb::compress_subtree_key(path, key);
        let merk = Merk::open(PrefixedRocksDbStorage::new(
            self.storage.clone(),
            subtree_prefix,
        )?)
        .map_err(|_| Error::InvalidPath("no subtree found under that path"))?;
        let mut has_keys = false;
        {
            let mut iter = merk.raw_iter();
            iter.seek_to_first();
            if iter.valid() {
                has_keys = true;
            }
        }
        Ok((merk, has_keys))
    }
}
