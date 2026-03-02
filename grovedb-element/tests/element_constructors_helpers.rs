use std::panic;

use grovedb_element::{
    error::ElementError, reference_path::ReferencePathType, CountValue, Element, ElementFlags,
    SumValue,
};
use grovedb_version::version::GroveVersion;
use integer_encoding::VarInt;

fn sample_flags() -> Option<ElementFlags> {
    Some(vec![9, 8, 7])
}

#[test]
fn constructors_create_expected_tree_variants() {
    // Empty tree constructors
    assert_eq!(Element::empty_tree(), Element::Tree(None, None));
    assert_eq!(
        Element::empty_tree_with_flags(sample_flags()),
        Element::Tree(None, sample_flags())
    );

    // Non-empty tree constructors
    assert_eq!(
        Element::new_tree(Some(vec![1, 2, 3])),
        Element::Tree(Some(vec![1, 2, 3]), None)
    );
    assert_eq!(
        Element::new_tree_with_flags(Some(vec![2]), sample_flags()),
        Element::Tree(Some(vec![2]), sample_flags())
    );
}

#[test]
fn constructors_create_expected_sum_and_big_sum_tree_variants() {
    // Empty sum tree constructors
    assert_eq!(Element::empty_sum_tree(), Element::SumTree(None, 0, None));
    assert_eq!(
        Element::empty_sum_tree_with_flags(sample_flags()),
        Element::SumTree(None, 0, sample_flags())
    );

    // Non-empty sum tree constructors
    assert_eq!(
        Element::new_sum_tree(Some(vec![3])),
        Element::SumTree(Some(vec![3]), 0, None)
    );
    assert_eq!(
        Element::new_sum_tree_with_flags(Some(vec![3]), sample_flags()),
        Element::SumTree(Some(vec![3]), 0, sample_flags())
    );
    assert_eq!(
        Element::new_sum_tree_with_flags_and_sum_value(Some(vec![3]), 11, sample_flags()),
        Element::SumTree(Some(vec![3]), 11, sample_flags())
    );

    // Empty big sum tree constructors
    assert_eq!(
        Element::empty_big_sum_tree(),
        Element::BigSumTree(None, 0, None)
    );
    assert_eq!(
        Element::empty_big_sum_tree_with_flags(sample_flags()),
        Element::BigSumTree(None, 0, sample_flags())
    );

    // Non-empty big sum tree constructors
    assert_eq!(
        Element::new_big_sum_tree(Some(vec![4])),
        Element::BigSumTree(Some(vec![4]), 0, None)
    );
    assert_eq!(
        Element::new_big_sum_tree_with_flags(Some(vec![4]), sample_flags()),
        Element::BigSumTree(Some(vec![4]), 0, sample_flags())
    );
    assert_eq!(
        Element::new_big_sum_tree_with_flags_and_sum_value(
            Some(vec![4]),
            i128::from(i64::MAX) + 1,
            sample_flags()
        ),
        Element::BigSumTree(Some(vec![4]), i128::from(i64::MAX) + 1, sample_flags())
    );
}

#[test]
fn constructors_create_expected_count_and_count_sum_tree_variants() {
    // Empty count tree constructors
    assert_eq!(
        Element::empty_count_tree(),
        Element::CountTree(None, 0, None)
    );
    assert_eq!(
        Element::empty_count_tree_with_flags(sample_flags()),
        Element::CountTree(None, 0, sample_flags())
    );

    // Non-empty count tree constructors
    assert_eq!(
        Element::new_count_tree(Some(vec![5])),
        Element::CountTree(Some(vec![5]), 0, None)
    );
    assert_eq!(
        Element::new_count_tree_with_flags(Some(vec![5]), sample_flags()),
        Element::CountTree(Some(vec![5]), 0, sample_flags())
    );
    assert_eq!(
        Element::new_count_tree_with_flags_and_count_value(Some(vec![5]), 9, sample_flags()),
        Element::CountTree(Some(vec![5]), 9, sample_flags())
    );

    // Empty count sum tree constructors
    assert_eq!(
        Element::empty_count_sum_tree(),
        Element::CountSumTree(None, 0, 0, None)
    );
    assert_eq!(
        Element::empty_count_sum_tree_with_flags(sample_flags()),
        Element::CountSumTree(None, 0, 0, sample_flags())
    );

    // Non-empty count sum tree constructors
    assert_eq!(
        Element::new_count_sum_tree(Some(vec![6])),
        Element::CountSumTree(Some(vec![6]), 0, 0, None)
    );
    assert_eq!(
        Element::new_count_sum_tree_with_flags(Some(vec![6]), sample_flags()),
        Element::CountSumTree(Some(vec![6]), 0, 0, sample_flags())
    );
    assert_eq!(
        Element::new_count_sum_tree_with_flags_and_sum_and_count_value(
            Some(vec![6]),
            2,
            -3,
            sample_flags()
        ),
        Element::CountSumTree(Some(vec![6]), 2, -3, sample_flags())
    );
}

