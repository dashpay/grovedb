//! Display / type-introspection tests for the three indexed-tree element
//! variants:
//! - `ProvableSumIndexedTree` (PSIT)
//! - `ProvableCountIndexedTree` (PCIT) — kept from the original layout
//! - `ProvableCountProvableSumIndexedTree` (PCPSIT) — new dual/triple-axis
//!
//! Lives in a separate file (rather than inline in `mod.rs`) so the
//! indexed-tree-specific assertions stay grouped and discoverable when
//! working on indexed-tree changes.

#![cfg(test)]

use grovedb_version::version::GroveVersion;

use super::*;
use crate::element_type::ElementType;

// -----------------------------------------------------------------
// PSIT
// -----------------------------------------------------------------

#[test]
fn provable_sum_indexed_tree_display_renders_fields() {
    let empty = Element::ProvableSumIndexedTree(None, None, 0, None);
    let s = format!("{}", empty);
    assert!(s.contains("ProvableSumIndexedTree"), "got: {}", s);
    assert!(s.contains("primary=None"));
    assert!(s.contains("secondary=None"));
    assert!(s.contains("sum=0"));
    assert!(!s.contains("flags"));

    let with_keys =
        Element::ProvableSumIndexedTree(Some(vec![0xab, 0xcd]), Some(vec![0xef]), -7, None);
    let s = format!("{}", with_keys);
    assert!(s.contains("primary=abcd"));
    assert!(s.contains("secondary=ef"));
    assert!(s.contains("sum=-7"));

    let with_flags = Element::ProvableSumIndexedTree(None, None, 3, Some(vec![1, 2, 3]));
    let s = format!("{}", with_flags);
    assert!(s.contains("flags"));
}

#[test]
fn provable_sum_indexed_tree_helpers_report_sum_and_type() {
    let psit = Element::ProvableSumIndexedTree(None, None, 5, None);
    assert!(psit.is_indexed_tree());
    assert!(psit.is_any_tree());
    // PSIT is NOT a "count-indexed tree" in the legacy single-axis sense.
    assert!(!psit.is_count_indexed_tree());
    assert_eq!(psit.element_type(), ElementType::ProvableSumIndexedTree);
    // sum_value_or_default returns the stored sum.
    assert_eq!(psit.sum_value_or_default(), 5);
    // It propagates the standard (1, sum) count-and-sum contribution like
    // any other sum-bearing leaf.
    assert_eq!(psit.count_sum_value_or_default(), (1, 5));

    // is_indexed_tree should look through NonCounted.
    let wrapped = Element::new_non_counted(Element::ProvableSumIndexedTree(None, None, 0, None))
        .expect("wrap ok");
    assert!(wrapped.is_indexed_tree());
}

#[test]
fn provable_sum_indexed_tree_bincode_round_trip() {
    let grove_version = GroveVersion::latest();
    let element = Element::ProvableSumIndexedTree(
        Some(vec![0xab, 0xcd]),
        Some(vec![0xef, 0x01]),
        -42,
        Some(vec![9, 8]),
    );
    let bytes = element.serialize(grove_version).expect("serialize ok");
    // Leading byte must be the new discriminant 21.
    assert_eq!(bytes[0], 21);
    let back = Element::deserialize(&bytes, grove_version).expect("deserialize ok");
    assert_eq!(back, element);
}

// -----------------------------------------------------------------
// PCIT (kept; existing behavior must remain intact)
// -----------------------------------------------------------------

#[test]
fn provable_count_indexed_tree_display_renders_fields() {
    let empty = Element::ProvableCountIndexedTree(None, None, 0, None);
    let s = format!("{}", empty);
    assert!(s.contains("ProvableCountIndexedTree"), "got: {}", s);
    assert!(s.contains("primary=None"));
    assert!(s.contains("count=0"));

    let populated =
        Element::ProvableCountIndexedTree(Some(vec![0x01]), Some(vec![0x02]), 42, Some(vec![9]));
    let s = format!("{}", populated);
    assert!(s.contains("primary=01"));
    assert!(s.contains("secondary=02"));
    assert!(s.contains("count=42"));
    assert!(s.contains("flags"));
}

#[test]
fn provable_count_indexed_tree_helpers_report_count_and_type() {
    let provable = Element::ProvableCountIndexedTree(None, None, 9, None);
    assert!(provable.is_count_indexed_tree());
    assert!(provable.is_indexed_tree());
    assert!(provable.is_any_tree());
    assert_eq!(
        provable.element_type(),
        ElementType::ProvableCountIndexedTree
    );

    // is_count_indexed_tree should look through NonCounted.
    let wrapped = Element::new_non_counted(Element::ProvableCountIndexedTree(None, None, 0, None))
        .expect("wrap ok");
    assert!(wrapped.is_count_indexed_tree());

    let item = Element::new_item(b"x".to_vec());
    assert!(!item.is_count_indexed_tree());
    assert!(!item.is_indexed_tree());
}

// -----------------------------------------------------------------
// PCPSIT
// -----------------------------------------------------------------

#[test]
fn provable_count_provable_sum_indexed_tree_one_axis_round_trip() {
    let grove_version = GroveVersion::latest();
    let element = Element::ProvableCountProvableSumIndexedTree(
        Some(vec![0x01]),
        7,
        13,
        vec![(0, Some(vec![0xaa]))],
        None,
    );
    let bytes = element.serialize(grove_version).expect("serialize ok");
    // Leading byte: discriminant 23 (the new variant slot).
    assert_eq!(bytes[0], 23);
    let back = Element::deserialize(&bytes, grove_version).expect("deserialize ok");
    assert_eq!(back, element);
}

