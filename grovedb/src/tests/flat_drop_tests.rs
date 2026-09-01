//! Tests for the flat-subtree drop primitive (issue #848):
//! `GroveDb::drop_flat_subtree`, the `SubelementsDeletionBehavior::DropFlat`
//! batch behavior, and `GroveDb::flush_pending_prefix_drops`.
//!
//! The invariants under test:
//! - the drop is O(1) in the subtree's contents (identical cost regardless
//!   of entry count) and never opens the child subtree;
//! - the grove is immediately consistent after the drop (element absent,
//!   `verify_grovedb` clean) while reclamation is still pending;
//! - reclamation empties every doomed namespace — the primary prefix and,
//!   for indexed primaries, all three axis-secondary prefixes;
//! - the redo record commits and rolls back atomically with the drop's
//!   transaction, and draining is deferred until the caller commits;
//! - draining is idempotent, resumable after a simulated crash, and
//!   refuses to tombstone a re-created (live) path — leaking instead of
//!   destroying;
//! - both entry points fail closed before `GROVE_V4`;
//! - absence of the dropped element is provable against the post-drop root
//!   hash, and references into the dropped subtree dangle with the typed
//!   corrupted-reference error rather than resolving to stale data.

mod tests {
    use grovedb_costs::OperationCost;
    use grovedb_merk::tree_type::TreeType;
    use grovedb_storage::{
        rocksdb_storage::{pending_prefix_drops_namespace, RocksDbStorage},
        RawIterator, Storage, StorageContext,
    };
    use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

    use crate::{
        batch::{QualifiedGroveDbOp, SubelementsDeletionBehavior},
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, Error,
    };

    /// Number of keys physically present in the data namespace under
    /// `prefix` (committed state).
    fn data_namespace_key_count(db: &TempGroveDb, prefix: [u8; 32]) -> usize {
        let tx = db.db.start_transaction();
        let ctx = db
            .db
            .get_transactional_storage_context_by_subtree_prefix(prefix, None, &tx)
            .unwrap();
        let mut iter = ctx.raw_iter();
        iter.seek_to_first().unwrap();
        let mut count = 0;
        while iter.valid().unwrap() {
            count += 1;
            iter.next().unwrap();
        }
        count
    }

    /// Whether a pending-prefix-drop redo record exists for `prefix`
    /// (committed state).
    fn record_exists(db: &TempGroveDb, prefix: [u8; 32]) -> bool {
        let tx = db.db.start_transaction();
        let ctx = db
            .db
            .get_transactional_storage_context_by_subtree_prefix(
                *pending_prefix_drops_namespace(),
                None,
                &tx,
            )
            .unwrap();
        ctx.get_meta(prefix)
            .unwrap()
            .expect("read record")
            .is_some()
    }

    fn prefix_of(path: &[&[u8]]) -> [u8; 32] {
        RocksDbStorage::build_prefix(path.into()).unwrap()
    }

