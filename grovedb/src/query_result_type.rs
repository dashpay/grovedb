use crate::Element;

#[derive(Copy, Clone)]
pub enum QueryResultType {
    QueryElementResultType,
    QueryKeyElementPairResultType,
    QueryPathKeyElementTrioResultType,
}

pub type QueryResultItems = Vec<QueryResultItem>;

pub fn query_result_items_to_elements(query_result_items: QueryResultItems) -> Vec<Element> {
    query_result_items
        .into_iter()
        .filter_map(|result_item| match result_item {
            QueryResultItem::ElementResultItem(element) => Some(element),
            QueryResultItem::KeyElementPairResultItem(_) => None,
            QueryResultItem::PathKeyElementTrioResultItem(_) => None,
        })
        .collect()
}

pub fn query_result_items_to_key_elements(
    query_result_items: QueryResultItems,
) -> Vec<KeyElementPair> {
    query_result_items
        .into_iter()
        .filter_map(|result_item| match result_item {
            QueryResultItem::ElementResultItem(_) => None,
            QueryResultItem::KeyElementPairResultItem(key_element_pair) => Some(key_element_pair),
            QueryResultItem::PathKeyElementTrioResultItem(_) => None,
        })
        .collect()
}

pub fn query_result_items_to_path_key_elements(
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
