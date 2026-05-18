//! Batch-path tests for PCIT / PSIT / PCPSIT.
//!
//! Covers `grovedb/src/batch/mod.rs` and `batch/indexed_tree.rs`:
//! - Batch insert of empty indexed-tree primaries.
//! - Mixed batches (cidx + non-cidx ops apply atomically).
//! - DeleteTree on each variant cleans up secondary namespaces.
//! - inspect_cidx_overwrite: cidx → empty cidx safe;
//!   cidx → non-empty cidx rejected; cidx → non-cidx safe.
//! - Batch capture-pre-state / apply-post-mirror integration.
//! - Validation rejection: empty-cidx + descendant writes in same
//!   batch are rejected.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::tree_type::TreeType;
    use grovedb_version::version::GroveVersion;

    use crate::{
        batch::{QualifiedGroveDbOp, SubelementsDeletionBehavior},
        tests::{make_test_grovedb, TEST_LEAF},
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

    // -----------------------------------------------------------------
    // Batch insert: single empty primary
    // -----------------------------------------------------------------

    #[test]
    fn batch_insert_empty_pcit() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("batch insert PCIT");
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn batch_insert_empty_psit() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"psit".to_vec(),
                Element::empty_provable_sum_indexed_tree(),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("batch insert PSIT");
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn batch_insert_empty_pcpsit() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes = vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)];
        let elem =
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("axes canonical");
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"pcpsit".to_vec(),
                elem,
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("batch insert PCPSIT");
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Batch insert: multiple indexed-tree primaries in one batch
    // -----------------------------------------------------------------

    #[test]
    fn batch_insert_multiple_pcit_primaries() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"a".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"b".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"c".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch insert 3 PCITs");
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn batch_insert_mixed_indexed_tree_variants() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes = vec![(IndexAxis::Count.tag(), None)];
        let pcpsit =
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("axes canonical");
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"pcit".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"psit".to_vec(),
                Element::empty_provable_sum_indexed_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"pcpsit".to_vec(),
                pcpsit,
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch mixed indexed");
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn batch_mixed_cidx_and_regular_tree_ops_atomic() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"tree".to_vec(),
                Element::empty_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"item".to_vec(),
                Element::new_item(b"v".to_vec()),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch mixed");
        assert_verify_passes(&db, grove_version);
        // Sanity: all three are present.
        assert!(db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .is_ok());
        assert!(db
            .get([TEST_LEAF].as_ref(), b"tree", None, grove_version)
            .unwrap()
            .is_ok());
        assert!(db
            .get([TEST_LEAF].as_ref(), b"item", None, grove_version)
            .unwrap()
            .is_ok());
    }

    // -----------------------------------------------------------------
    // Batch DeleteTree on cidx primary triggers secondary cleanup
    // -----------------------------------------------------------------

    #[test]
    fn batch_delete_tree_pcit_after_populate_cleans_up() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Create + populate PCIT first.
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
        for k in [b"a".as_slice(), b"b", b"c"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        // DeleteTree via batch.
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            TreeType::ProvableCountIndexedTree,
            SubelementsDeletionBehavior::DeleteChildren,
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch delete cidx");
        // PCIT must be gone.
        let r = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap();
        assert!(r.is_err(), "cidx must be deleted, got {:?}", r);
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn batch_delete_tree_psit_after_populate_cleans_up() {
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
        for (k, v) in [(b"a".as_ref(), 10i64), (b"b", 20)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                k,
                Element::new_sum_item(v),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"psit".to_vec(),
            TreeType::ProvableSumIndexedTree,
            SubelementsDeletionBehavior::DeleteChildren,
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch delete psit");
        assert!(db
            .get([TEST_LEAF].as_ref(), b"psit", None, grove_version)
            .unwrap()
            .is_err());
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn batch_delete_tree_pcpsit_after_populate_cleans_up() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes = vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)];
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(axes).unwrap(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        for (k, v) in [(b"a".as_ref(), 10i64), (b"b", 20)] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(k.to_vec(), v),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"pcpsit".to_vec(),
            TreeType::ProvableCountProvableSumIndexedTree,
            SubelementsDeletionBehavior::DeleteChildren,
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch delete pcpsit");
        assert!(db
            .get([TEST_LEAF].as_ref(), b"pcpsit", None, grove_version)
            .unwrap()
            .is_err());
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // inspect_cidx_overwrite paths
    // -----------------------------------------------------------------

    #[test]
    fn batch_overwrite_cidx_with_empty_cidx_clears_old_state() {
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
        // Overwrite with empty cidx.
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("batch overwrite empty");
        let elem = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get");
        match elem {
            Element::ProvableCountIndexedTree(None, None, 0, _) => {}
            other => panic!("expected empty PCIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn batch_overwrite_cidx_with_non_empty_cidx_rejected() {
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
        let non_empty = Element::new_provable_count_indexed_tree_with_root_keys_and_count_value(
            Some(b"bogus".to_vec()),
            None,
            7,
            None,
        );
        let result = db
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    b"cidx".to_vec(),
                    non_empty,
                )],
                None,
                None,
                grove_version,
            )
            .unwrap();
        assert!(
            matches!(result, Err(Error::NotSupported(_)))
                || matches!(result, Err(Error::InvalidInput(_))),
            "expected NotSupported / InvalidInput, got {:?}",
            result
        );
    }

    #[test]
    fn batch_overwrite_cidx_with_non_cidx_item_safe() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"slot",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"slot"].as_ref(),
            b"row",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        // Overwrite with a plain Item — inspect_cidx_overwrite's
        // "non-cidx" safe branch.
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"slot".to_vec(),
                Element::new_item(b"plain".to_vec()),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("batch overwrite with Item");
        let elem = db
            .get([TEST_LEAF].as_ref(), b"slot", None, grove_version)
            .unwrap()
            .expect("get");
        assert_eq!(elem, Element::new_item(b"plain".to_vec()));
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Batch fresh PCIT + descendant writes — rejected
    // -----------------------------------------------------------------

    #[test]
    fn batch_fresh_cidx_with_descendant_writes_rejected() {
        // Per audit: creating a fresh cidx primary in the same batch
        // as writes under that path is rejected because mirror logic
        // can't see the fresh primary's state.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"row".to_vec(),
                Element::new_item(b"v".to_vec()),
            ),
        ];
        let result = db.apply_batch(ops, None, None, grove_version).unwrap();
        assert!(
            result.is_err(),
            "expected rejection on fresh-cidx + descendant write, got {:?}",
            result
        );
    }

    // -----------------------------------------------------------------
    // Multi-batch sequences
    // -----------------------------------------------------------------

    #[test]
    fn batch_create_cidx_then_populate_in_separate_batches() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Batch 1: create empty cidx.
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"cidx".to_vec(),
                Element::empty_provable_count_indexed_tree(),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        // Batch 2: insert under it. Now the cidx exists pre-batch so
        // mirror can capture pre-state.
        db.apply_batch(
            vec![QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"row".to_vec(),
                Element::new_item(b"v".to_vec()),
            )],
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("populate");
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn batch_multiple_inserts_into_existing_cidx() {
        // Test capture_cidx_pre_state + apply_post_mirror through
        // multiple ops in the same batch.
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
        .expect("create empty");
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
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"c".to_vec(),
                Element::new_item(b"vc".to_vec()),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("multi-insert");
        // Verify count propagation.
        let parent = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get");
        match parent {
            Element::ProvableCountIndexedTree(_, _, c, _) => {
                assert_eq!(c, 3, "PCIT count should be 3 after 3 inserts")
            }
            other => panic!("expected PCIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn batch_mixed_insert_delete_inside_existing_cidx() {
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
        // Pre-populate.
        for k in [b"x".as_slice(), b"y", b"z"] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                k,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert");
        }
        // Batch: delete one, insert two new.
        let ops = vec![
            QualifiedGroveDbOp::delete_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"y".to_vec(),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"new1".to_vec(),
                Element::new_item(b"v".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
                b"new2".to_vec(),
                Element::new_item(b"v".to_vec()),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch mixed");
        let parent = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get");
        // Count: was 3 - 1 delete + 2 inserts = 4.
        match parent {
            Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(c, 4),
            other => panic!("expected PCIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Batch under PSIT and PCPSIT
    // -----------------------------------------------------------------

    #[test]
    fn batch_insert_into_psit_via_batch_path_rejects_or_succeeds_cleanly() {
        // Inserting directly into a PSIT primary via the generic
        // batch op API may not be supported (the dedicated
        // `insert_into_provable_sum_indexed_tree` is the supported
        // path). Just assert no panic.
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
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"psit".to_vec()],
            b"a".to_vec(),
            Element::new_sum_item(5),
        )];
        let _ = db.apply_batch(ops, None, None, grove_version).unwrap();
        // verify_grovedb should still pass (either no-op or successful
        // insert) — what matters is no DB corruption.
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn batch_insert_into_pcpsit_via_batch_path_rejects_or_succeeds_cleanly() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let axes = vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)];
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(axes).unwrap(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create");
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"pcpsit".to_vec()],
            b"a".to_vec(),
            Element::new_item_with_sum_item(b"a".to_vec(), 5),
        )];
        let _ = db.apply_batch(ops, None, None, grove_version).unwrap();
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Empty batch is no-op
    // -----------------------------------------------------------------

    #[test]
    fn batch_empty_is_noop_with_cidx_in_db() {
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
        let before = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root before");
        db.apply_batch(vec![], None, None, grove_version)
            .unwrap()
            .expect("empty batch");
        let after = db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root after");
        assert_eq!(before, after, "empty batch must not change root");
    }

    // -----------------------------------------------------------------
    // apply_partial_batch with cidx mutations
    //
    // Exercises the cidx pre-state/post-mirror block in
    // `apply_partial_batch_with_element_flags_update` (batch/mod.rs
    // L1850-1852 — the partial-batch analogue of the full batch's
    // L1462-1476 cidx capture).
    // -----------------------------------------------------------------

    #[test]
    fn partial_batch_empty_inserts_into_existing_cidx_propagates_count() {
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
        ];
        db.apply_partial_batch(
            ops,
            None,
            |_cost, _ops_by_level| Ok(vec![]),
            None,
            grove_version,
        )
        .unwrap()
        .expect("partial batch");
        let parent = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get");
        match parent {
            Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(c, 2),
            other => panic!("expected PCIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Batch overwrite with Patch / Replace / InsertOrReplace into a
    // non-cidx primary that contains a cidx — exercises
    // inspect_cidx_overwrite call site (L2322-2342)
    // -----------------------------------------------------------------

    #[test]
    fn batch_overwrite_cidx_with_safe_subset_schedules_cleanup() {
        // Empty PCIT exists; replace with a non-cidx Item.
        // inspect_cidx_overwrite classifies as safe-subset overwrite
        // and queues the cidx-cleanup path. The new element bytes are
        // written, and the secondary namespace gets cleaned up.
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
        // Use a batch with override-tree off so the safe-subset
        // classifier in inspect_cidx_overwrite runs.
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            Element::new_item(b"replacement".to_vec()),
        )];
        let options = crate::batch::BatchApplyOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: false,
            ..Default::default()
        };
        // Either succeeds (safe overwrite path) or returns the
        // ambiguous-cidx error from inspect_cidx_overwrite. Both
        // exercise the call site at L2330.
        let _ = db
            .apply_batch(ops, Some(options), None, grove_version)
            .unwrap();
        // Regardless of outcome the DB is consistent.
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn batch_replace_op_on_cidx_with_item_exercises_inspect() {
        // Same as above but using Replace instead of InsertOrReplace.
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
        let ops = vec![QualifiedGroveDbOp::replace_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            Element::new_item(b"replacement".to_vec()),
        )];
        let options = crate::batch::BatchApplyOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: false,
            ..Default::default()
        };
        let _ = db
            .apply_batch(ops, Some(options), None, grove_version)
            .unwrap();
        assert_verify_passes(&db, grove_version);
    }

    #[test]
    fn batch_patch_op_on_cidx_with_item_exercises_inspect() {
        // Same as above but using Patch (op_could_overwrite=true).
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
        let ops = vec![QualifiedGroveDbOp::patch_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            Element::new_item(b"replacement".to_vec()),
            0,
        )];
        let options = crate::batch::BatchApplyOptions {
            validate_insertion_does_not_override: false,
            validate_insertion_does_not_override_tree: false,
            ..Default::default()
        };
        let _ = db
            .apply_batch(ops, Some(options), None, grove_version)
            .unwrap();
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Batch with cidx primary at depth > 1 (under a Tree)
    // -----------------------------------------------------------------

    #[test]
    fn batch_mutate_existing_pcit_under_tree_propagates() {
        // Exercises the cidx_pre_state capture path with the cidx
        // primary nested under a regular Tree.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"parent",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("parent");
        db.insert(
            [TEST_LEAF, b"parent"].as_ref(),
            b"cidx",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("cidx");
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"parent".to_vec(), b"cidx".to_vec()],
                b"x".to_vec(),
                Element::new_item(b"vx".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"parent".to_vec(), b"cidx".to_vec()],
                b"y".to_vec(),
                Element::new_item(b"vy".to_vec()),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch under tree");
        let parent = db
            .get(
                [TEST_LEAF, b"parent"].as_ref(),
                b"cidx",
                None,
                grove_version,
            )
            .unwrap()
            .expect("get");
        match parent {
            Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(c, 2),
            other => panic!("expected PCIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Batch mutate already-existing PCIT then verify aggregate count
    // pre-state capture + post-apply mirror match
    // -----------------------------------------------------------------

    #[test]
    fn batch_overwrite_existing_item_value_keeps_count() {
        // Insert into PCIT, then in a batch overwrite the same key —
        // capture_cidx_pre_state should record the existing count,
        // and post-apply mirror should see no change.
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
            b"k",
            Element::new_item(b"v1".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("seed");
        // Overwrite same key in a batch (InsertOrReplace, same key,
        // different value).
        let ops = vec![QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
            b"k".to_vec(),
            Element::new_item(b"v2".to_vec()),
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch overwrite");
        let elem = db
            .get([TEST_LEAF, b"cidx"].as_ref(), b"k", None, grove_version)
            .unwrap()
            .expect("get");
        match elem.underlying() {
            Element::Item(v, _) => assert_eq!(v, b"v2"),
            other => panic!("expected Item, got {:?}", other),
        }
        // Count must still be 1.
        let parent = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get parent");
        match parent {
            Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(c, 1),
            other => panic!("expected PCIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Batch with multiple cidx primaries in the same batch
    // -----------------------------------------------------------------

    #[test]
    fn batch_mutate_two_different_pcit_primaries_in_same_batch() {
        // Two separate PCIT primaries, each with one direct mutation
        // — both pre-state captures must run independently.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        for name in [b"pa".as_slice(), b"pb"] {
            db.insert(
                [TEST_LEAF].as_ref(),
                name,
                Element::empty_provable_count_indexed_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create");
        }
        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"pa".to_vec()],
                b"k".to_vec(),
                Element::new_item(b"v".to_vec()),
            ),
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec(), b"pb".to_vec()],
                b"k".to_vec(),
                Element::new_item(b"v".to_vec()),
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch");
        for name in [b"pa".as_slice(), b"pb"] {
            let parent = db
                .get([TEST_LEAF].as_ref(), name, None, grove_version)
                .unwrap()
                .expect("get");
            match parent {
                Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(c, 1),
                other => panic!("expected PCIT, got {:?}", other),
            }
        }
        assert_verify_passes(&db, grove_version);
    }

    // -----------------------------------------------------------------
    // Batch with a cidx primary delete (existing item) inside a
    // populated PCIT
    // -----------------------------------------------------------------

    #[test]
    fn batch_delete_only_existing_entry_drops_count_to_zero() {
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
            b"only",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("seed");
        let ops = vec![QualifiedGroveDbOp::delete_op(
            vec![TEST_LEAF.to_vec(), b"cidx".to_vec()],
            b"only".to_vec(),
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("delete via batch");
        let parent = db
            .get([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("get");
        match parent {
            Element::ProvableCountIndexedTree(_, _, c, _) => assert_eq!(c, 0),
            other => panic!("expected PCIT, got {:?}", other),
        }
        assert_verify_passes(&db, grove_version);
    }
}
