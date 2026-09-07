//! Regression tests for issue #708: an add-on operation returned by a partial
//! batch's callback must not discard the ancestor update the paused batch is
//! still carrying.
//!
//! `apply_partial_batch` pauses after the subtree levels have run and hands
//! the callback the leftover ops — by default the root level, holding one
//! internal `ReplaceTreeRootKey` (or `InsertTreeWithRootHash`) per modified
//! child tree that carries the child's new root hash, root key and aggregate.
//! `BatchStructure::continue_from_ops` then filed each add-on op into the
//! same `(path, key)` map with a plain insert, so an add-on op addressed at a
//! modified child tree silently REPLACED its pending propagation op: the
//! child's writes were committed to storage but the parent element was
//! rewritten from the callback's bytes — root key `None`, stale aggregate,
//! stale hash — orphaning both the batch's writes and any rows the child
//! already held.
//!
//! `GROVE_V3` is live, so the fix is gated to `GROVE_V4`
//! (`apply_batch.add_on_op_collision: 1`): a colliding add-on insert is
//! merged with the pending propagation state exactly as the propagation
//! step merges an in-batch insert (element bytes from the callback, root
//! state from the batch), a colliding delete is accepted only if the child
//! ended the batch empty, and a collision with a pending non-Merk root
//! update is refused. An add-on that duplicates a still-unexecuted user op
//! of the initial batch is refused by the entry point's consistency check
//! (last op wins when that check is disabled, exactly as within one batch).
//! V3 keeps the legacy overwrite and that outcome is pinned below.

#[cfg(test)]
mod tests {
    use grovedb_merk::tree_type::TreeType;
    use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

