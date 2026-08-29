//! Integration tests for the backward-references flows: the `GROVE_V4`
//! insert/delete routers, the rule checks in
//! `bidirectional_references::handling`, and the query paths over the new
//! element family.

use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

use crate::{
    bidirectional_references::BidirectionalReference,
    operations::{delete::DeleteOptions, get::QueryItemOrSumReturnType, insert::InsertOptions},
    query_result_type::{QueryResultElement, QueryResultType},
    reference_path::ReferencePathType,
    tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
    Element, Error, PathQuery, Query,
};

fn flag_on() -> Option<InsertOptions> {
    Some(InsertOptions {
        propagate_backward_references: true,
        ..Default::default()
    })
}

fn sibling_bidi(key: &[u8], cascade: bool) -> Element {
    Element::BidirectionalReference(BidirectionalReference {
        forward_reference_path: ReferencePathType::SiblingReference(key.to_vec()),
        backward_reference_slot: 0,
        cascade_on_update: cascade,
        max_hop: None,
        flags: None,
    })
}

/// A test_leaf with one item-with-backwards-references under `value`.
fn db_with_bwr_item() -> TempGroveDb {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    db.insert(
        &[TEST_LEAF],
        b"value",
        Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db
}

#[test]
fn bidi_reference_must_target_backward_references_element() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    db.insert(
        &[TEST_LEAF],
        b"plain",
        Element::new_item(b"v".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    assert!(matches!(
        db.insert(
            &[TEST_LEAF],
            b"ref",
            sibling_bidi(b"plain", true),
            None,
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::BidirectionalReferenceRule(_))
    ));
}

#[test]
fn bidi_reference_chain_allows_only_one_backward_reference() {
    let grove_version = GroveVersion::latest();
    let db = db_with_bwr_item();

    db.insert(
        &[TEST_LEAF],
        b"refc",
        sibling_bidi(b"value", true),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF],
        b"refb",
        sibling_bidi(b"refc", true),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    // A second bidirectional reference onto `refc` (which is itself a
    // bidirectional reference) exceeds the 1-backward-reference budget for
    // reference chain members.
    assert!(matches!(
        db.insert(
            &[TEST_LEAF],
            b"refx",
            sibling_bidi(b"refc", true),
            None,
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::BidirectionalReferenceRule(_))
    ));
}

#[test]
fn item_supports_up_to_32_backward_references() {
    let grove_version = GroveVersion::latest();
    let db = db_with_bwr_item();

    for i in 0..32u8 {
        db.insert(
            &[TEST_LEAF],
            format!("ref{i:02}").as_bytes(),
            sibling_bidi(b"value", true),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap_or_else(|e| panic!("reference {i} should fit: {e}"));
    }

    assert!(matches!(
        db.insert(
            &[TEST_LEAF],
            b"ref32",
            sibling_bidi(b"value", true),
            None,
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::BidirectionalReferenceRule(_))
    ));
}

#[test]
fn bidi_reference_cannot_overwrite_item_with_backward_references() {
    let grove_version = GroveVersion::latest();
    let db = db_with_bwr_item();
    db.insert(
        &[TEST_LEAF],
        b"value2",
        Element::new_item_allowing_bidirectional_references(b"other".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    // Overwriting `value` (an item that may carry up to 32 backward
    // references) with a bidirectional reference (which carries at most 1)
    // is refused.
    assert!(matches!(
        db.insert(
            &[TEST_LEAF],
            b"value",
            sibling_bidi(b"value2", true),
            None,
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::BidirectionalReferenceRule(_))
    ));
}

#[test]
fn overwriting_bidi_reference_retargets_backward_bookkeeping() {
    let grove_version = GroveVersion::latest();
    let db = db_with_bwr_item();
    let tx = db.start_transaction();
    db.insert(
        &[TEST_LEAF],
        b"value2",
        Element::new_item_allowing_bidirectional_references(b"second".to_vec()),
        None,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF],
        b"refc",
        sibling_bidi(b"value", true),
        None,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .unwrap();

    // Retarget refc from `value` to `value2`.
    db.insert(
        &[TEST_LEAF],
        b"refc",
        sibling_bidi(b"value2", true),
        None,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .unwrap();

    let ref_hash = |tx| {
        use grovedb_merk::element::get::ElementFetchFromStorageExtensions;
        Element::get_value_hash(
            &db.open_transactional_merk_at_path(
                SubtreePath::from(&[TEST_LEAF]),
                tx,
                None,
                grove_version,
            )
            .unwrap()
            .unwrap(),
            b"refc",
            true,
            grove_version,
        )
        .unwrap()
        .unwrap()
        .unwrap()
    };

    // Updating the OLD target no longer touches refc...
    let before = ref_hash(&tx);
    db.insert(
        &[TEST_LEAF],
        b"value",
        Element::new_item_allowing_bidirectional_references(b"updated".to_vec()),
        flag_on(),
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .unwrap();
    assert_eq!(ref_hash(&tx), before);

    // ...while updating the NEW target propagates into refc.
    db.insert(
        &[TEST_LEAF],
        b"value2",
        Element::new_item_allowing_bidirectional_references(b"changed".to_vec()),
        flag_on(),
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .unwrap();
    assert_ne!(ref_hash(&tx), before);

    // The whole graph still verifies.
    assert!(db
        .verify_grovedb(Some(&tx), true, true, grove_version)
        .unwrap()
        .is_empty());
}

#[test]
fn overwriting_bidi_reference_with_backward_references_item_keeps_consistency() {
    let grove_version = GroveVersion::latest();
    let db = db_with_bwr_item();
    db.insert(
        &[TEST_LEAF],
        b"refc",
        sibling_bidi(b"value", true),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    // Overwrite the reference itself with an item that supports backward
    // references: the old backward slot on `value` is released and hashes
    // stay consistent.
    db.insert(
        &[TEST_LEAF],
        b"refc",
        Element::new_item_allowing_bidirectional_references(b"now an item".to_vec()),
        flag_on(),
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        db.get(&[TEST_LEAF], b"refc", None, grove_version)
            .unwrap()
            .unwrap(),
        Element::new_item_allowing_bidirectional_references(b"now an item".to_vec())
    );
    assert!(db
        .verify_grovedb(None, true, true, grove_version)
        .unwrap()
        .is_empty());
}

#[test]
fn cascade_requires_opt_in() {
    let grove_version = GroveVersion::latest();
    let db = db_with_bwr_item();
    db.insert(
        &[TEST_LEAF],
        b"stubborn",
        sibling_bidi(b"value", false),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    // Overwriting the target with a non-backward-references element would
    // cascade-delete `stubborn`, which did not opt in — the operation is
    // refused and nothing is committed.
    assert!(matches!(
        db.insert(
            &[TEST_LEAF],
            b"value",
            Element::new_item(b"plain now".to_vec()),
            flag_on(),
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::BidirectionalReferenceRule(_))
    ));
    assert_eq!(
        db.get(&[TEST_LEAF], b"value", None, grove_version)
            .unwrap()
            .unwrap(),
        Element::new_item_allowing_bidirectional_references(b"hello".to_vec())
    );

    // Deleting the target is refused for the same reason.
    assert!(matches!(
        db.delete(
            &[TEST_LEAF],
            b"value",
            Some(DeleteOptions {
                propagate_backward_references: true,
                ..Default::default()
            }),
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::BidirectionalReferenceRule(_))
    ));
}

#[test]
fn plain_references_work_under_the_flag() {
    let grove_version = GroveVersion::latest();
    let db = db_with_bwr_item();

    // A plain reference and a sum-carrying reference both insert through
    // the backward-references flow when the flag is set.
    db.insert(
        &[TEST_LEAF],
        b"plain_ref",
        Element::new_reference(ReferencePathType::SiblingReference(b"value".to_vec())),
        flag_on(),
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF],
        b"sum_ref",
        Element::new_reference_with_sum_item(
            ReferencePathType::SiblingReference(b"value".to_vec()),
            7,
        ),
        flag_on(),
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        db.get(&[TEST_LEAF], b"plain_ref", None, grove_version)
            .unwrap()
            .unwrap(),
        Element::new_item_allowing_bidirectional_references(b"hello".to_vec())
    );

    // References may not point at subtrees under the flag either.
    db.insert(
        &[TEST_LEAF],
        b"subtree",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(
        db.insert(
            &[TEST_LEAF],
            b"tree_ref",
            Element::new_reference(ReferencePathType::SiblingReference(b"subtree".to_vec())),
            flag_on(),
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::NotSupported(_))
    ));
}

#[test]
fn empty_trees_insert_under_the_flag() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    db.insert(
        &[TEST_LEAF],
        b"tree",
        Element::empty_tree(),
        flag_on(),
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF],
        b"sum_tree",
        Element::empty_sum_tree(),
        flag_on(),
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    // A tree literal claiming a root key is rejected outside batches.
    assert!(matches!(
        db.insert(
            &[TEST_LEAF],
            b"claimed",
            Element::Tree(Some(b"root".to_vec()), None),
            flag_on(),
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::InvalidCodeExecution(_))
    ));

    assert!(db
        .verify_grovedb(None, true, true, grove_version)
        .unwrap()
        .is_empty());
}

#[test]
fn specialized_types_and_wrappers_rejected_under_the_flag() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    for element in [
        Element::MmrTree(0, None),
        Element::new_non_counted(Element::new_item(b"x".to_vec())).unwrap(),
    ] {
        assert!(
            matches!(
                db.insert(
                    &[TEST_LEAF],
                    b"k",
                    element.clone(),
                    flag_on(),
                    None,
                    grove_version,
                )
                .unwrap(),
                Err(Error::NotSupported(_))
            ),
            "expected NotSupported under the flag for {element:?}"
        );
    }
}

#[test]
fn override_checks_apply_under_the_flag() {
    let grove_version = GroveVersion::latest();
    let db = db_with_bwr_item();

    assert!(matches!(
        db.insert(
            &[TEST_LEAF],
            b"value",
            Element::new_item_allowing_bidirectional_references(b"nope".to_vec()),
            Some(InsertOptions {
                propagate_backward_references: true,
                validate_insertion_does_not_override: true,
                ..Default::default()
            }),
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::OverrideNotAllowed(_))
    ));

    db.insert(
        &[TEST_LEAF],
        b"subtree",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    // Default options refuse overriding a tree; the flag routes through the
    // backward-references flow which applies the same check.
    assert!(matches!(
        db.insert(
            &[TEST_LEAF],
            b"subtree",
            Element::new_item_allowing_bidirectional_references(b"nope".to_vec()),
            flag_on(),
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::OverrideNotAllowed(_))
    ));
}

#[test]
fn delete_with_flag_handles_trees() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    // Empty tree: plain removal.
    db.insert(
        &[TEST_LEAF],
        b"empty",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.delete(
        &[TEST_LEAF],
        b"empty",
        Some(DeleteOptions {
            propagate_backward_references: true,
            ..Default::default()
        }),
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    // Non-empty tree without permission: refused (or silently skipped when
    // the options say not to error).
    db.insert(
        &[TEST_LEAF],
        b"full",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF, b"full"],
        b"k",
        Element::new_item(b"v".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(
        db.delete(
            &[TEST_LEAF],
            b"full",
            Some(DeleteOptions {
                propagate_backward_references: true,
                allow_deleting_non_empty_trees: false,
                deleting_non_empty_trees_returns_error: true,
                ..Default::default()
            }),
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::DeletingNonEmptyTree(_))
    ));
    db.delete(
        &[TEST_LEAF],
        b"full",
        Some(DeleteOptions {
            propagate_backward_references: true,
            allow_deleting_non_empty_trees: false,
            deleting_non_empty_trees_returns_error: false,
            ..Default::default()
        }),
        None,
        grove_version,
    )
    .unwrap()
    .expect("refusal without error flag is not an error");
    assert!(db
        .get(&[TEST_LEAF], b"full", None, grove_version)
        .unwrap()
        .is_ok());

    // Specialized data trees are rejected under the flag.
    db.insert(
        &[TEST_LEAF],
        b"mmr",
        Element::MmrTree(0, None),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(
        db.delete(
            &[TEST_LEAF],
            b"mmr",
            Some(DeleteOptions {
                propagate_backward_references: true,
                ..Default::default()
            }),
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::NotSupported(_))
    ));
}

#[test]
fn queries_resolve_backward_references_elements() {
    let grove_version = GroveVersion::latest();
    let db = db_with_bwr_item();
    db.insert(
        &[TEST_LEAF],
        b"ref",
        sibling_bidi(b"value", true),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    let mut query = Query::new();
    query.insert_key(b"value".to_vec());
    query.insert_key(b"ref".to_vec());
    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

    // Raw item values: the item directly and through the reference.
    let (values, _) = db
        .query_item_value(&path_query, true, true, true, None, grove_version)
        .unwrap()
        .unwrap();
    assert_eq!(values, vec![b"hello".to_vec(), b"hello".to_vec()]);

    // Item-or-sum view.
    let (values, _) = db
        .query_item_value_or_sum(&path_query, true, true, true, None, grove_version)
        .unwrap()
        .unwrap();
    assert!(matches!(
        values.as_slice(),
        [
            QueryItemOrSumReturnType::ItemData(a),
            QueryItemOrSumReturnType::ItemData(b)
        ] if a == b"hello" && b == b"hello"
    ));

    // Element view: the reference resolves to the underlying item element.
    let (elements, _) = db
        .query(
            &path_query,
            true,
            true,
            true,
            QueryResultType::QueryElementResultType,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    let elements: Vec<_> = elements
        .into_iterator()
        .map(|r| match r {
            QueryResultElement::ElementResultItem(e) => e,
            other => panic!("unexpected result shape: {other:?}"),
        })
        .collect();
    assert_eq!(
        elements,
        vec![
            Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
            Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
        ]
    );
}

#[test]
fn sum_queries_resolve_backward_references_sum_items() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    db.insert(
        &[TEST_LEAF],
        b"sums",
        Element::empty_sum_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF, b"sums"],
        b"s",
        Element::new_sum_item_allowing_bidirectional_references(5),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF, b"sums"],
        b"rs",
        sibling_bidi(b"s", true),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    let mut query = Query::new();
    query.insert_key(b"s".to_vec());
    query.insert_key(b"rs".to_vec());
    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"sums".to_vec()], query);

    let (sums, _) = db
        .query_sums(&path_query, true, true, true, None, grove_version)
        .unwrap()
        .unwrap();
    assert_eq!(sums, vec![5, 5]);

    let (values, _) = db
        .query_item_value_or_sum(&path_query, true, true, true, None, grove_version)
        .unwrap()
        .unwrap();
    assert!(matches!(
        values.as_slice(),
        [
            QueryItemOrSumReturnType::SumValue(a),
            QueryItemOrSumReturnType::SumValue(b)
        ] if *a == 5 && *b == 5
    ));
}

#[test]
fn retargeting_onto_a_chained_reference_propagates_the_end_hash() {
    // Regression for the overwrite branch of
    // `process_bidirectional_reference_insertion`: when the NEW target is
    // itself a bidirectional reference (a chain), the hash pushed to the
    // overwritten reference's own backward references must be the resolved
    // END-of-chain value hash, not the intermediate node's combined hash.
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    let tx = db.start_transaction();

    for (key, value) in [(b"v1".as_ref(), b"one".as_ref()), (b"v2", b"two")] {
        db.insert(
            &[TEST_LEAF],
            key,
            Element::new_item_allowing_bidirectional_references(value.to_vec()),
            None,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .unwrap();
    }
    // An intermediate reference onto v2, a target reference onto v1, and an
    // origin chained onto the target reference.
    db.insert(
        &[TEST_LEAF],
        b"mid",
        sibling_bidi(b"v2", true),
        None,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF],
        b"target_ref",
        sibling_bidi(b"v1", true),
        None,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF],
        b"origin",
        sibling_bidi(b"target_ref", true),
        None,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .unwrap();

    // Retarget `target_ref` onto `mid` — a CHAINED target (mid -> v2).
    // `origin`'s stored hash must be refreshed with v2's value hash.
    db.insert(
        &[TEST_LEAF],
        b"target_ref",
        sibling_bidi(b"mid", true),
        None,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        db.get(&[TEST_LEAF], b"origin", Some(&tx), grove_version)
            .unwrap()
            .unwrap(),
        Element::new_item_allowing_bidirectional_references(b"two".to_vec())
    );
    assert!(db
        .verify_grovedb(Some(&tx), true, true, grove_version)
        .unwrap()
        .is_empty());
}
