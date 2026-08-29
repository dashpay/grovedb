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

/// Proving a bidirectional reference through the main V1 subquery loop:
/// the emitted `KVRefValueHash` node must carry the reference's
/// combined-hash override and the STRIPPED dereferenced target, in both
/// query directions.
#[test]
fn proofs_dereference_bidirectional_references_in_both_directions() {
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

    for left_to_right in [true, false] {
        let mut query = Query::new_with_direction(left_to_right);
        query.insert_key(b"value".to_vec());
        query.insert_key(b"ref".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) = GroveDb::verify_query(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        let expected = Some(Element::new_item_allowing_bidirectional_references(
            b"hello".to_vec(),
        ));
        assert_eq!(result_set.len(), 2);
        for (_, _, element) in result_set {
            assert_eq!(element, expected, "left_to_right: {left_to_right}");
        }
    }
}

/// Bidirectional references living inside aggregate parents resolve
/// through the aggregate-carrying `KVRefValueHash{Sum,Count}` proof nodes
/// with the combined-hash override as the carried self-hash.
#[test]
fn proofs_dereference_bidirectional_references_in_aggregate_parents() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    for (tree_key, tree, target) in [
        (
            b"sums".as_slice(),
            Element::new_sum_tree(None),
            Element::new_sum_item_allowing_bidirectional_references(41),
        ),
        (
            b"counts".as_slice(),
            Element::new_count_tree(None),
            Element::new_item_allowing_bidirectional_references(b"counted".to_vec()),
        ),
    ] {
        db.insert(&[TEST_LEAF], tree_key, tree, None, None, grove_version)
            .unwrap()
            .unwrap();
        db.insert(
            &[TEST_LEAF, tree_key],
            b"target",
            target.clone(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
        db.insert(
            &[TEST_LEAF, tree_key],
            b"zref",
            sibling_bidi(b"target", true),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();

        let mut query = Query::new();
        query.insert_key(b"zref".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), tree_key.to_vec()], query);
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .unwrap();
        let (hash, result_set) = GroveDb::verify_query(&proof, &path_query, grove_version).unwrap();
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(
            result_set,
            vec![(
                vec![TEST_LEAF.to_vec(), tree_key.to_vec()],
                b"zref".to_vec(),
                Some(target)
            )],
            "tree: {}",
            String::from_utf8_lossy(tree_key)
        );
    }
}

/// Pre-V4 versions can neither store nor prove the family: inserts under
/// `GROVE_V1` are refused, and the V0 prover refuses to serve a tree that
/// contains it (the V0 wire format is frozen).
#[test]
fn pre_v4_versions_reject_the_family_end_to_end() {
    use grovedb_version::version::v1::GROVE_V1;

    let latest = GroveVersion::latest();
    let db = db_with_bwr_item();

    let v1 = &GROVE_V1;
    assert!(matches!(
        db.insert(
            &[TEST_LEAF],
            b"old",
            Element::new_item_allowing_bidirectional_references(b"x".to_vec()),
            None,
            None,
            v1,
        )
        .unwrap(),
        Err(Error::NotSupported(_))
    ));

    let mut query = Query::new();
    query.insert_key(b"value".to_vec());
    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);
    assert!(matches!(
        db.prove_query(&path_query, None, v1).unwrap(),
        Err(Error::NotSupported(_))
    ));

    // Sanity: the same db serves the same query under the latest version.
    db.prove_query(&path_query, None, latest).unwrap().unwrap();
}

/// A batch-inserted PLAIN reference may resolve through a pre-existing
/// bidirectional reference: the chain hash it commits to is the end
/// target's logical hash.
#[test]
fn batch_references_resolve_through_pre_existing_bidirectional_references() {
    use crate::batch::QualifiedGroveDbOp;

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

    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![TEST_LEAF.to_vec()],
        b"batchref".to_vec(),
        Element::new_reference(ReferencePathType::SiblingReference(b"ref".to_vec())),
    )];
    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("a plain reference chaining through a bidi reference is fine");

    assert!(db
        .verify_grovedb(None, true, true, grove_version)
        .unwrap()
        .is_empty());
}