    /// A populated flat tree at `[TEST_LEAF]/key` with `entries` items.
    fn seed_flat_tree(db: &TempGroveDb, key: &[u8], entries: u32, grove_version: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            key,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert flat tree");
        for i in 0..entries {
            db.insert(
                [TEST_LEAF, key].as_ref(),
                format!("key_{i:08}").as_bytes(),
                Element::new_item(b"value_bytes_0000".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert item");
        }
    }

    fn assert_grove_verifies(db: &TempGroveDb, grove_version: &GroveVersion) {
        let issues = db
            .verify_grovedb(None, true, false, grove_version)
            .expect("verify_grovedb");
        assert!(issues.is_empty(), "verification issues: {issues:?}");
    }

    // ── Standalone operation ────────────────────────────────────────────

    #[test]
    fn drop_flat_subtree_removes_element_and_reclaims_storage() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        seed_flat_tree(&db, b"flat", 50, grove_version);

        let prefix = prefix_of(&[TEST_LEAF, b"flat"]);
        assert!(data_namespace_key_count(&db, prefix) > 0);

        db.drop_flat_subtree([TEST_LEAF].as_ref(), b"flat", None, grove_version)
            .unwrap()
            .expect("drop");

        // The element is provably gone and the grove is consistent.
        assert!(matches!(
            db.get_raw([TEST_LEAF].as_ref().into(), b"flat", None, grove_version)
                .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
        assert_grove_verifies(&db, grove_version);

        // GroveDB owned the transaction, so reclamation already ran: the
        // namespace is physically empty and the redo record is gone.
        assert_eq!(data_namespace_key_count(&db, prefix), 0);
        assert!(!record_exists(&db, prefix));
    }

    #[test]
    fn drop_flat_subtree_cost_is_independent_of_contents() {
        let grove_version = GroveVersion::latest();

        let cost_of_drop = |entries: u32| -> OperationCost {
            let db = make_test_grovedb(grove_version);
            seed_flat_tree(&db, b"flat", entries, grove_version);
            db.drop_flat_subtree([TEST_LEAF].as_ref(), b"flat", None, grove_version)
                .cost_as_result()
                .expect("drop")
        };

        let small = cost_of_drop(5);
        let large = cost_of_drop(500);
        assert_eq!(
            small, large,
            "drop cost must not depend on the subtree's contents"
        );
    }

    #[test]
    fn drop_flat_subtree_rejects_non_tree_elements() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"item",
            Element::new_item(b"value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");

        assert!(matches!(
            db.drop_flat_subtree([TEST_LEAF].as_ref(), b"item", None, grove_version)
                .unwrap(),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn drop_flat_subtree_fails_closed_before_v4() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        seed_flat_tree(&db, b"flat", 3, grove_version);

        assert!(matches!(
            db.drop_flat_subtree([TEST_LEAF].as_ref(), b"flat", None, &GROVE_V3)
                .unwrap(),
            Err(Error::VersionError(_))
        ));
        // Nothing happened.
        assert!(db
            .get_raw([TEST_LEAF].as_ref().into(), b"flat", None, grove_version)
            .unwrap()
            .is_ok());
    }

    // ── Transactions: atomicity and deferred reclamation ────────────────

    #[test]
    fn drop_flat_subtree_with_caller_transaction_defers_reclamation_until_flush() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        seed_flat_tree(&db, b"flat", 20, grove_version);
        let prefix = prefix_of(&[TEST_LEAF, b"flat"]);

        let tx = db.start_transaction();
        db.drop_flat_subtree([TEST_LEAF].as_ref(), b"flat", Some(&tx), grove_version)
            .unwrap()
            .expect("drop in tx");

        // Uncommitted: the record is invisible to the drain, so a flush
        // reclaims nothing and the data is untouched.
        let report = db
            .flush_pending_prefix_drops(grove_version)
            .expect("flush before commit");
        assert_eq!(report.reclaimed_records, 0);
        assert_eq!(report.skipped_live, 0);
        assert!(data_namespace_key_count(&db, prefix) > 0);

        db.commit_transaction(tx).unwrap().expect("commit");

        // Committed but not yet flushed: the grove is consistent, the
        // orphaned bytes still exist, the record is durable.
        assert_grove_verifies(&db, grove_version);
        assert!(data_namespace_key_count(&db, prefix) > 0);
        assert!(record_exists(&db, prefix));

        let report = db
            .flush_pending_prefix_drops(grove_version)
            .expect("flush after commit");
        assert_eq!(report.reclaimed_records, 1);
        assert_eq!(data_namespace_key_count(&db, prefix), 0);
        assert!(!record_exists(&db, prefix));

        // Idempotent: nothing left to do.
        let report = db.flush_pending_prefix_drops(grove_version).expect("flush");
        assert_eq!(report.reclaimed_records, 0);
    }

    #[test]
    fn drop_flat_subtree_rolls_back_with_the_callers_transaction() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        seed_flat_tree(&db, b"flat", 10, grove_version);
        let prefix = prefix_of(&[TEST_LEAF, b"flat"]);

        let tx = db.start_transaction();
        db.drop_flat_subtree([TEST_LEAF].as_ref(), b"flat", Some(&tx), grove_version)
            .unwrap()
            .expect("drop in tx");
        db.rollback_transaction(&tx).expect("rollback");
        drop(tx);

        // The element, its data, and the absence of any record all reflect
        // the rollback.
        assert!(db
            .get_raw([TEST_LEAF].as_ref().into(), b"flat", None, grove_version)
            .unwrap()
            .is_ok());
        assert!(data_namespace_key_count(&db, prefix) > 0);
        assert!(!record_exists(&db, prefix));
        let report = db.flush_pending_prefix_drops(grove_version).expect("flush");
        assert_eq!(report.reclaimed_records, 0);
        assert_grove_verifies(&db, grove_version);
    }

    #[test]
    fn flush_resumes_after_simulated_crash_between_tombstones_and_record_removal() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        seed_flat_tree(&db, b"flat", 10, grove_version);
        let prefix = prefix_of(&[TEST_LEAF, b"flat"]);

        // Commit the drop through a caller transaction so no auto-drain
        // runs and the record persists.
        let tx = db.start_transaction();
        db.drop_flat_subtree([TEST_LEAF].as_ref(), b"flat", Some(&tx), grove_version)
            .unwrap()
            .expect("drop in tx");
        db.commit_transaction(tx).unwrap().expect("commit");

        // Simulate a crash after the tombstones were written but before
        // the record was removed.
        db.db
            .delete_prefix_ranges(&[prefix])
            .expect("manual tombstones");
        assert_eq!(data_namespace_key_count(&db, prefix), 0);
        assert!(record_exists(&db, prefix));

        // The next flush redoes the (idempotent) tombstones and completes.
        let report = db.flush_pending_prefix_drops(grove_version).expect("flush");
        assert_eq!(report.reclaimed_records, 1);
        assert!(!record_exists(&db, prefix));
    }

    #[test]
    fn flush_skips_a_recreated_path_and_leaks_instead_of_destroying() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        seed_flat_tree(&db, b"flat", 10, grove_version);
        let prefix = prefix_of(&[TEST_LEAF, b"flat"]);

        let tx = db.start_transaction();
        db.drop_flat_subtree([TEST_LEAF].as_ref(), b"flat", Some(&tx), grove_version)
            .unwrap()
            .expect("drop in tx");
        db.commit_transaction(tx).unwrap().expect("commit");

        // Contract violation: re-create the dropped path before flushing.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"flat",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("recreate tree");
        db.insert(
            [TEST_LEAF, b"flat"].as_ref(),
            b"fresh",
            Element::new_item(b"fresh_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert fresh item");

        // The drain must refuse to tombstone the live prefix — repeatedly.
        for _ in 0..2 {
            let report = db.flush_pending_prefix_drops(grove_version).expect("flush");
            assert_eq!(report.reclaimed_records, 0);
            assert_eq!(report.skipped_live, 1);
        }
        // Live reads follow Merk links from the new root, so the fresh data
        // is intact — the guard turned the violation into a leak, not
        // destruction.
        assert_eq!(
            db.get_raw(
                [TEST_LEAF, b"flat"].as_ref().into(),
                b"fresh",
                None,
                grove_version
            )
            .unwrap()
            .expect("fresh item survives"),
            Element::new_item(b"fresh_value".to_vec())
        );
        assert!(record_exists(&db, prefix));
        // And this is exactly why re-creating a dropped path before its
        // record drains is a contract violation: the old tree's stale nodes
        // still sit inside the re-derived (identical) prefix, polluting the
        // new tree's namespace — verification-visible, even though the
        // root hash and live reads are unaffected.
        let polluted = db
            .verify_grovedb(None, true, false, grove_version)
            .map(|issues| !issues.is_empty())
            .unwrap_or(true);
        assert!(
            polluted,
            "recreated-path pollution should be visible to verify_grovedb"
        );
    }

    // ── Indexed primaries ───────────────────────────────────────────────

    #[test]
    fn drop_flat_indexed_tree_reclaims_all_axis_secondary_namespaces() {
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
        .expect("insert PCIT");
        for i in 0..10u8 {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                &[i],
                Element::new_item(vec![i; 8]),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert row");
        }

        let primary_prefix = prefix_of(&[TEST_LEAF, b"cidx"]);
        let secondary_prefixes: Vec<[u8; 32]> = (0..3u8)
            .map(|axis_tag| {
                RocksDbStorage::secondary_prefix_for(&primary_prefix, axis_tag).unwrap()
            })
            .collect();
        assert!(data_namespace_key_count(&db, primary_prefix) > 0);
        // The count axis secondary is populated.
        assert!(data_namespace_key_count(&db, secondary_prefixes[0]) > 0);

        db.drop_flat_subtree([TEST_LEAF].as_ref(), b"cidx", None, grove_version)
            .unwrap()
            .expect("drop PCIT");

        assert_grove_verifies(&db, grove_version);
        assert_eq!(data_namespace_key_count(&db, primary_prefix), 0);
        for secondary_prefix in secondary_prefixes {
            assert_eq!(data_namespace_key_count(&db, secondary_prefix), 0);
        }
        assert!(!record_exists(&db, primary_prefix));
    }

    // ── Aggregate parents ───────────────────────────────────────────────

    #[test]
    fn drop_flat_sum_tree_under_sum_tree_parent_keeps_aggregates_consistent() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        db.insert(
            [TEST_LEAF].as_ref(),
            b"sums",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum parent");
        db.insert(
            [TEST_LEAF, b"sums"].as_ref(),
            b"keeper",
            Element::new_sum_item(5),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert keeper sum item");
        db.insert(
            [TEST_LEAF, b"sums"].as_ref(),
            b"flat_sums",
            Element::empty_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert child sum tree");
        for i in 0..4u8 {
            db.insert(
                [TEST_LEAF, b"sums", b"flat_sums"].as_ref(),
                &[i],
                Element::new_sum_item(10),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert child sum item");
        }

        db.drop_flat_subtree(
            [TEST_LEAF, b"sums"].as_ref(),
            b"flat_sums",
            None,
            grove_version,
        )
        .unwrap()
        .expect("drop sum child");

        // verify_grovedb recomputes aggregates recursively; a stale parent
        // sum would surface as an issue.
        assert_grove_verifies(&db, grove_version);
        assert_eq!(
            data_namespace_key_count(&db, prefix_of(&[TEST_LEAF, b"sums", b"flat_sums"])),
            0
        );
    }

    // ── Batch operation ─────────────────────────────────────────────────

    #[test]
    fn batch_drop_flat_applies_atomically_and_reclaims() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        seed_flat_tree(&db, b"flat", 30, grove_version);
        let prefix = prefix_of(&[TEST_LEAF, b"flat"]);

        let ops = vec![
            QualifiedGroveDbOp::insert_or_replace_op(
                vec![TEST_LEAF.to_vec()],
                b"sibling".to_vec(),
                Element::new_item(b"sibling_value".to_vec()),
            ),
            QualifiedGroveDbOp::delete_tree_op(
                vec![TEST_LEAF.to_vec()],
                b"flat".to_vec(),
                TreeType::NormalTree,
                SubelementsDeletionBehavior::DropFlat,
            ),
        ];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("apply batch");

        assert!(matches!(
            db.get_raw([TEST_LEAF].as_ref().into(), b"flat", None, grove_version)
                .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
        assert_eq!(
            db.get_raw([TEST_LEAF].as_ref().into(), b"sibling", None, grove_version)
                .unwrap()
                .expect("sibling applied"),
            Element::new_item(b"sibling_value".to_vec())
        );
        assert_grove_verifies(&db, grove_version);
        // Auto-drain ran (GroveDB owned the transaction).
        assert_eq!(data_namespace_key_count(&db, prefix), 0);
        assert!(!record_exists(&db, prefix));
    }

    #[test]
    fn batch_drop_flat_with_caller_transaction_defers_reclamation() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        seed_flat_tree(&db, b"flat", 15, grove_version);
        let prefix = prefix_of(&[TEST_LEAF, b"flat"]);

        let tx = db.start_transaction();
        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"flat".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DropFlat,
        )];
        db.apply_batch(ops, None, Some(&tx), grove_version)
            .unwrap()
            .expect("apply batch in tx");
        db.commit_transaction(tx).unwrap().expect("commit");

        assert!(data_namespace_key_count(&db, prefix) > 0);
        assert!(record_exists(&db, prefix));
        let report = db.flush_pending_prefix_drops(grove_version).expect("flush");
        assert_eq!(report.reclaimed_records, 1);
        assert_eq!(data_namespace_key_count(&db, prefix), 0);
        assert_grove_verifies(&db, grove_version);
    }

    #[test]
    fn batch_drop_flat_fails_closed_before_v4() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        seed_flat_tree(&db, b"flat", 3, grove_version);

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"flat".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DropFlat,
        )];
        assert!(matches!(
            db.apply_batch(ops, None, None, &GROVE_V3).unwrap(),
            Err(Error::VersionError(_))
        ));
        assert!(db
            .get_raw([TEST_LEAF].as_ref().into(), b"flat", None, grove_version)
            .unwrap()
            .is_ok());
    }

    #[test]
    fn batch_drop_flat_of_indexed_tree_reclaims_secondaries() {
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
        .expect("insert PCIT");
        for i in 0..5u8 {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                &[i],
                Element::new_item(vec![i; 8]),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert row");
        }
        let primary_prefix = prefix_of(&[TEST_LEAF, b"cidx"]);
        let count_secondary = RocksDbStorage::secondary_prefix_for(&primary_prefix, 0).unwrap();
        assert!(data_namespace_key_count(&db, count_secondary) > 0);

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"cidx".to_vec(),
            TreeType::ProvableCountIndexedTree,
            SubelementsDeletionBehavior::DropFlat,
        )];
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("apply batch");

        assert_grove_verifies(&db, grove_version);
        assert_eq!(data_namespace_key_count(&db, primary_prefix), 0);
        assert_eq!(data_namespace_key_count(&db, count_secondary), 0);
    }

    #[test]
    fn apply_operations_without_batching_routes_drop_flat() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        seed_flat_tree(&db, b"flat", 12, grove_version);
        let prefix = prefix_of(&[TEST_LEAF, b"flat"]);

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"flat".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DropFlat,
        )];
        db.apply_operations_without_batching(ops, None, None, grove_version)
            .unwrap()
            .expect("apply without batching");

        assert!(matches!(
            db.get_raw([TEST_LEAF].as_ref().into(), b"flat", None, grove_version)
                .unwrap(),
            Err(Error::PathKeyNotFound(_))
        ));
        assert_grove_verifies(&db, grove_version);
        // The routed drop_flat_subtree owned its transaction, so
        // reclamation already ran.
        assert_eq!(data_namespace_key_count(&db, prefix), 0);
        assert!(!record_exists(&db, prefix));
    }

    // ── Proofs and references ───────────────────────────────────────────

    #[test]
    fn drop_flat_subtree_absence_is_provable() {
        use grovedb_merk::proofs::Query;

        use crate::{GroveDb, PathQuery};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        seed_flat_tree(&db, b"flat", 25, grove_version);

        let mut query = Query::new();
        query.insert_key(b"flat".to_vec());
        let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

        // Sanity: before the drop the same query proves one result.
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove before drop");
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).expect("verify");
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert_eq!(result_set.len(), 1);

        db.drop_flat_subtree([TEST_LEAF].as_ref(), b"flat", None, grove_version)
            .unwrap()
            .expect("drop");

        // The dropped element's absence is provable against the new root
        // hash: an empty result set whose proof verifies.
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove after drop");
        let (hash, result_set) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).expect("verify");
        assert_eq!(hash, db.root_hash(None, grove_version).unwrap().unwrap());
        assert!(
            result_set.is_empty(),
            "dropped key must prove absent, got {result_set:?}"
        );
    }

    #[test]
    fn references_into_a_dropped_subtree_dangle_with_typed_errors() {
        use crate::{reference_path::ReferencePathType, tests::ANOTHER_TEST_LEAF};

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        seed_flat_tree(&db, b"flat", 5, grove_version);

        db.insert(
            [ANOTHER_TEST_LEAF].as_ref(),
            b"ref",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"flat".to_vec(),
                b"key_00000001".to_vec(),
            ])),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert reference");

        // Sanity: the reference resolves before the drop.
        assert_eq!(
            db.get([ANOTHER_TEST_LEAF].as_ref(), b"ref", None, grove_version)
                .unwrap()
                .expect("follow reference before drop"),
            Element::new_item(b"value_bytes_0000".to_vec())
        );

        db.drop_flat_subtree([TEST_LEAF].as_ref(), b"flat", None, grove_version)
            .unwrap()
            .expect("drop");

        // The reference now dangles: following it returns the typed
        // corrupted-reference error (the target's parent layer is gone),
        // never stale data. Managing such references is the caller's
        // responsibility, exactly as with `delete`.
        let followed = db
            .get([ANOTHER_TEST_LEAF].as_ref(), b"ref", None, grove_version)
            .unwrap();
        assert!(
            matches!(
                followed,
                Err(Error::CorruptedReferencePathParentLayerNotFound(_)
                    | Error::CorruptedReferencePathKeyNotFound(_)
                    | Error::CorruptedReferencePathNotFound(_))
            ),
            "expected a typed dangling-reference error, got {followed:?}"
        );
    }

    // ── Estimated costs ─────────────────────────────────────────────────

    /// Both batch estimators must dominate the actual cost of a `DropFlat`
    /// batch in every dimension — the estimate is the admission bound
    /// downstream, so an under-estimate would reject already-affordable
    /// state transitions (or admit under-funded ones).
    #[cfg(feature = "estimated_costs")]
    #[test]
    fn estimates_dominate_actual_for_drop_flat() {
        use std::collections::HashMap;

        use grovedb_costs::storage_cost::removal::StorageRemovedBytes::NoStorageRemoval;
        use grovedb_merk::estimated_costs::{
            average_case_costs::{
                EstimatedLayerCount::EstimatedLevel, EstimatedLayerInformation,
                EstimatedLayerSizes::AllSubtrees, EstimatedSumTrees::NoSumTrees,
            },
            worst_case_costs::WorstCaseLayerInformation::MaxElementsNumber,
        };

        use crate::{
            batch::{
                estimated_costs::EstimatedCostsType::{AverageCaseCostsType, WorstCaseCostsType},
                KeyInfoPath,
            },
            GroveDb,
        };

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        seed_flat_tree(&db, b"flat", 5, grove_version);

        let ops = vec![QualifiedGroveDbOp::delete_tree_op(
            vec![TEST_LEAF.to_vec()],
            b"flat".to_vec(),
            TreeType::NormalTree,
            SubelementsDeletionBehavior::DropFlat,
        )];

        let mut average_paths = HashMap::new();
        average_paths.insert(
            KeyInfoPath(vec![]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: EstimatedLevel(2, false),
                estimated_layer_sizes: AllSubtrees(32, NoSumTrees, None),
            },
        );
        average_paths.insert(
            KeyInfoPath::from_known_owned_path(vec![TEST_LEAF.to_vec()]),
            EstimatedLayerInformation {
                tree_type: TreeType::NormalTree,
                estimated_layer_count: EstimatedLevel(4, false),
                estimated_layer_sizes: AllSubtrees(32, NoSumTrees, None),
            },
        );
        let average = GroveDb::estimated_case_operations_for_batch(
            AverageCaseCostsType(average_paths),
            ops.clone(),
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("average case estimate");

        let mut worst_paths = HashMap::new();
        worst_paths.insert(KeyInfoPath(vec![]), MaxElementsNumber(4));
        worst_paths.insert(
            KeyInfoPath::from_known_owned_path(vec![TEST_LEAF.to_vec()]),
            MaxElementsNumber(4),
        );
        let worst = GroveDb::estimated_case_operations_for_batch(
            WorstCaseCostsType(worst_paths),
            ops.clone(),
            None,
            |_cost, _old_flags, _new_flags| Ok(false),
            |_flags, _removed_key_bytes, _removed_value_bytes| {
                Ok((NoStorageRemoval, NoStorageRemoval))
            },
            grove_version,
        )
        .cost_as_result()
        .expect("worst case estimate");

        let actual = db
            .apply_batch(ops, None, None, grove_version)
            .cost_as_result()
            .expect("apply batch");

        assert!(
            average.worse_or_eq_than(&actual),
            "average-case estimate must dominate actual;\nestimated {average:?}\nactual {actual:?}",
        );
        assert!(
            worst.worse_or_eq_than(&actual),
            "worst-case estimate must dominate actual;\nestimated {worst:?}\nactual {actual:?}",
        );
    }
}
