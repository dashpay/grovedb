//! What deleting an element of a `NormalTree` refunds, against what inserting
//! it was charged.
//!
//! From `GROVE_V5` the two are equal whatever the element is and however the
//! delete is declared. Earlier versions refund the raw value bytes of an
//! element that is not declared a tree: more than a tree was charged, less
//! than a sum item was.

#![cfg(feature = "minimal")]
#![allow(clippy::result_large_err)]

use grovedb::{
    batch::{QualifiedGroveDbOp, SubelementsDeletionBehavior},
    reference_path::ReferencePathType,
    Element, ElementFlags, GroveDb,
};
use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;
use grovedb_version::version::{v4::GROVE_V4, v5::GROVE_V5, GroveVersion, GROVE_VERSIONS};
use tempfile::TempDir;

const PARENT: &[u8] = b"parent";
const TARGET: &[u8] = b"target";
const KEY: &[u8; 32] = &[7; 32];

/// How the element is removed.
#[derive(Clone, Copy, Debug)]
enum Route {
    /// A batch `Delete`, which declares nothing about the element.
    BatchDelete,
    /// A batch `DeleteTree`, which declares the tree's type.
    BatchDeleteTree,
    /// `GroveDb::delete`, which reads the element first.
    Direct,
}

/// What one insert followed by one delete came to.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    charged: u32,
    refunded: u32,
    root_hash_after_insert: [u8; 32],
    root_hash_after_delete: [u8; 32],
}

fn flags() -> Option<ElementFlags> {
    Some(vec![0, 0, 0])
}

fn root_hash(db: &GroveDb, grove_version: &GroveVersion) -> [u8; 32] {
    db.root_hash(None, grove_version)
        .unwrap()
        .expect("expected the root hash")
}

/// Inserts `element` under a `NormalTree` and removes it by `route`.
fn insert_then_delete(element: Element, route: Route, grove_version: &GroveVersion) -> Outcome {
    let dir = TempDir::new().expect("expected a temporary directory");
    let db = GroveDb::open(dir.path()).expect("expected to open the grove");
    db.apply_batch(
        vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
                PARENT.to_vec(),
                Element::empty_tree(),
            ),
            // What a reference under test points at, and a neighbour so the
            // deleted key is never the tree's only one.
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![PARENT.to_vec()],
                TARGET.to_vec(),
                Element::new_item(vec![1]),
            ),
        ],
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("expected to set up the grove");

    let path = vec![PARENT.to_vec()];
    let tree_type = element.tree_type();

    let insert = db.apply_batch(
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            path.clone(),
            KEY.to_vec(),
            element,
        )],
        None,
        None,
        grove_version,
    );
    insert.value.expect("expected to insert the element");
    let charged = insert.cost.storage_cost.added_bytes;
    let root_hash_after_insert = root_hash(&db, grove_version);

    let delete = match route {
        Route::BatchDelete => db.apply_batch(
            vec![QualifiedGroveDbOp::delete_op(path, KEY.to_vec())],
            None,
            None,
            grove_version,
        ),
        Route::BatchDeleteTree => db.apply_batch(
            vec![QualifiedGroveDbOp::delete_tree_op(
                path,
                KEY.to_vec(),
                tree_type.expect("expected a tree for a tree delete"),
                SubelementsDeletionBehavior::DontCheckWithNoCleanup,
            )],
            None,
            None,
            grove_version,
        ),
        Route::Direct => db.delete([PARENT].as_ref(), KEY, None, None, grove_version),
    };
    delete.value.expect("expected to delete the element");
    let refunded = delete.cost.storage_cost.removed_bytes.total_removed_bytes();

    Outcome {
        charged,
        refunded,
        root_hash_after_insert,
        root_hash_after_delete: root_hash(&db, grove_version),
    }
}

fn trees() -> Vec<(&'static str, Element)> {
    vec![
        ("Tree", Element::empty_tree_with_flags(flags())),
        ("SumTree", Element::empty_sum_tree_with_flags(flags())),
        (
            "BigSumTree",
            Element::empty_big_sum_tree_with_flags(flags()),
        ),
        ("CountTree", Element::empty_count_tree_with_flags(flags())),
        (
            "CountSumTree",
            Element::empty_count_sum_tree_with_flags(flags()),
        ),
        (
            "ProvableCountTree",
            Element::empty_provable_count_tree_with_flags(flags()),
        ),
        ("Tree without flags", Element::empty_tree()),
    ]
}

fn sum_items() -> Vec<(&'static str, Element)> {
    vec![
        ("SumItem 5", Element::new_sum_item_with_flags(5, flags())),
        (
            "SumItem i64::MAX",
            Element::new_sum_item_with_flags(i64::MAX, flags()),
        ),
        ("SumItem without flags", Element::new_sum_item(5)),
        (
            "ItemWithSumItem",
            Element::new_item_with_sum_item_with_flags(vec![9; 20], 5, flags()),
        ),
    ]
}

