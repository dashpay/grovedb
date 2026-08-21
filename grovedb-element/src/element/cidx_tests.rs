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

// -----------------------------------------------------------------
// Extended bincode / equality round-trip coverage
// -----------------------------------------------------------------

/// PCIT bincode round-trip for both empty and populated states.
#[test]
fn provable_count_indexed_tree_bincode_round_trip() {
    let grove_version = GroveVersion::latest();
    for element in [
        // Empty.
        Element::ProvableCountIndexedTree(None, None, 0, None),
        // Populated.
        Element::ProvableCountIndexedTree(Some(vec![0x01, 0x02]), Some(vec![0x03, 0x04]), 42, None),
        // With flags.
        Element::ProvableCountIndexedTree(None, None, 0, Some(vec![0xAB])),
        // With max u64 count.
        Element::ProvableCountIndexedTree(
            Some(vec![0xFF]),
            Some(vec![0xFE]),
            u64::MAX,
            Some(vec![1, 2, 3]),
        ),
    ] {
        let bytes = element.serialize(grove_version).expect("serialize");
        // Discriminant byte 22.
        assert_eq!(
            bytes[0], 22,
            "PCIT discriminant must be 22 for {:?}",
            element
        );
        let back = Element::deserialize(&bytes, grove_version).expect("deserialize");
        assert_eq!(back, element);
    }
}

/// PSIT round-trip across edge values.
#[test]
fn provable_sum_indexed_tree_bincode_edge_values_round_trip() {
    let grove_version = GroveVersion::latest();
    for sum in [i64::MIN, i64::MIN + 1, -1, 0, 1, i64::MAX - 1, i64::MAX] {
        let element = Element::ProvableSumIndexedTree(None, None, sum, None);
        let bytes = element.serialize(grove_version).expect("serialize");
        assert_eq!(bytes[0], 21);
        let back = Element::deserialize(&bytes, grove_version).expect("deserialize");
        assert_eq!(back, element);
    }
}

/// PCPSIT round-trip across edge values for count and sum.
#[test]
fn provable_count_provable_sum_indexed_tree_edge_values_round_trip() {
    let grove_version = GroveVersion::latest();
    for (count, sum) in [
        (0u64, 0i64),
        (1, 1),
        (u64::MAX, i64::MAX),
        (u64::MAX, i64::MIN),
        (u64::MAX / 2, -(i64::MAX / 2)),
    ] {
        let axes = vec![
            (0u8, Some(vec![0xaa])),
            (1u8, None),
            (2u8, Some(vec![0xbb, 0xcc])),
        ];
        let element = Element::ProvableCountProvableSumIndexedTree(
            Some(vec![0x99]),
            count,
            sum,
            axes,
            Some(vec![9, 8, 7]),
        );
        let bytes = element.serialize(grove_version).expect("serialize");
        assert_eq!(bytes[0], 23);
        let back = Element::deserialize(&bytes, grove_version).expect("deserialize");
        assert_eq!(back, element);
    }
}

/// `validate_pcpsit_axes` integration: the canonical-axes invariant
/// must hold even on the with-flags constructor.
#[test]
fn provable_count_provable_sum_indexed_tree_with_flags_rejects_bad_axes() {
    let result = Element::empty_provable_count_provable_sum_indexed_tree_with_flags(
        vec![(1, None), (0, None)], // unsorted
        Some(vec![1, 2]),
    );
    assert!(
        result.is_err(),
        "unsorted axes must be rejected via flags constructor"
    );
}

/// Each IndexAxis tag value must round-trip through `try_from_tag` and
/// `tag()`.
#[test]
fn index_axis_tag_round_trip() {
    use crate::indexed::IndexAxis;
    for axis in [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg] {
        let back = IndexAxis::try_from_tag(axis.tag()).expect("known tag");
        assert_eq!(back, axis);
    }
}

/// PCPSIT element_type and helpers for the count-only, sum-only and
/// avg-only single-axis shapes.
#[test]
fn provable_count_provable_sum_indexed_tree_single_axis_helpers() {
    for (tag, _label) in [(0u8, "count"), (1u8, "sum"), (2u8, "avg")] {
        let elem =
            Element::empty_provable_count_provable_sum_indexed_tree(vec![(tag, None)]).expect("ok");
        assert!(elem.is_indexed_tree());
        assert!(elem.is_any_tree());
        // PCPSIT is the joint-axis variant; even single-axis
        // configurations qualify as count-and-sum-bearing children.
        assert!(elem.is_count_and_sum_bearing_child());
        assert!(elem.is_sum_bearing_child());
        // axes() returns Some with the configured tag.
        let axes = elem.axes().expect("axes");
        assert_eq!(axes.len(), 1);
        assert_eq!(axes[0].0, tag);
        assert!(axes[0].1.is_none());
    }
}

/// PSIT must NOT be considered a count-indexed tree (legacy single-axis
/// "cidx" predicate).
#[test]
fn provable_sum_indexed_tree_not_count_indexed_predicate() {
    let psit = Element::ProvableSumIndexedTree(None, None, 0, None);
    assert!(!psit.is_count_indexed_tree());
}

/// PCPSIT must NOT be considered a count-indexed tree (the legacy
/// single-axis predicate is reserved for PCIT only).
#[test]
fn provable_count_provable_sum_indexed_tree_not_count_indexed_predicate() {
    let pcpsit =
        Element::empty_provable_count_provable_sum_indexed_tree(vec![(0, None)]).expect("ok");
    assert!(!pcpsit.is_count_indexed_tree());
}

/// PCIT `is_sum_bearing_child` must return false: PCIT only contributes
/// count, not sum.
#[test]
fn provable_count_indexed_tree_not_sum_bearing() {
    let pcit = Element::ProvableCountIndexedTree(None, None, 0, None);
    assert!(!pcit.is_sum_bearing_child());
    assert!(!pcit.is_count_and_sum_bearing_child());
}
