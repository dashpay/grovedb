use grovedb_element::{error::ElementError, Element, ElementType};
use grovedb_version::version::GroveVersion;

#[test]
fn element_display_and_type_helpers_cover_all_variants() {
    let values = vec![
        (
            Element::Item(b"abc".to_vec(), Some(vec![1])),
            ElementType::Item,
            "item",
            "Item(abc, flags: [1])",
        ),
        (
            Element::Reference(
                grovedb_element::reference_path::ReferencePathType::SiblingReference(b"k".to_vec()),
                Some(2),
                Some(vec![3]),
            ),
            ElementType::Reference,
            "reference",
            "Reference(SiblingReference(6b), max_hop: 2, flags: [3])",
        ),
        (
            Element::Tree(Some(vec![0xab, 0xcd]), Some(vec![4])),
            ElementType::Tree,
            "tree",
            "Tree(abcd, flags: [4])",
        ),
        (
            Element::SumItem(-1, Some(vec![5])),
            ElementType::SumItem,
            "sum item",
            "SumItem(-1, flags: [5])",
        ),
        (
            Element::SumTree(Some(vec![1]), 2, Some(vec![6])),
            ElementType::SumTree,
            "sum tree",
            "SumTree(01, 2, flags: [6])",
        ),
        (
            Element::BigSumTree(Some(vec![2]), 3, Some(vec![7])),
            ElementType::BigSumTree,
            "big sum tree",
            "BigSumTree(02, 3, flags: [7])",
        ),
        (
            Element::CountTree(Some(vec![3]), 4, Some(vec![8])),
            ElementType::CountTree,
            "count tree",
            "CountTree(03, 4, flags: [8])",
        ),
        (
            Element::CountSumTree(Some(vec![4]), 5, 6, Some(vec![9])),
            ElementType::CountSumTree,
            "count sum tree",
            "CountSumTree(04, 5, 6, flags: [9])",
        ),
        (
            Element::ProvableCountTree(Some(vec![5]), 7, Some(vec![10])),
            ElementType::ProvableCountTree,
            "provable count tree",
            "ProvableCountTree(05, 7, flags: [10])",
        ),
        (
            Element::ItemWithSumItem(b"xyz".to_vec(), 8, Some(vec![11])),
            ElementType::ItemWithSumItem,
            "item with sum item",
            "ItemWithSumItem(xyz , 8, flags: [11])",
        ),
        (
            Element::ProvableCountSumTree(Some(vec![6]), 9, 10, Some(vec![12])),
            ElementType::ProvableCountSumTree,
            "provable count sum tree",
            "ProvableCountSumTree(06, 9, 10, flags: [12])",
        ),
        (
            Element::CommitmentTree(11, 12, Some(vec![13])),
            ElementType::CommitmentTree,
            "commitment tree",
            "CommitmentTree(count: 11, chunk_power: 12, flags: [13])",
        ),
        (
            Element::MmrTree(13, Some(vec![14])),
            ElementType::MmrTree,
            "mmr tree",
            "MmrTree(mmr_size: 13, flags: [14])",
        ),
        (
            Element::BulkAppendTree(14, 15, Some(vec![16])),
            ElementType::BulkAppendTree,
            "bulk_append_tree",
            "BulkAppendTree(total_count: 14, chunk_power: 15, flags: [16])",
        ),
        (
            Element::DenseAppendOnlyFixedSizeTree(17, 18, Some(vec![19])),
            ElementType::DenseAppendOnlyFixedSizeTree,
            "dense_tree",
            "DenseAppendOnlyFixedSizeTree(count: 17, height: 18, flags: [19])",
        ),
    ];

    for (element, expected_type, expected_type_str, expected_display) in values {
        assert_eq!(element.element_type(), expected_type);
        assert_eq!(element.type_str(), expected_type_str);
        assert_eq!(format!("{element}"), expected_display);
    }
}

