//! Default maintenance, explicit opt-out, and cached observation regressions.

use grovedb_merk::element::get::ElementFetchFromStorageExtensions;
use grovedb_path::SubtreePath;
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{BatchApplyOptions, QualifiedGroveDbOp},
    operations::{delete::DeleteOptions, insert::InsertOptions},
    reference_path::ReferencePathType,
    tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
    BidirectionalReference, DisplacedValue, Element, Error,
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

fn not_participant(ops: Vec<QualifiedGroveDbOp>) -> Vec<QualifiedGroveDbOp> {
    ops.into_iter()
        .map(|op| op.with_displaced_value(DisplacedValue::NotParticipant))
        .collect()
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
fn default_plain_overwrite_cascades_and_a_false_not_participant_claim_is_refused() {
    let version = GroveVersion::latest();
    for batch in [false, true] {
        for declared in [
            DisplacedValue::MayBeParticipant,
            DisplacedValue::NotParticipant,
        ] {
            let db = chain(true);
            let before = db.root_hash(None, version).unwrap().unwrap();
            let item = Element::new_item(b"plain".to_vec());
            let result = if batch {
                db.apply_batch(
                    vec![QualifiedGroveDbOp::insert_or_replace_op(
                        vec![TEST_LEAF.to_vec()],
                        b"value".to_vec(),
                        item.clone(),
                    )
                    .with_displaced_value(declared)],
                    None,
                    None,
                    version,
                )
                .unwrap()
            } else {
                db.insert(
                    &[TEST_LEAF],
                    b"value",
                    item.clone(),
                    Some(InsertOptions {
                        displaced_value: declared,
                        ..Default::default()
                    }),
                    None,
                    version,
                )
                .unwrap()
            };
            let refused = !declared.may_be_participant();
            if refused {
                // The stored value is read for the write anyway, so the
                // false claim is caught for free and nothing changes.
                assert!(matches!(result, Err(Error::NotSupported(_))));
                assert_eq!(db.root_hash(None, version).unwrap().unwrap(), before);
            } else {
                result.unwrap();
                assert_eq!(
                    db.get(&[TEST_LEAF], b"value", None, version)
                        .unwrap()
                        .unwrap(),
                    item
                );
            }
            for key in [b"r1".as_slice(), b"r2"] {
                assert_eq!(
                    db.get_raw(SubtreePath::from(&[TEST_LEAF]), key, None, version)
                        .unwrap()
                        .is_ok(),
                    refused
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
            .apply_batch(not_participant(ops), None, None, version)
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
fn not_participant_delete_of_a_participant_is_refused() {
    let version = GroveVersion::latest();
    let db = chain(true);
    let before = db.root_hash(None, version).unwrap().unwrap();
    let result = db.delete(
        &[TEST_LEAF],
        b"value",
        Some(DeleteOptions {
            displaced_value: DisplacedValue::NotParticipant,
            ..Default::default()
        }),
        None,
        version,
    );
    assert!(matches!(result.unwrap(), Err(Error::NotSupported(_))));
    assert_eq!(db.root_hash(None, version).unwrap().unwrap(), before);
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

#[test]
fn clear_subtree_refuses_incoming_and_outgoing_edges_before_mutating_the_transaction() {
    use crate::operations::delete::ClearOptions;
    let version = GroveVersion::latest();
    for incoming in [false, true] {
        for nested in [false, true] {
            let db = make_test_grovedb(version);
            let mut inside = vec![TEST_LEAF];
            if nested {
                db.insert(
                    inside.as_slice(),
                    b"inner",
                    Element::empty_tree(),
                    None,
                    None,
                    version,
                )
                .unwrap()
                .unwrap();
                inside.push(b"inner");
            }
            let root: Vec<&[u8]> = vec![];
            let (target_path, edge_path) = if incoming {
                (&inside, &root)
            } else {
                (&root, &inside)
            };
            db.insert(
                target_path.as_slice(),
                b"target",
                Element::new_item_allowing_bidirectional_references(vec![1]),
                None,
                None,
                version,
            )
            .unwrap()
            .unwrap();
            let mut target = target_path
                .iter()
                .map(|part| part.to_vec())
                .collect::<Vec<_>>();
            target.push(b"target".to_vec());
            db.insert(
                edge_path.as_slice(),
                b"edge",
                Element::BidirectionalReference(
                    BidirectionalReference {
                        forward_reference_path: ReferencePathType::AbsolutePathReference(target),
                        backward_references: Vec::new(),
                        cascade_on_update: true,
                        max_hop: None,
                    },
                    None,
                ),
                None,
                None,
                version,
            )
            .unwrap()
            .unwrap();
            let tx = db.start_transaction();
            let before = db.root_hash(Some(&tx), version).unwrap().unwrap();
            let result = db.clear_subtree(
                &[TEST_LEAF],
                Some(ClearOptions {
                    // Even a false no-subtrees assertion cannot bypass maintenance.
                    check_for_subtrees: !nested,
                    allow_deleting_subtrees: true,
                    ..Default::default()
                }),
                Some(&tx),
                version,
            );
            assert!(matches!(result, Err(Error::NotSupported(_))));
            assert_eq!(db.root_hash(Some(&tx), version).unwrap().unwrap(), before);
            assert!(db
                .verify_grovedb(Some(&tx), true, true, version)
                .unwrap()
                .is_empty());
            db.clear_subtree(
                &[TEST_LEAF],
                Some(ClearOptions {
                    allow_deleting_subtrees: true,
                    displaced_value: DisplacedValue::NotParticipant,
                    ..Default::default()
                }),
                Some(&tx),
                version,
            )
            .unwrap();
            if incoming {
                assert!(db
                    .get(root.as_slice(), b"edge", Some(&tx), version)
                    .unwrap()
                    .is_err());
            } else {
                // Public get_raw strips the registered referrers; inspect the
                // stored element to verify the trusted NotParticipant contract.
                let merk = db
                    .open_transactional_merk_at_path(
                        SubtreePath::from(root.as_slice()),
                        &tx,
                        None,
                        version,
                    )
                    .unwrap()
                    .unwrap();
                let target = Element::get(&merk, b"target", true, version)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    target.backward_references().unwrap().len(),
                    1,
                    "a raw clear declared NotParticipant leaves the registration stale"
                );
            }
        }
    }
}

#[test]
fn flat_drop_requires_not_participant_in_live_full_and_both_partial_segments() {
    use crate::batch::SubelementsDeletionBehavior;
    use grovedb_merk::TreeType;
    let version = GroveVersion::latest();
    for route in 0..4 {
        let db = chain(true);
        let tx = db.start_transaction();
        let before = db.root_hash(Some(&tx), version).unwrap().unwrap();
        let drop_op = QualifiedGroveDbOp::delete_tree_op(
            vec![],
            TEST_LEAF.to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DropFlat,
        );
        let result = match route {
            0 => db.drop_flat_subtree(
                &[] as &[&[u8]],
                TEST_LEAF,
                DisplacedValue::MayBeParticipant,
                Some(&tx),
                version,
            ),
            1 => db.apply_batch(vec![drop_op], None, Some(&tx), version),
            2 => db.apply_partial_batch(
                vec![drop_op],
                None,
                |_, _| panic!("initial drop must be refused before continuation"),
                Some(&tx),
                version,
            ),
            _ => db.apply_partial_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![],
                    b"unrelated".to_vec(),
                    Element::new_item(vec![9]),
                )],
                None,
                |_, _| Ok(vec![drop_op.clone()]),
                Some(&tx),
                version,
            ),
        };
        assert!(matches!(result.value, Err(Error::NotSupported(_))));
        if route < 3 {
            assert_eq!(
                result.cost,
                Default::default(),
                "reject before reading any subtree"
            );
        }
        assert_eq!(db.root_hash(Some(&tx), version).unwrap().unwrap(), before);
        assert!(db
            .verify_grovedb(Some(&tx), true, true, version)
            .unwrap()
            .is_empty());
    }
}

#[test]
fn ordinary_batch_conditionals_and_duplicate_positions_keep_executor_semantics() {
    let version = GroveVersion::latest();
    for scenario in 0..4 {
        let mut roots = Vec::new();
        for declared in [
            DisplacedValue::MayBeParticipant,
            DisplacedValue::NotParticipant,
        ] {
            let db = make_test_grovedb(version);
            db.insert(
                &[TEST_LEAF],
                b"value",
                Element::new_item(vec![1]),
                None,
                None,
                version,
            )
            .unwrap()
            .unwrap();
            let path = vec![TEST_LEAF.to_vec()];
            let replacement = Element::new_item(vec![2]);
            let ops = match scenario {
                0 => vec![QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
                    path,
                    b"value".to_vec(),
                    replacement,
                )],
                1 => vec![
                    QualifiedGroveDbOp::insert_only_known_to_not_already_exist_op(
                        path,
                        b"value".to_vec(),
                        replacement,
                    ),
                ],
                2 => vec![
                    QualifiedGroveDbOp::insert_or_replace_op(
                        path.clone(),
                        b"value".to_vec(),
                        replacement,
                    ),
                    QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
                        path,
                        b"value".to_vec(),
                        Element::new_item(vec![3]),
                    ),
                ],
                _ => vec![
                    QualifiedGroveDbOp::insert_or_replace_op(
                        path.clone(),
                        b"value".to_vec(),
                        replacement,
                    ),
                    QualifiedGroveDbOp::insert_or_replace_op(
                        path,
                        b"value".to_vec(),
                        Element::new_item(vec![3]),
                    ),
                ],
            };
            db.apply_batch(
                ops.into_iter()
                    .map(|op| op.with_displaced_value(declared))
                    .collect(),
                Some(BatchApplyOptions {
                    disable_operation_consistency_check: true,
                    ..Default::default()
                }),
                None,
                version,
            )
            .unwrap()
            .unwrap();
            roots.push(db.root_hash(None, version).unwrap().unwrap());
        }
        assert_eq!(
            roots[0], roots[1],
            "ordinary scenario {scenario} must not acquire reference planner semantics"
        );
    }
}

#[test]
fn partial_subtree_refusal_is_atomic_in_the_continuation_and_caller_transaction() {
    use crate::batch::SubelementsDeletionBehavior;
    use grovedb_merk::TreeType;
    let version = GroveVersion::latest();
    for continuation in [false, true] {
        let db = chain(true);
        let tx = db.start_transaction();
        let before = db.root_hash(Some(&tx), version).unwrap().unwrap();
        let mutation = QualifiedGroveDbOp::delete_tree_op(
            vec![],
            TEST_LEAF.to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DeleteChildren,
        );
        let initial = if continuation {
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![],
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
                Ok(if continuation {
                    vec![mutation.clone()]
                } else {
                    vec![]
                })
            },
            Some(&tx),
            version,
        );
        assert!(matches!(result.unwrap(), Err(Error::NotSupported(_))));
        assert_eq!(db.root_hash(Some(&tx), version).unwrap().unwrap(), before);
        assert!(db
            .verify_grovedb(Some(&tx), true, true, version)
            .unwrap()
            .is_empty());
    }
}

