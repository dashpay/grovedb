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
    Element, Error, GroveDb, PathQuery, Query,
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
        backward_references: Vec::new(),
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

#[test]
fn retargeting_into_a_cycle_is_rejected() {
    // Regression for a reproduced hang: with `A -> terminal` and `C -> A`,
    // retargeting `A -> C` used to validate against the pre-write graph
    // (following C resolved through the OLD A) and then loop forever in
    // backward propagation. The chain is now followed from the position
    // being written, so the prospective cycle is rejected before mutation.
    let grove_version = GroveVersion::latest();
    let db = db_with_bwr_item();

    db.insert(
        &[TEST_LEAF],
        b"a",
        sibling_bidi(b"value", true),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF],
        b"c",
        sibling_bidi(b"a", true),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    assert!(matches!(
        db.insert(
            &[TEST_LEAF],
            b"a",
            sibling_bidi(b"c", true),
            None,
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::CyclicReference)
    ));

    // Nothing was mutated: `a` still resolves through its original target
    // and the graph verifies.
    assert_eq!(
        db.get(&[TEST_LEAF], b"a", None, grove_version)
            .unwrap()
            .unwrap(),
        Element::new_item_allowing_bidirectional_references(b"hello".to_vec())
    );
    assert!(db
        .verify_grovedb(None, true, true, grove_version)
        .unwrap()
        .is_empty());
}

