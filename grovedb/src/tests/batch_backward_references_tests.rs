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
    Element::BidirectionalReference(
        BidirectionalReference {
            forward_reference_path: ReferencePathType::SiblingReference(key.to_vec()),
            backward_references: Vec::new(),
            cascade_on_update: cascade,
            max_hop: None,
        },
        None,
    )
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
            Element::ItemWithBackwardsReferences(b"x".to_vec(), vec![forged].into(), None),
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
                    end_hash: None,
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
    if let Element::BidirectionalReference(inner, _) = &mut reference {
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

// ─── Review round: merge restrictions + fresh subtrees ──────────────────

/// An `InsertIfNotExists` over an EXISTING key writes nothing: a
/// registration landing on that position must survive as a derived op
/// instead of being folded into the op that never executes.
#[test]
fn batch_no_op_insert_if_not_exists_does_not_swallow_registration() {
    use grovedb_merk::element::get::ElementFetchFromStorageExtensions;
    use grovedb_path::SubtreePath;

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
                QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
                    vec![TEST_LEAF.to_vec()],
                    b"value".to_vec(),
                    Element::new_item(b"loser".to_vec()),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"ref".to_vec(),
                    sibling_bidi(b"value", true),
                ),
            ],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    // The live twin: the conditional insert is a no-op, then the reference.
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

    // The registration must exist on the target.
    let tx = batch_db.start_transaction();
    let merk = batch_db
        .open_transactional_merk_at_path(SubtreePath::from(&[TEST_LEAF]), &tx, None, grove_version)
        .unwrap()
        .unwrap();
    let target = Element::get(&merk, b"value", true, grove_version)
        .unwrap()
        .unwrap();
    assert_eq!(
        target.backward_references().unwrap().len(),
        1,
        "the registration must not be swallowed by the non-executing insert"
    );
    drop(merk);
    drop(tx);

    roots_match(&batch_db, &live_db, grove_version);
}

/// A propagation rewrite colliding with a LATER plain overwrite of the
/// same position must be superseded by that overwrite — the caller's
/// write wins, with the dereg bookkeeping its displacement requires —
/// in either op order, matching sequential execution.
#[test]
fn batch_later_plain_overwrite_supersedes_propagation() {
    let grove_version = GroveVersion::latest();

    for flip in [false, true] {
        let (batch_db, live_db) = twin_dbs_with_chain(grove_version);

        let updated = Element::new_item_allowing_bidirectional_references(b"updated".to_vec());
        let squatter = Element::new_item(b"squatter".to_vec());
        let mut ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"value".to_vec(),
                updated.clone(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"r2".to_vec(),
                squatter.clone(),
            ),
        ];
        if flip {
            ops.reverse();
        }
        batch_db
            .apply_batch(ops, batch_flag_on(), None, grove_version)
            .unwrap()
            .unwrap();

        // The live twin executes the same canonical order.
        let live_ops: Vec<(&[u8], Element)> = if flip {
            vec![(b"r2", squatter.clone()), (b"value", updated.clone())]
        } else {
            vec![(b"value", updated.clone()), (b"r2", squatter.clone())]
        };
        for (key, element) in live_ops {
            live_db
                .insert(&[TEST_LEAF], key, element, flag_on(), None, grove_version)
                .unwrap()
                .unwrap();
        }

        // The caller's plain overwrite must have landed.
        assert_eq!(
            batch_db
                .get(&[TEST_LEAF], b"r2", None, grove_version)
                .unwrap()
                .unwrap(),
            Element::new_item(b"squatter".to_vec()),
            "the caller's overwrite must not be discarded by the propagation (flip: {flip})"
        );
        roots_match(&batch_db, &live_db, grove_version);
    }
}

