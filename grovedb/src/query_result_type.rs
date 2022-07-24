use crate::Element;

#[derive(Copy, Clone)]
pub enum QueryResultType {
    QueryElementResultType,
    QueryKeyElementPairResultType,
    QueryPathKeyElementTrioResultType,
}

pub type QueryResultItems = Vec<QueryResultItem>;

pub trait GetItemResults {
    fn to_elements(self) -> Vec<Element>;
    fn to_key_elements(self) -> Vec<KeyElementPair>;
    fn to_path_key_elements(self) -> Vec<PathKeyElementTrio>;
}

impl GetItemResults for QueryResultItems {
    fn to_elements(self) -> Vec<Element> {
        query_result_items_to_elements(self)
    }

    fn to_key_elements(self) -> Vec<KeyElementPair> {
        query_result_items_to_key_elements(self)
    }

    fn to_path_key_elements(self) -> Vec<PathKeyElementTrio> {
        query_result_items_to_path_key_elements(self)
    }
}

fn query_result_items_to_elements(query_result_items: QueryResultItems) -> Vec<Element> {
    query_result_items
        .into_iter()
        .filter_map(|result_item| match result_item {
            QueryResultItem::ElementResultItem(element) => Some(element),
            QueryResultItem::KeyElementPairResultItem(element_key_pair) => Some(element_key_pair.1),
            QueryResultItem::PathKeyElementTrioResultItem(path_key_element_trio) => {
                Some(path_key_element_trio.2)
            }
        })
        .collect()
}

fn query_result_items_to_key_elements(query_result_items: QueryResultItems) -> Vec<KeyElementPair> {
    query_result_items
        .into_iter()
        .filter_map(|result_item| match result_item {
            QueryResultItem::ElementResultItem(_) => None,
            QueryResultItem::KeyElementPairResultItem(key_element_pair) => Some(key_element_pair),
            QueryResultItem::PathKeyElementTrioResultItem(path_key_element_trio) => {
                Some((path_key_element_trio.1, path_key_element_trio.2))
            }
        })
        .collect()
}

fn query_result_items_to_path_key_elements(
    query_result_items: QueryResultItems,
) -> Vec<PathKeyElementTrio> {
    query_result_items
        .into_iter()
        .filter_map(|result_item| match result_item {
            QueryResultItem::ElementResultItem(_) => None,
            QueryResultItem::KeyElementPairResultItem(_) => None,
            QueryResultItem::PathKeyElementTrioResultItem(path_key_element_pair) => {
                Some(path_key_element_pair)
            }
        })
        .collect()
}

pub enum QueryResultItem {
    ElementResultItem(Element),
    KeyElementPairResultItem(KeyElementPair),
    PathKeyElementTrioResultItem(PathKeyElementTrio),
}

/// Type alias for key-element common pattern.
pub type KeyElementPair = (Vec<u8>, Element);

/// Type alias for path-key-element common pattern.
pub type PathKeyElementTrio = (Vec<Vec<u8>>, Vec<u8>, Element);
