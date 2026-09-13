//! Default maintenance, explicit opt-out, and cached observation regressions.

use grovedb_merk::{element::get::ElementFetchFromStorageExtensions, tree_type::TreeType};
use grovedb_path::SubtreePath;
use grovedb_storage::rocksdb_storage::RocksDbStorage;
use grovedb_version::version::GroveVersion;

use crate::{
    batch::{key_info::KeyInfo, BatchApplyOptions, GroveOp, KeyInfoPath, QualifiedGroveDbOp},
    operations::{
        delete::{DeleteOptions, DeleteUpTreeOptions},
        insert::InsertOptions,
    },
    reference_path::ReferencePathType,
    tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
    BackwardsReferences, BidirectionalReference, Element, Error, GroveDb,
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

fn dont_check(ops: Vec<QualifiedGroveDbOp>) -> Vec<QualifiedGroveDbOp> {
    ops.into_iter()
        .map(|op| op.dont_check_for_backwards_references())
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
        for declared in [BackwardsReferences::Check, BackwardsReferences::DontCheck] {
            let db = chain(true);
            let before = db.root_hash(None, version).unwrap().unwrap();
            let item = Element::new_item(b"plain".to_vec());
            let result = if batch {
                let op = QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"value".to_vec(),
                    item.clone(),
                );
                db.apply_batch(
                    vec![match declared {
                        BackwardsReferences::Check => op,
                        BackwardsReferences::DontCheck => op.dont_check_for_backwards_references(),
                    }],
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
                        backwards_references: declared,
                        ..Default::default()
                    }),
                    None,
                    version,
                )
                .unwrap()
            };
            let refused = !declared.should_check();
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
            .apply_batch(dont_check(ops), None, None, version)
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
            backwards_references: BackwardsReferences::DontCheck,
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
                    backwards_references: BackwardsReferences::DontCheck,
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
                // stored element to verify the trusted DontCheck contract.
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
                    "a raw clear declared DontCheck leaves the registration stale"
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
                BackwardsReferences::Check,
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
        for declared in [BackwardsReferences::Check, BackwardsReferences::DontCheck] {
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
                    .map(|op| match declared {
                        BackwardsReferences::Check => op,
                        BackwardsReferences::DontCheck => op.dont_check_for_backwards_references(),
                    })
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
            for declared in [BackwardsReferences::Check, BackwardsReferences::DontCheck] {
                let db = make_test_grovedb(version);
                seed(&db);
                let ops: Vec<QualifiedGroveDbOp> = delete_up_tree
                    .iter()
                    .cloned()
                    .map(|op| match declared {
                        BackwardsReferences::Check => op,
                        BackwardsReferences::DontCheck => op.dont_check_for_backwards_references(),
                    })
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
                "{behavior:?} partial={partial}: Check must cost exactly what \
                 DontCheck costs"
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
    for declared in [BackwardsReferences::Check, BackwardsReferences::DontCheck] {
        let db = make_test_grovedb(version);
        seed(&db);
        let ops: Vec<QualifiedGroveDbOp> = recursive
            .iter()
            .cloned()
            .map(|op| match declared {
                BackwardsReferences::Check => op,
                BackwardsReferences::DontCheck => op.dont_check_for_backwards_references(),
            })
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
/// batch options are supplied: a false `DontCheck` claim is refused
/// with `None` exactly as with `Some(BatchApplyOptions::default())`.
#[test]
fn non_batched_apply_keeps_the_op_declaration_without_options() {
    let version = GroveVersion::latest();
    let overwrite = QualifiedGroveDbOp::insert_or_replace_op(
        vec![TEST_LEAF.to_vec()],
        b"value".to_vec(),
        Element::new_item(vec![2]),
    )
    .dont_check_for_backwards_references();
    let delete = QualifiedGroveDbOp::delete_op(vec![TEST_LEAF.to_vec()], b"value".to_vec())
        .dont_check_for_backwards_references();
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
/// displaced value to check: declaring `DontCheck` on it is not a false
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
        .dont_check_for_backwards_references()],
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
/// `DontCheck` (a plain overwrite, or a `DeleteTree(Skip)` that ends up
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
                .dont_check_for_backwards_references()])
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

#[test]
fn delete_op_builders_carry_the_declared_displaced_value() {
    let version = GroveVersion::latest();
    let db = make_test_grovedb(version);
    db.insert(
        &[TEST_LEAF],
        b"item",
        Element::new_item(b"v".to_vec()),
        None,
        None,
        version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF],
        b"outer",
        Element::empty_tree(),
        None,
        None,
        version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF, b"outer"],
        b"inner",
        Element::empty_tree(),
        None,
        None,
        version,
    )
    .unwrap()
    .unwrap();

    let build = |path: &[&[u8]], key: &[u8], declared: BackwardsReferences| {
        db.delete_operation_for_delete_internal(
            SubtreePath::from(path),
            key,
            &DeleteOptions::default().with_backwards_references(declared),
            None,
            &[],
            None,
            version,
        )
        .unwrap()
        .unwrap()
        .expect("the delete op is built")
        .op
    };
    assert!(matches!(
        build(&[TEST_LEAF], b"item", BackwardsReferences::Check),
        GroveOp::Delete
    ));
    assert!(matches!(
        build(&[TEST_LEAF], b"item", BackwardsReferences::DontCheck),
        GroveOp::DeleteDontCheckForBackwardsReferences
    ));
    assert!(matches!(
        build(&[TEST_LEAF, b"outer"], b"inner", BackwardsReferences::Check),
        GroveOp::DeleteTree(..)
    ));
    assert!(matches!(
        build(
            &[TEST_LEAF, b"outer"],
            b"inner",
            BackwardsReferences::DontCheck
        ),
        GroveOp::DeleteTreeDontCheckForBackwardsReferences(..)
    ));

    // The up-tree chain declares every level from its own options.
    let chain_ops = |declared: BackwardsReferences| {
        db.delete_operations_for_delete_up_tree_while_empty(
            SubtreePath::from([TEST_LEAF, b"outer"].as_ref()),
            b"inner",
            &DeleteUpTreeOptions {
                // Stop before the root leaf, which cannot be deleted.
                stop_path_height: Some(0),
                backwards_references: declared,
                ..Default::default()
            },
            None,
            vec![],
            None,
            version,
        )
        .unwrap()
        .unwrap()
    };
    let checked = chain_ops(BackwardsReferences::Check);
    assert_eq!(checked.len(), 2, "inner, then the emptied outer");
    assert!(checked
        .iter()
        .all(|op| !op.op.is_dont_check_for_backwards_references()));
    let declared = chain_ops(BackwardsReferences::DontCheck);
    assert_eq!(declared.len(), 2);
    assert!(declared
        .iter()
        .all(|op| op.op.is_dont_check_for_backwards_references()));

    // The estimated builders declare the same way, so the estimator sees the
    // op the stateful path would apply.
    let path = KeyInfoPath::from_known_owned_path(vec![TEST_LEAF.to_vec()]);
    let key = KeyInfo::KnownKey(b"item".to_vec());
    let average = GroveDb::average_case_delete_operation_for_delete::<RocksDbStorage>(
        &path,
        &key,
        TreeType::NormalTree,
        false,
        true,
        0,
        (4, 8),
        BackwardsReferences::DontCheck,
        version,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(
        average.op,
        GroveOp::DeleteDontCheckForBackwardsReferences
    ));
    let worst = GroveDb::worst_case_delete_operation_for_delete::<RocksDbStorage>(
        &path,
        &key,
        TreeType::NormalTree,
        false,
        true,
        0,
        8,
        BackwardsReferences::Check,
        version,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(worst.op, GroveOp::Delete));
}