/// Elements charged by their serialized size. The item lengths sit on both
/// sides of the sizes where the stored value's length prefix grows a byte.
fn items_and_references() -> Vec<(String, Element)> {
    let mut elements: Vec<(String, Element)> = [0usize, 10, 55, 56, 57, 200, 16_300, 16_400]
        .into_iter()
        .map(|len| {
            (
                format!("Item of {len} bytes"),
                Element::new_item_with_flags(vec![3; len], flags()),
            )
        })
        .collect();
    elements.push((
        "Item without flags".to_string(),
        Element::new_item(vec![3; 10]),
    ));
    elements.push((
        "Reference".to_string(),
        Element::new_reference_with_flags(
            ReferencePathType::AbsolutePathReference(vec![PARENT.to_vec(), TARGET.to_vec()]),
            flags(),
        ),
    ));
    elements
}

#[test]
fn should_refund_what_a_tree_was_charged_however_its_delete_is_declared() {
    for (label, element) in trees() {
        for route in [Route::BatchDelete, Route::BatchDeleteTree, Route::Direct] {
            let outcome = insert_then_delete(element.clone(), route, &GROVE_V5);
            assert_eq!(
                outcome.refunded, outcome.charged,
                "{label} removed by {route:?}"
            );
        }
    }
}

#[test]
fn should_refund_what_a_sum_item_was_charged() {
    for (label, element) in sum_items() {
        for route in [Route::BatchDelete, Route::Direct] {
            let outcome = insert_then_delete(element.clone(), route, &GROVE_V5);
            assert_eq!(
                outcome.refunded, outcome.charged,
                "{label} removed by {route:?}"
            );
        }
    }
}

#[test]
fn should_refund_items_and_references_the_same_in_every_version() {
    for (label, element) in items_and_references() {
        for route in [Route::BatchDelete, Route::Direct] {
            let latest = insert_then_delete(element.clone(), route, GroveVersion::latest());
            assert_eq!(
                latest.refunded, latest.charged,
                "{label} removed by {route:?}"
            );
            for grove_version in GROVE_VERSIONS {
                assert_eq!(
                    insert_then_delete(element.clone(), route, grove_version),
                    latest,
                    "{label} removed by {route:?} at grove version {}",
                    grove_version.protocol_version
                );
            }
        }
    }
}

#[test]
fn should_refund_declared_tree_deletes_the_same_in_every_version() {
    for (label, element) in trees() {
        for route in [Route::BatchDeleteTree, Route::Direct] {
            let latest = insert_then_delete(element.clone(), route, GroveVersion::latest());
            for grove_version in GROVE_VERSIONS {
                assert_eq!(
                    insert_then_delete(element.clone(), route, grove_version),
                    latest,
                    "{label} removed by {route:?} at grove version {}",
                    grove_version.protocol_version
                );
            }
        }
    }
}

#[test]
fn should_keep_the_raw_byte_refund_before_grove_v5() {
    // (element, refunded minus charged) for a delete that does not declare a
    // tree. A tree is charged without its 32 byte value hash and with a 2
    // byte length prefix; raw bytes count the hash and a 1 byte prefix, and
    // the varint sums of the aggregate trees serialize below their fixed
    // charge. A sum item is charged 8 bytes more than its small value takes.
    let pinned: [(Element, i64); 4] = [
        (Element::empty_tree_with_flags(flags()), 31),
        (Element::empty_sum_tree_with_flags(flags()), 23),
        (Element::empty_count_tree_with_flags(flags()), 23),
        (Element::new_sum_item_with_flags(5, flags()), -8),
    ];
    for grove_version in GROVE_VERSIONS
        .iter()
        .filter(|grove_version| grove_version.protocol_version < GROVE_V5.protocol_version)
    {
        for (element, difference) in &pinned {
            let outcome = insert_then_delete(element.clone(), Route::BatchDelete, grove_version);
            assert_eq!(
                i64::from(outcome.refunded) - i64::from(outcome.charged),
                *difference,
                "{element:?} at grove version {}",
                grove_version.protocol_version
            );
        }
    }
}

#[test]
fn should_change_only_the_refund_from_grove_v5() {
    let elements = trees().into_iter().chain(sum_items());
    for (label, element) in elements {
        let before = insert_then_delete(element.clone(), Route::BatchDelete, &GROVE_V4);
        let after = insert_then_delete(element, Route::BatchDelete, &GROVE_V5);
        assert_eq!(after.charged, before.charged, "{label}");
        assert_eq!(
            after.root_hash_after_insert, before.root_hash_after_insert,
            "{label}"
        );
        assert_eq!(
            after.root_hash_after_delete, before.root_hash_after_delete,
            "{label}"
        );
        // The refund itself moves for all of them but a sum item whose value
        // fills its fixed charge (`should_keep_the_raw_byte_refund_before_grove_v5`
        // pins by how much).
        assert_eq!(after.refunded, after.charged, "{label}");
    }
}
