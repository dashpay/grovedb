//! Default maintenance, explicit opt-out, and cached observation regressions.

use grovedb_merk::element::get::ElementFetchFromStorageExtensions;
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{BatchApplyOptions, QualifiedGroveDbOp},
    operations::{delete::DeleteOptions, insert::InsertOptions},
    reference_path::ReferencePathType,
    tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
    BackwardReferencesPolicy, BidirectionalReference, Element, Error,
};

fn reference(target: &[u8], cascade: bool) -> Element {
    Element::BidirectionalReference(
        BidirectionalReference {
            forward_reference_path: ReferencePathType::SiblingReference(target.to_vec()),
            backward_references: Vec::new(),
            cascade_on_update: cascade,
            max_hop: None,
        },
        None,
    )
}

fn chain(cascade: bool) -> TempGroveDb {
    let version = GroveVersion::latest();
    let db = make_test_grovedb(version);
    // A new target and a two-edge chain are valid with no options at all.
    db.apply_batch(
        vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"r2".to_vec(),
                reference(b"r1", cascade),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"value".to_vec(),
                Element::new_item_allowing_bidirectional_references(b"old".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"r1".to_vec(),
                reference(b"value", cascade),
            ),
        ],
        None,
        None,
        version,
    )
    .unwrap()
    .unwrap();
    db
}

#[test]
fn default_live_and_batch_updates_preserve_registrations_and_refresh_hashes() {
    let version = GroveVersion::latest();
    let live = chain(true);
    let batched = chain(true);
    let updated = Element::new_item_allowing_bidirectional_references(b"updated".to_vec());
    live.insert(&[TEST_LEAF], b"value", updated.clone(), None, None, version)
        .unwrap()
        .unwrap();
    batched
        .apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"value".to_vec(),
                updated,
            )],
            None,
            None,
            version,
        )
        .unwrap()
        .unwrap();
    for db in [&live, &batched] {
        let tx = db.start_transaction();
        let merk = db
            .open_transactional_merk_at_path(SubtreePath::from(&[TEST_LEAF]), &tx, None, version)
            .unwrap()
            .unwrap();
        let stored = Element::get(&merk, b"value", true, version)
            .unwrap()
            .unwrap();
        assert_eq!(stored.backward_references().unwrap().len(), 1);
        assert_eq!(
            db.get(&[TEST_LEAF], b"r2", None, version).unwrap().unwrap(),
            Element::new_item_allowing_bidirectional_references(b"updated".to_vec())
        );
        drop(merk);
        drop(tx);
        assert!(db
            .verify_grovedb(None, true, true, version)
            .unwrap()
            .is_empty());
    }
    assert_eq!(
        live.root_hash(None, version).unwrap().unwrap(),
        batched.root_hash(None, version).unwrap().unwrap()
    );
    live.delete(&[TEST_LEAF], b"value", None, None, version)
        .unwrap()
        .unwrap();
    batched
        .apply_batch(
            vec![QualifiedGroveDbOp::delete_op(
                vec![TEST_LEAF.to_vec()],
                b"value".to_vec(),
            )],
            None,
            None,
            version,
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        live.root_hash(None, version).unwrap().unwrap(),
        batched.root_hash(None, version).unwrap().unwrap()
    );
    for db in [&live, &batched] {
        for key in [b"value".as_slice(), b"r1", b"r2"] {
            assert!(db.get(&[TEST_LEAF], key, None, version).unwrap().is_err());
        }
        assert!(db
            .verify_grovedb(None, true, true, version)
            .unwrap()
            .is_empty());
    }
}

#[test]
fn default_plain_overwrite_cascades_and_explicit_skip_keeps_dangling_references() {
    let version = GroveVersion::latest();
    for batch in [false, true] {
        for skip in [false, true] {
            let db = chain(true);
            let policy = if skip {
                BackwardReferencesPolicy::Skip
            } else {
                BackwardReferencesPolicy::Maintain
            };
            let item = Element::new_item(b"plain".to_vec());
            if batch {
                db.apply_batch(
                    vec![QualifiedGroveDbOp::insert_or_replace_op(
                        vec![TEST_LEAF.to_vec()],
                        b"value".to_vec(),
                        item.clone(),
                    )],
                    Some(BatchApplyOptions {
                        backward_references_policy: policy,
                        ..Default::default()
                    }),
                    None,
                    version,
                )
                .unwrap()
                .unwrap();
            } else {
                db.insert(
                    &[TEST_LEAF],
                    b"value",
                    item.clone(),
                    Some(InsertOptions {
                        backward_references_policy: policy,
                        ..Default::default()
                    }),
                    None,
                    version,
                )
                .unwrap()
                .unwrap();
            }
            assert_eq!(
                db.get(&[TEST_LEAF], b"value", None, version)
                    .unwrap()
                    .unwrap(),
                item
            );
            for key in [b"r1".as_slice(), b"r2"] {
                assert_eq!(
                    db.get_raw(SubtreePath::from(&[TEST_LEAF]), key, None, version)
                        .unwrap()
                        .is_ok(),
                    skip
                );
            }
        }
    }
}

