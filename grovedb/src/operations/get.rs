use std::collections::HashSet;

use crate::{Element, Error, GroveDb, PathQuery};

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

    pub fn get_query(
        &mut self,
        path_queries: &[PathQuery],
    ) -> Result<Vec<Element>, Error> {
        let mut result = Vec::new();
        for query in path_queries {
            let merk = self
                .subtrees
                .get(&Self::compress_subtree_key(query.path, None))
                .ok_or(Error::InvalidPath("no subtree found under that path"))?;
            let subtree_results = Element::get_query(merk, &query.query)?;
            result.extend_from_slice(&subtree_results);
        }
        Ok(result)
    }
}