#[test]
fn delete_up_tree_honors_the_declared_displaced_value() {
    let version = GroveVersion::latest();
    let db = make_test_grovedb(version);
    let sub: &[&[u8]] = &[TEST_LEAF, b"sub"];
    db.insert(
        &[TEST_LEAF],
        b"sub",
        Element::empty_tree(),
        None,
        None,
        version,
    )
    .unwrap()
    .unwrap();
    db.apply_batch(
        vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"sub".to_vec()],
                b"r2".to_vec(),
                reference(b"r1", true),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"sub".to_vec()],
                b"value".to_vec(),
                Element::new_item_allowing_bidirectional_references(b"old".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"sub".to_vec()],
                b"r1".to_vec(),
                reference(b"value", true),
            ),
        ],
        None,
        None,
        version,
    )
    .unwrap()
    .unwrap();
    let before = db.root_hash(None, version).unwrap().unwrap();

    // A false DontCheck claim over a participant is refused before
    // anything commits.
    let result = db
        .delete_up_tree_while_empty(
            sub,
            b"value",
            &DeleteUpTreeOptions {
                stop_path_height: Some(1),
                backwards_references: BackwardsReferences::DontCheck,
                ..Default::default()
            },
            None,
            version,
        )
        .unwrap();
    assert!(matches!(result, Err(Error::NotSupported(_))), "{result:?}");
    assert_eq!(db.root_hash(None, version).unwrap().unwrap(), before);
    assert!(db
        .get_raw(SubtreePath::from(sub), b"r1", None, version)
        .unwrap()
        .is_ok());

    // The default declaration maintains the chain: the cascading references
    // go with the value, and the subtree they lived in stays.
    db.delete_up_tree_while_empty(
        sub,
        b"value",
        &DeleteUpTreeOptions {
            stop_path_height: Some(1),
            ..Default::default()
        },
        None,
        version,
    )
    .unwrap()
    .unwrap();
    for key in [&b"value"[..], b"r1", b"r2"] {
        let got = db
            .get_raw(SubtreePath::from(sub), key, None, version)
            .unwrap();
        assert!(matches!(got, Err(Error::PathKeyNotFound(_))), "{got:?}");
    }
    assert!(db
        .get_raw(SubtreePath::from(&[TEST_LEAF]), b"sub", None, version)
        .unwrap()
        .is_ok());
}