#[test]
fn cascade_cleanup_cannot_replace_a_later_user_write() {
    let version = GroveVersion::latest();
    for disable_operation_consistency_check in [false, true] {
        let db = chain(true);
        let item = Element::new_item(b"user payload".to_vec());
        db.apply_batch(
            vec![
                QualifiedGroveDbOp::delete_op(vec![TEST_LEAF.to_vec()], b"r1".to_vec()),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"value".to_vec(),
                    item.clone(),
                ),
            ],
            Some(BatchApplyOptions {
                disable_operation_consistency_check,
                ..Default::default()
            }),
            None,
            version,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            db.get(&[TEST_LEAF], b"value", None, version)
                .unwrap()
                .unwrap(),
            item
        );
        assert!(db
            .get_raw(SubtreePath::from(&[TEST_LEAF]), b"r2", None, version)
            .unwrap()
            .is_err());
        assert!(db
            .verify_grovedb(None, true, true, version)
            .unwrap()
            .is_empty());
    }
}

#[test]
fn default_cascade_refusal_is_atomic_inside_a_callers_transaction() {
    let version = GroveVersion::latest();
    for batch in [false, true] {
        let db = chain(false);
        let tx = db.start_transaction();
        let before = db.root_hash(Some(&tx), version).unwrap().unwrap();
        let result = if batch {
            db.apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"value".to_vec(),
                    Element::new_item(vec![9]),
                )],
                None,
                Some(&tx),
                version,
            )
        } else {
            db.insert(
                &[TEST_LEAF],
                b"value",
                Element::new_item(vec![9]),
                None,
                Some(&tx),
                version,
            )
        };
        assert!(matches!(
            result.unwrap(),
            Err(Error::BidirectionalReferenceRule(_))
        ));
        assert_eq!(db.root_hash(Some(&tx), version).unwrap().unwrap(), before);
        assert!(db
            .verify_grovedb(Some(&tx), true, true, version)
            .unwrap()
            .is_empty());
    }
}

#[test]
fn partial_batch_old_value_observer_rejects_participants_in_both_segments() {
    let version = GroveVersion::latest();
    for continuation in [false, true] {
        let db = chain(true);
        let before = db.root_hash(None, version).unwrap().unwrap();
        let mutation = QualifiedGroveDbOp::delete_op(vec![TEST_LEAF.to_vec()], b"value".to_vec());
        let initial = if continuation {
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"unrelated".to_vec(),
                Element::new_item(vec![1]),
            )]
        } else {
            vec![mutation.clone()]
        };
        let result = db.apply_partial_batch(
            initial,
            None,
            |_, _| {
                if continuation {
                    Ok(vec![mutation.clone()])
                } else {
                    panic!("refused initial segment must not call continuation")
                }
            },
            None,
            version,
        );
        assert!(matches!(result.unwrap(), Err(Error::NotSupported(_))));
        assert_eq!(db.root_hash(None, version).unwrap().unwrap(), before);
    }
}

#[test]
fn ordinary_mutations_reuse_preparation_reads() {
    let version = GroveVersion::latest();
    for deleting in [false, true] {
        let automatic = make_test_grovedb(version);
        let skipped = make_test_grovedb(version);
        let initial: Vec<_> = (0u8..63)
            .map(|k| {
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    vec![k],
                    Element::new_item(vec![k; 32]),
                )
            })
            .collect();
        for db in [&automatic, &skipped] {
            db.apply_batch(initial.clone(), None, None, version)
                .unwrap()
                .unwrap();
        }
        let ops = if deleting {
            vec![QualifiedGroveDbOp::delete_op(
                vec![TEST_LEAF.to_vec()],
                vec![1],
            )]
        } else {
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                vec![1],
                Element::new_item(vec![99; 32]),
            )]
        };
        let observed_cost = automatic
            .apply_batch(ops.clone(), None, None, version)
            .cost_as_result()
            .unwrap();
        let skipped_cost = skipped
            .apply_batch(
                ops,
                Some(BatchApplyOptions {
                    backward_references_policy: BackwardReferencesPolicy::Skip,
                    ..Default::default()
                }),
                None,
                version,
            )
            .cost_as_result()
            .unwrap();
        assert_eq!(observed_cost, skipped_cost);
        assert_eq!(
            automatic.root_hash(None, version).unwrap().unwrap(),
            skipped.root_hash(None, version).unwrap().unwrap()
        );
    }
}

