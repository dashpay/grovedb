use std::collections::HashSet;

use storage::rocksdb_storage::OptimisticTransactionDBTransaction;

use crate::{Element, Error, GroveDb, PathQuery, Subtrees};

/// Limit of possible indirections
pub(crate) const MAX_REFERENCE_HOPS: usize = 10;

impl GroveDb {
    pub fn get<'a, P>(
        &self,
        path: P,
        key: &'a [u8],
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<Element, Error>
    where
        P: IntoIterator<Item = &'a [u8]>,
        <P as IntoIterator>::IntoIter: DoubleEndedIterator + ExactSizeIterator + Clone,
    {
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
                current_element =
                    self.get_raw(path_slice.iter().map(|x| x.as_slice()), key, transaction)?;
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
    pub(super) fn get_raw<'a, P>(
        &self,
        path: P,
        key: &'a [u8],
        transaction: Option<&OptimisticTransactionDBTransaction>,
    ) -> Result<Element, Error>
    where
        P: IntoIterator<Item = &'a [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let path_iter = path.into_iter();
        // If path is empty, then we need to combine the provided key and path
        // then use this to get merk.
        let subtrees = self.get_subtrees();
        if path_iter.len() == 0 {
            Ok(subtrees
                .borrow_mut([key], transaction)?
                .apply(|s| Element::Tree(s.root_hash())))
        } else {
            subtrees
                .borrow_mut(path_iter, transaction)?
                .apply(|s| Element::get(s, key))
        }
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
    ) -> Result<(Vec<Element>, u16), Error> {
        let path_slices = path_query
            .path
            .iter()
            .map(|x| x.as_slice())
            .collect::<Vec<_>>();
        Element::get_path_query(&path_slices, path_query, transaction, &subtrees)
    }
}
