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

//! Query result type

use std::collections::{BTreeMap, HashMap};
#[cfg(feature = "full")]
use std::vec::IntoIter;

#[cfg(feature = "full")]
use crate::Element;

#[cfg(feature = "full")]
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

#[cfg(feature = "full")]
/// Query result elements
pub struct QueryResultElements {
    /// Elements
    pub elements: Vec<QueryResultElement>,
}

#[cfg(feature = "full")]
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
}

#[cfg(feature = "full")]
impl Default for QueryResultElements {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "full")]
/// Query result element
pub enum QueryResultElement {
    /// Element result item
    ElementResultItem(Element),
    /// Key element pair result item
    KeyElementPairResultItem(KeyElementPair),
    /// Path key element trio result item
    PathKeyElementTrioResultItem(PathKeyElementTrio),
}

#[cfg(feature = "full")]
/// Type alias for key-element common pattern.
pub type KeyElementPair = (Vec<u8>, Element);

#[cfg(feature = "full")]
/// Type alias for key optional_element common pattern.
pub type KeyOptionalElementPair = (Vec<u8>, Option<Element>);

#[cfg(feature = "full")]
/// Type alias for path-key-element common pattern.
pub type PathKeyElementTrio = (Vec<Vec<u8>>, Vec<u8>, Element);