#[test]
fn constructors_create_expected_item_and_sum_item_variants() {
    assert_eq!(
        Element::new_item(vec![1, 2]),
        Element::Item(vec![1, 2], None)
    );
    assert_eq!(
        Element::new_item_with_flags(vec![1, 2], sample_flags()),
        Element::Item(vec![1, 2], sample_flags())
    );
    assert_eq!(Element::new_sum_item(12), Element::SumItem(12, None));
    assert_eq!(
        Element::new_sum_item_with_flags(-12, sample_flags()),
        Element::SumItem(-12, sample_flags())
    );
    assert_eq!(
        Element::new_item_with_sum_item(vec![1], 44),
        Element::ItemWithSumItem(vec![1], 44, None)
    );
    assert_eq!(
        Element::new_item_with_sum_item_with_flags(vec![1], -44, sample_flags()),
        Element::ItemWithSumItem(vec![1], -44, sample_flags())
    );
}

#[test]
fn constructors_create_expected_reference_variants() {
    let ref_path = ReferencePathType::SiblingReference(vec![5]);
    assert_eq!(
        Element::new_reference(ref_path.clone()),
        Element::Reference(ref_path.clone(), None, None)
    );
    assert_eq!(
        Element::new_reference_with_flags(ref_path.clone(), sample_flags()),
        Element::Reference(ref_path.clone(), None, sample_flags())
    );
    assert_eq!(
        Element::new_reference_with_hops(ref_path.clone(), Some(5)),
        Element::Reference(ref_path.clone(), Some(5), None)
    );
    assert_eq!(
        Element::new_reference_with_max_hops_and_flags(ref_path, Some(6), sample_flags()),
        Element::Reference(
            ReferencePathType::SiblingReference(vec![5]),
            Some(6),
            sample_flags()
        )
    );
}

#[test]
fn constructors_create_expected_provable_tree_variants() {
    // Provable count tree constructors
    assert_eq!(
        Element::empty_provable_count_tree(),
        Element::ProvableCountTree(None, 0, None)
    );
    assert_eq!(
        Element::empty_provable_count_tree_with_flags(sample_flags()),
        Element::ProvableCountTree(None, 0, sample_flags())
    );
    assert_eq!(
        Element::new_provable_count_tree(Some(vec![7])),
        Element::ProvableCountTree(Some(vec![7]), 0, None)
    );
    assert_eq!(
        Element::new_provable_count_tree_with_flags(Some(vec![7]), sample_flags()),
        Element::ProvableCountTree(Some(vec![7]), 0, sample_flags())
    );
    assert_eq!(
        Element::new_provable_count_tree_with_flags_and_count_value(
            Some(vec![7]),
            12,
            sample_flags()
        ),
        Element::ProvableCountTree(Some(vec![7]), 12, sample_flags())
    );

    // Provable count sum tree constructors
    assert_eq!(
        Element::empty_provable_count_sum_tree(),
        Element::ProvableCountSumTree(None, 0, 0, None)
    );
    assert_eq!(
        Element::empty_provable_count_sum_tree_with_flags(sample_flags()),
        Element::ProvableCountSumTree(None, 0, 0, sample_flags())
    );
    assert_eq!(
        Element::new_provable_count_sum_tree(Some(vec![8])),
        Element::ProvableCountSumTree(Some(vec![8]), 0, 0, None)
    );
    assert_eq!(
        Element::new_provable_count_sum_tree_with_flags(Some(vec![8]), sample_flags()),
        Element::ProvableCountSumTree(Some(vec![8]), 0, 0, sample_flags())
    );
    assert_eq!(
        Element::new_provable_count_sum_tree_with_flags_and_sum_and_count_value(
            Some(vec![8]),
            7,
            -9,
            sample_flags()
        ),
        Element::ProvableCountSumTree(Some(vec![8]), 7, -9, sample_flags())
    );
}

