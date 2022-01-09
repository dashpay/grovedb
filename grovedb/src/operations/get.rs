use std::collections::HashSet;
use merk::proofs::Query;
use merk::proofs::query::QueryItem;
use storage::RawIterator;
use std::ops::Range;

use crate::{Element, Error, GroveDb, PathQuery, SizedQuery};
use crate::subtree::raw_decode;

/// Limit of possible indirections
pub(crate) const MAX_REFERENCE_HOPS: usize = 10;

impl GroveDb {
    pub fn get(&self, path: &[&[u8]], key: &[u8]) -> Result<Element, Error> {
        match self.get_raw(path, key)? {
            Element::Reference(reference_path) => self.follow_reference(reference_path),
            other => Ok(other),
        }
    }

    fn follow_reference(&self, mut path: Vec<Vec<u8>>) -> Result<Element, Error> {
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
    pub(super) fn get_raw(&self, path: &[&[u8]], key: &[u8]) -> Result<Element, Error> {
        let merk = self
            .subtrees
            .get(&Self::compress_subtree_key(path, None))
            .ok_or(Error::InvalidPath("no subtree found under that path"))?;
        Element::get(&merk, key)
    }

    pub fn get_path_queries(&mut self, path_queries: &[&PathQuery]) -> Result<Vec<Element>, Error> {
        let mut result = Vec::new();
        for query in path_queries {
            let merk = self
                .subtrees
                .get(&Self::compress_subtree_key(query.path, None))
                .ok_or(Error::InvalidPath("no subtree found under that path"))?;
            let (subtree_results, skipped) = Element::get_sized_query(merk, &query.query)?;
            result.extend_from_slice(&subtree_results);
        }
        Ok(result)
    }

    pub fn get_path_query(
        &mut self,
        path_query: &PathQuery,
    ) -> Result<Vec<Element>, Error> {
        let path = path_query.path;
        let merk = self
            .subtrees
            .get(&Self::compress_subtree_key(path, None))
            .ok_or(Error::InvalidPath("no subtree found under that path"))?;
        let sized_query = &path_query.query;
        let mut result = Vec::new();
        let mut iter = merk.raw_iter();

        let mut limit = if sized_query.limit.is_some() { sized_query.limit.unwrap() } else { u16::MAX };
        let mut offset = if sized_query.offset.is_some() { sized_query.offset.unwrap() } else { 0 as u16};

        for item in sized_query.query.iter() {
            match item {
                QueryItem::Key(key) => {
                    result.push(Element::get(merk, key)?);
                }
                QueryItem::Range(Range { start, end }) => {
                    iter.seek(if sized_query.left_to_right {start} else {end});
                    while limit > 0 && iter.valid() && iter.key().is_some() && iter.key() != Some(if sized_query.left_to_right {end} else {start}) {
                        let element =
                            raw_decode(iter.value().expect("if key exists then value should too"))?;
                        match element {
                            Element::Tree(_) => {
                                // if the query had a subquery then we should get elements from it
                                if path_query.subquery_key.is_some() {
                                    let subquery_key = path_query.subquery_key.unwrap();
                                    // this means that for each element we should get the element at the subquery_key
                                    let mut path_vec = path.to_vec();
                                    path_vec.push(iter.key().expect("key should exist"));

                                    if path_query.subquery.is_some() {
                                        path_vec.push(subquery_key);

                                        let inner_merk = self
                                            .subtrees
                                            .get(&Self::compress_subtree_key(path_vec.as_slice(), None))
                                            .ok_or(Error::InvalidPath("no subtree found under that path"))?;
                                        let inner_limit = if sized_query.limit.is_some() { Some(limit) } else { None };
                                        let inner_offset = if sized_query.offset.is_some() { Some(offset) } else { None };
                                        let inner_query = SizedQuery::new(path_query.subquery.clone().unwrap(), inner_limit , inner_offset, sized_query.left_to_right);
                                        let (mut sub_elements , skipped) = Element::get_sized_query(inner_merk, &inner_query)?;
                                        limit -= sub_elements.len() as u16;
                                        offset -= skipped;
                                        result.append(&mut sub_elements);
                                    } else {
                                        let inner_merk = self
                                            .subtrees
                                            .get(&Self::compress_subtree_key(path_vec.as_slice(), None))
                                            .ok_or(Error::InvalidPath("no subtree found under that path"))?;
                                        if offset == 0 {
                                            result.push(Element::get(inner_merk, subquery_key)?);
                                            limit -= 1;
                                        } else {
                                            offset -= 1;
                                        }
                                    }
                                }
                            }
                            _ => {
                                if offset == 0 {
                                    result.push(element);
                                    limit -= 1;
                                } else {
                                    offset -= 1;
                                }
                            }
                        }
                        if sized_query.left_to_right {iter.next();} else {iter.prev();}
                    }
                }
                QueryItem::RangeInclusive(r) => {
                    let start = r.start();
                    let end = r.end();
                    iter.seek(if sized_query.left_to_right {start} else {end});
                    let mut work = true;
                    while iter.valid() && iter.key().is_some() && work {
                        if iter.key() == Some(if sized_query.left_to_right {end} else {start}) {
                            work = false;
                        }
                        if offset == 0 {
                            let element =
                                raw_decode(iter.value().expect("if key exists then value should too"))?;
                            result.push(element);
                            limit -= 1;
                        } else {
                            offset -= 1;
                        }
                        if sized_query.left_to_right {iter.next();} else {iter.prev();}
                    }
                }
            }
        }
        Ok(result)
    }
}