/// A callback `DeleteTree(DontCheckWithNoCleanup)` at the same key as an
/// initial-segment `DeleteTree(DeleteChildren)` must not change how the
/// initial deletion is cleaned up: the cleanup walk still runs, so it still
/// clears the subtree's storage and still refuses a participant the batch
/// does not explicitly delete.
#[test]
fn callback_delete_tree_cannot_erase_the_initial_segments_cleanup() {
    use crate::batch::SubelementsDeletionBehavior;
    use grovedb_merk::TreeType;
    use grovedb_storage::{Storage, StorageContext};
    let version = GroveVersion::latest();
    let seed = |db: &TempGroveDb, child: Element| {
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
        db.insert(&[TEST_LEAF, b"tree"], b"value", child, None, None, version)
            .unwrap()
            .unwrap();
    };
    let ops = || {
        (
            vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"tree".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::DeleteChildren,
            )],
            |_: &_, _: &_| {
                Ok(vec![QualifiedGroveDbOp::delete_tree_op(
                    vec![TEST_LEAF.to_vec()],
                    b"tree".to_vec(),
                    TreeType::NormalTree,
                    SubelementsDeletionBehavior::DontCheckWithNoCleanup,
                )])
            },
        )
    };

    // A plain child: the batch applies and the initial deletion's cleanup
    // still clears the subtree's storage.
    let db = make_test_grovedb(version);
    seed(&db, Element::new_item(vec![1]));
    let (initial, callback) = ops();
    db.apply_partial_batch(initial, None, callback, None, version)
        .unwrap()
        .expect("both deletions apply");
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
        storage.get(b"value").unwrap().unwrap().is_none(),
        "the initial DeleteChildren must still clean the subtree's storage"
    );
    drop(storage);
    drop(tx);
    assert!(db
        .verify_grovedb(None, true, true, version)
        .unwrap()
        .is_empty());

    // A participant referenced from outside the subtree: the cleanup walk
    // of the initial deletion still refuses it, so nothing dangles.
    let db = make_test_grovedb(version);
    seed(
        &db,
        Element::new_item_allowing_bidirectional_references(vec![1]),
    );
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
    let (initial, callback) = ops();
    let result = db
        .apply_partial_batch(initial, None, callback, None, version)
        .unwrap();
    assert!(matches!(result, Err(Error::NotSupported(_))), "{result:?}");
    assert_eq!(db.root_hash(None, version).unwrap().unwrap(), before);
    db.get(&[TEST_LEAF], b"outside", None, version)
        .unwrap()
        .expect("the outside reference still resolves");
    assert!(db
        .verify_grovedb(None, true, true, version)
        .unwrap()
        .is_empty());
}