/// Delete-up-tree shape: the batch deletes a leaf, then removes the emptied
/// trees above it with `DontCheckWithNoCleanup`. Every element the batch
/// removes is read as an old value on the way, so `Maintain` has nothing
/// left to scan and adds no reads over `Skip`, in full and partial batches
/// alike. `DeleteChildren` keeps its scan: GroveDB cannot see what that
/// removal takes with it.
#[test]
fn declared_empty_subtree_removals_skip_the_participant_scan() {
    use crate::batch::SubelementsDeletionBehavior;
    use grovedb_merk::TreeType;
    let version = GroveVersion::latest();
    let seed = |db: &TempGroveDb| {
        db.insert(
            &[TEST_LEAF],
            b"a",
            Element::empty_tree(),
            None,
            None,
            version,
        )
        .unwrap()
        .unwrap();
        db.insert(
            &[TEST_LEAF, b"a"],
            b"b",
            Element::empty_tree(),
            None,
            None,
            version,
        )
        .unwrap()
        .unwrap();
        db.insert(
            &[TEST_LEAF, b"a", b"b"],
            b"c",
            Element::new_item(vec![1]),
            None,
            None,
            version,
        )
        .unwrap()
        .unwrap();
    };
    // Every declared-empty or apply-time-checked removal behaves the same:
    // `DontCheckWithNoCleanup` trusts the batch's own deletes, `Error` and
    // `Skip` verify emptiness against them at apply time.
    for behavior in [
        SubelementsDeletionBehavior::DontCheckWithNoCleanup,
        SubelementsDeletionBehavior::Error,
        SubelementsDeletionBehavior::Skip,
    ] {
        let delete_up_tree = [
            QualifiedGroveDbOp::delete_op(
                vec![TEST_LEAF.to_vec(), b"a".to_vec(), b"b".to_vec()],
                b"c".to_vec(),
            ),
            QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec(), b"a".to_vec()],
                b"b".to_vec(),
                TreeType::NormalTree,
                behavior,
            ),
            QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"a".to_vec(),
                TreeType::NormalTree,
                behavior,
            ),
        ];
        for partial in [false, true] {
            let mut runs = Vec::new();
            for declared in [
                DisplacedValue::MayBeParticipant,
                DisplacedValue::NotParticipant,
            ] {
                let db = make_test_grovedb(version);
                seed(&db);
                let ops: Vec<QualifiedGroveDbOp> = delete_up_tree
                    .iter()
                    .cloned()
                    .map(|op| op.with_displaced_value(declared))
                    .collect();
                let result = if partial {
                    db.apply_partial_batch(ops, None, |_, _| Ok(vec![]), None, version)
                } else {
                    db.apply_batch(ops, None, None, version)
                };
                result.value.expect("delete up the tree");
                // `Skip` checks emptiness before the same-batch deletes land
                // and silently keeps the populated chain; the other two
                // remove it.
                assert_eq!(
                    db.get_raw(SubtreePath::from(&[TEST_LEAF]), b"a", None, version)
                        .unwrap()
                        .is_err(),
                    !matches!(behavior, SubelementsDeletionBehavior::Skip),
                    "{behavior:?} partial={partial}"
                );
                runs.push((result.cost, db.root_hash(None, version).unwrap().unwrap()));
            }
            let (may_be, not) = (&runs[0], &runs[1]);
            assert_eq!(may_be.1, not.1, "{behavior:?} partial={partial}: root hash");
            assert_eq!(
                may_be.0, not.0,
                "{behavior:?} partial={partial}: MayBeParticipant must cost exactly what \
                 NotParticipant costs"
            );
        }
    }

    let recursive = [QualifiedGroveDbOp::delete_tree_op(
        vec![TEST_LEAF.to_vec()],
        b"a".to_vec(),
        TreeType::NormalTree,
        SubelementsDeletionBehavior::DeleteChildren,
    )];
    let mut costs = Vec::new();
    for declared in [
        DisplacedValue::MayBeParticipant,
        DisplacedValue::NotParticipant,
    ] {
        let db = make_test_grovedb(version);
        seed(&db);
        let ops: Vec<QualifiedGroveDbOp> = recursive
            .iter()
            .cloned()
            .map(|op| op.with_displaced_value(declared))
            .collect();
        let result = db.apply_batch(ops, None, None, version);
        result.value.expect("recursive delete");
        costs.push(result.cost);
    }
    assert_eq!(
        costs[0], costs[1],
        "DeleteChildren checks its contents on the cleanup walk it makes anyway"
    );
}