#[test]
fn constructors_create_expected_commitment_mmr_bulk_dense_variants() {
    // Commitment tree constructors
    assert_eq!(
        Element::empty_commitment_tree(4),
        Element::CommitmentTree(0, 4, None)
    );
    assert_eq!(
        Element::empty_commitment_tree_with_flags(4, sample_flags()),
        Element::CommitmentTree(0, 4, sample_flags())
    );
    assert_eq!(
        Element::new_commitment_tree(11, 5, sample_flags()),
        Element::CommitmentTree(11, 5, sample_flags())
    );

    // MMR tree constructors
    assert_eq!(Element::empty_mmr_tree(), Element::MmrTree(0, None));
    assert_eq!(
        Element::empty_mmr_tree_with_flags(sample_flags()),
        Element::MmrTree(0, sample_flags())
    );
    assert_eq!(
        Element::new_mmr_tree(17, sample_flags()),
        Element::MmrTree(17, sample_flags())
    );

    // Bulk append tree constructors
    assert_eq!(
        Element::empty_bulk_append_tree(6),
        Element::BulkAppendTree(0, 6, None)
    );
    assert_eq!(
        Element::empty_bulk_append_tree_with_flags(6, sample_flags()),
        Element::BulkAppendTree(0, 6, sample_flags())
    );
    assert_eq!(
        Element::new_bulk_append_tree(99, 7, sample_flags()),
        Element::BulkAppendTree(99, 7, sample_flags())
    );

    // Dense tree constructors
    assert_eq!(
        Element::empty_dense_tree(9),
        Element::DenseAppendOnlyFixedSizeTree(0, 9, None)
    );
    assert_eq!(
        Element::empty_dense_tree_with_flags(9, sample_flags()),
        Element::DenseAppendOnlyFixedSizeTree(0, 9, sample_flags())
    );
    assert_eq!(
        Element::new_dense_tree(13, 9, sample_flags()),
        Element::DenseAppendOnlyFixedSizeTree(13, 9, sample_flags())
    );
}

#[test]
fn constructors_enforce_chunk_power_bounds() {
    let commitment = panic::catch_unwind(|| Element::empty_commitment_tree(32));
    assert!(commitment.is_err());

    let commitment_flags =
        panic::catch_unwind(|| Element::empty_commitment_tree_with_flags(255, Some(vec![1])));
    assert!(commitment_flags.is_err());

    let bulk = panic::catch_unwind(|| Element::empty_bulk_append_tree(100));
    assert!(bulk.is_err());

    let bulk_flags = panic::catch_unwind(|| Element::empty_bulk_append_tree_with_flags(50, None));
    assert!(bulk_flags.is_err());
}