#[test]
fn serialize_deserialize_round_trip_all_element_types_and_errors() {
    let grove_version = GroveVersion::latest();
    let elements = vec![
        Element::new_item_with_flags(vec![1, 2, 3], Some(vec![9])),
        Element::new_reference_with_max_hops_and_flags(
            grovedb_element::reference_path::ReferencePathType::UpstreamFromElementHeightReference(
                1,
                vec![b"x".to_vec()],
            ),
            Some(3),
            Some(vec![8]),
        ),
        Element::new_tree_with_flags(Some(vec![1]), Some(vec![7])),
        Element::new_sum_item_with_flags(-9, Some(vec![6])),
        Element::new_sum_tree_with_flags_and_sum_value(Some(vec![2]), 7, Some(vec![5])),
        Element::new_big_sum_tree_with_flags_and_sum_value(
            Some(vec![3]),
            123_456_789,
            Some(vec![4]),
        ),
        Element::new_count_tree_with_flags_and_count_value(Some(vec![4]), 7, Some(vec![3])),
        Element::new_count_sum_tree_with_flags_and_sum_and_count_value(
            Some(vec![5]),
            8,
            -7,
            Some(vec![2]),
        ),
        Element::new_provable_count_tree_with_flags_and_count_value(
            Some(vec![6]),
            9,
            Some(vec![1]),
        ),
        Element::new_item_with_sum_item_with_flags(vec![7], 10, Some(vec![0])),
        Element::new_provable_count_sum_tree_with_flags_and_sum_and_count_value(
            Some(vec![8]),
            11,
            -12,
            Some(vec![12]),
        ),
        Element::new_commitment_tree(12, 5, Some(vec![11])),
        Element::new_mmr_tree(13, Some(vec![10])),
        Element::new_bulk_append_tree(14, 6, Some(vec![9])),
        Element::new_dense_tree(15, 7, Some(vec![8])),
    ];

    for element in elements {
        let serialized = element.serialize(grove_version).unwrap();
        let size = element.serialized_size(grove_version).unwrap();
        assert_eq!(serialized.len(), size);

        let deserialized = Element::deserialize(&serialized, grove_version).unwrap();
        assert_eq!(deserialized, element);

        let parsed_type = ElementType::from_serialized_value(&serialized).unwrap();
        assert_eq!(parsed_type, element.element_type());
    }

    let empty_err = ElementType::from_serialized_value(&[]).unwrap_err();
    assert!(matches!(
        empty_err,
        ElementError::CorruptedData(msg) if msg.contains("empty value")
    ));

    let type_err = ElementType::try_from(255).unwrap_err();
    assert!(matches!(
        type_err,
        ElementError::CorruptedData(msg) if msg.contains("Unknown element type discriminant")
    ));

    let deserialize_err = Element::deserialize(&[255, 1, 2], grove_version).unwrap_err();
    assert!(matches!(
        deserialize_err,
        ElementError::CorruptedData(msg) if msg.contains("unable to deserialize element")
    ));
}

/// Covers the `None` flags branch in Display for all 15 variants,
/// the `None` max_hop branch for Reference, and the `None` root_key
/// branch for tree-type variants.
#[test]
fn element_display_without_flags_covers_none_branches() {
    use grovedb_element::reference_path::ReferencePathType;

    let values: Vec<(Element, &str)> = vec![
        // Uses non-allowed bytes to cover hex_to_ascii's else (hex) branch
        (Element::Item(vec![0x00, 0x01], None), "Item(0x0001)"),
        (
            Element::Reference(
                ReferencePathType::SiblingReference(b"k".to_vec()),
                None,
                None,
            ),
            "Reference(SiblingReference(6b), max_hop: None)",
        ),
        (Element::Tree(None, None), "Tree(None)"),
        (Element::SumItem(-1, None), "SumItem(-1)"),
        (Element::SumTree(None, 2, None), "SumTree(None, 2)"),
        (Element::BigSumTree(None, 3, None), "BigSumTree(None, 3)"),
        (Element::CountTree(None, 4, None), "CountTree(None, 4)"),
        (
            Element::CountSumTree(None, 5, 6, None),
            "CountSumTree(None, 5, 6)",
        ),
        (
            Element::ProvableCountTree(None, 7, None),
            "ProvableCountTree(None, 7)",
        ),
        (
            Element::ItemWithSumItem(b"xyz".to_vec(), 8, None),
            "ItemWithSumItem(xyz , 8)",
        ),
        (
            Element::ProvableCountSumTree(None, 9, 10, None),
            "ProvableCountSumTree(None, 9, 10)",
        ),
        (
            Element::CommitmentTree(11, 12, None),
            "CommitmentTree(count: 11, chunk_power: 12)",
        ),
        (Element::MmrTree(13, None), "MmrTree(mmr_size: 13)"),
        (
            Element::BulkAppendTree(14, 15, None),
            "BulkAppendTree(total_count: 14, chunk_power: 15)",
        ),
        (
            Element::DenseAppendOnlyFixedSizeTree(17, 18, None),
            "DenseAppendOnlyFixedSizeTree(count: 17, height: 18)",
        ),
    ];

    for (element, expected_display) in values {
        let display = format!("{element}");
        assert_eq!(display, expected_display);
        assert!(
            !display.contains("flags:"),
            "Display for {:?} should not contain 'flags:' when flags is None, got: {}",
            element.type_str(),
            display
        );
    }
}
