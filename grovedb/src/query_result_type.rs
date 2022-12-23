use std::{
    collections::{BTreeMap, HashMap},
    vec::IntoIter,
};

use crate::{Element, Error};

#[derive(Copy, Clone)]
pub enum QueryResultType {
    QueryElementResultType,
    QueryKeyElementPairResultType,
    QueryPathKeyElementTrioResultType,
}

pub struct QueryResultElements {
    pub elements: Vec<QueryResultElement>,
}

impl QueryResultElements {
    pub fn new() -> Self {
        QueryResultElements { elements: vec![] }
    }

    pub(crate) fn from_elements(elements: Vec<QueryResultElement>) -> Self {
        QueryResultElements { elements }
    }

    pub fn len(&self) -> usize {
        self.elements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    pub fn into_iterator(self) -> IntoIter<QueryResultElement> {
        self.elements.into_iter()
    }

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
}

impl Default for QueryResultElements {
    fn default() -> Self {
        Self::new()
    }
}

pub enum QueryResultElement {
    ElementResultItem(Element),
    KeyElementPairResultItem(KeyElementPair),
    PathKeyElementTrioResultItem(PathKeyElementTrio),
}

#[cfg(feature = "full")]
impl QueryResultElement {
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

#[cfg(feature = "full")]
/// Type alias for a path.
pub type Path = Vec<Vec<u8>>;

#[cfg(feature = "full")]
/// Type alias for a Key.
pub type Key = Vec<u8>;

#[cfg(feature = "full")]
/// Type alias for key-element common pattern.
pub type KeyElementPair = (Key, Element);

/// Type alias for key optional_element common pattern.
pub type KeyOptionalElementPair = (Key, Option<Element>);

#[cfg(feature = "full")]
/// Type alias for path-key common pattern.
pub type PathKey = (Path, Key);

/// Type alias for path-key-element common pattern.
pub type PathKeyElementTrio = (Path, Key, Element);

#[cfg(feature = "full")]
/// Type alias for path - key - optional_element common pattern.
pub type PathKeyOptionalElementTrio = (Path, Key, Option<Element>);
