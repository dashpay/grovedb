//! Display / type-introspection tests for `CountIndexedTree` and
//! `ProvableCountIndexedTree` element variants.
//!
//! Lives in a separate file (rather than inline in `mod.rs`) so the
//! cidx-specific assertions stay grouped and discoverable when working
//! on cidx-related changes.

#![cfg(test)]

use crate::element_type::ElementType;

use super::*;

#[test]
fn count_indexed_tree_display_renders_fields() {
    let empty = Element::CountIndexedTree(None, None, 0, None);
    let s = format!("{}", empty);
    assert!(s.contains("CountIndexedTree"), "got: {}", s);
    assert!(s.contains("primary=None"));
    assert!(s.contains("secondary=None"));
    assert!(s.contains("count=0"));
    assert!(!s.contains("flags"));

    let with_keys = Element::CountIndexedTree(Some(vec![0xab, 0xcd]), Some(vec![0xef]), 7, None);
    let s = format!("{}", with_keys);
    assert!(s.contains("primary=abcd"));
    assert!(s.contains("secondary=ef"));
    assert!(s.contains("count=7"));

    let with_flags = Element::CountIndexedTree(None, None, 3, Some(vec![1, 2, 3]));
    let s = format!("{}", with_flags);
    assert!(s.contains("flags"));
}

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
fn count_indexed_tree_helpers_report_count_and_type() {
    let cidx = Element::CountIndexedTree(None, None, 5, None);
    assert!(cidx.is_count_indexed_tree());
    assert!(cidx.is_any_tree());
    assert_eq!(cidx.element_type(), ElementType::CountIndexedTree);

    let provable = Element::ProvableCountIndexedTree(None, None, 9, None);
    assert!(provable.is_count_indexed_tree());
    assert!(provable.is_any_tree());
    assert_eq!(
        provable.element_type(),
        ElementType::ProvableCountIndexedTree
    );

    // is_count_indexed_tree should look through NonCounted.
    let wrapped = Element::NonCounted(Box::new(Element::CountIndexedTree(None, None, 0, None)));
    assert!(wrapped.is_count_indexed_tree());

    let item = Element::new_item(b"x".to_vec());
    assert!(!item.is_count_indexed_tree());
}