/// A flagged batch can create a subtree and populate it — with ordinary
/// items, family items, and references — in the same batch: reads under
/// the staged subtree resolve through the overlay, never committed
/// storage (which does not hold the parent yet).
///
/// Exact root parity with a live sequential twin is NOT asserted here:
/// batch-built and sequentially-built merks legitimately differ in tree
/// shape when constructing a FRESH subtree (a long-standing property of
/// plain unflagged batches too). The master root-parity invariant covers
/// deltas on existing trees; fresh construction asserts semantic
/// equivalence and order-insensitivity of the expansion instead.
#[test]
fn batch_populates_a_subtree_created_in_the_same_batch() {
    use grovedb_merk::element::get::ElementFetchFromStorageExtensions;
    use grovedb_path::SubtreePath;

    let grove_version = GroveVersion::latest();
    let sub_path = vec![TEST_LEAF.to_vec(), b"sub".to_vec()];
    let nested_path = vec![TEST_LEAF.to_vec(), b"sub".to_vec(), b"nested".to_vec()];
    let ops = || {
        vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"sub".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                sub_path.clone(),
                b"plain".to_vec(),
                Element::new_item(b"p".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                sub_path.clone(),
                b"family".to_vec(),
                Element::new_item_allowing_bidirectional_references(b"f".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                sub_path.clone(),
                b"ref".to_vec(),
                sibling_bidi(b"family", true),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                sub_path.clone(),
                b"nested".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                nested_path.clone(),
                b"deep".to_vec(),
                Element::new_item(b"d".to_vec()),
            ),
        ]
    };

    let batch_db = make_test_grovedb(grove_version);
    batch_db
        .apply_batch(ops(), batch_flag_on(), None, grove_version)
        .unwrap()
        .expect("a batch may create and populate a subtree under the flag");

    // The expansion is order-insensitive: the reversed op order (children
    // before their parent trees, reference before its target) produces the
    // byte-identical root.
    let reversed_db = make_test_grovedb(grove_version);
    let mut reversed = ops();
    reversed.reverse();
    reversed_db
        .apply_batch(reversed, batch_flag_on(), None, grove_version)
        .unwrap()
        .unwrap();
    assert_eq!(
        batch_db.root_hash(None, grove_version).unwrap().unwrap(),
        reversed_db.root_hash(None, grove_version).unwrap().unwrap(),
        "op order must not change the outcome"
    );

    // The bookkeeping landed: the in-subtree registration exists and the
    // reference resolves through the fresh subtree.
    let tx = batch_db.start_transaction();
    let merk = batch_db
        .open_transactional_merk_at_path(
            SubtreePath::from(&[TEST_LEAF, b"sub".as_slice()]),
            &tx,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
    let family = Element::get(&merk, b"family", true, grove_version)
        .unwrap()
        .unwrap();
    assert_eq!(family.backward_references().unwrap().len(), 1);
    drop(merk);
    drop(tx);
    assert_eq!(
        batch_db
            .get(&[TEST_LEAF, b"sub".as_slice()], b"ref", None, grove_version)
            .unwrap()
            .unwrap(),
        Element::new_item_allowing_bidirectional_references(b"f".to_vec()),
    );
    assert_eq!(
        batch_db
            .get(
                &[TEST_LEAF, b"sub".as_slice(), b"nested".as_slice()],
                b"deep",
                None,
                grove_version
            )
            .unwrap()
            .unwrap(),
        Element::new_item(b"d".to_vec()),
    );
    assert!(batch_db
        .verify_grovedb(None, true, true, grove_version)
        .unwrap()
        .is_empty());
}

/// A flagged overwrite of a family item carrying a DANGLING registration
/// (its referrer was removed through an unflagged batch) plans a stale-
/// entry cleanup targeting the op's own position: that cleanup must fold
/// into the op itself, not become a second op that fails consistency.
#[test]
fn batch_flagged_overwrite_folds_own_stale_cleanup() {
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
        // Remove the referrer through the supported UNFLAGGED batch path:
        // the registration on `value` is left dangling.
        db.apply_batch(
            vec![QualifiedGroveDbOp::delete_op(
                vec![TEST_LEAF.to_vec()],
                b"ref".to_vec(),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
        db
    };
    let (batch_db, live_db) = (build(), build());

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
        .expect("the stale-entry cleanup must fold into the overwrite itself");
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

/// `Patch` executes as a general element write and can create a tree: the
/// fresh-subtree pre-scan must see it, or a child write under the patched
/// tree reads a parent absent from committed storage.
#[test]
fn batch_patch_created_subtree_is_fresh() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    db.apply_batch(
        vec![
            QualifiedGroveDbOp::patch_op(
                vec![TEST_LEAF.to_vec()],
                b"sub".to_vec(),
                Element::empty_tree(),
                0,
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"sub".to_vec()],
                b"item".to_vec(),
                Element::new_item(b"i".to_vec()),
            ),
        ],
        batch_flag_on(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("a Patch-created subtree is fresh like any other tree write");

    assert_eq!(
        db.get(
            &[TEST_LEAF, b"sub".as_slice()],
            b"item",
            None,
            grove_version
        )
        .unwrap()
        .unwrap(),
        Element::new_item(b"i".to_vec()),
    );
    assert!(db
        .verify_grovedb(None, true, true, grove_version)
        .unwrap()
        .is_empty());
}

/// Bidirectional-edge positions are bounded to
/// `MAX_BACKWARD_REFERENCES_GROVE_DEPTH` subtree levels — the depth the
/// estimation models charge per derived propagation. Deeper referrers are
/// rejected at registration, in the batch and the live flow alike.
#[test]
fn batch_registration_depth_is_bounded() {
    use crate::bidirectional_references::MAX_BACKWARD_REFERENCES_GROVE_DEPTH;

    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);

    // Nested plain trees down to one level beyond the bound (plain trees
    // themselves have no depth rule).
    let mut deep_path: Vec<Vec<u8>> = vec![TEST_LEAF.to_vec()];
    while deep_path.len() < MAX_BACKWARD_REFERENCES_GROVE_DEPTH + 1 {
        let segment = format!("d{}", deep_path.len()).into_bytes();
        let path_refs: Vec<&[u8]> = deep_path.iter().map(|p| p.as_slice()).collect();
        db.insert(
            path_refs.as_slice(),
            &segment,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
        deep_path.push(segment);
    }
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

    let ref_to_value = || {
        Element::BidirectionalReference(
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"value".to_vec(),
                ]),
                backward_references: Vec::new(),
                cascade_on_update: true,
                max_hop: None,
            },
            None,
        )
    };

    // One level beyond the bound: rejected in the batch and live flows.
    let too_deep = deep_path.clone();
    assert!(matches!(
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                too_deep.clone(),
                b"ref".to_vec(),
                ref_to_value(),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::BidirectionalReferenceRule(_))
    ));
    let path_refs: Vec<&[u8]> = too_deep.iter().map(|p| p.as_slice()).collect();
    assert!(matches!(
        db.insert(
            path_refs.as_slice(),
            b"ref",
            ref_to_value(),
            None,
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::BidirectionalReferenceRule(_))
    ));

    // At the bound: accepted.
    let at_bound = &deep_path[..MAX_BACKWARD_REFERENCES_GROVE_DEPTH];
    db.apply_batch(
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            at_bound.to_vec(),
            b"ref".to_vec(),
            ref_to_value(),
        )],
        batch_flag_on(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("a referrer at the depth bound is valid");
    assert!(db
        .verify_grovedb(None, true, true, grove_version)
        .unwrap()
        .is_empty());
}