#[test]
fn value_helpers_and_conversion_errors_work() {
    let item = Element::new_item(vec![1, 2, 3]);
    let sum_item = Element::new_sum_item(-5);
    let item_with_sum = Element::new_item_with_sum_item(vec![4, 5], 8);
    let sum_tree = Element::new_sum_tree_with_flags_and_sum_value(Some(vec![10]), -22, None);
    let big_sum_tree =
        Element::new_big_sum_tree_with_flags_and_sum_value(Some(vec![11]), 123_456_789_012, None);
    let count_tree = Element::new_count_tree_with_flags_and_count_value(Some(vec![12]), 42, None);
    let count_sum_tree =
        Element::new_count_sum_tree_with_flags_and_sum_and_count_value(Some(vec![13]), 7, -6, None);
    let provable_count_tree =
        Element::new_provable_count_tree_with_flags_and_count_value(Some(vec![14]), 19, None);
    let provable_count_sum_tree =
        Element::new_provable_count_sum_tree_with_flags_and_sum_and_count_value(
            Some(vec![15]),
            5,
            4,
            None,
        );

    assert_eq!(item.sum_value_or_default(), 0);
    assert_eq!(sum_item.sum_value_or_default(), -5);
    assert_eq!(item_with_sum.sum_value_or_default(), 8);
    assert_eq!(sum_tree.sum_value_or_default(), -22);
    assert_eq!(count_sum_tree.sum_value_or_default(), -6);
    assert_eq!(provable_count_sum_tree.sum_value_or_default(), 4);

    assert_eq!(item.count_value_or_default(), 1);
    assert_eq!(count_tree.count_value_or_default(), 42);
    assert_eq!(count_sum_tree.count_value_or_default(), 7);
    assert_eq!(provable_count_tree.count_value_or_default(), 19);
    assert_eq!(provable_count_sum_tree.count_value_or_default(), 5);

    assert_eq!(item.count_sum_value_or_default(), (1, 0));
    assert_eq!(sum_item.count_sum_value_or_default(), (1, -5));
    assert_eq!(item_with_sum.count_sum_value_or_default(), (1, 8));
    assert_eq!(sum_tree.count_sum_value_or_default(), (1, -22));
    assert_eq!(count_tree.count_sum_value_or_default(), (42, 0));
    assert_eq!(count_sum_tree.count_sum_value_or_default(), (7, -6));
    assert_eq!(provable_count_tree.count_sum_value_or_default(), (19, 0));
    assert_eq!(provable_count_sum_tree.count_sum_value_or_default(), (5, 4));

    assert_eq!(item.big_sum_value_or_default(), 0);
    assert_eq!(sum_item.big_sum_value_or_default(), -5);
    assert_eq!(item_with_sum.big_sum_value_or_default(), 8);
    assert_eq!(sum_tree.big_sum_value_or_default(), -22);
    assert_eq!(count_sum_tree.big_sum_value_or_default(), -6);
    assert_eq!(provable_count_sum_tree.big_sum_value_or_default(), 4);
    assert_eq!(big_sum_tree.big_sum_value_or_default(), 123_456_789_012);

    assert_eq!(sum_item.as_sum_item_value().unwrap(), -5);
    assert_eq!(item_with_sum.as_sum_item_value().unwrap(), 8);
    assert!(matches!(
        item.as_sum_item_value(),
        Err(ElementError::WrongElementType("expected a sum item"))
    ));

    assert_eq!(sum_item.clone().into_sum_item_value().unwrap(), -5);
    assert_eq!(item_with_sum.clone().into_sum_item_value().unwrap(), 8);
    assert!(matches!(
        item.clone().into_sum_item_value(),
        Err(ElementError::WrongElementType("expected a sum item"))
    ));

    assert_eq!(sum_tree.as_sum_tree_value().unwrap(), -22);
    assert!(matches!(
        item.as_sum_tree_value(),
        Err(ElementError::WrongElementType("expected a sum tree"))
    ));
    assert_eq!(sum_tree.clone().into_sum_tree_value().unwrap(), -22);
    assert!(matches!(
        item.clone().into_sum_tree_value(),
        Err(ElementError::WrongElementType("expected a sum tree"))
    ));

    assert_eq!(item.as_item_bytes().unwrap(), &[1, 2, 3]);
    assert_eq!(item_with_sum.as_item_bytes().unwrap(), &[4, 5]);
    assert!(matches!(
        sum_item.as_item_bytes(),
        Err(ElementError::WrongElementType("expected an item"))
    ));

    assert_eq!(item.clone().into_item_bytes().unwrap(), vec![1, 2, 3]);
    assert_eq!(item_with_sum.clone().into_item_bytes().unwrap(), vec![4, 5]);
    assert!(matches!(
        sum_item.clone().into_item_bytes(),
        Err(ElementError::WrongElementType("expected an item"))
    ));

    let ref_path = ReferencePathType::AbsolutePathReference(vec![vec![1], vec![2]]);
    let reference = Element::new_reference(ref_path.clone());
    assert_eq!(
        reference.clone().into_reference_path_type().unwrap(),
        ref_path.clone()
    );
    assert!(matches!(
        item.clone().into_reference_path_type(),
        Err(ElementError::WrongElementType("expected a reference"))
    ));

    assert!(sum_tree.is_sum_tree());
    assert!(big_sum_tree.is_big_sum_tree());
    assert!(Element::empty_tree().is_basic_tree());
    assert!(Element::empty_tree().is_any_tree());
    assert!(Element::empty_sum_tree().is_any_tree());
    assert!(Element::empty_big_sum_tree().is_any_tree());
    assert!(Element::empty_count_tree().is_any_tree());
    assert!(Element::empty_count_sum_tree().is_any_tree());
    assert!(Element::empty_provable_count_tree().is_any_tree());
    assert!(Element::empty_provable_count_sum_tree().is_any_tree());
    assert!(Element::empty_commitment_tree(1).is_any_tree());
    assert!(Element::empty_mmr_tree().is_any_tree());
    assert!(Element::empty_bulk_append_tree(1).is_any_tree());
    assert!(Element::empty_dense_tree(2).is_any_tree());

    assert!(Element::empty_commitment_tree(2).is_commitment_tree());
    assert!(Element::empty_mmr_tree().is_mmr_tree());
    assert!(Element::empty_bulk_append_tree(2).is_bulk_append_tree());
    assert!(Element::empty_dense_tree(3).is_dense_tree());

    assert!(reference.is_reference());
    assert!(item.is_any_item());
    assert!(sum_item.is_any_item());
    assert!(item_with_sum.is_any_item());
    assert!(item.is_basic_item());
    assert!(item.has_basic_item());
    assert!(item_with_sum.has_basic_item());
    assert!(sum_item.is_sum_item());
    assert!(item_with_sum.is_sum_item());
    assert!(item_with_sum.is_item_with_sum_item());

    assert!(Element::empty_commitment_tree(2).uses_non_merk_data_storage());
    assert!(Element::empty_mmr_tree().uses_non_merk_data_storage());
    assert!(Element::empty_bulk_append_tree(2).uses_non_merk_data_storage());
    assert!(Element::empty_dense_tree(2).uses_non_merk_data_storage());
    assert!(!Element::empty_tree().uses_non_merk_data_storage());
    assert!(!item.uses_non_merk_data_storage());

    assert_eq!(
        Element::new_commitment_tree(44, 2, None).non_merk_entry_count(),
        Some(44)
    );
    assert_eq!(
        Element::new_mmr_tree(66, None).non_merk_entry_count(),
        Some(66)
    );
    assert_eq!(
        Element::new_bulk_append_tree(77, 2, None).non_merk_entry_count(),
        Some(77)
    );
    assert_eq!(
        Element::new_dense_tree(88, 3, None).non_merk_entry_count(),
        Some(88)
    );
    assert_eq!(Element::empty_tree().non_merk_entry_count(), None);
    assert_eq!(item.non_merk_entry_count(), None);
}