/// A declared-empty removal relies on the batch's own deletes for
/// maintenance: the explicitly deleted participant cascades its referrer in
/// a full batch, and a partial batch still refuses the participant mutation
/// before anything is committed.
#[test]
fn declared_empty_subtree_removal_still_maintains_the_deleted_participant() {
    use crate::batch::SubelementsDeletionBehavior;
    use grovedb_merk::TreeType;
    let version = GroveVersion::latest();
    let seed = |db: &TempGroveDb| {
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
    };
    let ops = vec![
        QualifiedGroveDbOp::delete_op(
            vec![TEST_LEAF.to_vec(), b"tree".to_vec()],
            b"value".to_vec(),
        ),
        QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"tree".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DontCheckWithNoCleanup,
        ),
    ];

    let db = make_test_grovedb(version);
    seed(&db);
    db.apply_batch(ops.clone(), None, None, version)
        .unwrap()
        .expect("the explicit participant delete cascades its referrer");
    assert!(db
        .get_raw(SubtreePath::from(&[TEST_LEAF]), b"outside", None, version)
        .unwrap()
        .is_err());
    assert!(db
        .get_raw(SubtreePath::from(&[TEST_LEAF]), b"tree", None, version)
        .unwrap()
        .is_err());
    assert!(db
        .verify_grovedb(None, true, true, version)
        .unwrap()
        .is_empty());

    let db = make_test_grovedb(version);
    seed(&db);
    let before = db.root_hash(None, version).unwrap().unwrap();
    let result = db.apply_partial_batch(ops, None, |_, _| Ok(vec![]), None, version);
    assert!(matches!(result.unwrap(), Err(Error::NotSupported(_))));
    assert_eq!(db.root_hash(None, version).unwrap().unwrap(), before);
}