/// A derived rewrite carries a precomputed node value hash. A flags
/// callback that mutates the element mid-apply (storage flags absorbing the
/// bytes a referrer entry costs, say) changes the stored bytes, so merk
/// recomputes the node value hash from the FINAL bytes with the family's
/// two-layer scheme — the batch succeeds and every node still verifies.
///
/// The mutation lands on the REFERRER (a bidirectional reference, whose
/// recomputed hash must fold its end hash back in) and on an unreferenced
/// item; the referenced target carries no flags, so the callback leaves it
/// alone. A callback mutating a REFERENCED element would leave its
/// referrers committed to the pre-mutation bytes — exactly as plain
/// references behave under a mutating callback (the batch resolves pending
/// targets from the op's bytes, before any just-in-time rewrite) — which
/// `verify_grovedb` reports and the next flagged write of the target heals.
#[test]
fn batch_flags_mutation_on_derived_rewrite_rehashes_final_bytes() {
    let grove_version = GroveVersion::latest();

    let build = || {
        let db = make_test_grovedb(grove_version);
        db.insert(
            &[TEST_LEAF],
            b"value",
            Element::ItemWithBackwardsReferences(b"hello".to_vec(), Default::default(), None),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
        db.insert(
            &[TEST_LEAF],
            b"r1",
            Element::BidirectionalReference(
                BidirectionalReference {
                    forward_reference_path: ReferencePathType::SiblingReference(b"value".to_vec()),
                    backward_references: Vec::new(),
                    cascade_on_update: true,
                    max_hop: None,
                },
                Some(vec![1]),
            ),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
        // An unreferenced flagged item: its rewrite goes through the
        // user-op family arm (item scheme, no end hash).
        db.insert(
            &[TEST_LEAF],
            b"lone",
            Element::ItemWithBackwardsReferences(
                b"one".to_vec(),
                Default::default(),
                Some(vec![2]),
            ),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
        db
    };
    let ops = || {
        vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"value".to_vec(),
                Element::ItemWithBackwardsReferences(b"upd".to_vec(), Default::default(), None),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"lone".to_vec(),
                Element::ItemWithBackwardsReferences(
                    b"two".to_vec(),
                    Default::default(),
                    Some(vec![2]),
                ),
            ),
        ]
    };

    // A callback that leaves flags alone: the derived rewrite of r1 goes
    // through untouched.
    let db = build();
    db.apply_batch_with_element_flags_update(
        ops(),
        batch_flag_on(),
        |_cost, _old_flags, _new_flags| Ok(false),
        |_flags, _removed_key_bytes, _removed_value_bytes| {
            Ok((
                grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval,
                grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval,
            ))
        },
        None,
        grove_version,
    )
    .unwrap()
    .expect("an inert flags callback leaves derived rewrites intact");
    assert!(db
        .verify_grovedb(None, true, true, grove_version)
        .unwrap()
        .is_empty());

    // A callback that MUTATES flags: the mutated bytes are what gets
    // stored, and their node value hash is recomputed to match — for the
    // derived rewrite of the referrer (reference scheme, end hash folded
    // back in) and for the user-written flagged item (item scheme).
    let db = build();
    let mut mutated = 0usize;
    db.apply_batch_with_element_flags_update(
        ops(),
        batch_flag_on(),
        |_cost, _old_flags, new_flags| {
            new_flags.push(7);
            mutated += 1;
            Ok(true)
        },
        |_flags, _removed_key_bytes, _removed_value_bytes| {
            Ok((
                grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval,
                grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval,
            ))
        },
        None,
        grove_version,
    )
    .unwrap()
    .expect("a flags mutation on a provided-hash write is rehashed, not refused");
    // The just-in-time loop may consult the callback more than once per
    // element (it re-runs after a size change on the ORIGINAL bytes), so
    // count elements, not calls: r1 and lone carry flags; value does not.
    assert!(mutated >= 2, "{mutated}");

    let stored_r1 = db
        .get_raw([TEST_LEAF].as_ref().into(), b"r1", None, grove_version)
        .unwrap()
        .expect("r1 stays present");
    assert_eq!(stored_r1.get_flags(), &Some(vec![1, 7]));
    let stored_lone = db
        .get_raw([TEST_LEAF].as_ref().into(), b"lone", None, grove_version)
        .unwrap()
        .expect("lone stays present");
    assert_eq!(stored_lone.get_flags(), &Some(vec![2, 7]));
    assert_eq!(stored_lone.as_item_bytes().expect("item"), b"two");
    let resolved = db
        .get(&[TEST_LEAF], b"r1", None, grove_version)
        .unwrap()
        .expect("r1 resolves");
    assert_eq!(resolved.as_item_bytes().expect("item"), b"upd");
    assert!(
        db.verify_grovedb(None, true, true, grove_version)
            .unwrap()
            .is_empty(),
        "every rewritten node's bytes must agree with its committed hash"
    );
}

/// Hand-built wrappers around backward-references elements (no constructor
/// produces them, deserialization rejects them) are refused at the batch
/// entry gate instead of being stored as bytes no reader accepts.
#[test]
fn batch_refuses_wrapped_backward_references_elements() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    db.insert(
        &[TEST_LEAF],
        b"ct",
        Element::empty_count_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("count tree host");

    let wrapped = Element::NonCounted(Box::new(Element::ItemWithBackwardsReferences(
        b"x".to_vec(),
        Default::default(),
        None,
    )));
    for flag in [batch_flag_on(), None] {
        let err = db
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"ct".to_vec()],
                    b"w".to_vec(),
                    wrapped.clone(),
                )],
                flag,
                None,
                grove_version,
            )
            .unwrap()
            .expect_err("a wrapped backward-references element is refused");
        assert!(
            matches!(err, Error::InvalidBatchOperation(_)),
            "wrapped family element must be an invalid batch operation, got {err:?}"
        );
    }
    assert!(db
        .get_raw(
            [TEST_LEAF, b"ct"].as_ref().into(),
            b"w",
            None,
            grove_version
        )
        .unwrap()
        .is_err());
}

