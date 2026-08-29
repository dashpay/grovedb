//! Batch support for the backward-references family (batching M2–M4): the
//! master invariant is that a batch under
//! `BatchApplyOptions::propagate_backward_references` produces the exact
//! root hash the live flagged flow produces for the same logical
//! operations — including `BidirectionalReference` ops, in-batch targets
//! and chains, retargets, identical-edge no-ops, and the M4 conflict
//! rules.

use grovedb_version::version::GroveVersion;

use crate::{
    batch::{BatchApplyOptions, QualifiedGroveDbOp},
    bidirectional_references::BidirectionalReference,
    operations::{delete::DeleteOptions, insert::InsertOptions},
    reference_path::ReferencePathType,
    tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
    Element, Error,
};

fn flag_on() -> Option<InsertOptions> {
    Some(InsertOptions {
        propagate_backward_references: true,
        ..Default::default()
    })
}

fn batch_flag_on() -> Option<BatchApplyOptions> {
    Some(BatchApplyOptions {
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

/// Two identical databases: TEST_LEAF holding a registered target chain
/// `r2 -> r1 -> value`.
fn twin_dbs_with_chain(grove_version: &GroveVersion) -> (TempGroveDb, TempGroveDb) {
    let build = || {
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
        db.insert(
            &[TEST_LEAF],
            b"r1",
            sibling_bidi(b"value", true),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
        db.insert(
            &[TEST_LEAF],
            b"r2",
            sibling_bidi(b"r1", true),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
        db
    };
    (build(), build())
}

fn roots_match(batch_db: &TempGroveDb, live_db: &TempGroveDb, grove_version: &GroveVersion) {
    assert_eq!(
        batch_db.root_hash(None, grove_version).unwrap().unwrap(),
        live_db.root_hash(None, grove_version).unwrap().unwrap(),
        "batch and live flows must produce byte-identical root hashes"
    );
    assert!(batch_db
        .verify_grovedb(None, true, true, grove_version)
        .unwrap()
        .is_empty());
}

#[test]
fn batch_fresh_insert_matches_live() {
    let grove_version = GroveVersion::latest();
    let batch_db = make_test_grovedb(grove_version);
    let live_db = make_test_grovedb(grove_version);

    batch_db
        .apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"value".to_vec(),
                Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    live_db
        .insert(
            &[TEST_LEAF],
            b"value",
            Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
            flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();

    roots_match(&batch_db, &live_db, grove_version);
}

#[test]
fn batch_overwrite_propagates_along_the_chain_like_live() {
    let grove_version = GroveVersion::latest();
    let (batch_db, live_db) = twin_dbs_with_chain(grove_version);

    let updated = Element::new_item_allowing_bidirectional_references(b"updated".to_vec());
    batch_db
        .apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"value".to_vec(),
                updated.clone(),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    live_db
        .insert(
            &[TEST_LEAF],
            b"value",
            updated,
            flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();

    roots_match(&batch_db, &live_db, grove_version);
}

#[test]
fn batch_sum_twin_overwrite_matches_live() {
    let grove_version = GroveVersion::latest();
    let build = || {
        let db = make_test_grovedb(grove_version);
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
        db.insert(
            &[TEST_LEAF, b"sums"],
            b"twin",
            Element::new_item_with_sum_item_allowing_bidirectional_references(b"pay".to_vec(), 5),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
        db.insert(
            &[TEST_LEAF, b"sums"],
            b"ref",
            sibling_bidi(b"twin", true),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
        db
    };
    let (batch_db, live_db) = (build(), build());

    let updated =
        Element::new_item_with_sum_item_allowing_bidirectional_references(b"pay2".to_vec(), 8);
    batch_db
        .apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"sums".to_vec()],
                b"twin".to_vec(),
                updated.clone(),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    live_db
        .insert(
            &[TEST_LEAF, b"sums"],
            b"twin",
            updated,
            flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();

    roots_match(&batch_db, &live_db, grove_version);
}

#[test]
fn batch_delete_cascades_like_live() {
    let grove_version = GroveVersion::latest();
    let (batch_db, live_db) = twin_dbs_with_chain(grove_version);

    batch_db
        .apply_batch(
            vec![QualifiedGroveDbOp::delete_op(
                vec![TEST_LEAF.to_vec()],
                b"value".to_vec(),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    live_db
        .delete(
            &[TEST_LEAF],
            b"value",
            Some(DeleteOptions {
                propagate_backward_references: true,
                ..Default::default()
            }),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();

    // The whole chain is gone on both sides.
    for db in [&batch_db, &live_db] {
        for key in [b"value".as_slice(), b"r1", b"r2"] {
            assert!(matches!(
                db.get(&[TEST_LEAF], key, None, grove_version).unwrap(),
                Err(Error::PathKeyNotFound(_))
            ));
        }
    }
    roots_match(&batch_db, &live_db, grove_version);
}

#[test]
fn batch_overwrite_with_plain_item_cascades_like_live() {
    let grove_version = GroveVersion::latest();
    let (batch_db, live_db) = twin_dbs_with_chain(grove_version);

    let plain = Element::new_item(b"plain".to_vec());
    batch_db
        .apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"value".to_vec(),
                plain.clone(),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    live_db
        .insert(
            &[TEST_LEAF],
            b"value",
            plain,
            flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();

    roots_match(&batch_db, &live_db, grove_version);
}

#[test]
fn batch_cascade_requires_consent() {
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
    db.insert(
        &[TEST_LEAF],
        b"r1",
        sibling_bidi(b"value", false),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    assert!(matches!(
        db.apply_batch(
            vec![QualifiedGroveDbOp::delete_op(
                vec![TEST_LEAF.to_vec()],
                b"value".to_vec(),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::BidirectionalReferenceRule(_))
    ));
}

#[test]
fn batch_clears_caller_supplied_referrer_lists() {
    use grovedb_merk::element::get::ElementFetchFromStorageExtensions;
    use grovedb_path::SubtreePath;

    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    let forged = crate::bidirectional_references::BackwardReference {
        inverted_reference: ReferencePathType::SiblingReference(b"victim".to_vec()),
        cascade_on_update: true,
    };
    db.apply_batch(
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"planted".to_vec(),
            Element::ItemWithBackwardsReferences(b"x".to_vec(), vec![forged], None),
        )],
        batch_flag_on(),
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    let tx = db.start_transaction();
    let merk = db
        .open_transactional_merk_at_path(SubtreePath::from(&[TEST_LEAF]), &tx, None, grove_version)
        .unwrap()
        .unwrap();
    assert_eq!(
        Element::get(&merk, b"planted", true, grove_version)
            .unwrap()
            .unwrap()
            .backward_references()
            .unwrap()
            .len(),
        0,
        "forged referrer entries must not persist through batches"
    );
}

#[test]
fn batch_rejections_hold() {
    let grove_version = GroveVersion::latest();
    let (db, _other) = twin_dbs_with_chain(grove_version);

    // Family item ops without the flag: rejected.
    assert!(matches!(
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"fresh".to_vec(),
                Element::new_item_allowing_bidirectional_references(b"x".to_vec()),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::NotSupported(_))
    ));

    // BidirectionalReference element ops without the flag: rejected.
    assert!(matches!(
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"newref".to_vec(),
                sibling_bidi(b"value", true),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::NotSupported(_))
    ));

    // The internal derived op cannot be supplied by callers.
    assert!(matches!(
        db.apply_batch(
            vec![QualifiedGroveDbOp {
                path: crate::batch::KeyInfoPath::from_known_owned_path(vec![TEST_LEAF.to_vec()]),
                key: Some(crate::batch::key_info::KeyInfo::KnownKey(b"value".to_vec())),
                op: crate::batch::GroveOp::ReplaceBackwardReferenceFamilyMember {
                    element: Element::new_item_allowing_bidirectional_references(b"x".to_vec()),
                    node_value_hash: [7; 32],
                },
            }],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::NotSupported(_))
    ));

    // A derived rewrite colliding with a user op on the same position
    // (the propagation from overwriting `value` must rewrite `r1`, which
    // another op deletes): the M4 conflict rules fail closed.
    assert!(matches!(
        db.apply_batch(
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"value".to_vec(),
                    Element::new_item_allowing_bidirectional_references(b"updated".to_vec()),
                ),
                QualifiedGroveDbOp::delete_op(vec![TEST_LEAF.to_vec()], b"r1".to_vec()),
            ],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::InvalidBatchOperation(_))
    ));

    // Pre-V4 versions fail closed even with the flag.
    let v3 = &grovedb_version::version::v3::GROVE_V3;
    assert!(matches!(
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"fresh".to_vec(),
                Element::new_item_allowing_bidirectional_references(b"x".to_vec()),
            )],
            batch_flag_on(),
            None,
            v3,
        )
        .unwrap(),
        Err(Error::NotSupported(_))
    ));
}

// ─── Batching M3: BidirectionalReference ops ────────────────────────────

#[test]
fn batch_bidi_insert_with_existing_target_matches_live() {
    let grove_version = GroveVersion::latest();
    let build = || {
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
    };
    let (batch_db, live_db) = (build(), build());

    batch_db
        .apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"ref".to_vec(),
                sibling_bidi(b"value", true),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    live_db
        .insert(
            &[TEST_LEAF],
            b"ref",
            sibling_bidi(b"value", true),
            flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();

    roots_match(&batch_db, &live_db, grove_version);
}

#[test]
fn batch_bidi_insert_with_in_batch_target_matches_live_in_any_op_order() {
    let grove_version = GroveVersion::latest();
    let target = Element::new_item_allowing_bidirectional_references(b"fresh".to_vec());

    // The reference op comes FIRST in the batch — the preprocessor must
    // still resolve it against the target created by the second op.
    for (op_a, op_b) in [
        (
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"ref".to_vec(),
                sibling_bidi(b"value", true),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"value".to_vec(),
                target.clone(),
            ),
        ),
        (
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"value".to_vec(),
                target.clone(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"ref".to_vec(),
                sibling_bidi(b"value", true),
            ),
        ),
    ] {
        let batch_db = make_test_grovedb(grove_version);
        let live_db = make_test_grovedb(grove_version);
        batch_db
            .apply_batch(vec![op_a, op_b], batch_flag_on(), None, grove_version)
            .unwrap()
            .unwrap();
        // The live twin's only valid sequential order is target first.
        live_db
            .insert(
                &[TEST_LEAF],
                b"value",
                target.clone(),
                flag_on(),
                None,
                grove_version,
            )
            .unwrap()
            .unwrap();
        live_db
            .insert(
                &[TEST_LEAF],
                b"ref",
                sibling_bidi(b"value", true),
                flag_on(),
                None,
                grove_version,
            )
            .unwrap()
            .unwrap();
        roots_match(&batch_db, &live_db, grove_version);
    }
}

#[test]
fn batch_whole_chain_created_in_one_batch_matches_live() {
    let grove_version = GroveVersion::latest();
    let batch_db = make_test_grovedb(grove_version);
    let live_db = make_test_grovedb(grove_version);

    // Shuffled op order: r2 -> r1 -> value, submitted referrers-first.
    batch_db
        .apply_batch(
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"r2".to_vec(),
                    sibling_bidi(b"r1", true),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"r1".to_vec(),
                    sibling_bidi(b"value", true),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"value".to_vec(),
                    Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
                ),
            ],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    for (key, element) in [
        (
            b"value".as_slice(),
            Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
        ),
        (b"r1", sibling_bidi(b"value", true)),
        (b"r2", sibling_bidi(b"r1", true)),
    ] {
        live_db
            .insert(&[TEST_LEAF], key, element, flag_on(), None, grove_version)
            .unwrap()
            .unwrap();
    }

    roots_match(&batch_db, &live_db, grove_version);
}

#[test]
fn batch_retarget_matches_live() {
    let grove_version = GroveVersion::latest();
    let build = || {
        let db = make_test_grovedb(grove_version);
        for (key, element) in [
            (
                b"a".as_slice(),
                Element::new_item_allowing_bidirectional_references(b"va".to_vec()),
            ),
            (
                b"b",
                Element::new_item_allowing_bidirectional_references(b"vb".to_vec()),
            ),
            (b"ref", sibling_bidi(b"a", true)),
        ] {
            db.insert(&[TEST_LEAF], key, element, flag_on(), None, grove_version)
                .unwrap()
                .unwrap();
        }
        db
    };
    let (batch_db, live_db) = (build(), build());

    batch_db
        .apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"ref".to_vec(),
                sibling_bidi(b"b", true),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    live_db
        .insert(
            &[TEST_LEAF],
            b"ref",
            sibling_bidi(b"b", true),
            flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();

    roots_match(&batch_db, &live_db, grove_version);
}

#[test]
fn batch_retarget_with_upstream_referrer_matches_live() {
    let grove_version = GroveVersion::latest();
    // r2 -> r1 -> value; retarget r1 onto a second item: r1's registration
    // moves, and r2 must be rewritten with the new end hash.
    let build = || {
        let (db, _twin) = twin_dbs_with_chain(grove_version);
        db.insert(
            &[TEST_LEAF],
            b"other",
            Element::new_item_allowing_bidirectional_references(b"other".to_vec()),
            flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
        db
    };
    let (batch_db, live_db) = (build(), build());

    batch_db
        .apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"r1".to_vec(),
                sibling_bidi(b"other", true),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    live_db
        .insert(
            &[TEST_LEAF],
            b"r1",
            sibling_bidi(b"other", true),
            flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();

    roots_match(&batch_db, &live_db, grove_version);
}

#[test]
fn batch_identical_edge_reinsert_is_a_no_op() {
    let grove_version = GroveVersion::latest();
    let (batch_db, live_db) = twin_dbs_with_chain(grove_version);
    let root_before = batch_db.root_hash(None, grove_version).unwrap().unwrap();

    batch_db
        .apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"r1".to_vec(),
                sibling_bidi(b"value", true),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();

    assert_eq!(
        batch_db.root_hash(None, grove_version).unwrap().unwrap(),
        root_before,
        "an identical-edge re-insert must not change the root"
    );
    roots_match(&batch_db, &live_db, grove_version);
}

#[test]
fn batch_bidi_delete_matches_live() {
    let grove_version = GroveVersion::latest();
    let (batch_db, live_db) = twin_dbs_with_chain(grove_version);

    // Deleting r1 removes its registration on `value` and cascades r2.
    batch_db
        .apply_batch(
            vec![QualifiedGroveDbOp::delete_op(
                vec![TEST_LEAF.to_vec()],
                b"r1".to_vec(),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    live_db
        .delete(
            &[TEST_LEAF],
            b"r1",
            Some(DeleteOptions {
                propagate_backward_references: true,
                ..Default::default()
            }),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();

    for db in [&batch_db, &live_db] {
        assert!(matches!(
            db.get(&[TEST_LEAF], b"r2", None, grove_version).unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
    }
    roots_match(&batch_db, &live_db, grove_version);
}

#[test]
fn batch_overwrite_bidi_with_plain_item_matches_live() {
    let grove_version = GroveVersion::latest();
    let (batch_db, live_db) = twin_dbs_with_chain(grove_version);

    let plain = Element::new_item(b"plain".to_vec());
    batch_db
        .apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"r1".to_vec(),
                plain.clone(),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    live_db
        .insert(&[TEST_LEAF], b"r1", plain, flag_on(), None, grove_version)
        .unwrap()
        .unwrap();

    roots_match(&batch_db, &live_db, grove_version);
}

#[test]
fn batch_two_refs_to_same_target_matches_live() {
    let grove_version = GroveVersion::latest();
    let build = || {
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
    };
    let (batch_db, live_db) = (build(), build());

    batch_db
        .apply_batch(
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"ra".to_vec(),
                    sibling_bidi(b"value", true),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"rb".to_vec(),
                    sibling_bidi(b"value", true),
                ),
            ],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    for key in [b"ra".as_slice(), b"rb"] {
        live_db
            .insert(
                &[TEST_LEAF],
                key,
                sibling_bidi(b"value", true),
                flag_on(),
                None,
                grove_version,
            )
            .unwrap()
            .unwrap();
    }

    roots_match(&batch_db, &live_db, grove_version);
}

#[test]
fn batch_ref_plus_target_overwrite_in_same_batch_matches_live() {
    let grove_version = GroveVersion::latest();
    let (batch_db, live_db) = twin_dbs_with_chain(grove_version);

    // Overwrite the registered target AND add a new reference to it in the
    // same batch: the registration merges into the overwrite op's element
    // and the existing chain is rewritten with the new end hash.
    let updated = Element::new_item_allowing_bidirectional_references(b"updated".to_vec());
    batch_db
        .apply_batch(
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"value".to_vec(),
                    updated.clone(),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"rnew".to_vec(),
                    sibling_bidi(b"value", true),
                ),
            ],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    live_db
        .insert(
            &[TEST_LEAF],
            b"value",
            updated,
            flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    live_db
        .insert(
            &[TEST_LEAF],
            b"rnew",
            sibling_bidi(b"value", true),
            flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();

    roots_match(&batch_db, &live_db, grove_version);
}

#[test]
fn batch_component_budget_enforced_against_prospective_state() {
    use crate::operations::get::MAX_REFERENCE_HOPS;

    let grove_version = GroveVersion::latest();

    // A whole chain created in one batch stays valid up to the hop budget…
    let db = make_test_grovedb(grove_version);
    let mut ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
        vec![TEST_LEAF.to_vec()],
        b"t0".to_vec(),
        Element::new_item_allowing_bidirectional_references(b"v".to_vec()),
    )];
    for i in 1..MAX_REFERENCE_HOPS {
        ops.push(QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            format!("t{i}").into_bytes(),
            sibling_bidi(format!("t{}", i - 1).as_bytes(), true),
        ));
    }
    db.apply_batch(ops.clone(), batch_flag_on(), None, grove_version)
        .unwrap()
        .expect("a chain at the hop budget is valid");

    // …and the COMPONENT budget rejects splicing a pending chain under an
    // existing referrer: r2 -> r1 -> value exists on disk; the batch
    // creates a fresh chain t9 -> … -> t0 and retargets r1 onto t9.
    // Upstream (r2) plus downstream (the 10 pending hops) exceeds the
    // budget, even though every downstream element exists only
    // prospectively in this same batch.
    let (db, _twin) = twin_dbs_with_chain(grove_version);
    ops.push(QualifiedGroveDbOp::insert_or_replace_op(
        vec![TEST_LEAF.to_vec()],
        b"r1".to_vec(),
        sibling_bidi(format!("t{}", MAX_REFERENCE_HOPS - 1).as_bytes(), true),
    ));
    assert!(matches!(
        db.apply_batch(ops, batch_flag_on(), None, grove_version)
            .unwrap(),
        Err(Error::BidirectionalReferenceRule(_))
    ));
}

// ─── Batching M4: conflict rules ────────────────────────────────────────

#[test]
fn batch_ref_insert_with_target_deleted_in_same_batch_errors() {
    let grove_version = GroveVersion::latest();

    // Both op orders: the rule is order-independent.
    for flip in [false, true] {
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

        let mut ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"ref".to_vec(),
                sibling_bidi(b"value", true),
            ),
            QualifiedGroveDbOp::delete_op(vec![TEST_LEAF.to_vec()], b"value".to_vec()),
        ];
        if flip {
            ops.reverse();
        }
        assert!(
            matches!(
                db.apply_batch(ops, batch_flag_on(), None, grove_version)
                    .unwrap(),
                Err(Error::InvalidBatchOperation(_))
                    | Err(Error::CorruptedReferencePathKeyNotFound(_))
            ),
            "a reference and its target's deletion cannot share a batch"
        );
    }
}

#[test]
fn batch_cascade_hitting_a_user_write_errors() {
    let grove_version = GroveVersion::latest();
    let (db, _twin) = twin_dbs_with_chain(grove_version);

    // Deleting `value` cascades r1 and r2; another op writes r2.
    assert!(matches!(
        db.apply_batch(
            vec![
                QualifiedGroveDbOp::delete_op(vec![TEST_LEAF.to_vec()], b"value".to_vec()),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"r2".to_vec(),
                    Element::new_item(b"squatter".to_vec()),
                ),
            ],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::InvalidBatchOperation(_))
    ));
}

#[test]
fn batch_refresh_reference_on_bidi_errors() {
    let grove_version = GroveVersion::latest();
    let (db, _twin) = twin_dbs_with_chain(grove_version);

    let refresh = QualifiedGroveDbOp {
        path: crate::batch::KeyInfoPath::from_known_owned_path(vec![TEST_LEAF.to_vec()]),
        key: Some(crate::batch::key_info::KeyInfo::KnownKey(b"r1".to_vec())),
        op: crate::batch::GroveOp::RefreshReference {
            reference_path_type: ReferencePathType::SiblingReference(b"value".to_vec()),
            max_reference_hop: None,
            mode: crate::batch::RefreshReferenceMode::PlainReferenceTrusted,
            flags: None,
            non_counted: false,
        },
    };
    assert!(matches!(
        db.apply_batch(vec![refresh], batch_flag_on(), None, grove_version)
            .unwrap(),
        Err(Error::NotSupported(_))
    ));
}

#[test]
fn batch_plain_reference_can_point_at_in_batch_family_target() {
    let grove_version = GroveVersion::latest();
    let batch_db = make_test_grovedb(grove_version);
    let live_db = make_test_grovedb(grove_version);

    // An ordinary (one-way) reference resolving through a family item
    // written in the same batch commits the item's LOGICAL hash.
    batch_db
        .apply_batch(
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"value".to_vec(),
                    Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"plainref".to_vec(),
                    Element::new_reference(ReferencePathType::SiblingReference(b"value".to_vec())),
                ),
            ],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    live_db
        .insert(
            &[TEST_LEAF],
            b"value",
            Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
            flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    live_db
        .insert(
            &[TEST_LEAF],
            b"plainref",
            Element::new_reference(ReferencePathType::SiblingReference(b"value".to_vec())),
            flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();

    roots_match(&batch_db, &live_db, grove_version);
}

#[test]
fn batch_bidi_ops_keep_caller_authority_rules() {
    use grovedb_merk::element::get::ElementFetchFromStorageExtensions;
    use grovedb_path::SubtreePath;

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

    // A caller-supplied referrer list on an inserted reference is not
    // theirs to claim: it must be cleared on a fresh insert.
    let forged = crate::bidirectional_references::BackwardReference {
        inverted_reference: ReferencePathType::SiblingReference(b"victim".to_vec()),
        cascade_on_update: true,
    };
    let mut reference = sibling_bidi(b"value", true);
    if let Element::BidirectionalReference(inner) = &mut reference {
        inner.backward_references.push(forged);
    }
    db.apply_batch(
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"ref".to_vec(),
            reference,
        )],
        batch_flag_on(),
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();

    let tx = db.start_transaction();
    let merk = db
        .open_transactional_merk_at_path(SubtreePath::from(&[TEST_LEAF]), &tx, None, grove_version)
        .unwrap()
        .unwrap();
    assert_eq!(
        Element::get(&merk, b"ref", true, grove_version)
            .unwrap()
            .unwrap()
            .backward_references()
            .unwrap()
            .len(),
        0,
        "forged referrer entries must not persist through batches"
    );
    assert!(db
        .verify_grovedb(None, true, true, grove_version)
        .unwrap()
        .is_empty());
}
