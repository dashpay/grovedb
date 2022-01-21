use std::{
    arch::x86_64::_mm_extract_si64,
    collections::{HashMap, HashSet},
    ops::Range,
    rc::Rc,
};

use merk::{
    proofs::{query::QueryItem, Query},
    Merk,
};
use storage::{
    rocksdb_storage::{OptimisticTransactionDBTransaction, PrefixedRocksDbStorage},
    RawIterator,
};

use crate::{subtree::raw_decode, Element, Error, GroveDb, PathQuery, SizedQuery};

/// Limit of possible indirections
pub(crate) const MAX_REFERENCE_HOPS: usize = 10;

impl GroveDb {
    pub fn get(
        &self,
        path: &[&[u8]],
        key: &[u8],
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<Element, Error> {
        match self.get_raw(path, key, transaction)? {
            Element::Reference(reference_path) => {
                self.follow_reference(reference_path, transaction)
            }
            other => Ok(other),
        }
    }

    fn follow_reference(
        &self,
        mut path: Vec<Vec<u8>>,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<Element, Error> {
        let mut hops_left = MAX_REFERENCE_HOPS;
        let mut current_element;
        let mut visited = HashSet::new();

        while hops_left > 0 {
            if visited.contains(&path) {
                return Err(Error::CyclicReference);
            }
            if let Some((key, path_slice)) = path.split_last() {
                current_element = self.get_raw(
                    path_slice
                        .iter()
                        .map(|x| x.as_slice())
                        .collect::<Vec<_>>()
                        .as_slice(),
                    key,
                    transaction,
                )?;
            } else {
                return Err(Error::InvalidPath("empty path"));
            }
            visited.insert(path);
            match current_element {
                Element::Reference(reference_path) => path = reference_path,
                other => return Ok(other),
            }
            hops_left -= 1;
        }
        Err(Error::ReferenceLimit)
    }

    /// Get tree item without following references
    pub(super) fn get_raw(
        &self,
        path: &[&[u8]],
        key: &[u8],
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<Element, Error> {
        Element::get(&self.get_subtree(path, transaction)?, key)
    }

    pub fn get_subtree(
        &self,
        path: &[&[u8]],
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<Merk<PrefixedRocksDbStorage>, Error> {
        let subtree_prefix = GroveDb::compress_subtree_key(path, None);
        match transaction {
            None => {
                let (subtree, has_keys) = self.get_subtree_with_key_info(path, None)?;
                if !has_keys {
                    // if the subtree has no keys, it's either empty or invalid
                    // we can confirm that it's an empty tree by checking if it was inserted into
                    // the parent tree
                    let (key, parent_path) =
                        path.split_last().ok_or(Error::InvalidPath("empty path"))?;

                    // if parent path is empty, we are dealing with root leaf node
                    // we can confirm validity of a root leaf node by checking root_leaf_keys
                    if parent_path.is_empty(){
                        if self.root_leaf_keys.contains_key(&subtree_prefix){
                            return Ok(subtree);
                        } else {
                            return Err(Error::InvalidPath("no subtree found under that path"));
                        }
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
            Some(_) => {
                if let Some(merk) = self.temp_subtrees.get(&subtree_prefix) {
                    Ok(merk.clone())
                } else {
                    Err(Error::InvalidPath("no subtree found under that path"))
                }
            }
        }
    }

    fn get_subtree_with_key_info(
        &self,
        path: &[&[u8]],
        key: Option<&[u8]>
    ) -> Result<(Merk<PrefixedRocksDbStorage>, bool), Error> {
        let subtree_prefix = GroveDb::compress_subtree_key(path, key);
        let merk = Merk::open(PrefixedRocksDbStorage::new(self.storage(), subtree_prefix)?)
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

    pub fn get_path_queries(
        &mut self,
        path_queries: &[&PathQuery],
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<Vec<Vec<u8>>, Error> {
        let elements = self.get_path_queries_raw(path_queries, transaction)?;
        let results = elements
            .into_iter()
            .map(|element| match element {
                Element::Reference(reference_path) => {
                    let maybe_item = self.follow_reference(reference_path, transaction)?;
                    if let Element::Item(item) = maybe_item {
                        Ok(item)
                    } else {
                        Err(Error::InvalidQuery("the reference must result in an item"))
                    }
                }
                other => Err(Error::InvalidQuery(
                    "path_queries can only refer to references",
                )),
            })
            .collect::<Result<Vec<Vec<u8>>, Error>>()?;
        Ok(results)
    }

    pub fn get_path_queries_raw(
        &mut self,
        path_queries: &[&PathQuery],
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<Vec<Element>, Error> {
        let mut result = Vec::new();
        for query in path_queries {
            let (query_results, _) = self.get_path_query_raw(query, transaction)?;
            result.extend_from_slice(&query_results);
        }
        Ok(result)
    }

    pub fn get_path_query(
        &mut self,
        path_query: &PathQuery,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<(Vec<Vec<u8>>, u16), Error> {
        let (elements, skipped) = self.get_path_query_raw(path_query, transaction)?;
        let results = elements
            .into_iter()
            .map(|element| match element {
                Element::Reference(reference_path) => {
                    let maybe_item = self.follow_reference(reference_path, transaction)?;
                    if let Element::Item(item) = maybe_item {
                        Ok(item)
                    } else {
                        Err(Error::InvalidQuery("the reference must result in an item"))
                    }
                }
                Element::Item(item) => Ok(item),
                Element::Tree(_) => Err(Error::InvalidQuery(
                    "path_queries can only refer to items and references",
                )),
            })
            .collect::<Result<Vec<Vec<u8>>, Error>>()?;
        Ok((results, skipped))
    }

    pub fn get_path_query_raw(
        &mut self,
        path_query: &PathQuery,
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<(Vec<Element>, u16), Error> {
        let subtrees = match transaction {
            None => &self.subtrees,
            Some(_) => &self.temp_subtrees,
        };
        self.get_path_query_on_trees_raw(path_query, subtrees)
    }

    fn get_path_query_on_trees_raw(
        &self,
        path_query: &PathQuery,
        subtrees: &HashMap<Vec<u8>, Merk<PrefixedRocksDbStorage>>,
    ) -> Result<(Vec<Element>, u16), Error> {
        let path = path_query.path;
        let merk = subtrees
            .get(&Self::compress_subtree_key(path, None))
            .ok_or(Error::InvalidPath("no subtree found under that path"))?;
        Element::get_path_query(merk, path_query, Some(subtrees))
    }
}