/// The mirror image: a callback `DeleteTree(DontCheckWithNoCleanup)` at the
/// same key as an initial-segment replacement of a populated tree must not
/// exempt that replacement from the participant scan. The initial-segment
/// footprint guard refuses the add-on op before that question arises, and
/// this pins that the refusal leaves nothing dangling.
#[test]
fn callback_delete_tree_cannot_exempt_an_initial_replacement() {
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
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"tree".to_vec(),
            Element::empty_tree(),
        )],
        None,
        |_, _| {
            Ok(vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"tree".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::DontCheckWithNoCleanup,
            )])
        },
        None,
        version,
    );
    let result = result.unwrap();
    assert!(
        matches!(result, Err(Error::InvalidBatchOperation(_))),
        "{result:?}"
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

/// Every displacing op converts to its twin and back, reports its
/// declaration, and renders as such; ops without a twin are returned as is.
#[test]
fn twin_conversions_and_debug_output() {
    use crate::batch::SubelementsDeletionBehavior;
    use grovedb_merk::TreeType;
    let path = vec![TEST_LEAF.to_vec()];
    let item = Element::new_item(vec![1]);
    let checked_ops = vec![
        QualifiedGroveDbOp::insert_or_replace_op(path.clone(), b"a".to_vec(), item.clone()),
        QualifiedGroveDbOp::replace_op(path.clone(), b"b".to_vec(), item.clone()),
        QualifiedGroveDbOp::patch_op(path.clone(), b"c".to_vec(), item.clone(), 0),
        QualifiedGroveDbOp::delete_op(path.clone(), b"d".to_vec()),
        QualifiedGroveDbOp::delete_tree_op(
            path.clone(),
            b"t".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DeleteChildren,
        ),
    ];
    for checked in checked_ops {
        assert!(!checked.op.is_dont_check_for_backwards_references());
        assert_eq!(
            checked.op.backwards_references(),
            BackwardsReferences::Check
        );
        let twin = checked.clone().dont_check_for_backwards_references();
        assert!(twin.op.is_dont_check_for_backwards_references());
        assert_eq!(
            twin.op.backwards_references(),
            BackwardsReferences::DontCheck
        );
        assert_ne!(twin.op, checked.op);
        assert_eq!(twin.op.clone().checked(), checked.op);
        assert_eq!(checked.op.clone().checked(), checked.op);
        assert_eq!(
            twin.clone()
                .with_backwards_references(BackwardsReferences::Check)
                .op,
            checked.op
        );
        assert_eq!(
            checked
                .clone()
                .with_backwards_references(BackwardsReferences::DontCheck)
                .op,
            twin.op
        );
        let rendered = format!("{twin:?}");
        assert!(
            rendered.contains("dont check for backwards references"),
            "{rendered}"
        );
        assert!(!format!("{checked:?}").contains("dont check"));
        assert_eq!(twin.op.to_u8(), checked.op.to_u8());
        assert!(checked.op < twin.op);
    }
    let no_twin =
        QualifiedGroveDbOp::insert_only_known_to_not_already_exist_op(path, b"n".to_vec(), item);
    let unchanged = no_twin.clone().dont_check_for_backwards_references();
    assert_eq!(unchanged.op, no_twin.op);
    assert!(!unchanged.op.is_dont_check_for_backwards_references());
    assert_eq!(unchanged.op.checked(), no_twin.op);
}

/// The write twins (`InsertOrReplace`, `Replace`, `Patch`) flow through the
/// full batch (planner and executor, including root propagation onto a
/// twin that inserts a subtree the same batch writes into), the partial
/// batch (initial segment footprint) and the non-batched adapter, and land
/// exactly like their checked ops.
#[test]
fn write_twins_apply_through_every_batch_path() {
    let version = GroveVersion::latest();
    let db = make_test_grovedb(version);
    for key in [&b"a"[..], b"b", b"c"] {
        db.insert(
            &[TEST_LEAF],
            key,
            Element::new_item(vec![1, 2, 3]),
            None,
            None,
            version,
        )
        .unwrap()
        .unwrap();
    }
    for tree in [&b"sub"[..], b"sub2"] {
        db.insert(
            &[TEST_LEAF],
            tree,
            Element::empty_tree(),
            None,
            None,
            version,
        )
        .unwrap()
        .unwrap();
    }
    let leaf = || vec![TEST_LEAF.to_vec()];
    let under = |tree: &[u8]| vec![TEST_LEAF.to_vec(), tree.to_vec()];
    let item = |byte: u8| Element::new_item(vec![byte, byte, byte]);

    // Full batch: item twins plus empty-subtree twins whose contents the
    // same batch writes, so the parent-level op is rebuilt with the
    // propagated root.
    db.apply_batch(
        vec![
            QualifiedGroveDbOp::insert_or_replace_op(leaf(), b"a".to_vec(), item(4))
                .dont_check_for_backwards_references(),
            QualifiedGroveDbOp::replace_op(leaf(), b"b".to_vec(), item(4))
                .dont_check_for_backwards_references(),
            QualifiedGroveDbOp::patch_op(leaf(), b"c".to_vec(), item(4), 0)
                .dont_check_for_backwards_references(),
            QualifiedGroveDbOp::insert_or_replace_op(
                leaf(),
                b"sub".to_vec(),
                Element::empty_tree(),
            )
            .dont_check_for_backwards_references(),
            QualifiedGroveDbOp::insert_or_replace_op(under(b"sub"), b"y".to_vec(), item(7))
                .dont_check_for_backwards_references(),
            QualifiedGroveDbOp::replace_op(leaf(), b"sub2".to_vec(), Element::empty_tree())
                .dont_check_for_backwards_references(),
            QualifiedGroveDbOp::insert_or_replace_op(under(b"sub2"), b"z".to_vec(), item(8)),
        ],
        None,
        None,
        version,
    )
    .unwrap()
    .expect("write twins over plain values apply");
    for key in [&b"a"[..], b"b", b"c"] {
        assert_eq!(
            db.get(&[TEST_LEAF], key, None, version).unwrap().unwrap(),
            item(4)
        );
    }
    assert_eq!(
        db.get(&[TEST_LEAF, b"sub"], b"y", None, version)
            .unwrap()
            .unwrap(),
        item(7)
    );
    assert_eq!(
        db.get(&[TEST_LEAF, b"sub2"], b"z", None, version)
            .unwrap()
            .unwrap(),
        item(8)
    );

    // Partial batch: the same twins in the initial segment, a twin in the
    // callback.
    db.apply_partial_batch(
        vec![
            QualifiedGroveDbOp::insert_or_replace_op(leaf(), b"a".to_vec(), item(1))
                .dont_check_for_backwards_references(),
            QualifiedGroveDbOp::replace_op(leaf(), b"b".to_vec(), item(1))
                .dont_check_for_backwards_references(),
            QualifiedGroveDbOp::patch_op(leaf(), b"c".to_vec(), item(1), 0)
                .dont_check_for_backwards_references(),
        ],
        None,
        |_, _| {
            Ok(vec![QualifiedGroveDbOp::replace_op(
                vec![TEST_LEAF.to_vec()],
                b"a".to_vec(),
                Element::new_item(vec![2, 2, 2]),
            )
            .dont_check_for_backwards_references()])
        },
        None,
        version,
    )
    .unwrap()
    .expect("write twins apply in both partial-batch segments");
    assert_eq!(
        db.get(&[TEST_LEAF], b"a", None, version).unwrap().unwrap(),
        item(2)
    );

    // Non-batched adapter.
    db.apply_operations_without_batching(
        vec![
            QualifiedGroveDbOp::replace_op(leaf(), b"b".to_vec(), item(3))
                .dont_check_for_backwards_references(),
            QualifiedGroveDbOp::insert_or_replace_op(leaf(), b"c".to_vec(), item(3))
                .dont_check_for_backwards_references(),
        ],
        None,
        None,
        version,
    )
    .unwrap()
    .expect("write twins apply without batching");
    assert_eq!(
        db.get(&[TEST_LEAF], b"b", None, version).unwrap().unwrap(),
        item(3)
    );
    assert!(db
        .verify_grovedb(None, true, true, version)
        .unwrap()
        .is_empty());
}

/// A live insert that replaces a populated subtree reads none of its
/// contents, so the default declaration scans it: a participant inside is
/// refused, a subtree without one is replaced.
#[test]
fn live_tree_replacement_scans_participants_when_checked() {
    let version = GroveVersion::latest();
    let seed = |db: &TempGroveDb, child: Element| {
        db.insert(
            &[TEST_LEAF],
            b"sub",
            Element::empty_tree(),
            None,
            None,
            version,
        )
        .unwrap()
        .unwrap();
        db.insert(&[TEST_LEAF, b"sub"], b"value", child, None, None, version)
            .unwrap()
            .unwrap();
    };

    let db = make_test_grovedb(version);
    seed(&db, Element::new_item(vec![1]));
    let over_tree = || {
        Some(InsertOptions {
            validate_insertion_does_not_override_tree: false,
            ..Default::default()
        })
    };
    db.insert(
        &[TEST_LEAF],
        b"sub",
        Element::empty_tree(),
        over_tree(),
        None,
        version,
    )
    .unwrap()
    .expect("a subtree without participants is replaced");

    let db = make_test_grovedb(version);
    seed(
        &db,
        Element::new_item_allowing_bidirectional_references(vec![1]),
    );
    db.insert(
        &[TEST_LEAF],
        b"outside",
        Element::BidirectionalReference(
            BidirectionalReference {
                forward_reference_path: ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"sub".to_vec(),
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
    let result = db
        .insert(
            &[TEST_LEAF],
            b"sub",
            Element::empty_tree(),
            over_tree(),
            None,
            version,
        )
        .unwrap();
    assert!(matches!(result, Err(Error::NotSupported(_))), "{result:?}");
    assert_eq!(db.root_hash(None, version).unwrap().unwrap(), before);
    db.get(&[TEST_LEAF], b"outside", None, version)
        .unwrap()
        .expect("the outside reference still resolves");
}

/// A partial batch replacing a populated subtree with a `DontCheck` twin
/// skips the post-apply participant scan (the observer records the twin's
/// declaration), and a `DeleteChildren` removal whose participant the
/// batch explicitly deletes passes the cleanup walk's accounting.
#[test]
fn dont_check_replacement_in_a_partial_batch_and_accounted_participant_on_the_cleanup_walk() {
    use crate::batch::SubelementsDeletionBehavior;
    use grovedb_merk::TreeType;
    let version = GroveVersion::latest();
    let db = make_test_grovedb(version);
    db.insert(
        &[TEST_LEAF],
        b"sub",
        Element::empty_tree(),
        None,
        None,
        version,
    )
    .unwrap()
    .unwrap();
    db.insert(
        &[TEST_LEAF, b"sub"],
        b"x",
        Element::new_item(vec![1]),
        None,
        None,
        version,
    )
    .unwrap()
    .unwrap();
    db.apply_partial_batch(
        vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"sub".to_vec(),
            Element::empty_tree(),
        )
        .dont_check_for_backwards_references()],
        None,
        |_, _| Ok(vec![]),
        None,
        version,
    )
    .unwrap()
    .expect("a DontCheck replacement of a plain subtree is not scanned");

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
    db.apply_batch(
        vec![
            QualifiedGroveDbOp::delete_op(
                vec![TEST_LEAF.to_vec(), b"tree".to_vec()],
                b"value".to_vec(),
            ),
            QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"tree".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::DeleteChildren,
            ),
        ],
        None,
        None,
        version,
    )
    .unwrap()
    .expect("the explicitly deleted participant is accounted for on the cleanup walk");
    assert!(db
        .get_raw(SubtreePath::from(&[TEST_LEAF]), b"outside", None, version)
        .unwrap()
        .is_err());
    assert!(db
        .verify_grovedb(None, true, true, version)
        .unwrap()
        .is_empty());
}
