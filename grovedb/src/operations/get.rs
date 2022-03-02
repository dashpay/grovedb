use std::collections::HashSet;

use merk::Merk;

use crate::{Element, Error, GroveDb, PathQuery, TransactionArg};

/// Limit of possible indirections
pub(crate) const MAX_REFERENCE_HOPS: usize = 10;

impl GroveDb {
    pub fn get<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        transaction: TransactionArg,
    ) -> Result<Element, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
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
        transaction: TransactionArg,
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
                return Err(Error::CorruptedPath("empty path"));
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
    pub(super) fn get_raw<'p, P>(
        &self,
        path: P,
        key: &'p [u8],
        transaction: TransactionArg,
    ) -> Result<Element, Error>
    where
        P: IntoIterator<Item = &'p [u8]>,
        <P as IntoIterator>::IntoIter: ExactSizeIterator + DoubleEndedIterator + Clone,
    {
        let path_iter = path.into_iter();
        if path_iter.len() == 0 {
            if let Some(tx) = transaction {
                let subtree_storage = self
                    .db
                    .get_prefixed_transactional_context_from_path([key], tx);
                let subtree = Merk::open(subtree_storage)
                    .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))?;
                Ok(Element::Tree(subtree.root_hash()))
            } else {
                let subtree_storage = self
                    .db
                    .get_prefixed_context_from_path([key]);
                let subtree = Merk::open(subtree_storage)
                    .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))?;
                Ok(Element::Tree(subtree.root_hash()))
            }
        } else {
            if let Some(tx) = transaction {
                let subtree_storage = self
                    .db
                    .get_prefixed_transactional_context_from_path(path_iter, tx);
                let subtree = Merk::open(subtree_storage)
                    .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))?;
                Ok(Element::get(&subtree, key)?)
            } else {
                let subtree_storage = self
                    .db
                    .get_prefixed_context_from_path(path_iter);
                let subtree = Merk::open(subtree_storage)
                    .map_err(|_| Error::CorruptedData("cannot open a subtree".to_owned()))?;
                Ok(Element::get(&subtree, key)?)
            }
        }
    }

    // pub fn get_path_queries(
    //     &mut self,
    //     path_queries: &[&PathQuery],
    //     transaction: Option<&OptimisticTransactionDBTransaction>,
    // ) -> Result<Vec<Vec<u8>>, Error> {
    //     let elements = self.get_path_queries_raw(path_queries, transaction)?;
    //     let results = elements
    //         .into_iter()
    //         .map(|element| match element {
    //             Element::Reference(reference_path) => {
    //                 let maybe_item = self.follow_reference(reference_path,
    // transaction)?;                 if let Element::Item(item) = maybe_item {
    //                     Ok(item)
    //                 } else {
    //                     Err(Error::InvalidQuery("the reference must result in an
    // item"))                 }
    //             }
    //             _ => Err(Error::InvalidQuery(
    //                 "path_queries can only refer to references",
    //             )),
    //         })
    //         .collect::<Result<Vec<Vec<u8>>, Error>>()?;
    //     Ok(results)
    // }

    // pub fn get_path_queries_raw(
    //     &mut self,
    //     path_queries: &[&PathQuery],
    //     transaction: Option<&OptimisticTransactionDBTransaction>,
    // ) -> Result<Vec<Element>, Error> {
    //     let mut result = Vec::new();
    //     for query in path_queries {
    //         let (query_results, _) = self.get_path_query_raw(query,
    // transaction)?;         result.extend_from_slice(&query_results);
    //     }
    //     Ok(result)
    // }

    // pub fn get_path_query(
    //     &mut self,
    //     path_query: &PathQuery,
    //     transaction: Option<&OptimisticTransactionDBTransaction>,
    // ) -> Result<(Vec<Vec<u8>>, u16), Error> {
    //     let (elements, skipped) = self.get_path_query_raw(path_query,
    // transaction)?;     let results = elements
    //         .into_iter()
    //         .map(|element| match element {
    //             Element::Reference(reference_path) => {
    //                 let maybe_item = self.follow_reference(reference_path,
    // transaction)?;                 if let Element::Item(item) = maybe_item {
    //                     Ok(item)
    //                 } else {
    //                     Err(Error::InvalidQuery("the reference must result in an
    // item"))                 }
    //             }
    //             Element::Item(item) => Ok(item),
    //             Element::Tree(_) => Err(Error::InvalidQuery(
    //                 "path_queries can only refer to items and references",
    //             )),
    //         })
    //         .collect::<Result<Vec<Vec<u8>>, Error>>()?;
    //     Ok((results, skipped))
    // }

    // pub fn get_path_query_raw(
    //     &mut self,
    //     path_query: &PathQuery,
    //     transaction: Option<&OptimisticTransactionDBTransaction>,
    // ) -> Result<(Vec<Element>, u16), Error> {
    //     let subtrees = self.get_subtrees();
    //     self.get_path_query_on_trees_raw(path_query, subtrees, transaction)
    // }

    // fn get_path_query_on_trees_raw(
    //     &self,
    //     path_query: &PathQuery,
    //     subtrees: Subtrees,
    //     transaction: Option<&OptimisticTransactionDBTransaction>,
    // ) -> Result<(Vec<Element>, u16), Error> {
    //     let path_slices = path_query
    //         .path
    //         .iter()
    //         .map(|x| x.as_slice())
    //         .collect::<Vec<_>>();
    //     Element::get_path_query(&path_slices, path_query, transaction, &subtrees)
    // }
}
