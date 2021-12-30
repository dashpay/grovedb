//! Module for subtrees handling.
//! Subtrees handling is isolated so basically this module is about adapting
//! Merk API to GroveDB needs.
use std::ops::Range;

use merk::{
    proofs::{query::QueryItem, Query},
    tree::Tree,
    Op,
};
use serde::{Deserialize, Serialize};
use storage::{
    rocksdb_storage::{PrefixedRocksDbStorage, RawPrefixedIterator},
    RawIterator, Store,
};

use crate::{Error, Merk};

/// Variants of GroveDB stored entities
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Element {
    /// An ordinary value
    Item(Vec<u8>),
    /// A reference to an object by its path
    Reference(Vec<Vec<u8>>),
    /// A subtree, contains a root hash of the underlying Merk.
    /// Hash is stored to make Merk become different when its subtrees have
    /// changed, otherwise changes won't be reflected in parent trees.
    Tree([u8; 32]),
}

impl Element {
    // TODO: improve API to avoid creation of Tree elements with uncertain state
    pub fn empty_tree() -> Element {
        Element::Tree(Default::default())
    }

    /// Delete an element from Merk under a key
    pub fn delete(merk: &mut Merk<PrefixedRocksDbStorage>, key: Vec<u8>) -> Result<(), Error> {
        // TODO: delete references on this element
        let batch = [(key, Op::Delete)];
        merk.apply(&batch, &[])
            .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    /// Get an element from Merk under a key; path should be resolved and proper
    /// Merk should be loaded by this moment
    pub fn get(merk: &Merk<PrefixedRocksDbStorage>, key: &[u8]) -> Result<Element, Error> {
        let element = bincode::deserialize(
            merk.get(&key)
                .map_err(|e| Error::CorruptedData(e.to_string()))?
                .ok_or(Error::InvalidPath("key not found in Merk"))?
                .as_slice(),
        )
        .map_err(|_| Error::CorruptedData(String::from("unable to deserialize element")))?;
        Ok(element)
    }

    pub fn get_query(
        merk: &Merk<PrefixedRocksDbStorage>,
        query: &Query,
    ) -> Result<Vec<Element>, Error> {
        let mut result = Vec::new();
        let mut iter = merk.raw_iter();

        for item in query.iter() {
            match item {
                QueryItem::Key(key) => {
                    result.push(Element::get(merk, key)?);
                }
                QueryItem::Range(Range { start, end }) => {
                    iter.seek(start);
                    while iter.valid() && iter.key().is_some() && iter.key() != Some(end) {
                        let element =
                            raw_decode(iter.value().expect("if key exists then value should too"))?;
                        result.push(element);
                        iter.next();
                    }
                }
                QueryItem::RangeInclusive(r) => {
                    let start = r.start();
                    let end = r.end();
                    iter.seek(start);
                    let mut work = true;
                    while iter.valid() && iter.key().is_some() && work {
                        if iter.key() == Some(end) {
                            work = false;
                        }
                        let element =
                            raw_decode(iter.value().expect("if key exists then value should too"))?;
                        result.push(element);
                        iter.next();
                    }
                }
            }
        }
        Ok(result)
    }

    /// Insert an element in Merk under a key; path should be resolved and
    /// proper Merk should be loaded by this moment
    pub fn insert(
        &self,
        merk: &mut Merk<PrefixedRocksDbStorage>,
        key: Vec<u8>,
    ) -> Result<(), Error> {
        let batch =
            [(
                key,
                Op::Put(bincode::serialize(self).map_err(|_| {
                    Error::CorruptedData(String::from("unable to serialize element"))
                })?),
            )];
        merk.apply(&batch, &[])
            .map_err(|e| Error::CorruptedData(e.to_string()))
    }

    pub fn iterator(mut raw_iter: RawPrefixedIterator) -> ElementsIterator {
        raw_iter.seek_to_first();
        ElementsIterator { raw_iter }
    }
}

pub struct ElementsIterator<'a> {
    raw_iter: RawPrefixedIterator<'a>,
}