/// A `DeleteTree(Skip)` over a populated tree executes nothing, so its
/// behavior must not exempt a callback replacement of that same tree from
/// the participant scan: the replacement is refused before commit and the
/// outside reference keeps resolving.
#[test]
fn skipped_delete_tree_does_not_exempt_a_callback_replacement() {
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
    db.insert(
        &[TEST_LEAF],
        b"outside",
        Element::BidirectionalReference(
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
        ),
        None,
        None,
        version,
    )
    .unwrap()
    .unwrap();
    let before = db.root_hash(None, version).unwrap().unwrap();
    let result = db.apply_partial_batch(
        vec![QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"tree".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::Skip,
        )],
        None,
        |_, _| {
            Ok(vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"tree".to_vec(),
                Element::empty_tree(),
            )])
        },
        None,
        version,
    );
    assert!(matches!(result.unwrap(), Err(Error::NotSupported(_))));
    assert_eq!(db.root_hash(None, version).unwrap().unwrap(), before);
    db.get(&[TEST_LEAF], b"outside", None, version)
        .unwrap()
        .expect("the outside reference still resolves");
    assert!(db
        .verify_grovedb(None, true, true, version)
        .unwrap()
        .is_empty());
}

/// The non-batched adapter must carry each op's declaration whether or not
/// batch options are supplied: a false `NotParticipant` claim is refused
/// with `None` exactly as with `Some(BatchApplyOptions::default())`.
#[test]
fn non_batched_apply_keeps_the_op_declaration_without_options() {
    let version = GroveVersion::latest();
    let overwrite = QualifiedGroveDbOp::insert_or_replace_op(
        vec![TEST_LEAF.to_vec()],
        b"value".to_vec(),
        Element::new_item(vec![2]),
    )
    .with_displaced_value(DisplacedValue::NotParticipant);
    let delete = QualifiedGroveDbOp::delete_op(vec![TEST_LEAF.to_vec()], b"value".to_vec())
        .with_displaced_value(DisplacedValue::NotParticipant);
    for op in [overwrite, delete] {
        for options in [None, Some(BatchApplyOptions::default())] {
            let db = chain(true);
            let before = db.root_hash(None, version).unwrap().unwrap();
            let result =
                db.apply_operations_without_batching(vec![op.clone()], options, None, version);
            assert!(
                matches!(result.unwrap(), Err(Error::NotSupported(_))),
                "{op:?} must be refused"
            );
            assert_eq!(db.root_hash(None, version).unwrap().unwrap(), before);
        }
    }
}