    use crate::{
        batch::{BatchApplyOptions, GroveOp, QualifiedGroveDbOp, SubelementsDeletionBehavior},
        tests::{common::EMPTY_PATH, make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, Error,
    };

    const CHILD: &[u8] = b"child";
    const CHILD_VALUE: &[u8] = b"child_value";
    const EXISTING: &[u8] = b"existing";
    const EXISTING_VALUE: &[u8] = b"existing_value";
    const NEW_FLAGS: &[u8] = &[7, 7, 7];

    fn seed(grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            EXISTING,
            Element::new_item(EXISTING_VALUE.to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("seed item under TEST_LEAF");
        db
    }

    fn child_insert() -> QualifiedGroveDbOp {
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            CHILD.to_vec(),
            Element::new_item(CHILD_VALUE.to_vec()),
        )
    }

    fn assert_clean(db: &TempGroveDb, grove_version: &GroveVersion) {
        let issues = db
            .verify_grovedb(None, true, false, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "verify_grovedb issues: {issues:?}");
    }

    /// The leftover map handed to the callback must carry the child's root
    /// propagation at the root level so the collision below is real.
    fn assert_leftover_carries_root_update(
        leftover: &Option<crate::batch::OpsByLevelPath>,
        key: &[u8],
    ) {
        let leftover = leftover
            .as_ref()
            .expect("paused batch hands back leftover ops");
        let root_level = leftover.get(0).expect("root level present");
        let root_ops = root_level
            .get(&crate::batch::KeyInfoPath::from_known_owned_path(vec![]))
            .expect("root path present");
        let op = root_ops
            .get(&crate::batch::key_info::KeyInfo::KnownKey(key.to_vec()))
            .expect("pending op for the modified child tree");
        assert!(
            matches!(
                op,
                GroveOp::ReplaceTreeRootKey { .. } | GroveOp::InsertTreeWithRootHash { .. }
            ),
            "expected a pending root propagation, got {op:?}"
        );
    }

    #[test]
    fn add_on_replace_of_modified_tree_keeps_its_children() {
        let grove_version = GroveVersion::latest();
        let db = seed(grove_version);

        db.apply_partial_batch(
            vec![child_insert()],
            None,
            |_cost, leftover| {
                assert_leftover_carries_root_update(leftover, TEST_LEAF);
                Ok(vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![],
                    TEST_LEAF.to_vec(),
                    Element::empty_tree_with_flags(Some(NEW_FLAGS.to_vec())),
                )])
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("add-on replace of the modified tree is merged");

        let tree = db
            .get(EMPTY_PATH, TEST_LEAF, None, grove_version)
            .unwrap()
            .expect("tree element");
        assert_eq!(tree.get_flags(), &Some(NEW_FLAGS.to_vec()));
        assert!(
            matches!(tree, Element::Tree(Some(_), _)),
            "root key must survive the add-on replace: {tree:?}"
        );

        let child = db
            .get([TEST_LEAF].as_ref(), CHILD, None, grove_version)
            .unwrap()
            .expect("the batch's own write is reachable");
        assert_eq!(child, Element::new_item(CHILD_VALUE.to_vec()));
        let existing = db
            .get([TEST_LEAF].as_ref(), EXISTING, None, grove_version)
            .unwrap()
            .expect("the pre-existing row is reachable");
        assert_eq!(existing, Element::new_item(EXISTING_VALUE.to_vec()));
        assert_clean(&db, grove_version);
    }

    #[test]
    fn add_on_replace_of_tree_created_in_batch_keeps_its_children() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let new_tree = b"new_tree";

        db.apply_partial_batch(
            vec![
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![],
                    new_tree.to_vec(),
                    Element::empty_tree(),
                ),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![new_tree.to_vec()],
                    CHILD.to_vec(),
                    Element::new_item(CHILD_VALUE.to_vec()),
                ),
            ],
            None,
            |_cost, leftover| {
                assert_leftover_carries_root_update(leftover, new_tree);
                Ok(vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![],
                    new_tree.to_vec(),
                    Element::empty_tree_with_flags(Some(NEW_FLAGS.to_vec())),
                )])
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("add-on replace of the created tree is merged");

        let tree = db
            .get(EMPTY_PATH, new_tree, None, grove_version)
            .unwrap()
            .expect("tree element");
        assert_eq!(tree.get_flags(), &Some(NEW_FLAGS.to_vec()));
        let child = db
            .get([new_tree.as_slice()].as_ref(), CHILD, None, grove_version)
            .unwrap()
            .expect("child written in the same batch is reachable");
        assert_eq!(child, Element::new_item(CHILD_VALUE.to_vec()));
        assert_clean(&db, grove_version);
    }

    #[test]
    fn add_on_replace_of_modified_sum_tree_keeps_its_aggregate() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let sum_tree = b"sum_tree";
        db.insert(
            EMPTY_PATH,
            sum_tree,
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum tree");

        db.apply_partial_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![sum_tree.to_vec()],
                b"s1".to_vec(),
                Element::new_sum_item(5),
            )],
            None,
            |_cost, _leftover| {
                Ok(vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![],
                    sum_tree.to_vec(),
                    Element::empty_sum_tree_with_flags(Some(NEW_FLAGS.to_vec())),
                )])
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("add-on replace of the modified sum tree is merged");

        let tree = db
            .get(EMPTY_PATH, sum_tree, None, grove_version)
            .unwrap()
            .expect("sum tree element");
        assert_eq!(tree.get_flags(), &Some(NEW_FLAGS.to_vec()));
        assert_eq!(
            tree.sum_value_or_default(),
            5,
            "aggregate survives: {tree:?}"
        );
        assert_clean(&db, grove_version);
    }

    #[test]
    fn add_on_item_over_modified_tree_is_refused() {
        let grove_version = GroveVersion::latest();
        let db = seed(grove_version);

        let result = db
            .apply_partial_batch(
                vec![child_insert()],
                None,
                |_cost, _leftover| {
                    Ok(vec![QualifiedGroveDbOp::insert_or_replace_op(
                        vec![],
                        TEST_LEAF.to_vec(),
                        Element::new_item(b"not a tree".to_vec()),
                    )])
                },
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(Error::InvalidBatchOperation(_))),
            "expected InvalidBatchOperation, got {result:?}"
        );

        // Nothing was committed: the tree still holds only the seed row.
        let existing = db
            .get([TEST_LEAF].as_ref(), EXISTING, None, grove_version)
            .unwrap()
            .expect("seed row intact");
        assert_eq!(existing, Element::new_item(EXISTING_VALUE.to_vec()));
        assert!(matches!(
            db.get([TEST_LEAF].as_ref(), CHILD, None, grove_version)
                .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
        assert_clean(&db, grove_version);
    }

    #[test]
    fn add_on_delete_of_still_populated_tree_is_refused() {
        let grove_version = GroveVersion::latest();
        let db = seed(grove_version);

        let result = db
            .apply_partial_batch(
                vec![child_insert()],
                None,
                |_cost, _leftover| {
                    Ok(vec![QualifiedGroveDbOp::delete_tree_op(
                        vec![],
                        TEST_LEAF.to_vec(),
                        TreeType::NormalTree,
                        SubelementsDeletionBehavior::DeleteChildren,
                    )])
                },
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(Error::InvalidBatchOperation(_))),
            "expected InvalidBatchOperation, got {result:?}"
        );
        assert_clean(&db, grove_version);
    }

    #[test]
    fn add_on_delete_of_tree_emptied_by_batch_is_accepted() {
        let grove_version = GroveVersion::latest();
        let db = seed(grove_version);

        db.apply_partial_batch(
            vec![QualifiedGroveDbOp::delete_op(
                vec![TEST_LEAF.to_vec()],
                EXISTING.to_vec(),
            )],
            None,
            |_cost, _leftover| {
                Ok(vec![QualifiedGroveDbOp::delete_tree_op(
                    vec![],
                    TEST_LEAF.to_vec(),
                    TreeType::NormalTree,
                    SubelementsDeletionBehavior::Error,
                )])
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("deleting a tree the batch emptied is fine");

        assert!(matches!(
            db.get(EMPTY_PATH, TEST_LEAF, None, grove_version).unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
        assert_clean(&db, grove_version);
    }

    #[test]
    fn add_on_op_over_pending_user_op_is_refused() {
        let grove_version = GroveVersion::latest();
        let db = seed(grove_version);
        let root_item = b"root_item";

        let result = db
            .apply_partial_batch(
                vec![
                    child_insert(),
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![],
                        root_item.to_vec(),
                        Element::new_item(b"first".to_vec()),
                    ),
                ],
                None,
                |_cost, _leftover| {
                    Ok(vec![QualifiedGroveDbOp::insert_or_replace_op(
                        vec![],
                        root_item.to_vec(),
                        Element::new_item(b"second".to_vec()),
                    )])
                },
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(Error::InvalidBatchOperation(_))),
            "expected InvalidBatchOperation, got {result:?}"
        );
        assert_clean(&db, grove_version);
    }

    /// With the consistency check disabled a cross-segment user duplicate
    /// follows the same last-op-wins rule as an in-batch duplicate — but
    /// the ancestor-update merge is not optional: the root propagation the
    /// callback collides with is still composed, never overwritten.
    #[test]
    fn consistency_check_disabled_keeps_last_wins_but_still_merges_root_state() {
        let grove_version = GroveVersion::latest();
        let db = seed(grove_version);
        let root_item = b"root_item";
        let options = BatchApplyOptions {
            disable_operation_consistency_check: true,
            ..Default::default()
        };

        db.apply_partial_batch(
            vec![
                child_insert(),
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![],
                    root_item.to_vec(),
                    Element::new_item(b"first".to_vec()),
                ),
            ],
            Some(options),
            |_cost, _leftover| {
                Ok(vec![
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![],
                        root_item.to_vec(),
                        Element::new_item(b"second".to_vec()),
                    ),
                    QualifiedGroveDbOp::insert_or_replace_op(
                        vec![],
                        TEST_LEAF.to_vec(),
                        Element::empty_tree_with_flags(Some(NEW_FLAGS.to_vec())),
                    ),
                ])
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("consistency check disabled: duplicates accepted");

        assert_eq!(
            db.get(EMPTY_PATH, root_item, None, grove_version)
                .unwrap()
                .expect("root item"),
            Element::new_item(b"second".to_vec()),
            "user duplicate: last op wins"
        );
        let tree = db
            .get(EMPTY_PATH, TEST_LEAF, None, grove_version)
            .unwrap()
            .expect("tree element");
        assert_eq!(tree.get_flags(), &Some(NEW_FLAGS.to_vec()));
        assert_eq!(
            db.get([TEST_LEAF].as_ref(), CHILD, None, grove_version)
                .unwrap()
                .expect("child still reachable"),
            Element::new_item(CHILD_VALUE.to_vec())
        );
        assert_clean(&db, grove_version);
    }

    #[test]
    fn add_on_op_on_a_fresh_key_is_unaffected() {
        let grove_version = GroveVersion::latest();
        let db = seed(grove_version);
        let root_item = b"root_item";

        db.apply_partial_batch(
            vec![child_insert()],
            None,
            |_cost, _leftover| {
                Ok(vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![],
                    root_item.to_vec(),
                    Element::new_item(b"v".to_vec()),
                )])
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("non-colliding add-on op");

        assert_eq!(
            db.get(EMPTY_PATH, root_item, None, grove_version)
                .unwrap()
                .expect("add-on row"),
            Element::new_item(b"v".to_vec())
        );
        assert_eq!(
            db.get([TEST_LEAF].as_ref(), CHILD, None, grove_version)
                .unwrap()
                .expect("batch row"),
            Element::new_item(CHILD_VALUE.to_vec())
        );
        assert_clean(&db, grove_version);
    }

    /// V3 is live: the legacy overwrite is pinned so a replay evaluates as
    /// it did. The add-on element wins and the child tree's rows are
    /// orphaned behind a rootless parent element.
    #[test]
    fn grove_v3_keeps_legacy_overwrite() {
        let grove_version = &GROVE_V3;
        let db = seed(grove_version);

        db.apply_partial_batch(
            vec![child_insert()],
            None,
            |_cost, _leftover| {
                Ok(vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![],
                    TEST_LEAF.to_vec(),
                    Element::empty_tree_with_flags(Some(NEW_FLAGS.to_vec())),
                )])
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("legacy V3 accepts the overwrite");

        let tree = db
            .get(EMPTY_PATH, TEST_LEAF, None, grove_version)
            .unwrap()
            .expect("tree element");
        assert!(
            matches!(tree, Element::Tree(None, _)),
            "legacy: the parent element lost its root key: {tree:?}"
        );
        assert!(matches!(
            db.get([TEST_LEAF].as_ref(), CHILD, None, grove_version)
                .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
        assert!(matches!(
            db.get([TEST_LEAF].as_ref(), EXISTING, None, grove_version)
                .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
    }
}