fn raw_decode(bytes: &[u8]) -> Result<Element, Error> {
    let tree = <Tree as Store>::decode(bytes).map_err(|e| Error::CorruptedData(e.to_string()))?;
    let element: Element = bincode::deserialize(tree.value())
        .map_err(|_| Error::CorruptedData(String::from("unable to deserialize element")))?;
    Ok(element)
}

impl<'a> ElementsIterator<'a> {
    pub fn next(&mut self) -> Result<Option<(Vec<u8>, Element)>, Error> {
        Ok(if self.raw_iter.valid() {
            if let Some((key, value)) = self.raw_iter.key().zip(self.raw_iter.value()) {
                let element = raw_decode(value)?;
                let key = key.to_vec();
                self.raw_iter.next();
                Some((key, element))
            } else {
                None
            }
        } else {
            None
        })
    }
}

#[cfg(test)]
mod tests {
    use merk::test_utils::TempMerk;

    use super::*;

    #[test]
    fn test_success_insert() {
        let mut merk = TempMerk::new();
        Element::empty_tree()
            .insert(&mut merk, b"mykey".to_vec())
            .expect("expected successful insertion");
        Element::Item(b"value".to_vec())
            .insert(&mut merk, b"another-key".to_vec())
            .expect("expected successful insertion 2");

        assert_eq!(
            Element::get(&merk, b"another-key").expect("expected successful get"),
            Element::Item(b"value".to_vec()),
        );
    }

    #[test]
    fn test_get_query() {
        let mut merk = TempMerk::new();
        Element::Item(b"ayyd".to_vec())
            .insert(&mut merk, b"d".to_vec())
            .expect("expected successful insertion");
        Element::Item(b"ayyc".to_vec())
            .insert(&mut merk, b"c".to_vec())
            .expect("expected successful insertion");
        Element::Item(b"ayya".to_vec())
            .insert(&mut merk, b"a".to_vec())
            .expect("expected successful insertion");
        Element::Item(b"ayyb".to_vec())
            .insert(&mut merk, b"b".to_vec())
            .expect("expected successful insertion");

        // Test queries by key
        let mut query = Query::new();
        query.insert_key(b"c".to_vec());
        query.insert_key(b"a".to_vec());
        assert_eq!(
            Element::get_query(&mut merk, &query).expect("expected successful get_query"),
            vec![
                Element::Item(b"ayya".to_vec()),
                Element::Item(b"ayyc".to_vec())
            ]
        );

        // Test range query
        let mut query = Query::new();
        query.insert_range(b"b".to_vec()..b"d".to_vec());
        query.insert_range(b"a".to_vec()..b"c".to_vec());
        assert_eq!(
            Element::get_query(&mut merk, &query).expect("expected successful get_query"),
            vec![
                Element::Item(b"ayya".to_vec()),
                Element::Item(b"ayyb".to_vec()),
                Element::Item(b"ayyc".to_vec())
            ]
        );

        // Test range inclusive query
        let mut query = Query::new();
        query.insert_range_inclusive(b"b".to_vec()..=b"d".to_vec());
        query.insert_range(b"b".to_vec()..b"c".to_vec());
        assert_eq!(
            Element::get_query(&mut merk, &query).expect("expected successful get_query"),
            vec![
                Element::Item(b"ayyb".to_vec()),
                Element::Item(b"ayyc".to_vec()),
                Element::Item(b"ayyd".to_vec())
            ]
        );

        // Test overlaps
        let mut query = Query::new();
        query.insert_key(b"a".to_vec());
        query.insert_range(b"b".to_vec()..b"d".to_vec());
        query.insert_range(b"a".to_vec()..b"c".to_vec());
        assert_eq!(
            Element::get_query(&mut merk, &query).expect("expected successful get_query"),
            vec![
                Element::Item(b"ayya".to_vec()),
                Element::Item(b"ayyb".to_vec()),
                Element::Item(b"ayyc".to_vec())
            ]
        );
    }
}
