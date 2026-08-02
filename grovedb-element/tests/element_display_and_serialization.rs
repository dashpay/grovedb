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
        (
            Element::ReferenceWithSumItem(
                grovedb_element::reference_path::ReferencePathType::SiblingReference(b"k".to_vec()),
                Some(4),
                42,
                Some(vec![21]),
            ),
            ElementType::ReferenceWithSumItem,
            "reference with sum item",
            "ReferenceWithSumItem(SiblingReference(6b), max_hop: 4, sum: 42, flags: [21])",
        ),
    ];

    for (element, expected_type, expected_type_str, expected_display) in values {
        assert_eq!(element.element_type(), expected_type);
        assert_eq!(element.type_str(), expected_type_str);
        assert_eq!(format!("{element}"), expected_display);
    }
}

/// Display + type helpers for the indexed-tree element variants
/// (PCIT, PSIT, PCPSIT) and wrappers (NonCounted, NotSummed,
/// NotCountedOrSummed). These arms are exercised here in addition
/// to the all-variant test above which omits them.
#[test]
fn element_display_indexed_tree_and_wrapper_variants() {
    // ProvableCountIndexedTree
    let pcit =
        Element::ProvableCountIndexedTree(Some(vec![0xAA]), Some(vec![0xBB]), 42, Some(vec![1, 2]));
    let s = format!("{pcit}");
    assert!(
        s.contains("ProvableCountIndexedTree")
            && s.contains("primary=aa")
            && s.contains("secondary=bb")
            && s.contains("count=42"),
        "Display: {}",
        s
    );

    let pcit_empty = Element::ProvableCountIndexedTree(None, None, 0, None);
    let s = format!("{pcit_empty}");
    assert!(s.contains("primary=None") && s.contains("secondary=None"));
    assert!(!s.contains("flags:"));

    // ProvableSumIndexedTree
    let psit =
        Element::ProvableSumIndexedTree(Some(vec![0xCC]), Some(vec![0xDD]), -77, Some(vec![5]));
    let s = format!("{psit}");
    assert!(
        s.contains("ProvableSumIndexedTree") && s.contains("primary=cc") && s.contains("sum=-77"),
        "Display: {}",
        s
    );

    // ProvableCountProvableSumIndexedTree (multi-axis)
    let pcpsit = Element::ProvableCountProvableSumIndexedTree(
        Some(vec![0xEE]),
        3,
        15,
        vec![(0u8, Some(vec![0x11])), (1u8, None)],
        None,
    );
    let s = format!("{pcpsit}");
    assert!(
        s.contains("ProvableCountProvableSumIndexedTree")
            && s.contains("count=3")
            && s.contains("sum=15")
            && s.contains("axes=[")
            && s.contains("(0, 11)")
            && s.contains("(1, None)"),
        "Display: {}",
        s
    );

    // Wrapper variants — Display delegates to inner.
    let nc = Element::NonCounted(Box::new(Element::Item(b"v".to_vec(), None)));
    assert_eq!(format!("{nc}"), "NonCounted(Item(v))");

    let ns = Element::NotSummed(Box::new(Element::SumTree(None, 5, None)));
    assert_eq!(format!("{ns}"), "NotSummed(SumTree(None, 5))");

    let ncs = Element::NotCountedOrSummed(Box::new(Element::SumTree(None, 7, None)));
    assert_eq!(format!("{ncs}"), "NotCountedOrSummed(SumTree(None, 7))");
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
        Element::new_reference_with_sum_item_with_max_hops_and_flags(
            grovedb_element::reference_path::ReferencePathType::AbsolutePathReference(vec![
                b"a".to_vec(),
                b"b".to_vec(),
            ]),
            Some(4),
            -42,
            Some(vec![7]),
        ),
        // ProvableSumTree (discriminant 19) — exercises the
        // `19 => Ok(ElementType::ProvableSumTree)` arm in
        // `TryFrom<u8>` via the round-trip through
        // `from_serialized_value`.
        Element::new_provable_sum_tree_with_flags_and_sum_value(
            Some(vec![19]),
            -77,
            Some(vec![19]),
        ),
        // ProvableCountProvableSumTree (discriminant 20) — exercises
        // the `20 => Ok(ElementType::ProvableCountProvableSumTree)`
        // arm and pins the wider on-disk allowlist in
        // `from_serialized_value`'s NonCounted branch.
        Element::new_provable_count_provable_sum_tree_with_flags_and_sum_and_count_value(
            Some(vec![20]),
            42,
            -13,
            Some(vec![20]),
        ),
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

    // Wrapper-discriminant + PCPS-inner-discriminant round-trips.
    // Each pins a specific `from_serialized_value` arm for the
    // `ProvableCountProvableSumTree` inner:
    //   * NonCounted(PCPS)              -> base-allowlist accepts inner=20
    //   * NotSummed(PCPS)               -> arm `20 => NotSummedProvableCountProvableSumTree`
    //   * NotCountedOrSummed(PCPS)      -> arm `20 => NotCountedOrSummedProvableCountProvableSumTree`
    // Same dual-axis coverage as the loop above but for the three
    // wrapper layers.
    let pcps_inner =
        Element::new_provable_count_provable_sum_tree_with_flags_and_sum_and_count_value(
            None, 9, -3, None,
        );
    let non_counted_pcps = Element::new_non_counted(pcps_inner.clone()).expect("wrap pcps");
    let nc_bytes = non_counted_pcps
        .serialize(grove_version)
        .expect("serialize");
    assert_eq!(
        ElementType::from_serialized_value(&nc_bytes).expect("parse NonCounted(PCPS)"),
        ElementType::NonCountedProvableCountProvableSumTree
    );

    let not_summed_pcps = Element::new_not_summed(pcps_inner.clone()).expect("wrap pcps");
    let ns_bytes = not_summed_pcps.serialize(grove_version).expect("serialize");
    assert_eq!(
        ElementType::from_serialized_value(&ns_bytes).expect("parse NotSummed(PCPS)"),
        ElementType::NotSummedProvableCountProvableSumTree
    );

    let ncs_pcps = Element::new_not_counted_or_summed(pcps_inner.clone()).expect("wrap pcps");
    let ncs_bytes = ncs_pcps.serialize(grove_version).expect("serialize");
    assert_eq!(
        ElementType::from_serialized_value(&ncs_bytes).expect("parse NotCountedOrSummed(PCPS)"),
        ElementType::NotCountedOrSummedProvableCountProvableSumTree
    );

    // Same wrapped-PCPS types round-trip through `TryFrom<u8>` —
    // exercises arms 849 / 853 / 865 in element_type.rs.
    assert_eq!(
        ElementType::try_from(148).expect("discriminant 148 -> NonCountedPCPS"),
        ElementType::NonCountedProvableCountProvableSumTree
    );
    assert_eq!(
        ElementType::try_from(178).expect("discriminant 178 -> NotSummedPCPS"),
        ElementType::NotSummedProvableCountProvableSumTree
    );
    assert_eq!(
        ElementType::try_from(194).expect("discriminant 194 -> NotCountedOrSummedPCPS"),
        ElementType::NotCountedOrSummedProvableCountProvableSumTree
    );

    // Error-path arms: pass a wrapper byte with an INVALID inner
    // discriminant. NotSummed and NotCountedOrSummed both reject
    // anything outside the sum-bearing-tree allowlist; the error
    // message must mention the allowlist.
    let bad_not_summed = ElementType::from_serialized_value(&[16, 0]).unwrap_err();
    assert!(
        matches!(&bad_not_summed, ElementError::CorruptedData(msg) if msg.contains("sum-bearing tree base type")),
        "expected NotSummed error mentioning sum-bearing tree base type; got {bad_not_summed:?}"
    );
    let bad_ncs = ElementType::from_serialized_value(&[17, 0]).unwrap_err();
    assert!(
        matches!(&bad_ncs, ElementError::CorruptedData(msg) if msg.contains("sum-bearing tree base")),
        "expected NotCountedOrSummed error mentioning sum-bearing tree base; got {bad_ncs:?}"
    );

    // type_str arms for the three wrapped-PCPS twins.
    assert_eq!(
        non_counted_pcps.type_str(),
        "non_counted provable count provable sum tree"
    );
    assert_eq!(
        not_summed_pcps.type_str(),
        "not_summed provable count provable sum tree"
    );
    assert_eq!(
        ncs_pcps.type_str(),
        "not_counted_or_summed provable count provable sum tree"
    );
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
        (
            Element::ReferenceWithSumItem(
                ReferencePathType::SiblingReference(b"k".to_vec()),
                None,
                7,
                None,
            ),
            "ReferenceWithSumItem(SiblingReference(6b), max_hop: None, sum: 7)",
        ),
        (
            Element::ProvableSumTree(None, 19, None),
            "ProvableSumTree(None, 19)",
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
