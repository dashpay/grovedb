//! Tests for query_result_type.rs conversion methods

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::{
        Element, Error,
        query_result_type::{
            BTreeMapLevelResult, BTreeMapLevelResultOrItem, QueryResultElement,
            QueryResultElements, QueryResultType,
        },
    };

    // ---------------------------------------------------------------------------
    // Helpers
    // ---------------------------------------------------------------------------

    /// Build a mixed `QueryResultElements` containing one of each variant type.
    fn mixed_elements() -> QueryResultElements {
        QueryResultElements::from_elements(vec![
            QueryResultElement::ElementResultItem(Element::new_item(vec![1])),
            QueryResultElement::KeyElementPairResultItem((
                b"key_a".to_vec(),
                Element::new_item(vec![2]),
            )),
            QueryResultElement::PathKeyElementTrioResultItem((
                vec![b"root".to_vec(), b"child".to_vec()],
                b"key_b".to_vec(),
                Element::new_item(vec![3]),
            )),
        ])
    }

    /// Build `QueryResultElements` with multiple PathKeyElementTrioResultItems
    /// that share path segments, useful for grouping tests.
    fn multi_path_elements() -> QueryResultElements {
        QueryResultElements::from_elements(vec![
            QueryResultElement::PathKeyElementTrioResultItem((
                vec![b"a".to_vec(), b"shared".to_vec()],
                b"k1".to_vec(),
                Element::new_item(vec![10]),
            )),
            QueryResultElement::PathKeyElementTrioResultItem((
                vec![b"a".to_vec(), b"shared".to_vec()],
                b"k2".to_vec(),
                Element::new_item(vec![20]),
            )),
            QueryResultElement::PathKeyElementTrioResultItem((
                vec![b"b".to_vec(), b"other".to_vec()],
                b"k3".to_vec(),
                Element::new_item(vec![30]),
            )),
        ])
    }

    // ---------------------------------------------------------------------------
    // to_elements
    // ---------------------------------------------------------------------------

    #[test]
    fn test_to_elements_extracts_from_all_variants() {
        let elems = mixed_elements().to_elements();
        assert_eq!(elems.len(), 3);
        assert_eq!(elems[0], Element::new_item(vec![1]));
        assert_eq!(elems[1], Element::new_item(vec![2]));
        assert_eq!(elems[2], Element::new_item(vec![3]));
    }

    #[test]
    fn test_to_elements_empty() {
        let elems = QueryResultElements::new().to_elements();
        assert!(
            elems.is_empty(),
            "to_elements on empty should return empty vec"
        );
    }

    // ---------------------------------------------------------------------------
    // to_key_elements
    // ---------------------------------------------------------------------------

    #[test]
    fn test_to_key_elements_filters_element_result_item() {
        let pairs = mixed_elements().to_key_elements();
        // ElementResultItem is filtered out, so only 2 remain
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], (b"key_a".to_vec(), Element::new_item(vec![2])));
        assert_eq!(pairs[1], (b"key_b".to_vec(), Element::new_item(vec![3])));
    }

    #[test]
    fn test_to_key_elements_only_element_result_items() {
        let qre = QueryResultElements::from_elements(vec![QueryResultElement::ElementResultItem(
            Element::new_item(vec![99]),
        )]);
        let pairs = qre.to_key_elements();
        assert!(
            pairs.is_empty(),
            "all ElementResultItems should be filtered out"
        );
    }

    // ---------------------------------------------------------------------------
    // to_keys
    // ---------------------------------------------------------------------------

    #[test]
    fn test_to_keys_filters_element_result_item() {
        let keys = mixed_elements().to_keys();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0], b"key_a".to_vec());
        assert_eq!(keys[1], b"key_b".to_vec());
    }

    #[test]
    fn test_to_keys_empty_when_only_element_result_items() {
        let qre = QueryResultElements::from_elements(vec![
            QueryResultElement::ElementResultItem(Element::new_item(vec![1])),
            QueryResultElement::ElementResultItem(Element::new_item(vec![2])),
        ]);
        let keys = qre.to_keys();
        assert!(
            keys.is_empty(),
            "should produce no keys from ElementResultItems"
        );
    }

    // ---------------------------------------------------------------------------
    // to_key_elements_btree_map
    // ---------------------------------------------------------------------------

    #[test]
    fn test_to_key_elements_btree_map() {
        let map = mixed_elements().to_key_elements_btree_map();
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get(b"key_a".as_slice()),
            Some(&Element::new_item(vec![2]))
        );
        assert_eq!(
            map.get(b"key_b".as_slice()),
            Some(&Element::new_item(vec![3]))
        );
    }

    // ---------------------------------------------------------------------------
    // to_key_elements_hash_map
    // ---------------------------------------------------------------------------

    #[test]
    fn test_to_key_elements_hash_map() {
        let map = mixed_elements().to_key_elements_hash_map();
        assert_eq!(map.len(), 2);
        assert_eq!(
            map.get(b"key_a".as_slice()),
            Some(&Element::new_item(vec![2]))
        );
        assert_eq!(
            map.get(b"key_b".as_slice()),
            Some(&Element::new_item(vec![3]))
        );
    }

    // ---------------------------------------------------------------------------
    // to_path_key_elements
    // ---------------------------------------------------------------------------

    #[test]
    fn test_to_path_key_elements_only_trio_variants() {
        let trios = mixed_elements().to_path_key_elements();
        // Only the PathKeyElementTrioResultItem survives
        assert_eq!(trios.len(), 1);
        assert_eq!(
            trios[0],
            (
                vec![b"root".to_vec(), b"child".to_vec()],
                b"key_b".to_vec(),
                Element::new_item(vec![3]),
            )
        );
    }

    #[test]
    fn test_to_path_key_elements_empty_when_no_trios() {
        let qre = QueryResultElements::from_elements(vec![
            QueryResultElement::ElementResultItem(Element::new_item(vec![1])),
            QueryResultElement::KeyElementPairResultItem((
                b"k".to_vec(),
                Element::new_item(vec![2]),
            )),
        ]);
        let trios = qre.to_path_key_elements();
        assert!(trios.is_empty(), "should produce no trios");
    }

    // ---------------------------------------------------------------------------
    // to_path_key_elements_btree_map
    // ---------------------------------------------------------------------------

    #[test]
    fn test_to_path_key_elements_btree_map() {
        let map = mixed_elements().to_path_key_elements_btree_map();
        assert_eq!(map.len(), 1);
        let path_key = (vec![b"root".to_vec(), b"child".to_vec()], b"key_b".to_vec());
        assert_eq!(map.get(&path_key), Some(&Element::new_item(vec![3])));
    }

    // ---------------------------------------------------------------------------
    // to_last_path_to_keys_btree_map
    // ---------------------------------------------------------------------------

    #[test]
    fn test_to_last_path_to_keys_btree_map() {
        let map = multi_path_elements().to_last_path_to_keys_btree_map();
        // Last path segment for first two is "shared", for the third is "other"
        assert_eq!(map.len(), 2);
        let shared_keys = map
            .get(b"shared".as_slice())
            .expect("should have 'shared' entry");
        assert_eq!(shared_keys.len(), 2);
        assert!(shared_keys.contains(&b"k1".to_vec()));
        assert!(shared_keys.contains(&b"k2".to_vec()));

        let other_keys = map
            .get(b"other".as_slice())
            .expect("should have 'other' entry");
        assert_eq!(other_keys, &vec![b"k3".to_vec()]);
    }

    #[test]
    fn test_to_last_path_to_keys_ignores_non_trio_items() {
        let map = mixed_elements().to_last_path_to_keys_btree_map();
        // Only one trio present with last path = "child"
        assert_eq!(map.len(), 1);
        let keys = map
            .get(b"child".as_slice())
            .expect("should have 'child' entry");
        assert_eq!(keys, &vec![b"key_b".to_vec()]);
    }

    #[test]
    fn test_to_last_path_to_keys_empty_path_ignored() {
        // A trio with an empty path: pop returns None, so it should be skipped
        let qre = QueryResultElements::from_elements(vec![
            QueryResultElement::PathKeyElementTrioResultItem((
                vec![],
                b"orphan".to_vec(),
                Element::new_item(vec![99]),
            )),
        ]);
        let map = qre.to_last_path_to_keys_btree_map();
        assert!(map.is_empty(), "empty path should be skipped");
    }

    // ---------------------------------------------------------------------------
    // to_path_to_key_elements_btree_map
    // ---------------------------------------------------------------------------

    #[test]
    fn test_to_path_to_key_elements_btree_map() {
        let map = multi_path_elements().to_path_to_key_elements_btree_map();
        // Two distinct full paths
        assert_eq!(map.len(), 2);

        let shared_path = vec![b"a".to_vec(), b"shared".to_vec()];
        let shared_inner = map.get(&shared_path).expect("should have shared path");
        assert_eq!(shared_inner.len(), 2);
        assert_eq!(
            shared_inner.get(b"k1".as_slice()),
            Some(&Element::new_item(vec![10]))
        );
        assert_eq!(
            shared_inner.get(&b"k2".to_vec()),
            Some(&Element::new_item(vec![20]))
        );

        let other_path = vec![b"b".to_vec(), b"other".to_vec()];
        let other_inner = map.get(&other_path).expect("should have other path");
        assert_eq!(other_inner.len(), 1);
        assert_eq!(
            other_inner.get(b"k3".as_slice()),
            Some(&Element::new_item(vec![30]))
        );
    }

    // ---------------------------------------------------------------------------
    // to_last_path_to_key_elements_btree_map
    // ---------------------------------------------------------------------------

    #[test]
    fn test_to_last_path_to_key_elements_btree_map() {
        let map = multi_path_elements().to_last_path_to_key_elements_btree_map();
        assert_eq!(map.len(), 2);

        let shared_inner = map
            .get(b"shared".as_slice())
            .expect("should have 'shared' key");
        assert_eq!(shared_inner.len(), 2);
        assert_eq!(
            shared_inner.get(b"k1".as_slice()),
            Some(&Element::new_item(vec![10]))
        );
        assert_eq!(
            shared_inner.get(&b"k2".to_vec()),
            Some(&Element::new_item(vec![20]))
        );

        let other_inner = map
            .get(b"other".as_slice())
            .expect("should have 'other' key");
        assert_eq!(other_inner.len(), 1);
        assert_eq!(
            other_inner.get(b"k3".as_slice()),
            Some(&Element::new_item(vec![30]))
        );
    }

    // ---------------------------------------------------------------------------
    // to_last_path_to_elements_btree_map
    // ---------------------------------------------------------------------------

    #[test]
    fn test_to_last_path_to_elements_btree_map() {
        let map = multi_path_elements().to_last_path_to_elements_btree_map();
        assert_eq!(map.len(), 2);

        let shared_elems = map
            .get(b"shared".as_slice())
            .expect("should have 'shared' key");
        assert_eq!(shared_elems.len(), 2);
        assert!(shared_elems.contains(&Element::new_item(vec![10])));
        assert!(shared_elems.contains(&Element::new_item(vec![20])));

        let other_elems = map
            .get(b"other".as_slice())
            .expect("should have 'other' key");
        assert_eq!(other_elems, &vec![Element::new_item(vec![30])]);
    }

    // ---------------------------------------------------------------------------
    // to_btree_map_level_results
    // ---------------------------------------------------------------------------

    #[test]
    fn test_to_btree_map_level_results() {
        let result = multi_path_elements().to_btree_map_level_results();
        // Top level: keys "a" and "b"
        assert_eq!(result.key_values.len(), 2);

        // Drill into "a" -> "shared" -> {k1, k2}
        let a_level = match result.key_values.get(b"a".as_slice()) {
            Some(BTreeMapLevelResultOrItem::BTreeMapLevelResult(inner)) => inner,
            other => panic!("expected BTreeMapLevelResult at 'a', got: {:?}", other),
        };
        let shared_level = match a_level.key_values.get(b"shared".as_slice()) {
            Some(BTreeMapLevelResultOrItem::BTreeMapLevelResult(inner)) => inner,
            other => panic!("expected BTreeMapLevelResult at 'shared', got: {:?}", other),
        };
        assert_eq!(shared_level.key_values.len(), 2);
        match shared_level.key_values.get(b"k1".as_slice()) {
            Some(BTreeMapLevelResultOrItem::ResultItem(elem)) => {
                assert_eq!(elem, &Element::new_item(vec![10]));
            }
            other => panic!("expected ResultItem at 'k1', got: {:?}", other),
        }

        // Drill into "b" -> "other" -> {k3}
        let b_level = match result.key_values.get(b"b".as_slice()) {
            Some(BTreeMapLevelResultOrItem::BTreeMapLevelResult(inner)) => inner,
            other => panic!("expected BTreeMapLevelResult at 'b', got: {:?}", other),
        };
        let other_level = match b_level.key_values.get(b"other".as_slice()) {
            Some(BTreeMapLevelResultOrItem::BTreeMapLevelResult(inner)) => inner,
            other => panic!("expected BTreeMapLevelResult at 'other', got: {:?}", other),
        };
        assert_eq!(other_level.key_values.len(), 1);
        match other_level.key_values.get(b"k3".as_slice()) {
            Some(BTreeMapLevelResultOrItem::ResultItem(elem)) => {
                assert_eq!(elem, &Element::new_item(vec![30]));
            }
            other => panic!("expected ResultItem at 'k3', got: {:?}", other),
        }
    }

    // ---------------------------------------------------------------------------
    // to_previous_of_last_path_to_keys_btree_map
    // ---------------------------------------------------------------------------

    #[test]
    fn test_to_previous_of_last_path_to_keys_btree_map() {
        let map = multi_path_elements().to_previous_of_last_path_to_keys_btree_map();
        // Paths: [a, shared] -> previous of last = "a"
        //        [b, other]  -> previous of last = "b"
        assert_eq!(map.len(), 2);
        let a_keys = map.get(b"a".as_slice()).expect("should have 'a' entry");
        assert_eq!(a_keys.len(), 2);
        assert!(a_keys.contains(&b"k1".to_vec()));
        assert!(a_keys.contains(&b"k2".to_vec()));

        let b_keys = map.get(b"b".as_slice()).expect("should have 'b' entry");
        assert_eq!(b_keys, &vec![b"k3".to_vec()]);
    }

    #[test]
    fn test_to_previous_of_last_path_single_segment_skipped() {
        // Path with only one segment: after popping last, path is empty, second pop
        // returns None.
        let qre = QueryResultElements::from_elements(vec![
            QueryResultElement::PathKeyElementTrioResultItem((
                vec![b"only".to_vec()],
                b"k".to_vec(),
                Element::new_item(vec![1]),
            )),
        ]);
        let map = qre.to_previous_of_last_path_to_keys_btree_map();
        assert!(
            map.is_empty(),
            "single-segment path should be skipped (no previous-of-last)"
        );
    }

    // ---------------------------------------------------------------------------
    // BTreeMapLevelResult::len_of_values_at_path
    // ---------------------------------------------------------------------------

    #[test]
    fn test_len_of_values_at_path_various() {
        let result = multi_path_elements().to_btree_map_level_results();

        // Root has 2 keys: "a" and "b"
        assert_eq!(result.len_of_values_at_path(&[]), 2);

        // "a" has 1 child: "shared"
        assert_eq!(result.len_of_values_at_path(&[b"a"]), 1);

        // "a" -> "shared" has 2 leaf items: k1, k2
        assert_eq!(result.len_of_values_at_path(&[b"a", b"shared"]), 2);

        // Non-existent path returns 0
        assert_eq!(result.len_of_values_at_path(&[b"nonexistent"]), 0);

        // Path that reaches a ResultItem before the end returns 0
        assert_eq!(
            result.len_of_values_at_path(&[b"a", b"shared", b"k1", b"deeper"]),
            0
        );
    }

    // ---------------------------------------------------------------------------
    // QueryResultElement::map_element
    // ---------------------------------------------------------------------------

    #[test]
    fn test_map_element_on_element_result_item() {
        let item = QueryResultElement::ElementResultItem(Element::new_item(vec![1]));
        let mapped = item
            .map_element(|_| Ok(Element::new_item(vec![99])))
            .expect("map_element should succeed");
        assert_eq!(
            mapped,
            QueryResultElement::ElementResultItem(Element::new_item(vec![99]))
        );
    }

    #[test]
    fn test_map_element_on_key_element_pair() {
        let item = QueryResultElement::KeyElementPairResultItem((
            b"my_key".to_vec(),
            Element::new_item(vec![1]),
        ));
        let mapped = item
            .map_element(|_| Ok(Element::new_item(vec![42])))
            .expect("map_element should succeed");
        assert_eq!(
            mapped,
            QueryResultElement::KeyElementPairResultItem((
                b"my_key".to_vec(),
                Element::new_item(vec![42]),
            ))
        );
    }

    #[test]
    fn test_map_element_on_path_key_element_trio() {
        let item = QueryResultElement::PathKeyElementTrioResultItem((
            vec![b"p".to_vec()],
            b"k".to_vec(),
            Element::new_item(vec![1]),
        ));
        let mapped = item
            .map_element(|_| Ok(Element::new_item(vec![77])))
            .expect("map_element should succeed");
        assert_eq!(
            mapped,
            QueryResultElement::PathKeyElementTrioResultItem((
                vec![b"p".to_vec()],
                b"k".to_vec(),
                Element::new_item(vec![77]),
            ))
        );
    }

    #[test]
    fn test_map_element_propagates_error() {
        let item = QueryResultElement::ElementResultItem(Element::new_item(vec![1]));
        let result = item.map_element(|_| Err(Error::InternalError("test error".to_string())));
        assert!(result.is_err(), "map_element should propagate the error");
    }

    // ---------------------------------------------------------------------------
    // Display impls
    // ---------------------------------------------------------------------------

    #[test]
    fn test_query_result_type_display() {
        let s = format!("{}", QueryResultType::QueryElementResultType);
        assert_eq!(s, "QueryElementResultType");

        let s = format!("{}", QueryResultType::QueryKeyElementPairResultType);
        assert_eq!(s, "QueryKeyElementPairResultType");

        let s = format!("{}", QueryResultType::QueryPathKeyElementTrioResultType);
        assert_eq!(s, "QueryPathKeyElementTrioResultType");
    }

    #[test]
    fn test_query_result_elements_display() {
        let qre = QueryResultElements::from_elements(vec![QueryResultElement::ElementResultItem(
            Element::new_item(vec![1]),
        )]);
        let s = format!("{}", qre);
        assert!(
            s.contains("QueryResultElements"),
            "display should contain type name"
        );
        assert!(
            s.contains("ElementResultItem"),
            "display should contain variant name"
        );
    }

    #[test]
    fn test_query_result_element_display_all_variants() {
        // ElementResultItem
        let s = format!(
            "{}",
            QueryResultElement::ElementResultItem(Element::new_item(vec![1]))
        );
        assert!(s.starts_with("ElementResultItem"));

        // KeyElementPairResultItem — key "abc" is printable ASCII
        let s = format!(
            "{}",
            QueryResultElement::KeyElementPairResultItem((
                b"abc".to_vec(),
                Element::new_item(vec![2]),
            ))
        );
        assert!(s.contains("KeyElementPairResultItem"));
        assert!(s.contains("abc"));

        // PathKeyElementTrioResultItem
        let s = format!(
            "{}",
            QueryResultElement::PathKeyElementTrioResultItem((
                vec![b"root".to_vec()],
                b"key".to_vec(),
                Element::new_item(vec![3]),
            ))
        );
        assert!(s.contains("PathKeyElementTrioResultItem"));
        assert!(s.contains("root"));
        assert!(s.contains("key"));
    }

    #[test]
    fn test_btree_map_level_result_display() {
        let result = multi_path_elements().to_btree_map_level_results();
        let s = format!("{}", result);
        assert!(
            s.contains("BTreeMapLevelResult"),
            "display should contain type name"
        );
    }

    #[test]
    fn test_btree_map_level_result_or_item_display() {
        let item_variant = BTreeMapLevelResultOrItem::ResultItem(Element::new_item(vec![1]));
        let s = format!("{}", item_variant);
        // Should delegate to Element's Display
        assert!(!s.is_empty(), "ResultItem display should not be empty");

        let level_variant = BTreeMapLevelResultOrItem::BTreeMapLevelResult(BTreeMapLevelResult {
            key_values: BTreeMap::new(),
        });
        let s = format!("{}", level_variant);
        assert!(
            s.contains("BTreeMapLevelResult"),
            "should display as BTreeMapLevelResult"
        );
    }

    // ---------------------------------------------------------------------------
    // len / is_empty / default
    // ---------------------------------------------------------------------------

    #[test]
    fn test_query_result_elements_len_and_is_empty() {
        let empty = QueryResultElements::new();
        assert!(empty.is_empty(), "new() should be empty");
        assert_eq!(empty.len(), 0);

        let non_empty = mixed_elements();
        assert!(!non_empty.is_empty());
        assert_eq!(non_empty.len(), 3);
    }

    #[test]
    fn test_query_result_elements_default() {
        let default_qre = QueryResultElements::default();
        assert!(default_qre.is_empty(), "default should be empty");
    }
}
