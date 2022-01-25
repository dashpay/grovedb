use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    rc::Rc,
};

use merk::Merk;
use storage::{
    rocksdb_storage::{OptimisticTransactionDBTransaction, PrefixedRocksDbStorage},
    RawIterator,
};

use crate::{Element, Error, GroveDb, PathQuery, Subtrees};

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
        // If path is empty, then we need to combine the provided key and path
        // then use this to get merk.
        let merk_result;
        if path.is_empty() {
           merk_result = self.get_subtrees().get(&[key], transaction)?;
        } else {
            merk_result = self.get_subtrees().get(path, transaction)?;
        }

        let (merk, prefix) = merk_result;

        let elem;
        if path.is_empty(){
           elem = Ok(Element::Tree(merk.root_hash()));
        } else {
            elem = Element::get(&merk, key);
        }

        if let Some(prefix) = prefix {
            self.get_subtrees()
                .insert_temp_tree_with_prefix(prefix, merk, transaction);
        } else {
            self.get_subtrees()
                .insert_temp_tree(path, merk, transaction);
        }

        elem
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
                _ => Err(Error::InvalidQuery(
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
        let subtrees = self.get_subtrees();
        self.get_path_query_on_trees_raw(path_query, subtrees, transaction)
    }

    fn get_path_query_on_trees_raw(
        &self,
        path_query: &PathQuery,
        subtrees: Subtrees,
        transaction: Option<&OptimisticTransactionDBTransaction>,
        // subtrees: &HashMap<Vec<u8>, Merk<PrefixedRocksDbStorage>>,
    ) -> Result<(Vec<Element>, u16), Error> {
        let path = path_query.path;
        let (merk, prefix) = subtrees.get(path, transaction)?;

        let elem = Element::get_path_query(&merk, path_query, Some(&subtrees));

        if let Some(prefix) = prefix{
            subtrees.insert_temp_tree_with_prefix(prefix, merk, transaction);
        } else {
            subtrees.insert_temp_tree(path, merk, transaction);
        }

        elem
    }
}
