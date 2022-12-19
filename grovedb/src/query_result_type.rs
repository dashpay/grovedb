#[cfg(feature = "full")]
use std::vec::IntoIter;

#[cfg(feature = "full")]
use crate::Element;

#[cfg(feature = "full")]
#[derive(Copy, Clone)]
pub enum QueryResultType {
    QueryElementResultType,
    QueryKeyElementPairResultType,
    QueryPathKeyElementTrioResultType,
}

#[cfg(feature = "full")]
pub struct QueryResultElements {
    pub elements: Vec<QueryResultElement>,
}

#[cfg(feature = "full")]
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

    pub fn into_iter(self) -> IntoIter<QueryResultElement> {
        self.elements.into_iter()
    }

    pub fn to_elements(self) -> Vec<Element> {
        self.elements
            .into_iter()
            .filter_map(|result_item| match result_item {
                QueryResultElement::ElementResultItem(element) => Some(element),
                QueryResultElement::KeyElementPairResultItem(element_key_pair) => {
                    Some(element_key_pair.1)
                }
                QueryResultElement::PathKeyElementTrioResultItem(path_key_element_trio) => {
                    Some(path_key_element_trio.2)
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
pub enum QueryResultElement {
    ElementResultItem(Element),
    KeyElementPairResultItem(KeyElementPair),
    PathKeyElementTrioResultItem(PathKeyElementTrio),
}

#[cfg(feature = "full")]
/// Type alias for key-element common pattern.
pub type KeyElementPair = (Vec<u8>, Element);

#[cfg(feature = "full")]
/// Type alias for path-key-element common pattern.
pub type PathKeyElementTrio = (Vec<Vec<u8>>, Vec<u8>, Element);
