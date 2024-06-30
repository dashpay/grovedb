// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Determines the query result form

use std::{
    collections::{BTreeMap, HashMap},
    vec::IntoIter,
};

pub use grovedb_merk::proofs::query::{Key, Path, PathKey};

use crate::{operations::proof::util::ProvedPathKeyValue, Element, Error};

#[derive(Copy, Clone)]
/// Query result type
pub enum QueryResultType {
    /// Query element result type
    QueryElementResultType,
    /// Query key element pair result type
    QueryKeyElementPairResultType,
    /// Query path key element trio result type
    QueryPathKeyElementTrioResultType,
}

/// Query result elements
#[derive(Debug, Clone)]
pub struct QueryResultElements {
    /// Elements
    pub elements: Vec<QueryResultElement>,
}

impl QueryResultElements {
    /// New
    pub fn new() -> Self {
        QueryResultElements { elements: vec![] }
    }

    /// From elements
    pub(crate) fn from_elements(elements: Vec<QueryResultElement>) -> Self {
        QueryResultElements { elements }
    }

    /// Length
    pub fn len(&self) -> usize {
        self.elements.len()
    }

    /// Is empty?
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// Into iterator
    pub fn into_iterator(self) -> IntoIter<QueryResultElement> {
        self.elements.into_iter()
    }

    /// To elements
    pub fn to_elements(self) -> Vec<Element> {
        self.elements
            .into_iter()
            .map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(element) => element,
                QueryResultElement::KeyElementPairResultItem(element_key_pair) => {
                    element_key_pair.1
                }
                QueryResultElement::PathKeyElementTrioResultItem(path_key_element_trio) => {
                    path_key_element_trio.2
                }
            })
            .collect()
    }

    /// To key elements
    pub fn to_key_elements(self) -> Vec<KeyElementPair> {
        self.elements
            .into_iter()
            .filter_map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(_) => None,
                QueryResultElement::KeyElementPairResultItem(key_element_pair) => {
                    Some(key_element_pair)
                }
                QueryResultElement::PathKeyElementTrioResultItem(path_key_element_trio) => {
                    Some((path_key_element_trio.1, path_key_element_trio.2))
                }
            })
            .collect()
    }

    /// To keys
    pub fn to_keys(self) -> Vec<Key> {
        self.elements
            .into_iter()
            .filter_map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(_) => None,
                QueryResultElement::KeyElementPairResultItem(key_element_pair) => {
                    Some(key_element_pair.0)
                }
                QueryResultElement::PathKeyElementTrioResultItem(path_key_element_trio) => {
                    Some(path_key_element_trio.1)
                }
            })
            .collect()
    }

    /// To key elements btree map
    pub fn to_key_elements_btree_map(self) -> BTreeMap<Vec<u8>, Element> {
        self.elements
            .into_iter()
            .filter_map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(_) => None,
                QueryResultElement::KeyElementPairResultItem(key_element_pair) => {
                    Some(key_element_pair)
                }
                QueryResultElement::PathKeyElementTrioResultItem(path_key_element_trio) => {
                    Some((path_key_element_trio.1, path_key_element_trio.2))
                }
            })
            .collect()
    }

    /// To key elements hash map
    pub fn to_key_elements_hash_map(self) -> HashMap<Vec<u8>, Element> {
        self.elements
            .into_iter()
            .filter_map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(_) => None,
                QueryResultElement::KeyElementPairResultItem(key_element_pair) => {
                    Some(key_element_pair)
                }
                QueryResultElement::PathKeyElementTrioResultItem(path_key_element_trio) => {
                    Some((path_key_element_trio.1, path_key_element_trio.2))
                }
            })
            .collect()
    }

    /// To path key elements
    pub fn to_path_key_elements(self) -> Vec<PathKeyElementTrio> {
        self.elements
            .into_iter()
            .filter_map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(_) => None,
                QueryResultElement::KeyElementPairResultItem(_) => None,
                QueryResultElement::PathKeyElementTrioResultItem(path_key_element_pair) => {
                    Some(path_key_element_pair)
                }
            })
            .collect()
    }

    /// To path key elements btree map
    pub fn to_path_key_elements_btree_map(self) -> BTreeMap<PathKey, Element> {
        self.elements
            .into_iter()
            .filter_map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(_) => None,
                QueryResultElement::KeyElementPairResultItem(_) => None,
                QueryResultElement::PathKeyElementTrioResultItem((path, key, element)) => {
                    Some(((path, key), element))
                }
            })
            .collect()
    }

    /// To last path to keys btree map
    /// This is useful if for example the element is a sum item and isn't
    /// important Used in Platform Drive for getting voters for multiple
    /// contenders
    pub fn to_last_path_to_keys_btree_map(self) -> BTreeMap<Key, Vec<Key>> {
        let mut map: BTreeMap<Vec<u8>, Vec<Key>> = BTreeMap::new();

        for result_item in self.elements.into_iter() {
            if let QueryResultElement::PathKeyElementTrioResultItem((mut path, key, _)) =
                result_item
            {
                if let Some(last) = path.pop() {
                    map.entry(last).or_insert_with(Vec::new).push(key);
                }
            }
        }

        map
    }

    /// To last path to key, elements btree map
    pub fn to_last_path_to_key_elements_btree_map(self) -> BTreeMap<Key, BTreeMap<Key, Element>> {
        let mut map: BTreeMap<Vec<u8>, BTreeMap<Key, Element>> = BTreeMap::new();

        for result_item in self.elements.into_iter() {
            if let QueryResultElement::PathKeyElementTrioResultItem((mut path, key, element)) =
                result_item
            {
                if let Some(last) = path.pop() {
                    map.entry(last)
                        .or_insert_with(BTreeMap::new)
                        .insert(key, element);
                }
            }
        }

        map
    }

    /// To last path to elements btree map
    /// This is useful if the key is not import
    pub fn to_last_path_to_elements_btree_map(self) -> BTreeMap<Key, Vec<Element>> {
        let mut map: BTreeMap<Vec<u8>, Vec<Element>> = BTreeMap::new();

        for result_item in self.elements.into_iter() {
            if let QueryResultElement::PathKeyElementTrioResultItem((mut path, _, element)) =
                result_item
            {
                if let Some(last) = path.pop() {
                    map.entry(last).or_insert_with(Vec::new).push(element);
                }
            }
        }

        map
    }

    /// To last path to keys btree map
    /// This is useful if for example the element is a sum item and isn't
    /// important Used in Platform Drive for getting voters for multiple
    /// contenders
    pub fn to_previous_of_last_path_to_keys_btree_map(self) -> BTreeMap<Key, Vec<Key>> {
        let mut map: BTreeMap<Vec<u8>, Vec<Key>> = BTreeMap::new();

        for result_item in self.elements.into_iter() {
            if let QueryResultElement::PathKeyElementTrioResultItem((mut path, key, _)) =
                result_item
            {
                if let Some(_) = path.pop() {
                    if let Some(last) = path.pop() {
                        map.entry(last).or_insert_with(Vec::new).push(key);
                    }
                }
            }
        }

        map
    }
}

