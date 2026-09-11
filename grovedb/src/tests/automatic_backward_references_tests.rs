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
                    backward_references_policy: BackwardReferencesPolicy::Skip,
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
                // stored element to verify the explicit Skip contract.
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
                    "Skip leaves the registration deliberately stale"
                );
            }
        }
    }
}

#[test]
fn flat_drop_requires_explicit_skip_in_live_full_and_both_partial_segments() {
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
                BackwardReferencesPolicy::Maintain,
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
        for policy in [
            BackwardReferencesPolicy::Maintain,
            BackwardReferencesPolicy::Skip,
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
                ops,
                Some(BatchApplyOptions {
                    disable_operation_consistency_check: true,
                    backward_references_policy: policy,
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