#[test]
fn provable_count_provable_sum_indexed_tree_two_axes_round_trip() {
    let grove_version = GroveVersion::latest();
    let element = Element::ProvableCountProvableSumIndexedTree(
        None,
        5,
        -100,
        vec![(0, Some(vec![0xaa])), (1, None)],
        Some(vec![9, 8]),
    );
    let bytes = element.serialize(grove_version).expect("serialize ok");
    assert_eq!(bytes[0], 23);
    let back = Element::deserialize(&bytes, grove_version).expect("deserialize ok");
    assert_eq!(back, element);
}

#[test]
fn provable_count_provable_sum_indexed_tree_three_axes_round_trip() {
    let grove_version = GroveVersion::latest();
    let element = Element::ProvableCountProvableSumIndexedTree(
        Some(vec![0xff]),
        1000,
        i64::MIN,
        vec![(0, None), (1, Some(vec![0xbb])), (2, None)],
        None,
    );
    let bytes = element.serialize(grove_version).expect("serialize ok");
    assert_eq!(bytes[0], 23);
    let back = Element::deserialize(&bytes, grove_version).expect("deserialize ok");
    assert_eq!(back, element);
}

#[test]
fn provable_count_provable_sum_indexed_tree_helpers() {
    let pcpsit = Element::ProvableCountProvableSumIndexedTree(
        Some(vec![0x01]),
        7,
        13,
        vec![(0, None), (1, None)],
        None,
    );
    assert!(pcpsit.is_indexed_tree());
    assert!(pcpsit.is_any_tree());
    // PCPSIT is NOT a "count-indexed tree" in the legacy single-axis sense
    // — it's the dual/triple-axis variant. `is_count_indexed_tree` is
    // reserved for PCIT.
    assert!(!pcpsit.is_count_indexed_tree());
    assert_eq!(
        pcpsit.element_type(),
        ElementType::ProvableCountProvableSumIndexedTree
    );
    assert_eq!(pcpsit.count_value_or_default(), 7);
    assert_eq!(pcpsit.sum_value_or_default(), 13);
    assert_eq!(pcpsit.count_sum_value_or_default(), (7, 13));

    // axes() returns the slice for PCPSIT and None for everything else.
    let axes = pcpsit.axes().expect("Some axes for PCPSIT");
    assert_eq!(axes.len(), 2);
    let other = Element::ProvableCountIndexedTree(None, None, 0, None);
    assert!(other.axes().is_none());
}

#[test]
fn provable_count_provable_sum_indexed_tree_rejects_unsorted_axes() {
    // The constructor must reject any axes TLV that is not strictly
    // ascending. `(1, _)` followed by `(0, _)` is invalid.
    let bad = vec![(1u8, None), (0u8, None)];
    let result = Element::new_provable_count_provable_sum_indexed_tree(None, 0, 0, bad, None);
    assert!(result.is_err());
}

#[test]
fn provable_count_provable_sum_indexed_tree_rejects_duplicate_tags() {
    // Strict ascending => no duplicates.
    let bad = vec![(0u8, None), (0u8, None)];
    let result = Element::new_provable_count_provable_sum_indexed_tree(None, 0, 0, bad, None);
    assert!(result.is_err());
}

#[test]
fn provable_count_provable_sum_indexed_tree_rejects_empty_axes() {
    let result = Element::new_provable_count_provable_sum_indexed_tree(None, 0, 0, vec![], None);
    assert!(result.is_err());
}

#[test]
fn provable_count_provable_sum_indexed_tree_rejects_more_than_three_axes() {
    let bad = vec![
        (0u8, None),
        (1u8, None),
        (2u8, None),
        // Even if we add a fourth in the canonical 0..=2 range, the tag
        // must be unique — so this is rejected on the duplicate-tag rule.
        // To force the >3 path specifically, use a fake tag (but then
        // try_from_tag rejects it first). Either way, the constructor
        // rejects.
        (2u8, None),
    ];
    let result = Element::new_provable_count_provable_sum_indexed_tree(None, 0, 0, bad, None);
    assert!(result.is_err());
}

#[test]
fn provable_count_provable_sum_indexed_tree_rejects_unknown_tag() {
    let bad = vec![(7u8, None)];
    let result = Element::new_provable_count_provable_sum_indexed_tree(None, 0, 0, bad, None);
    assert!(result.is_err());
}

#[test]
fn provable_count_provable_sum_indexed_tree_constructor_accepts_canonical_axes() {
    for axes in [
        vec![(0u8, None)],
        vec![(1u8, Some(vec![0xab]))],
        vec![(2u8, None)],
        vec![(0u8, None), (1u8, None)],
        vec![(0u8, None), (2u8, None)],
        vec![(1u8, None), (2u8, None)],
        vec![(0u8, None), (1u8, None), (2u8, None)],
    ] {
        let result = Element::new_provable_count_provable_sum_indexed_tree(
            Some(vec![0x01]),
            1,
            1,
            axes.clone(),
            None,
        );
        assert!(result.is_ok(), "axes {:?} should be accepted", axes);
    }
}