/// Deleting a NON-EMPTY subtree under the flag is refused: its
/// descendants may hold bidirectional-reference participants whose
/// bookkeeping the batch engine's wholesale clearing would skip. Empty
/// subtrees still delete.
#[test]
fn batch_flagged_non_empty_subtree_deletion_is_refused() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    db.insert(
        &[TEST_LEAF],
        b"sub",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF, b"sub"],
        b"member",
        Element::new_item_allowing_bidirectional_references(b"x".to_vec()),
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
                b"sub".to_vec(),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::NotSupported(_))
    ));

    // A subtree created AND populated within the same batch cannot be
    // deleted by it either.
    let db2 = make_test_grovedb(grove_version);
    assert!(matches!(
        db2.apply_batch(
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"sub".to_vec(),
                    Element::empty_tree(),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"sub".to_vec()],
                    b"member".to_vec(),
                    Element::new_item(b"x".to_vec()),
                ),
                QualifiedGroveDbOp::delete_op(vec![TEST_LEAF.to_vec()], b"sub".to_vec()),
            ],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap(),
        Err(_)
    ));

    // An EMPTY subtree still deletes under the flag.
    let db3 = make_test_grovedb(grove_version);
    db3.insert(
        &[TEST_LEAF],
        b"empty_sub",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db3.apply_batch(
        vec![QualifiedGroveDbOp::delete_op(
            vec![TEST_LEAF.to_vec()],
            b"empty_sub".to_vec(),
        )],
        batch_flag_on(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("an empty subtree deletes under the flag");
}

/// The prospective-component budget must validate against PENDING edges
/// in the same unordered batch, not just stored state: a batch that
/// retargets B onto a longer tail AND raises A's `max_hop` has a valid
/// final state and must be accepted in either op order — and a batch
/// that retargets A away from B frees B from A's budget entirely.
#[test]
fn batch_paired_upstream_updates_validate_against_pending_edges() {
    let grove_version = GroveVersion::latest();
    let a_with = |max_hop: u8| {
        Element::BidirectionalReference(
            BidirectionalReference {
                forward_reference_path: ReferencePathType::SiblingReference(b"b".to_vec()),
                backward_references: Vec::new(),
                cascade_on_update: true,
                max_hop: Some(max_hop),
            },
            None,
        )
    };
    let build = || {
        let db = make_test_grovedb(grove_version);
        for (key, element) in [
            (
                b"c".as_slice(),
                Element::new_item_allowing_bidirectional_references(b"c".to_vec()),
            ),
            (
                b"e",
                Element::new_item_allowing_bidirectional_references(b"e".to_vec()),
            ),
            (
                b"f",
                Element::new_item_allowing_bidirectional_references(b"f".to_vec()),
            ),
            (b"b", sibling_bidi(b"c", true)),
            (b"d", sibling_bidi(b"e", true)),
        ] {
            db.insert(&[TEST_LEAF], key, element, None, None, grove_version)
                .unwrap()
                .unwrap();
        }
        db.insert(&[TEST_LEAF], b"a", a_with(2), None, None, grove_version)
            .unwrap()
            .unwrap();
        db
    };

    // Retarget B onto d -> e (A now needs 3 hops) AND raise A to 3, in
    // BOTH op orders: accepted, byte-identical to the live sequential
    // twin (which must raise A first).
    for flip in [false, true] {
        let batch_db = build();
        let live_db = build();
        let mut ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"b".to_vec(),
                sibling_bidi(b"d", true),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"a".to_vec(),
                a_with(3),
            ),
        ];
        if flip {
            ops.reverse();
        }
        batch_db
            .apply_batch(ops, batch_flag_on(), None, grove_version)
            .unwrap()
            .unwrap_or_else(|e| {
                panic!(
                    "the paired update is valid against A's PENDING budget (flip: {flip}): {e:?}"
                )
            });
        live_db
            .insert(&[TEST_LEAF], b"a", a_with(3), None, None, grove_version)
            .unwrap()
            .unwrap();
        live_db
            .insert(
                &[TEST_LEAF],
                b"b",
                sibling_bidi(b"d", true),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .unwrap();
        roots_match(&batch_db, &live_db, grove_version);
    }

    // Retargeting A AWAY from B in the same batch frees B from A's
    // budget: accepted in both op orders too.
    for flip in [false, true] {
        let batch_db = build();
        let a_away = Element::BidirectionalReference(
            BidirectionalReference {
                forward_reference_path: ReferencePathType::SiblingReference(b"f".to_vec()),
                backward_references: Vec::new(),
                cascade_on_update: true,
                max_hop: Some(2),
            },
            None,
        );
        let mut ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"b".to_vec(),
                sibling_bidi(b"d", true),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"a".to_vec(),
                a_away.clone(),
            ),
        ];
        if flip {
            ops.reverse();
        }
        batch_db
            .apply_batch(ops, batch_flag_on(), None, grove_version)
            .unwrap()
            .unwrap_or_else(|e| panic!("A detaches from B in the same batch; B's retarget is free (flip: {flip}): {e:?}"));
        assert!(batch_db
            .verify_grovedb(None, true, true, grove_version)
            .unwrap()
            .is_empty());
    }

    // Control: retargeting B alone (stored A still declares 2) stays
    // rejected.
    let db = build();
    assert!(matches!(
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"b".to_vec(),
                sibling_bidi(b"d", true),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap(),
        Err(Error::BidirectionalReferenceRule(_))
    ));
}