/// A conditional insert over an existing key writes nothing, so it has no
/// displaced value to check: declaring `NotParticipant` on it is not a false
/// claim even when the existing value participates.
#[test]
fn skipped_conditional_insert_checks_no_displaced_value() {
    let version = GroveVersion::latest();
    let db = chain(true);
    let before = db.root_hash(None, version).unwrap().unwrap();
    db.apply_batch(
        vec![QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
            vec![TEST_LEAF.to_vec()],
            b"value".to_vec(),
            Element::new_item(vec![2]),
        )
        .with_displaced_value(DisplacedValue::NotParticipant)],
        None,
        None,
        version,
    )
    .unwrap()
    .expect("a skipped conditional insert displaces nothing");
    assert_eq!(db.root_hash(None, version).unwrap().unwrap(), before);
}

/// A removal observed in the initial segment keeps the declaration of the
/// op that displaced it: a callback op at the same path declared
/// `NotParticipant` (a plain overwrite, or a `DeleteTree(Skip)` that ends up
/// skipped) cannot speak for the earlier tree replacement.
#[test]
fn initial_segment_removal_keeps_its_own_declaration() {
    use crate::batch::SubelementsDeletionBehavior;
    use grovedb_merk::TreeType;
    let version = GroveVersion::latest();
    for callback_deletes in [false, true] {
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
        db.insert(
            &[TEST_LEAF],
            b"outside",
            Element::BidirectionalReference(
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
            ),
            None,
            None,
            version,
        )
        .unwrap()
        .unwrap();
        let before = db.root_hash(None, version).unwrap().unwrap();
        let result = db.apply_partial_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"tree".to_vec(),
                Element::new_item(vec![1]),
            )],
            None,
            |_, _| {
                Ok(vec![if callback_deletes {
                    QualifiedGroveDbOp::delete_tree_op(
                        vec![TEST_LEAF.to_vec()],
                        b"tree".to_vec(),
                        TreeType::NormalTree,
                        SubelementsDeletionBehavior::Skip,
                    )
                } else {
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![TEST_LEAF.to_vec()],
                        b"tree".to_vec(),
                        Element::new_item(vec![2]),
                    )
                }
                .with_displaced_value(DisplacedValue::NotParticipant)])
            },
            None,
            version,
        );
        assert!(
            matches!(result.unwrap(), Err(Error::NotSupported(_))),
            "callback_deletes={callback_deletes}"
        );
        assert_eq!(db.root_hash(None, version).unwrap().unwrap(), before);
        db.get(&[TEST_LEAF], b"outside", None, version)
            .unwrap()
            .expect("the outside reference still resolves");
        assert!(db
            .verify_grovedb(None, true, true, version)
            .unwrap()
            .is_empty());
    }
}