#[test]
fn flag_accessors_and_setters_cover_all_paths() {
    let mut element = Element::new_sum_item_with_flags(5, Some(vec![1, 2]));

    assert_eq!(element.get_flags(), &Some(vec![1, 2]));

    {
        let flags_mut = element.get_flags_mut();
        *flags_mut = Some(vec![9, 9]);
    }
    assert_eq!(element.get_flags(), &Some(vec![9, 9]));

    element.set_flags(None);
    assert_eq!(element.get_flags(), &None);

    let owned = element.get_flags_owned();
    assert_eq!(owned, None);

    let owned_item = Element::new_item_with_flags(vec![1], Some(vec![4, 5]));
    assert_eq!(owned_item.get_flags_owned(), Some(vec![4, 5]));
}

#[test]
fn required_item_space_matches_manual_formula() {
    let grove_version = GroveVersion::latest();
    let len: u32 = 127;
    let flag_len: u32 = 511;

    let required = Element::required_item_space(len, flag_len, grove_version).unwrap();
    let expected =
        len + len.required_space() as u32 + flag_len + flag_len.required_space() as u32 + 1;

    assert_eq!(required, expected);
}

#[test]
fn convert_if_reference_to_absolute_reference_converts_and_preserves_other_types() {
    let path = [b"root".as_ref(), b"branch".as_ref()];
    let key = Some(b"leaf".as_ref());

    let cousin_ref = Element::new_reference_with_max_hops_and_flags(
        ReferencePathType::CousinReference(b"other".to_vec()),
        Some(3),
        Some(vec![7]),
    );
    let converted = cousin_ref
        .clone()
        .convert_if_reference_to_absolute_reference(&path, key)
        .unwrap();
    assert_eq!(
        converted,
        Element::Reference(
            ReferencePathType::AbsolutePathReference(vec![
                b"root".to_vec(),
                b"other".to_vec(),
                b"leaf".to_vec(),
            ]),
            Some(3),
            Some(vec![7]),
        )
    );

    let absolute_ref = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
        b"a".to_vec(),
        b"b".to_vec(),
    ]));
    assert_eq!(
        absolute_ref
            .clone()
            .convert_if_reference_to_absolute_reference(&path, key)
            .unwrap(),
        absolute_ref
    );

    let non_ref = Element::new_item(vec![1, 2, 3]);
    assert_eq!(
        non_ref
            .clone()
            .convert_if_reference_to_absolute_reference(&path, key)
            .unwrap(),
        non_ref
    );

    let bad_ref = Element::new_reference(ReferencePathType::CousinReference(b"x".to_vec()));
    let err = bad_ref
        .convert_if_reference_to_absolute_reference(&[], None)
        .unwrap_err();
    assert!(matches!(
        err,
        ElementError::InvalidInput("reference stored path cannot satisfy reference constraints")
    ));
}

#[test]
fn tree_and_item_defaults_use_alias_types() {
    let _sum_value: SumValue = Element::new_sum_item(1).as_sum_item_value().unwrap();
    let _count_value: CountValue = Element::new_count_tree(Some(vec![1])).count_value_or_default();
}