/// The flagged insert path applies the same overwrite guards as the plain
/// path: overwriting a tree is refused when the option forbids it, and a
/// fresh tree insert must arrive empty.
#[test]
fn flagged_inserts_enforce_tree_shape_guards() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    db.insert(
        &[TEST_LEAF],
        b"tree",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    let opts = Some(InsertOptions {
        validate_insertion_does_not_override: false,
        validate_insertion_does_not_override_tree: true,
        propagate_backward_references: true,
        ..Default::default()
    });
    assert!(matches!(
        db.insert(
            &[TEST_LEAF],
            b"tree",
            Element::new_item(b"clobber".to_vec()),
            opts,
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::OverrideNotAllowed(_))
    ));

    // A non-empty tree element cannot be written through the non-batch path.
    assert!(db
        .insert(
            &[TEST_LEAF],
            b"tree2",
            Element::Tree(Some(b"phantom".to_vec()), None),
            flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .is_err());
}

/// Reads that follow a reference whose target was removed by an unflagged
/// write surface the dedicated corrupted-reference error.
#[test]
fn dangling_bidirectional_reference_reads_report_corruption() {
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

    // Unflagged delete skips all bookkeeping — allowed, consistency
    // forfeited.
    db.delete(&[TEST_LEAF], b"value", None, None, grove_version)
        .unwrap()
        .unwrap();

    assert!(matches!(
        db.get(&[TEST_LEAF], b"ref", None, grove_version).unwrap(),
        Err(Error::CorruptedReferencePathKeyNotFound(_))
    ));

    // Same through a transaction.
    let tx = db.start_transaction();
    assert!(matches!(
        db.get(&[TEST_LEAF], b"ref", Some(&tx), grove_version)
            .unwrap(),
        Err(Error::CorruptedReferencePathKeyNotFound(_))
    ));
}

/// Lazy cleanup of dangling referrer entries ON A REFERENCE node: in the
/// chain a -> b -> value, removing `a` without bookkeeping leaves `b`
/// with a dead referrer entry; the next flagged propagation through `b`
/// clears it (and rewrites `b` against the end hash it commits to).
#[test]
fn propagation_cleans_dangling_referrers_on_chained_references() {
    use grovedb_merk::element::get::ElementFetchFromStorageExtensions;

    let grove_version = GroveVersion::latest();
    let db = db_with_bwr_item();
    let tx = db.start_transaction();
    db.insert(
        &[TEST_LEAF],
        b"b",
        sibling_bidi(b"value", true),
        None,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF],
        b"a",
        sibling_bidi(b"b", true),
        None,
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .unwrap();

    // Remove the chain head without bookkeeping.
    db.delete(&[TEST_LEAF], b"a", None, Some(&tx), grove_version)
        .unwrap()
        .unwrap();

    // Flagged update of the end target propagates through `b`, which finds
    // its referrer `a` dangling and lazily drops the entry.
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

    let merk = db
        .open_transactional_merk_at_path(SubtreePath::from(&[TEST_LEAF]), &tx, None, grove_version)
        .unwrap()
        .unwrap();
    let b_full = Element::get(&merk, b"b", true, grove_version)
        .unwrap()
        .unwrap();
    assert_eq!(
        b_full.backward_references().unwrap().len(),
        0,
        "the dangling referrer entry must be lazily dropped"
    );
    drop(merk);

    assert!(db
        .verify_grovedb(Some(&tx), true, true, grove_version)
        .unwrap()
        .is_empty());
}

/// Retargeting tolerates old targets that were rewritten without
/// bookkeeping: the removal step finds either an element that no longer
/// supports backward references or one whose referrer entry is gone, and
/// treats both as already-clean.
#[test]
fn retargeting_tolerates_targets_rewritten_without_bookkeeping() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    for (key, value) in [
        (b"t1".as_slice(), b"one".as_slice()),
        (b"t2", b"two"),
        (b"t3", b"three"),
        (b"t4", b"four"),
    ] {
        db.insert(
            &[TEST_LEAF],
            key,
            Element::new_item_allowing_bidirectional_references(value.to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    }
    db.insert(
        &[TEST_LEAF],
        b"r1",
        sibling_bidi(b"t1", true),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF],
        b"r2",
        sibling_bidi(b"t3", true),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    // t1 overwritten by a PLAIN item (no backward-references support) via
    // an unflagged write; retargeting r1 finds nothing to clean.
    db.insert(
        &[TEST_LEAF],
        b"t1",
        Element::new_item(b"plain".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF],
        b"r1",
        sibling_bidi(b"t2", true),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    // t3 overwritten by a FRESH backward-references item (empty referrer
    // list) via an unflagged write; retargeting r2 finds its entry gone.
    db.insert(
        &[TEST_LEAF],
        b"t3",
        Element::new_item_allowing_bidirectional_references(b"fresh".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF],
        b"r2",
        sibling_bidi(b"t4", true),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    assert!(db
        .verify_grovedb(None, true, true, grove_version)
        .unwrap()
        .is_empty());
}

/// Every Provable* aggregate parent refuses backward-references items, and
/// a bidirectional reference cannot be written through the plain
/// `Element::insert` (it must carry its resolved end hash).
#[test]
fn provable_parents_and_direct_inserts_reject_the_family() {
    use grovedb_merk::element::insert::ElementInsertToStorageExtensions;

    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    for (key, tree, item) in [
        (
            b"pc".as_slice(),
            Element::new_provable_count_tree(None),
            Element::new_item_allowing_bidirectional_references(b"x".to_vec()),
        ),
        (
            b"ps",
            Element::new_provable_sum_tree(None),
            Element::new_sum_item_allowing_bidirectional_references(1),
        ),
        (
            b"pcs",
            Element::new_provable_count_sum_tree(None),
            Element::new_sum_item_allowing_bidirectional_references(1),
        ),
        (
            b"pcps",
            Element::new_provable_count_provable_sum_tree(None),
            Element::new_sum_item_allowing_bidirectional_references(1),
        ),
    ] {
        db.insert(&[TEST_LEAF], key, tree, None, None, grove_version)
            .unwrap()
            .unwrap();
        assert!(
            db.insert(&[TEST_LEAF, key], b"k", item, None, None, grove_version)
                .unwrap()
                .is_err(),
            "Provable* parent {} must reject the family",
            String::from_utf8_lossy(key)
        );
    }

    let tx = db.start_transaction();
    let mut merk = db
        .open_transactional_merk_at_path(SubtreePath::from(&[TEST_LEAF]), &tx, None, grove_version)
        .unwrap()
        .unwrap();
    assert!(
        sibling_bidi(b"whatever", true)
            .insert(&mut merk, b"direct", None, grove_version)
            .unwrap()
            .is_err(),
        "plain Element::insert must refuse bidirectional references"
    );
}

/// The verifier refuses backward-references elements smuggled inside the
/// PLAIN value-carrying node kinds: their value hash is `H(value)` there,
/// which would let forged bytes ride unbound on the carried hash. It also
/// refuses the dedicated node kind inside a frozen V0 envelope.
#[test]
fn verifier_rejects_family_smuggled_into_plain_nodes_and_v0_envelopes() {
    use bincode::config;
    use grovedb_merk::{proofs::Node, TreeFeatureType};

    use crate::operations::proof::{
        GroveDBProof, GroveDBProofV0, GroveDBProofV1, LayerProof, MerkOnlyLayerProof, ProofBytes,
        ProveOptions,
    };

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
    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);
    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .unwrap();

    // Rebuild the honest node as a plain KVValueHash: rejected by node-type
    // triage regardless of the carried hash.
    let forged = swap_backward_references_node(&proof, &path_query, |key, value, backrefs| {
        Node::KVValueHash(key, value, backrefs)
    })
    .expect("proof must contain a KVBackwardsReferencesValueHash node");
    let err = GroveDb::verify_query(&forged, &path_query, grove_version)
        .expect_err("KVValueHash smuggling must fail");
    assert!(
        err.to_string()
            .contains("KVValueHash node must not contain"),
        "got: {err}"
    );

    // Same through KVValueHashFeatureType.
    let forged = swap_backward_references_node(&proof, &path_query, |key, value, backrefs| {
        Node::KVValueHashFeatureType(key, value, backrefs, TreeFeatureType::BasicMerkNode)
    })
    .expect("proof must contain a KVBackwardsReferencesValueHash node");
    let err = GroveDb::verify_query(&forged, &path_query, grove_version)
        .expect_err("KVValueHashFeatureType smuggling must fail");
    assert!(
        err.to_string()
            .contains("KVValueHashFeatureType node must not contain"),
        "got: {err}"
    );

    // Downgrade the honest V1 envelope to V0 wholesale: the V0 verifier
    // must refuse the node kind (frozen wire format).
    let cfg = config::standard()
        .with_big_endian()
        .with_limit::<{ 256 * 1024 * 1024 }>();
    let (decoded, _): (GroveDBProof, _) = bincode::decode_from_slice(&proof, cfg).unwrap();
    let GroveDBProof::V1(GroveDBProofV1 { root_layer }) = decoded else {
        panic!("expected a V1 envelope");
    };
    fn downgrade(layer: LayerProof) -> Option<MerkOnlyLayerProof> {
        let ProofBytes::Merk(merk_proof) = layer.merk_proof else {
            return None;
        };
        let mut lower_layers = std::collections::BTreeMap::new();
        for (key, lower) in layer.lower_layers {
            lower_layers.insert(key, downgrade(lower)?);
        }
        Some(MerkOnlyLayerProof {
            merk_proof,
            lower_layers,
        })
    }
    let v0 = GroveDBProof::V0(GroveDBProofV0 {
        root_layer: downgrade(root_layer).expect("plain merk layers"),
        prove_options: ProveOptions::default(),
    });
    let v0_bytes =
        bincode::encode_to_vec(v0, config::standard().with_big_endian().with_no_limit()).unwrap();
    let err = GroveDb::verify_query(&v0_bytes, &path_query, grove_version)
        .expect_err("V0 envelopes must not carry the node kind");
    assert!(
        err.to_string().contains("not allowed in V0 proofs"),
        "got: {err}"
    );
}

/// Replace the first `KVBackwardsReferencesValueHash` node in the leaf
/// merk proof with whatever `build` returns from its parts, re-encoding
/// the envelope. `None` if no such node exists.
fn swap_backward_references_node(
    proof: &[u8],
    path_query: &PathQuery,
    build: impl Fn(Vec<u8>, Vec<u8>, [u8; 32]) -> grovedb_merk::proofs::Node,
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
    let mut swapped = false;
    for op in ops.iter_mut() {
        let rebuilt = match op {
            Op::Push(Node::KVBackwardsReferencesValueHash(key, value, hash)) => {
                Op::Push(build(key.clone(), value.clone(), *hash))
            }
            Op::PushInverted(Node::KVBackwardsReferencesValueHash(key, value, hash)) => {
                Op::PushInverted(build(key.clone(), value.clone(), *hash))
            }
            _ => continue,
        };
        *op = rebuilt;
        swapped = true;
        break;
    }
    if !swapped {
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

/// Plain writes routed through the pre-V4 dispatch arms still work on a
/// database that also holds V4 content elsewhere: the flag-less v0 bodies
/// are selected for `GROVE_V3` and overwrite through the storage-read
/// funnel.
#[test]
fn plain_writes_still_route_through_pre_v4_dispatch() {
    use grovedb_version::version::v3::GROVE_V3;

    let db = make_test_grovedb(GroveVersion::latest());
    let v3 = &GROVE_V3;
    db.insert(
        &[TEST_LEAF],
        b"plain",
        Element::new_item(b"one".to_vec()),
        None,
        None,
        v3,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF],
        b"plain",
        Element::new_item(b"two".to_vec()),
        None,
        None,
        v3,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        db.get(&[TEST_LEAF], b"plain", None, v3).unwrap().unwrap(),
        Element::new_item(b"two".to_vec())
    );
}

/// `verify_grovedb` recomputes the combined (inner ‖ backrefs) hash for
/// backward-references items and reports nodes whose stored value hash
/// does not match — e.g. one written with a corrupt provided hash.
#[test]
fn verify_grovedb_reports_corrupt_provided_value_hashes() {
    use grovedb_merk::{tree::Op as MerkOp, TreeFeatureType};
    use grovedb_storage::{Storage, StorageBatch};

    let grove_version = GroveVersion::latest();
    let db = db_with_bwr_item();
    let tx = db.start_transaction();

    let bytes = Element::new_item_allowing_bidirectional_references(b"evil".to_vec())
        .serialize(grove_version)
        .unwrap();
    let batch = StorageBatch::new();
    let mut merk = db
        .open_transactional_merk_at_path(
            SubtreePath::from(&[TEST_LEAF]),
            &tx,
            Some(&batch),
            grove_version,
        )
        .unwrap()
        .unwrap();
    merk.apply::<_, Vec<u8>>(
        &[(
            b"bad".to_vec(),
            MerkOp::PutWithProvidedValueHash(bytes, [7; 32], TreeFeatureType::BasicMerkNode),
        )],
        &[],
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    drop(merk);
    db.db
        .commit_multi_context_batch(batch, Some(&tx))
        .unwrap()
        .unwrap();

    let issues = db
        .verify_grovedb(Some(&tx), true, true, grove_version)
        .unwrap();
    assert!(
        issues
            .keys()
            .any(|path| path.last().map(|k| k.as_slice()) == Some(b"bad".as_slice())),
        "the corrupt node must be reported, got: {issues:?}"
    );
}

/// The batch fast path for hop-budget-1 references binds the target's
/// merk-stored value hash directly; for a backward-references terminal
/// that stored hash is the COMBINED (inner ‖ backrefs) hash, so the fast
/// path must fall back to the logical hash instead.
#[test]
fn batch_hop_one_references_commit_the_logical_hash_of_family_targets() {
    use crate::batch::QualifiedGroveDbOp;

    let grove_version = GroveVersion::latest();
    let db = db_with_bwr_item();
    // Register a referrer so combined != logical.
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

    let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![TEST_LEAF.to_vec()],
        b"hop1".to_vec(),
        Element::Reference(
            ReferencePathType::SiblingReference(b"value".to_vec()),
            Some(1),
            None,
        ),
    )];
    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("hop-1 reference to a backward-references item is well-formed");

    assert!(db
        .verify_grovedb(None, true, true, grove_version)
        .unwrap()
        .is_empty());
}

/// Every query surface resolves the whole terminal-element zoo through
/// references: plain items, sum items, combined items, and their
/// backward-references twins — directly and dereferenced.
#[test]
fn query_surfaces_resolve_every_terminal_shape_through_references() {
    let grove_version = GroveVersion::latest();
    let db = db_with_bwr_item();
    db.insert(
        &[TEST_LEAF],
        b"sums",
        Element::new_sum_tree(None),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    for (key, element) in [
        (b"sum".as_slice(), Element::new_sum_item(9)),
        (b"isi", Element::ItemWithSumItem(b"both".to_vec(), 4, None)),
        (
            b"bsum",
            Element::new_sum_item_allowing_bidirectional_references(2),
        ),
    ] {
        db.insert(
            &[TEST_LEAF, b"sums"],
            key,
            element,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    }
    let into_sums = |key: &[u8]| {
        ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"sums".to_vec(),
            key.to_vec(),
        ])
    };
    for (key, target) in [
        (b"r_sum".as_slice(), into_sums(b"sum")),
        (b"r_isi", into_sums(b"isi")),
        (
            b"r_val",
            ReferencePathType::SiblingReference(b"value".to_vec()),
        ),
    ] {
        db.insert(
            &[TEST_LEAF],
            key,
            Element::new_reference(target),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    }
    db.insert(
        &[TEST_LEAF],
        b"r_bsum",
        Element::BidirectionalReference(BidirectionalReference {
            forward_reference_path: into_sums(b"bsum"),
            backward_references: Vec::new(),
            cascade_on_update: true,
            max_hop: None,
            flags: None,
        }),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    let query_for = |path: Vec<Vec<u8>>, keys: &[&[u8]]| {
        let mut query = Query::new();
        for key in keys {
            query.insert_key(key.to_vec());
        }
        PathQuery::new_unsized(path, query)
    };
    let deref_query = query_for(
        vec![TEST_LEAF.to_vec()],
        &[b"value", b"r_sum", b"r_isi", b"r_val", b"r_bsum"],
    );
    let direct_query = query_for(
        vec![TEST_LEAF.to_vec(), b"sums".to_vec()],
        &[b"sum", b"isi", b"bsum"],
    );

    // Raw item values across dereferenced and direct shapes.
    let (values, _) = db
        .query_item_value(&deref_query, true, true, true, None, grove_version)
        .unwrap()
        .unwrap();
    assert_eq!(values.len(), 5);
    let (values, _) = db
        .query_item_value(&direct_query, true, true, true, None, grove_version)
        .unwrap()
        .unwrap();
    assert_eq!(values.len(), 3);

    // Item-or-sum view sees the sums as sums.
    for query in [&deref_query, &direct_query] {
        let (values, _) = db
            .query_item_value_or_sum(query, true, true, true, None, grove_version)
            .unwrap()
            .unwrap();
        assert!(!values.is_empty());
    }

    // Sum-only surface: direct and dereferenced sum items.
    let (values, _) = db
        .query_sums(
            &query_for(vec![TEST_LEAF.to_vec()], &[b"r_sum", b"r_bsum"]),
            true,
            true,
            true,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    assert_eq!(values.iter().sum::<i64>(), 9 + 2);
    let (values, _) = db
        .query_sums(
            &query_for(
                vec![TEST_LEAF.to_vec(), b"sums".to_vec()],
                &[b"sum", b"bsum"],
            ),
            true,
            true,
            true,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    assert_eq!(values.iter().sum::<i64>(), 9 + 2);

    // The deprecated encoded-many surface accepts reference-only results.
    let refs_query = query_for(
        vec![TEST_LEAF.to_vec()],
        &[b"r_sum", b"r_isi", b"r_val", b"r_bsum"],
    );
    #[allow(deprecated)]
    let encoded = db
        .query_encoded_many(&[&refs_query], true, true, true, None, grove_version)
        .unwrap()
        .unwrap();
    assert_eq!(encoded.len(), 4);
}

/// The non-batch insert path refuses every non-empty tree variant, not
/// just plain trees.
#[test]
fn flagged_inserts_refuse_every_non_empty_tree_variant() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    let phantom = Some(b"phantom".to_vec());
    for (idx, tree) in [
        Element::Tree(phantom.clone(), None),
        Element::SumTree(phantom.clone(), 0, None),
        Element::BigSumTree(phantom.clone(), 0, None),
        Element::CountTree(phantom.clone(), 0, None),
        Element::CountSumTree(phantom.clone(), 0, 0, None),
        Element::ProvableCountTree(phantom.clone(), 0, None),
        Element::ProvableCountSumTree(phantom.clone(), 0, 0, None),
        Element::ProvableSumTree(phantom.clone(), 0, None),
        Element::ProvableCountProvableSumTree(phantom.clone(), 0, 0, None),
    ]
    .into_iter()
    .enumerate()
    {
        assert!(
            matches!(
                db.insert(
                    &[TEST_LEAF],
                    format!("t{idx}").as_bytes(),
                    tree,
                    flag_on(),
                    None,
                    grove_version,
                )
                .unwrap(),
                Err(Error::InvalidCodeExecution(_))
            ),
            "non-empty tree variant {idx} must be refused"
        );
    }
}