/// A skipped callback `DeleteTree(Skip)` must not erase the cleanup behavior
/// of the `DeleteTree(DeleteChildren)` the initial segment executed at the
/// same path: the removed subtree's storage is still cleaned.
#[test]
fn skipped_callback_delete_keeps_the_executed_deletions_cleanup() {
    use crate::batch::SubelementsDeletionBehavior;
    use grovedb_merk::TreeType;
    use grovedb_storage::{Storage, StorageContext};
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
        b"child",
        Element::new_item(vec![1]),
        None,
        None,
        version,
    )
    .unwrap()
    .unwrap();
    db.apply_partial_batch(
        vec![QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"tree".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DeleteChildren,
        )],
        None,
        |_, _| {
            Ok(vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"tree".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::Skip,
            )])
        },
        None,
        version,
    )
    .unwrap()
    .expect("the executed deletion applies; the skipped one is dropped");
    assert!(db
        .get_raw(SubtreePath::from(&[TEST_LEAF]), b"tree", None, version)
        .unwrap()
        .is_err());
    let tx = db.start_transaction();
    let storage = db
        .db
        .get_transactional_storage_context(SubtreePath::from(&[TEST_LEAF, b"tree"]), None, &tx)
        .unwrap();
    assert!(
        storage.get(b"child").unwrap().unwrap().is_none(),
        "the executed DeleteChildren must still clean the subtree's storage"
    );
    assert!(db
        .verify_grovedb(None, true, true, version)
        .unwrap()
        .is_empty());
}