#[test]
fn skip_delete_is_an_explicit_opt_out() {
    let version = GroveVersion::latest();
    let db = chain(true);
    db.delete(
        &[TEST_LEAF],
        b"value",
        Some(DeleteOptions {
            backward_references_policy: BackwardReferencesPolicy::Skip,
            ..Default::default()
        }),
        None,
        version,
    )
    .unwrap()
    .unwrap();
    assert!(db
        .get_raw(SubtreePath::from(&[TEST_LEAF]), b"r1", None, version)
        .unwrap()
        .is_ok());
}

#[test]
fn recursive_removal_cannot_bypass_descendant_maintenance() {
    use crate::batch::SubelementsDeletionBehavior;
    use grovedb_merk::TreeType;
    let version = GroveVersion::latest();
    let db = make_test_grovedb(version);
    db.insert(
        &[TEST_LEAF],
        b"tree",
        Element::empty_tree(),
        None,
        None,
        version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF, b"tree"],
        b"value",
        Element::new_item_allowing_bidirectional_references(vec![1]),
        None,
        None,
        version,
    )
    .unwrap()
    .unwrap();
    let outside = Element::BidirectionalReference(
        BidirectionalReference {
            forward_reference_path: ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"tree".to_vec(),
                b"value".to_vec(),
            ]),
            backward_references: Vec::new(),
            cascade_on_update: true,
            max_hop: None,
        },
        None,
    );
    db.insert(&[TEST_LEAF], b"outside", outside, None, None, version)
        .unwrap()
        .unwrap();
    let before = db.root_hash(None, version).unwrap().unwrap();
    for replace in [false, true] {
        let op = if replace {
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"tree".to_vec(),
                Element::new_item(vec![2]),
            )
        } else {
            QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"tree".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::DeleteChildren,
            )
        };
        for partial in [false, true] {
            let result = if partial {
                db.apply_partial_batch(vec![op.clone()], None, |_, _| Ok(vec![]), None, version)
            } else {
                db.apply_batch(vec![op.clone()], None, None, version)
            };
            assert!(matches!(result.unwrap(), Err(Error::NotSupported(_))));
            assert_eq!(db.root_hash(None, version).unwrap().unwrap(), before);
        }
    }
    db.delete(
        &[TEST_LEAF],
        b"tree",
        Some(DeleteOptions {
            allow_deleting_non_empty_trees: true,
            ..Default::default()
        }),
        None,
        version,
    )
    .unwrap()
    .unwrap();
    assert!(db
        .get_raw(SubtreePath::from(&[TEST_LEAF]), b"outside", None, version)
        .unwrap()
        .is_err());
    assert!(db
        .verify_grovedb(None, true, true, version)
        .unwrap()
        .is_empty());
}

#[test]
fn known_new_assertion_cannot_hide_a_displaced_participant() {
    let version = GroveVersion::latest();
    let db = chain(true);
    let before = db.root_hash(None, version).unwrap().unwrap();
    let result = db.apply_batch(
        vec![
            QualifiedGroveDbOp::insert_only_known_to_not_already_exist_op(
                vec![TEST_LEAF.to_vec()],
                b"value".to_vec(),
                Element::new_item(vec![1]),
            ),
        ],
        None,
        None,
        version,
    );
    assert!(matches!(
        result.unwrap(),
        Err(Error::InvalidBatchOperation(_))
    ));
    assert_eq!(db.root_hash(None, version).unwrap().unwrap(), before);
}

#[test]
fn live_participant_below_indexed_primary_refuses_before_commit_and_batch_works() {
    let version = GroveVersion::latest();
    let db = make_test_grovedb(version);
    db.apply_batch(
        vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"idx".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"idx".to_vec()],
                b"group".to_vec(),
                Element::empty_count_tree(),
            ),
        ],
        None,
        None,
        version,
    )
    .unwrap()
    .unwrap();
    let before = db.root_hash(None, version).unwrap().unwrap();
    let family = Element::new_item_allowing_bidirectional_references(vec![1]);
    let result = db.insert(
        &[TEST_LEAF, b"idx", b"group"],
        b"value",
        family.clone(),
        None,
        None,
        version,
    );
    assert!(matches!(result.unwrap(), Err(Error::NotSupported(_))));
    assert_eq!(db.root_hash(None, version).unwrap().unwrap(), before);
    db.apply_batch(
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"idx".to_vec(), b"group".to_vec()],
            b"value".to_vec(),
            family,
        )],
        None,
        None,
        version,
    )
    .unwrap()
    .unwrap();
    assert!(db
        .verify_grovedb(None, true, true, version)
        .unwrap()
        .is_empty());
}