/// A conditional insert whose gate will SKIP it must not advertise a
/// pending edge: retargeting B while an `insert_if_not_exists_or_skip_op`
/// "raises" the budget of the ALREADY-EXISTING upstream A is rejected in
/// both op orders — the conditional writes nothing, so A's stored budget
/// governs the component.
#[test]
fn batch_skipped_conditional_does_not_relax_upstream_budget() {
    let grove_version = GroveVersion::latest();
    let a_with = |max_hop: u8| {
        Element::BidirectionalReference(
            BidirectionalReference {
                forward_reference_path: ReferencePathType::SiblingReference(b"b".to_vec()),
                backward_references: Vec::new(),
                cascade_on_update: true,
                max_hop: Some(max_hop),
            },
            None,
        )
    };
    let build = || {
        let db = make_test_grovedb(grove_version);
        for (key, element) in [
            (
                b"c".as_slice(),
                Element::new_item_allowing_bidirectional_references(b"c".to_vec()),
            ),
            (
                b"e",
                Element::new_item_allowing_bidirectional_references(b"e".to_vec()),
            ),
            (b"b", sibling_bidi(b"c", true)),
            (b"d", sibling_bidi(b"e", true)),
        ] {
            db.insert(&[TEST_LEAF], key, element, None, None, grove_version)
                .unwrap()
                .unwrap();
        }
        db.insert(&[TEST_LEAF], b"a", a_with(2), None, None, grove_version)
            .unwrap()
            .unwrap();
        db
    };

    for flip in [false, true] {
        let db = build();
        let mut ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"b".to_vec(),
                sibling_bidi(b"d", true),
            ),
            QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
                vec![TEST_LEAF.to_vec()],
                b"a".to_vec(),
                a_with(3),
            ),
        ];
        if flip {
            ops.reverse();
        }
        assert!(
            matches!(
                db.apply_batch(ops, batch_flag_on(), None, grove_version)
                    .unwrap(),
                Err(Error::BidirectionalReferenceRule(_))
            ),
            "a skipped conditional must not relax the stored budget (flip: {flip})"
        );
        // The stored graph is untouched and still reads through A.
        assert!(db
            .verify_grovedb(None, true, true, grove_version)
            .unwrap()
            .is_empty());
    }
}