impl Default for QueryResultElements {
    fn default() -> Self {
        Self::new()
    }
}

/// Query result element
#[derive(Debug, Clone)]
pub enum QueryResultElement {
    /// Element result item
    ElementResultItem(Element),
    /// Key element pair result item
    KeyElementPairResultItem(KeyElementPair),
    /// Path key element trio result item
    PathKeyElementTrioResultItem(PathKeyElementTrio),
}

#[cfg(feature = "full")]
impl QueryResultElement {
    /// Map element
    pub fn map_element(
        self,
        map_function: impl FnOnce(Element) -> Result<Element, Error>,
    ) -> Result<Self, Error> {
        Ok(match self {
            QueryResultElement::ElementResultItem(element) => {
                QueryResultElement::ElementResultItem(map_function(element)?)
            }
            QueryResultElement::KeyElementPairResultItem((key, element)) => {
                QueryResultElement::KeyElementPairResultItem((key, map_function(element)?))
            }
            QueryResultElement::PathKeyElementTrioResultItem((path, key, element)) => {
                QueryResultElement::PathKeyElementTrioResultItem((
                    path,
                    key,
                    map_function(element)?,
                ))
            }
        })
    }
}

#[cfg(any(feature = "full", feature = "verify"))]
/// Type alias for key-element common pattern.
pub type KeyElementPair = (Key, Element);

#[cfg(any(feature = "full", feature = "verify"))]
/// Type alias for key optional_element common pattern.
pub type KeyOptionalElementPair = (Key, Option<Element>);

#[cfg(any(feature = "full", feature = "verify"))]
/// Type alias for path-key-element common pattern.
pub type PathKeyElementTrio = (Path, Key, Element);

#[cfg(any(feature = "full", feature = "verify"))]
/// Type alias for path - key - optional_element common pattern.
pub type PathKeyOptionalElementTrio = (Path, Key, Option<Element>);

#[cfg(any(feature = "full", feature = "verify"))]
impl TryFrom<ProvedPathKeyValue> for PathKeyOptionalElementTrio {
    type Error = Error;

    fn try_from(proved_path_key_value: ProvedPathKeyValue) -> Result<Self, Self::Error> {
        let element = Element::deserialize(proved_path_key_value.value.as_slice())?;
        Ok((
            proved_path_key_value.path,
            proved_path_key_value.key,
            Some(element),
        ))
    }
}

#[cfg(feature = "full")]
#[cfg(test)]
mod tests {
    use crate::{
        operations::proof::util::ProvedPathKeyValue, query_result_type::PathKeyOptionalElementTrio,
        Element,
    };

    #[test]
    fn test_single_proved_path_key_value_to_path_key_optional_element() {
        let path = vec![b"1".to_vec(), b"2".to_vec()];
        let proved_path_key_value = ProvedPathKeyValue {
            path: path.clone(),
            key: b"a".to_vec(),
            value: vec![0, 1, 4, 0],
            proof: [0; 32],
        };
        let path_key_element_trio: PathKeyOptionalElementTrio = proved_path_key_value
            .try_into()
            .expect("should convert to path key optional element trio");
        assert_eq!(
            path_key_element_trio,
            (path, b"a".to_vec(), Some(Element::new_item(vec![4])))
        );
    }
}
