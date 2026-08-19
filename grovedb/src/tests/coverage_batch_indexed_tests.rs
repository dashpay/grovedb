//! Coverage tests targeting uncovered diff lines of PR #657.
//!
//! All of these exercise indexed-tree (PCIT / PSIT / PCPSIT) paths in
//! `grovedb/src/batch/mod.rs`:
//!
//! - `GroveOp` classification for the aggregate-indexed propagation op
//!   (sort tag, `Debug` label) and the "no cidx mirror needed" arm for
//!   the non-Merk append-tree leaf ops.
//! - The empty-creation guards of all three indexed variants: non-empty
//!   rejection, and the `InsertIfNotExists` existence probe with both
//!   its error and its skip outcome.
//! - Bubble-up arms: rejection of "create + populate an indexed tree in
//!   one batch", and carrying cidx secondary state into a parent level
//!   that exists but has no map for the cidx's own parent path.
//! - `DeleteTree` with `SubelementsDeletionBehavior::DontCheckWithNoCleanup`
//!   on an indexed primary (full and partial batch), which must still
//!   queue the per-axis secondary sweep.
//! - The cidx secondary-merk opener used by `continue_partial_apply_body`
//!   when an indexed primary level is executed after a partial-batch
//!   pause.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::{tree::AggregateData, tree_type::TreeType};
    use grovedb_version::version::GroveVersion;

    use crate::IndexedAxisEntrySliceExt;

    use crate::{
        batch::{BatchApplyOptions, GroveOp, QualifiedGroveDbOp, SubelementsDeletionBehavior},
        tests::{common::EMPTY_PATH, make_test_grovedb, ANOTHER_TEST_LEAF, TEST_LEAF},
        Element, Error,
    };

    fn assert_verify_passes(db: &crate::GroveDb, grove_version: &GroveVersion) {
        let issues = db
            .verify_grovedb(None, true, true, grove_version)
            .expect("verify_grovedb must not return a hard error");
        assert!(
            issues.is_empty(),
            "verify_grovedb reported issues: {:?}",
            issues
        );
    }

    fn pcpsit_axes() -> Vec<(u8, Option<Vec<u8>>)> {
        vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)]
    }

    fn empty_pcpsit() -> Element {
        Element::empty_provable_count_provable_sum_indexed_tree(pcpsit_axes())
            .expect("axes canonical")
    }

    // =================================================================
    // GroveOp variant classification
    //
    // batch/mod.rs L489 (`to_u8` tag for
    // `ReplaceAggregateIndexedTreeRootKeys`), L842 (its `Debug`
    // rendering) and L550 (`can_mutate_child_count` = false for the
    // non-Merk append-tree leaf ops).
    // =================================================================

    /// Pin the sort tag and the debug rendering of the
    /// aggregate-indexed propagation op. The tag is load-bearing:
    /// `Ord for GroveOp` sorts purely on it, so the batch pipeline's
    /// op ordering changes if the number changes.
    #[test]
    fn coverage_replace_aggregate_indexed_root_keys_sort_tag_and_debug() {
        let op = GroveOp::ReplaceAggregateIndexedTreeRootKeys {
            primary_hash: [1u8; 32],
            primary_root_key: Some(b"pk".to_vec()),
            primary_aggregate_data: AggregateData::ProvableCount(3),
            axes: vec![(0u8, [2u8; 32], Some(b"sk".to_vec()))],
        };
        // L489: the exact tag, not just a relative ordering.
        assert_eq!(op.to_u8(), 17);

        // Ordering consequences of that tag: it sorts after every
        // other propagation op and after DeleteTree.
        let delete_tree = GroveOp::DeleteTree(
            TreeType::ProvableCountIndexedTree,
            SubelementsDeletionBehavior::Error,
        );
        let plain_insert = GroveOp::InsertOrReplace {
            element: Element::new_item(b"v".to_vec()),
        };
        assert!(op > delete_tree);
        assert!(op > plain_insert);
        assert_eq!(delete_tree.to_u8(), 0);

        // L842: the Debug label for the variant.
        let mut qualified = QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            Element::new_item(b"placeholder".to_vec()),
        );
        qualified.op = op;
        let rendered = format!("{:?}", qualified);
        assert!(
            rendered.contains("Replace CountIndexedTree primary+secondary roots"),
            "unexpected debug rendering: {rendered}"
        );
    }

    /// L550: the four non-Merk append-tree leaf ops never require a
    /// cidx secondary mirror, while every count-affecting op does.
    /// This is the exhaustive-match guard against the nested-cidx
    /// stale-secondary bug class.
    #[test]
    fn coverage_non_merk_leaf_ops_do_not_mutate_child_count() {
        let dense = QualifiedGroveDbOp::dense_tree_insert_op(vec![b"d".to_vec()], b"v".to_vec());
        let bulk = QualifiedGroveDbOp::bulk_append_op(vec![b"b".to_vec()], b"v".to_vec());
        let mmr = QualifiedGroveDbOp::mmr_tree_append_op(vec![b"m".to_vec()], b"v".to_vec());
        let commitment = QualifiedGroveDbOp::commitment_tree_insert_op(
            vec![b"c".to_vec()],
            [7u8; 32],
            [8u8; 32],
            [9u8; 32],
            b"payload".to_vec(),
        );
        for op in [&dense.op, &bulk.op, &mmr.op, &commitment.op] {
            assert!(
                !op.can_mutate_child_count(),
                "non-Merk leaf op must not request a cidx mirror: {op:?}"
            );
        }

        // Discriminating counterpart: count-affecting ops do request it.
        assert!(GroveOp::Delete.can_mutate_child_count());
        assert!(GroveOp::InsertOrReplace {
            element: Element::new_item(b"v".to_vec())
        }
        .can_mutate_child_count());
        assert!(GroveOp::ReplaceAggregateIndexedTreeRootKeys {
            primary_hash: [0u8; 32],
            primary_root_key: None,
            primary_aggregate_data: AggregateData::ProvableCount(0),
            axes: vec![(0u8, [0u8; 32], None)],
        }
        .can_mutate_child_count());
    }

    // =================================================================
    // Empty-creation validation: PCIT (batch/mod.rs L2487-2495)
    // =================================================================

    /// L2488-2494: a `ProvableCountIndexedTree` that already claims
    /// root keys / a non-zero count cannot be created through the
    /// batch path.
    #[test]
    fn coverage_batch_insert_non_empty_pcit_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let non_empty = Element::new_provable_count_indexed_tree_with_root_keys_and_count_value(
            Some(b"primary".to_vec()),
            Some(b"secondary".to_vec()),
            9,
            None,
        );
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"pcit_bad".to_vec(),
            non_empty,
        )];
        match db.apply_batch(ops, None, None, grove_version).unwrap() {
            Err(Error::InvalidBatchOperation(msg)) => {
                assert!(
                    msg.contains("CountIndexedTree must be empty at the moment of batch insertion"),
                    "expected PCIT empty-validation msg, got: {msg}"
                );
            }
            other => panic!("expected InvalidBatchOperation, got {other:?}"),
        }
        // Nothing was written.
        assert!(db
            .get([TEST_LEAF].as_ref(), b"pcit_bad", None, grove_version)
            .unwrap()
            .is_err());
    }

    /// L2500-2506: `InsertIfNotExists` on a PCIT runs the
    /// existence probe; at a fresh key the probe says "absent" and
    /// the empty primary is created.
    #[test]
    fn coverage_batch_insert_if_not_exists_pcit_fresh_key_creates() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let before = db.root_hash(None, grove_version).unwrap().expect("root");
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_if_not_exists_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert-if-not-exists on a fresh key must create the PCIT");
        let elem = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("PCIT present");
        assert!(matches!(
            elem,
            Element::ProvableCountIndexedTree(None, None, 0, None)
        ));
        let after = db.root_hash(None, grove_version).unwrap().expect("root");
        assert_ne!(before, after, "creating the PCIT must move the root hash");
        assert_verify_passes(&db, grove_version);
    }

    /// L2509-2517: `InsertIfNotExists` with `error_if_exists` on an
    /// existing PCIT key returns the PCIT-specific duplicate error.
    #[test]
    fn coverage_batch_insert_if_not_exists_pcit_existing_key_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        let before = db.root_hash(None, grove_version).unwrap().expect("root");
        match db
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_if_not_exists_op(
                    vec![TEST_LEAF.to_vec()],
                    b"cidx".to_vec(),
                    Element::empty_provable_count_indexed_tree(),
                )],
                None,
                None,
                grove_version,
            )
            .unwrap()
        {
            Err(Error::InvalidBatchOperation(msg)) => assert_eq!(
                msg,
                "attempting to insert CountIndexedTree element that already exists"
            ),
            other => panic!("expected InvalidBatchOperation, got {other:?}"),
        }
        let after = db.root_hash(None, grove_version).unwrap().expect("root");
        assert_eq!(before, after, "rejected batch must not change the root");
        assert_verify_passes(&db, grove_version);
    }

    /// L2519: the `or_skip` flavour silently skips the existing PCIT
    /// key (no error, no state change).
    #[test]
    fn coverage_batch_insert_if_not_exists_or_skip_pcit_existing_key_is_noop() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"row",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        let before = db.root_hash(None, grove_version).unwrap().expect("root");

        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("skip flavour must not error");

        // The populated PCIT survived: an overwrite by the empty
        // element would have reset the count to 0.
        let elem = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("PCIT present");
        match elem {
            Element::ProvableCountIndexedTree(_, _, count, _) => assert_eq!(count, 1),
            other => panic!("expected PCIT, got {other:?}"),
        }
        let after = db.root_hash(None, grove_version).unwrap().expect("root");
        assert_eq!(before, after, "skipped insert must not change the root");
        assert_verify_passes(&db, grove_version);
    }

    // =================================================================
    // Empty-creation validation: PSIT (batch/mod.rs L2559-2583)
    // =================================================================

    /// L2562-2570: existence probe on a PSIT `InsertIfNotExists` at a
    /// fresh key.
    #[test]
    fn coverage_batch_insert_if_not_exists_psit_fresh_key_creates() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let before = db.root_hash(None, grove_version).unwrap().expect("root");
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_if_not_exists_op(
                vec![TEST_LEAF.to_vec()],
                b"psit".to_vec(),
                Element::empty_provable_sum_indexed_tree(),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert-if-not-exists on a fresh key must create the PSIT");
        let elem = db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap()
            .expect("PSIT present");
        assert!(matches!(
            elem,
            Element::ProvableSumIndexedTree(None, None, 0, None)
        ));
        assert_ne!(
            before,
            db.root_hash(None, grove_version).unwrap().expect("root"),
            "creating the PSIT must move the root hash"
        );
        assert_verify_passes(&db, grove_version);
    }

    /// L2571-2579: duplicate PSIT key with `error_if_exists`.
    #[test]
    fn coverage_batch_insert_if_not_exists_psit_existing_key_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        let before = db.root_hash(None, grove_version).unwrap().expect("root");
        match db
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_if_not_exists_op(
                    vec![TEST_LEAF.to_vec()],
                    b"psit".to_vec(),
                    Element::empty_provable_sum_indexed_tree(),
                )],
                None,
                None,
                grove_version,
            )
            .unwrap()
        {
            Err(Error::InvalidBatchOperation(msg)) => assert_eq!(
                msg,
                "attempting to insert ProvableSumIndexedTree element that already exists"
            ),
            other => panic!("expected InvalidBatchOperation, got {other:?}"),
        }
        assert_eq!(
            before,
            db.root_hash(None, grove_version).unwrap().expect("root"),
            "rejected batch must not change the root"
        );
        assert_verify_passes(&db, grove_version);
    }

    /// L2581: `or_skip` on an existing PSIT key is a silent no-op and
    /// leaves the populated sum in place.
    #[test]
    fn coverage_batch_insert_if_not_exists_or_skip_psit_existing_key_is_noop() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"a",
            Element::new_sum_item(17),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        let before = db.root_hash(None, grove_version).unwrap().expect("root");

        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
                vec![TEST_LEAF.to_vec()],
                b"psit".to_vec(),
                Element::empty_provable_sum_indexed_tree(),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("skip flavour must not error");

        let elem = db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap()
            .expect("PSIT present");
        match elem {
            Element::ProvableSumIndexedTree(_, _, sum, _) => assert_eq!(sum, 17),
            other => panic!("expected PSIT, got {other:?}"),
        }
        assert_eq!(
            before,
            db.root_hash(None, grove_version).unwrap().expect("root"),
            "skipped insert must not change the root"
        );
        assert_verify_passes(&db, grove_version);
    }

    // =================================================================
    // Empty-creation validation: PCPSIT (batch/mod.rs L2649-2674)
    // =================================================================

    /// L2652-2660: existence probe on a PCPSIT `InsertIfNotExists` at
    /// a fresh key (runs after the canonical-axes validation).
    #[test]
    fn coverage_batch_insert_if_not_exists_pcpsit_fresh_key_creates() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let before = db.root_hash(None, grove_version).unwrap().expect("root");
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_if_not_exists_op(
                vec![TEST_LEAF.to_vec()],
                b"pcpsit".to_vec(),
                empty_pcpsit(),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert-if-not-exists on a fresh key must create the PCPSIT");
        let elem = db
            .get([TEST_LEAF].as_ref(), b"pcpsit", None, grove_version)
            .unwrap()
            .expect("PCPSIT present");
        match elem {
            Element::ProvableCountProvableSumIndexedTree(primary, count, sum, axes, _) => {
                assert!(primary.is_none());
                assert_eq!((count, sum), (0, 0));
                assert_eq!(axes, pcpsit_axes());
            }
            other => panic!("expected PCPSIT, got {other:?}"),
        }
        assert_ne!(
            before,
            db.root_hash(None, grove_version).unwrap().expect("root"),
            "creating the PCPSIT must move the root hash"
        );
        assert_verify_passes(&db, grove_version);
    }

    /// L2661-2669: duplicate PCPSIT key with `error_if_exists`.
    #[test]
    fn coverage_batch_insert_if_not_exists_pcpsit_existing_key_errors() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            empty_pcpsit(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        let before = db.root_hash(None, grove_version).unwrap().expect("root");
        match db
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_if_not_exists_op(
                    vec![TEST_LEAF.to_vec()],
                    b"pcpsit".to_vec(),
                    empty_pcpsit(),
                )],
                None,
                None,
                grove_version,
            )
            .unwrap()
        {
            Err(Error::InvalidBatchOperation(msg)) => assert_eq!(
                msg,
                "attempting to insert ProvableCountProvableSumIndexedTree element that already \
                 exists"
            ),
            other => panic!("expected InvalidBatchOperation, got {other:?}"),
        }
        assert_eq!(
            before,
            db.root_hash(None, grove_version).unwrap().expect("root"),
            "rejected batch must not change the root"
        );
        assert_verify_passes(&db, grove_version);
    }

    /// L2672: `or_skip` on an existing PCPSIT key is a silent no-op
    /// and leaves the populated count/sum in place.
    #[test]
    fn coverage_batch_insert_if_not_exists_or_skip_pcpsit_existing_key_is_noop() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            empty_pcpsit(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"a",
            Element::new_item_with_sum_item(b"a".to_vec(), 25),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        let before = db.root_hash(None, grove_version).unwrap().expect("root");

        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_if_not_exists_or_skip_op(
                vec![TEST_LEAF.to_vec()],
                b"pcpsit".to_vec(),
                empty_pcpsit(),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("skip flavour must not error");

        let elem = db
            .get([TEST_LEAF].as_ref(), b"pcpsit", None, grove_version)
            .unwrap()
            .expect("PCPSIT present");
        match elem {
            Element::ProvableCountProvableSumIndexedTree(_, count, sum, _, _) => {
                assert_eq!((count, sum), (1, 25))
            }
            other => panic!("expected PCPSIT, got {other:?}"),
        }
        assert_eq!(
            before,
            db.root_hash(None, grove_version).unwrap().expect("root"),
            "skipped insert must not change the root"
        );
        assert_verify_passes(&db, grove_version);
    }

    // =================================================================
    // Bubble-up: fresh indexed tree populated in the same batch
    // (batch/mod.rs L3961-3981)
    // =================================================================

    /// Creating a PSIT and writing into it in the same batch is
    /// supported: the bubble-up emits
    /// `InsertAggregateIndexedTreeRootKeys` carrying the in-batch
    /// element, and the sum index reflects the rows immediately.
    #[test]
    fn coverage_batch_fresh_psit_populated_in_same_batch_succeeds() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"psit".to_vec(),
                Element::empty_provable_sum_indexed_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"psit".to_vec()],
                b"a".to_vec(),
                Element::new_sum_item(5),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("fresh PSIT create + populate in one batch is supported");
        assert_eq!(
            db.indexed_sum_top_k([TEST_LEAF, b"psit"].as_ref(), 5, true, None, grove_version)
                .unwrap()
                .expect("sum top_k")
                .key_pairs(),
            vec![(5i64, b"a".to_vec())],
            "the sum index must reflect the row inserted alongside the creation"
        );
        assert_verify_passes(&db, grove_version);
    }

    /// PCPSIT flavour of the same one-batch create + populate.
    #[test]
    fn coverage_batch_fresh_pcpsit_populated_in_same_batch_succeeds() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"pcpsit".to_vec(),
                empty_pcpsit(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"pcpsit".to_vec()],
                b"a".to_vec(),
                Element::new_item_with_sum_item(b"a".to_vec(), 5),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("fresh PCPSIT create + populate in one batch is supported");
        assert_eq!(
            db.indexed_sum_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                5,
                true,
                None,
                grove_version
            )
            .unwrap()
            .expect("sum top_k")
            .key_pairs(),
            vec![(5i64, b"a".to_vec())],
            "the sum index must reflect the row inserted alongside the creation"
        );
        assert_verify_passes(&db, grove_version);
    }

    // =================================================================
    // Bubble-up: cidx state into a parent level that exists but has no
    // entry for the cidx's own parent path (batch/mod.rs L4038-4047)
    // =================================================================

    /// L4041-4047: the level above the cidx primary already exists in
    /// `ops_by_level_paths` (because an unrelated path of the same
    /// depth is in the batch) but has no map for the cidx's parent
    /// path yet — the cidx secondary state must still be carried up
    /// via `ReplaceAggregateIndexedTreeRootKeys`, not the plain
    /// `ReplaceTreeRootKey`.
    #[test]
    fn coverage_batch_cidx_bubble_up_with_sibling_level_entry() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");

        // Level 2 op inside the cidx primary + level 1 op on a
        // completely different root leaf. The level-1 map therefore
        // exists when the cidx bubbles up, but does not contain
        // [TEST_LEAF].
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"a".to_vec(),
                Element::new_item(b"va".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"b".to_vec(),
                Element::new_item(b"vb".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![ANOTHER_TEST_LEAF.to_vec()],
                b"other".to_vec(),
                Element::new_item(b"o".to_vec()),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("mixed-level batch");

        // The cidx count propagated through the aggregate-indexed op.
        let parent = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get cidx");
        match parent {
            Element::ProvableCountIndexedTree(_, secondary_root_key, count, _) => {
                assert_eq!(count, 2);
                assert!(
                    secondary_root_key.is_some(),
                    "the secondary root key must have been carried up"
                );
            }
            other => panic!("expected PCIT, got {other:?}"),
        }
        // The secondary index really was mirrored.
        let top = db
            .indexed_count_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top_k");
        assert_eq!(top.len(), 2);
        // The unrelated sibling op landed too.
        assert_eq!(
            db.get([ANOTHER_TEST_LEAF].as_ref(), b"other", None, grove_version)
                .unwrap()
                .expect("sibling item"),
            Element::new_item(b"o".to_vec())
        );
        assert_verify_passes(&db, grove_version);
    }

    // =================================================================
    // DeleteTree with DontCheckWithNoCleanup on indexed primaries
    // (batch/mod.rs L4794-4796 full batch, L5308-5310 partial batch)
    // =================================================================

    /// L4795: `DontCheckWithNoCleanup` skips the emptiness check and
    /// the primary cleanup, but an indexed primary must still be
    /// queued for the per-axis secondary sweep.
    #[test]
    fn coverage_batch_delete_tree_no_cleanup_sweeps_pcit_secondary() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        for k in [b"a".as_slice(), b"b"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate");
        }
        assert_eq!(
            db.indexed_count_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
                .unwrap()
                .expect("top_k before")
                .len(),
            2
        );

        db.apply_batch(
            vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                TreeType::ProvableCountIndexedTree,
                SubelementsDeletionBehavior::DontCheckWithNoCleanup,
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("batch delete with DontCheckWithNoCleanup");

        assert!(
            db.get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
                .unwrap()
                .is_err(),
            "the PCIT element must be gone"
        );

        // Re-create at the same key: the secondary namespace must have
        // been swept, otherwise the two stale entries resurface.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("re-create");
        let after = db
            .indexed_count_top_k([TEST_LEAF, b"cidx"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top_k after");
        assert!(
            after.is_empty(),
            "stale count-secondary entries survived the DontCheckWithNoCleanup delete: {after:?}"
        );
    }

    /// L4795 for a PCPSIT: the sweep is per-axis and unconditional, so
    /// both the count and the sum secondaries are cleared.
    #[test]
    fn coverage_batch_delete_tree_no_cleanup_sweeps_pcpsit_axes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            empty_pcpsit(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        for (k, s) in [(b"a".as_ref(), 10i64), (b"b", 20)] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(k.to_vec(), s),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate");
        }

        db.apply_batch(
            vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"pcpsit".to_vec(),
                TreeType::ProvableCountProvableSumIndexedTree,
                SubelementsDeletionBehavior::DontCheckWithNoCleanup,
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("batch delete with DontCheckWithNoCleanup");

        assert!(db
            .get([TEST_LEAF].as_ref(), b"pcpsit", None, grove_version)
            .unwrap()
            .is_err());

        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            empty_pcpsit(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("re-create");
        let count_after = db
            .indexed_count_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                10,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("count top_k after");
        let sum_after = db
            .indexed_sum_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                10,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("sum top_k after");
        assert!(
            count_after.is_empty() && sum_after.is_empty(),
            "stale axis secondaries survived: count={count_after:?} sum={sum_after:?}"
        );
    }

    /// L5308-5310: the same `DontCheckWithNoCleanup` queuing in the
    /// **partial** batch entry point, whose secondary sweep runs after
    /// `continue_partial_apply_body`.
    #[test]
    fn coverage_partial_batch_delete_tree_no_cleanup_sweeps_psit_secondary() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        for (k, s) in [(b"a".as_ref(), 10i64), (b"b", 20)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                k,
                Element::new_sum_item(s),
                None,
                grove_version,
            )
            .unwrap()
            .expect("populate");
        }
        assert_eq!(
            db.indexed_sum_top_k([TEST_LEAF, b"psit"].as_ref(), 10, true, None, grove_version)
                .unwrap()
                .expect("top_k before")
                .len(),
            2
        );

        db.apply_partial_batch(
            vec![QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"psit".to_vec(),
                TreeType::ProvableSumIndexedTree,
                SubelementsDeletionBehavior::DontCheckWithNoCleanup,
            )],
            None,
            |_cost, _ops_by_level| Ok(vec![]),
            None,
            grove_version,
        )
        .unwrap()
        .expect("partial batch delete with DontCheckWithNoCleanup");

        assert!(db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap()
            .is_err());

        db.insert(
            [TEST_LEAF].as_ref(),
            b"psit",
            Element::empty_provable_sum_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("re-create");
        let after = db
            .indexed_sum_top_k([TEST_LEAF, b"psit"].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top_k after");
        assert!(
            after.is_empty(),
            "stale sum-secondary entries survived the partial-batch delete: {after:?}"
        );
    }

    // =================================================================
    // Partial batch: cidx secondary opened during the *continue* phase
    // (batch/mod.rs L5570-5580)
    // =================================================================

    /// L5570-5580: the secondary-merk opener handed to
    /// `continue_partial_apply_body`. It only runs when a cidx
    /// primary level is executed *after* the pause, so the batch
    /// pauses at depth 2 while the cidx primary sits at depth 1 (a
    /// PCIT directly under the root).
    #[test]
    fn coverage_partial_batch_continue_phase_opens_cidx_secondary() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Root-level PCIT: its items live at path depth 1, which is
        // below the pause height and therefore executes in the
        // continue phase.
        db.insert(
            EMPTY_PATH,
            b"rcidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create root cidx");
        // A depth-2 target so the batch has a level above the pause.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"sub",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create sub tree");

        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"rcidx".to_vec()],
                b"a".to_vec(),
                Element::new_item(b"va".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![b"rcidx".to_vec()],
                b"b".to_vec(),
                Element::new_item(b"vb".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"sub".to_vec()],
                b"x".to_vec(),
                Element::new_item(b"vx".to_vec()),
            ),
        ];
        let options = BatchApplyOptions {
            batch_pause_height: Some(2),
            ..Default::default()
        };
        // Proof that the cidx level really is deferred to the continue
        // phase: at the pause point its ops are still sitting in the
        // leftover map, unexecuted.
        let cidx_level_still_pending = std::cell::Cell::new(false);
        db.apply_partial_batch(
            ops,
            Some(options),
            |_cost, ops_by_level| {
                let leftover = ops_by_level
                    .as_ref()
                    .expect("the batch must have paused before the root level");
                let pending_at_level_1 = leftover
                    .get(1u32)
                    .map(|ops_by_path| {
                        ops_by_path
                            .keys()
                            .any(|path| path.to_path() == vec![b"rcidx".to_vec()])
                    })
                    .unwrap_or(false);
                cidx_level_still_pending.set(pending_at_level_1);
                Ok(vec![])
            },
            None,
            grove_version,
        )
        .unwrap()
        .expect("partial batch paused above the cidx level");
        assert!(
            cidx_level_still_pending.get(),
            "the cidx primary's ops must still be pending at the pause point, otherwise the \
             continue-phase secondary opener is never used"
        );

        let parent = db
            .get(EMPTY_PATH, b"rcidx", None, grove_version)
            .unwrap()
            .expect("get root cidx");
        match parent {
            Element::ProvableCountIndexedTree(_, secondary_root_key, count, _) => {
                assert_eq!(count, 2);
                assert!(
                    secondary_root_key.is_some(),
                    "the continue-phase mirror must have produced a secondary root key"
                );
            }
            other => panic!("expected PCIT, got {other:?}"),
        }
        // The secondary was actually mirrored in the continue phase.
        let top = db
            .indexed_count_top_k([b"rcidx".as_ref()].as_ref(), 10, true, None, grove_version)
            .unwrap()
            .expect("top_k");
        assert_eq!(top.len(), 2);
        // The depth-2 write from the first phase also landed.
        assert_eq!(
            db.get([TEST_LEAF, b"sub"].as_ref(), b"x", None, grove_version)
                .unwrap()
                .expect("sub item"),
            Element::new_item(b"vx".to_vec())
        );
        assert_verify_passes(&db, grove_version);
    }
}