/// A detached ancestor must not consume component budget: with stored
/// `A -> B`, retargeting A away in the same batch lets B take a
/// downstream chain of exactly `MAX_REFERENCE_HOPS` — the boundary case
/// the walk previously rejected by counting the detached A as hop one.
#[test]
fn batch_detached_ancestor_frees_full_downstream_budget() {
    use crate::operations::get::MAX_REFERENCE_HOPS;

    let grove_version = GroveVersion::latest();

    for flip in [false, true] {
        let db = make_test_grovedb(grove_version);
        // The full-budget tail: t0 <- t1 <- … <- t9 (10 hops from B).
        db.insert(
            &[TEST_LEAF],
            b"t0",
            Element::new_item_allowing_bidirectional_references(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .unwrap();
        for i in 1..MAX_REFERENCE_HOPS {
            db.insert(
                &[TEST_LEAF],
                format!("t{i}").as_bytes(),
                sibling_bidi(format!("t{}", i - 1).as_bytes(), true),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .unwrap();
        }
        // Stored A -> B (B currently targets a short chain) and a spare
        // family item for A's retarget.
        for (key, element) in [
            (
                b"x".as_slice(),
                Element::new_item_allowing_bidirectional_references(b"x".to_vec()),
            ),
            (
                b"f",
                Element::new_item_allowing_bidirectional_references(b"f".to_vec()),
            ),
            (b"b", sibling_bidi(b"x", true)),
            (b"a", sibling_bidi(b"b", true)),
        ] {
            db.insert(&[TEST_LEAF], key, element, None, None, grove_version)
                .unwrap()
                .unwrap();
        }

        let mut ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"b".to_vec(),
                sibling_bidi(format!("t{}", MAX_REFERENCE_HOPS - 1).as_bytes(), true),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"a".to_vec(),
                sibling_bidi(b"f", true),
            ),
        ];
        if flip {
            ops.reverse();
        }
        db.apply_batch(ops, batch_flag_on(), None, grove_version)
            .unwrap()
            .unwrap_or_else(|e| {
                panic!("the detached A must not count against B's component (flip: {flip}): {e:?}")
            });
        assert!(db
            .verify_grovedb(None, true, true, grove_version)
            .unwrap()
            .is_empty());
    }
}

/// The declared referrer capacity is enforced by the batch preprocessor
/// exactly as by the live flow: a full target refuses a registration, an
/// update may not lower the capacity below the registered referrers, and a
/// raise admits more — in the same batch as the new referrer.
#[test]
fn batch_enforces_declared_capacity() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    let referrer = || {
        Element::BidirectionalReference(
            BidirectionalReference {
                forward_reference_path: ReferencePathType::SiblingReference(b"target".to_vec()),
                backward_references: Vec::new(),
                cascade_on_update: true,
                max_hop: None,
            },
            None,
        )
    };
    db.insert(
        &[TEST_LEAF],
        b"target",
        Element::new_item_allowing_bidirectional_references_with_capacity(b"v".to_vec(), 1),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .unwrap();
    db.insert(&[TEST_LEAF], b"r1", referrer(), None, None, grove_version)
        .unwrap()
        .unwrap();

    let err = db
        .apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"r2".to_vec(),
                referrer(),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .expect_err("the target is full");
    assert!(
        matches!(err, Error::BidirectionalReferenceRule(_)),
        "{err:?}"
    );

    let err = db
        .apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"target".to_vec(),
                Element::new_item_allowing_bidirectional_references_with_capacity(b"w".to_vec(), 0),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .expect_err("one referrer does not fit a capacity of zero");
    assert!(
        matches!(err, Error::BidirectionalReferenceRule(_)),
        "{err:?}"
    );

    db.apply_batch(
        vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"target".to_vec(),
                Element::new_item_allowing_bidirectional_references_with_capacity(b"w".to_vec(), 2),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"r2".to_vec(),
                referrer(),
            ),
        ],
        batch_flag_on(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("the raise and the second referrer land together");
    for key in [b"r1".as_ref(), b"r2"] {
        assert_eq!(
            db.get(&[TEST_LEAF], key, None, grove_version)
                .unwrap()
                .unwrap()
                .as_item_bytes()
                .unwrap(),
            b"w"
        );
    }
    // Public reads strip the referrer list but keep the declaration; the
    // two registrations show through the capacity now being exhausted.
    let stored = db
        .get_raw([TEST_LEAF].as_ref().into(), b"target", None, grove_version)
        .unwrap()
        .unwrap();
    assert_eq!(stored.max_incoming_references(), Some(2));
    assert!(stored.backward_references().unwrap().is_empty());
    assert!(db
        .apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"r3".to_vec(),
                referrer(),
            )],
            batch_flag_on(),
            None,
            grove_version,
        )
        .unwrap()
        .is_err());
    assert!(db
        .verify_grovedb(None, true, true, grove_version)
        .unwrap()
        .is_empty());
}