#[test]
fn reinserting_an_identical_edge_is_a_no_op() {
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
    // A second reference chained onto `ref` occupies its single backward
    // slot — an identical reinsertion of `chained` must still succeed.
    db.insert(
        &[TEST_LEAF],
        b"chained",
        sibling_bidi(b"ref", true),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    let root_before = db.root_hash(None, grove_version).unwrap().unwrap();

    for key in [b"ref".as_ref(), b"chained"] {
        db.insert(
            &[TEST_LEAF],
            key,
            sibling_bidi(if key == b"ref" { b"value" } else { b"ref" }, true),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap_or_else(|e| panic!("identical reinsertion of {key:?} must be a no-op: {e}"));
    }

    assert_eq!(
        db.root_hash(None, grove_version).unwrap().unwrap(),
        root_before,
        "identical reinsertions must not move the root hash"
    );
    assert!(db
        .verify_grovedb(None, true, true, grove_version)
        .unwrap()
        .is_empty());
}

#[test]
fn propagation_skips_and_cleans_origins_removed_without_bookkeeping() {
    // An origin removed through a path that performs no backward-references
    // bookkeeping (here: a batch delete, which is rejected only for ops
    // CARRYING the element family, not for ops touching participants)
    // leaves a dangling slot on its target. Later flagged updates must not
    // fail on it: the slot is skipped and lazily cleaned.
    let grove_version = GroveVersion::latest();
    let db = db_with_bwr_item();

    db.insert(
        &[TEST_LEAF],
        b"origin",
        sibling_bidi(b"value", true),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    // Batch-delete the origin: no backward-references bookkeeping runs.
    db.apply_batch(
        vec![crate::batch::QualifiedGroveDbOp::delete_op(
            vec![TEST_LEAF.to_vec()],
            b"origin".to_vec(),
        )],
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    // A flagged update of the target now encounters the dangling slot —
    // it must succeed, and afterwards the slot is free again.
    db.insert(
        &[TEST_LEAF],
        b"value",
        Element::new_item_allowing_bidirectional_references(b"updated".to_vec()),
        flag_on(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("dangling backward reference must be skipped, not fatal");

    assert!(db
        .verify_grovedb(None, true, true, grove_version)
        .unwrap()
        .is_empty());
}

#[test]
fn delete_with_flag_rejects_rows_of_indexed_primaries() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    db.insert(
        &[TEST_LEAF],
        b"pcit",
        Element::empty_provable_count_indexed_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    // The guard fires on the CONTAINING Merk's type, before any key lookup.
    assert!(matches!(
        db.delete(
            &[TEST_LEAF, b"pcit"],
            b"row",
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

/// The design invariant of on-element referrer lists: registering a new
/// referrer rewrites only the TARGET node (its referrer list, hence its
/// combined value hash) — referrers that already point at it keep their
/// stored node hashes bit-for-bit, because they commit to the target's
/// LOGICAL (stripped) hash, which the registration does not touch.
#[test]
fn registering_a_referrer_leaves_existing_referrer_nodes_untouched() {
    use grovedb_merk::element::get::ElementFetchFromStorageExtensions;

    let grove_version = GroveVersion::latest();
    let db = db_with_bwr_item();
    let tx = db.start_transaction();

    let node_hash = |tx, key: &[u8]| {
        Element::get_value_hash(
            &db.open_transactional_merk_at_path(
                SubtreePath::from(&[TEST_LEAF]),
                tx,
                None,
                grove_version,
            )
            .unwrap()
            .unwrap(),
            key,
            true,
            grove_version,
        )
        .unwrap()
        .unwrap()
        .unwrap()
    };

    db.insert(
        &[TEST_LEAF],
        b"ra",
        sibling_bidi(b"value", true),
        None,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .unwrap();

    let ra_before = node_hash(&tx, b"ra");
    let target_before = node_hash(&tx, b"value");

    db.insert(
        &[TEST_LEAF],
        b"rb",
        sibling_bidi(b"value", true),
        None,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .unwrap();

    // The target node re-hashed (its referrer list grew)...
    assert_ne!(node_hash(&tx, b"value"), target_before);
    // ...while the first referrer's stored node hash is untouched.
    assert_eq!(node_hash(&tx, b"ra"), ra_before);

    // The referrer list lives on the element at the merk level (both
    // registrations present)...
    let merk = db
        .open_transactional_merk_at_path(SubtreePath::from(&[TEST_LEAF]), &tx, None, grove_version)
        .unwrap()
        .unwrap();
    let full = Element::get(&merk, b"value", true, grove_version)
        .unwrap()
        .unwrap();
    assert_eq!(full.backward_references().unwrap().len(), 2);
    drop(merk);

    // ...but is stripped from public reads.
    let public = db
        .get(&[TEST_LEAF], b"value", Some(&tx), grove_version)
        .unwrap()
        .unwrap();
    assert_eq!(public.backward_references().unwrap().len(), 0);

    assert!(db
        .verify_grovedb(Some(&tx), true, true, grove_version)
        .unwrap()
        .is_empty());
}

/// Proofs over backward-references items ship the dedicated
/// `KVBackwardsReferencesValueHash` node: the payload is the STRIPPED
/// element and the referrer list rides along only as its 32-byte hash.
/// The verifier recombines the two, so tampering with either the payload
/// or the referrer-list hash breaks the root-hash chain.
#[test]
fn proofs_carry_stripped_payload_and_bind_the_referrer_hash() {
    let grove_version = GroveVersion::latest();
    let db = db_with_bwr_item();
    // A registered referrer, so the node's referrer-list hash is non-trivial.
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
    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .unwrap();

    // Honest proof: verifies against the live root hash and yields the
    // stripped element (no referrer list crosses the proof boundary).
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query, grove_version).unwrap();
    assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
    assert_eq!(
        result_set,
        vec![(
            vec![TEST_LEAF.to_vec()],
            b"value".to_vec(),
            Some(Element::new_item_allowing_bidirectional_references(
                b"hello".to_vec()
            ))
        )]
    );

    // Tampering with the referrer-list hash breaks verification.
    let tampered = tamper_backward_references_node(&proof, &path_query, |value, backrefs_hash| {
        backrefs_hash[0] ^= 1;
        let _ = value;
    })
    .expect("proof must contain a KVBackwardsReferencesValueHash node");
    assert!(
        GroveDb::verify_query(&tampered, &path_query, grove_version).is_err(),
        "flipped referrer-list hash must be rejected"
    );

    // So does tampering with the stripped payload bytes.
    let tampered = tamper_backward_references_node(&proof, &path_query, |value, _| {
        let last = value.len() - 1;
        value[last] ^= 1;
    })
    .expect("proof must contain a KVBackwardsReferencesValueHash node");
    assert!(
        GroveDb::verify_query(&tampered, &path_query, grove_version).is_err(),
        "flipped payload byte must be rejected"
    );
}

/// Decode the GroveDB proof envelope, walk to the leaf merk proof, apply
/// `mutate` to the first `KVBackwardsReferencesValueHash` node's
/// (stripped-value, referrer-list-hash) pair, and re-encode. `None` if the
/// leaf proof holds no such node.
fn tamper_backward_references_node(
    proof: &[u8],
    path_query: &PathQuery,
    mutate: impl Fn(&mut Vec<u8>, &mut [u8; 32]),
) -> Option<Vec<u8>> {
    use bincode::config;
    use grovedb_merk::proofs::{encoding::encode_into, Decoder, Node, Op};

    use crate::operations::proof::{GroveDBProof, GroveDBProofV1, LayerProof, ProofBytes};

    let cfg = config::standard()
        .with_big_endian()
        .with_limit::<{ 256 * 1024 * 1024 }>();
    let (mut decoded, _): (GroveDBProof, _) = bincode::decode_from_slice(proof, cfg).ok()?;

    let GroveDBProof::V1(GroveDBProofV1 { root_layer }) = &mut decoded else {
        return None;
    };
    let mut layer: &mut LayerProof = root_layer;
    for key in &path_query.path {
        layer = layer.lower_layers.get_mut(key)?;
    }
    let ProofBytes::Merk(leaf_bytes) = &mut layer.merk_proof else {
        return None;
    };

    let mut ops: Vec<Op> = Vec::new();
    for op in Decoder::new(leaf_bytes) {
        ops.push(op.ok()?);
    }

    let mut tampered = false;
    for op in ops.iter_mut() {
        match op {
            Op::Push(Node::KVBackwardsReferencesValueHash(_, value, backrefs_hash))
            | Op::PushInverted(Node::KVBackwardsReferencesValueHash(_, value, backrefs_hash)) => {
                mutate(value, backrefs_hash);
                tampered = true;
                break;
            }
            _ => {}
        }
    }
    if !tampered {
        return None;
    }

    let mut new_leaf = Vec::new();
    encode_into(ops.iter(), &mut new_leaf);
    *leaf_bytes = new_leaf;

    bincode::encode_to_vec(
        decoded,
        config::standard().with_big_endian().with_no_limit(),
    )
    .ok()
}
