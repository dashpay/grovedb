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
//! - one record does not hold up the others: a record below an element
//!   that is not a tree is reclaimed, and a record whose liveness check
//!   fails is counted as unresolved, left in place and checked again by
//!   the next drain;
//! - both entry points fail closed before `GROVE_V4`;
//! - absence of the dropped element is provable against the post-drop root
//!   hash, and references into the dropped subtree dangle with the typed
//!   corrupted-reference error rather than resolving to stale data.

mod tests {

    use crate::BackwardsReferences;
    use grovedb_costs::OperationCost;
    use grovedb_merk::tree_type::TreeType;
    use grovedb_path::SubtreePath;
    use grovedb_storage::{
        rocksdb_storage::{pending_prefix_drops_namespace, RocksDbStorage},
        RawIterator, Storage, StorageContext,
    };
    use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

    use crate::{
        batch::{QualifiedGroveDbOp, SubelementsDeletionBehavior},
        tests::{make_test_grovedb, TempGroveDb, ANOTHER_TEST_LEAF, TEST_LEAF},
        Element, Error, PendingPrefixDropsReport,
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
        seed_flat_tree_at(db, &[TEST_LEAF], key, entries, grove_version);
    }

    /// A populated flat tree at `parent`/`key` with `entries` items.
    fn seed_flat_tree_at(
        db: &TempGroveDb,
        parent: &[&[u8]],
        key: &[u8],
        entries: u32,
        grove_version: &GroveVersion,
    ) {
        db.insert(
            parent,
            key,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert flat tree");
        let mut tree_path = parent.to_vec();
        tree_path.push(key);
        for i in 0..entries {
            db.insert(
                tree_path.as_slice(),
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

    /// Flat-drop `parent`/`key` through a caller transaction and commit it,
    /// so no drain runs and the redo record stays pending.
    fn drop_flat_and_leave_pending(
        db: &TempGroveDb,
        parent: &[&[u8]],
        key: &[u8],
        grove_version: &GroveVersion,
    ) {
        let tx = db.start_transaction();
        db.drop_flat_subtree(
            parent,
            key,
            BackwardsReferences::DontCheck,
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("drop in tx");
        db.commit_transaction(tx).unwrap().expect("commit");
    }

    /// `count` keys under `[parent]` whose subtree prefixes sort after
    /// `after`. The drain visits records in prefix order, so the drops of
    /// these keys are visited after the record keyed by `after`.
    fn keys_drained_after(parent: &[u8], after: [u8; 32], count: usize) -> Vec<Vec<u8>> {
        (0u32..)
            .map(|i| format!("flat_{i}").into_bytes())
            .filter(|key| prefix_of(&[parent, key]) > after)
            .take(count)
            .collect()
    }

    /// Write `value` at `key` straight into the data storage of the subtree
    /// at `path`, bypassing Merk.
    fn put_raw(db: &TempGroveDb, path: &[&[u8]], key: &[u8], value: &[u8]) {
        let tx = db.start_transaction();
        let storage = db
            .db
            .get_immediate_storage_context(path.into(), &tx)
            .unwrap();
        storage
            .put(key, value, None, None)
            .unwrap()
            .expect("raw put");
        drop(storage);
        db.commit_transaction(tx).unwrap().expect("commit raw put");
    }

    /// The bytes stored at `key` in the data storage of the subtree at
    /// `path`, read without Merk.
    fn get_raw_bytes(db: &TempGroveDb, path: &[&[u8]], key: &[u8]) -> Vec<u8> {
        let tx = db.start_transaction();
        let storage = db
            .db
            .get_immediate_storage_context(path.into(), &tx)
            .unwrap();
        storage
            .get(key)
            .unwrap()
            .expect("raw get")
            .expect("raw value present")
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

        db.drop_flat_subtree(
            [TEST_LEAF].as_ref(),
            b"flat",
            BackwardsReferences::DontCheck,
            None,
            grove_version,
        )
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
            db.drop_flat_subtree(
                [TEST_LEAF].as_ref(),
                b"flat",
                BackwardsReferences::DontCheck,
                None,
                grove_version,
            )
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
    fn batch_flat_drop_cost_is_independent_of_contents_with_explicit_skip() {
        let version = GroveVersion::latest();
        let cost_of_drop = |entries: u32| {
            let db = make_test_grovedb(version);
            seed_flat_tree(&db, b"flat", entries, version);
            db.apply_batch(
                vec![QualifiedGroveDbOp::delete_tree_op(
                    vec![TEST_LEAF.to_vec()],
                    b"flat".to_vec(),
                    TreeType::NormalTree,
                    SubelementsDeletionBehavior::DropFlat,
                )
                .dont_check_for_backwards_references()],
                None,
                None,
                version,
            )
            .cost_as_result()
            .expect("explicit flat drop")
        };
        assert_eq!(cost_of_drop(5), cost_of_drop(500));
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
            db.drop_flat_subtree(
                [TEST_LEAF].as_ref(),
                b"item",
                BackwardsReferences::DontCheck,
                None,
                grove_version
            )
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
            db.drop_flat_subtree(
                [TEST_LEAF].as_ref(),
                b"flat",
                BackwardsReferences::DontCheck,
                None,
                &GROVE_V3
            )
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
        db.drop_flat_subtree(
            [TEST_LEAF].as_ref(),
            b"flat",
            BackwardsReferences::DontCheck,
            Some(&tx),
            grove_version,
        )
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
        db.drop_flat_subtree(
            [TEST_LEAF].as_ref(),
            b"flat",
            BackwardsReferences::DontCheck,
            Some(&tx),
            grove_version,
        )
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
        db.drop_flat_subtree(
            [TEST_LEAF].as_ref(),
            b"flat",
            BackwardsReferences::DontCheck,
            Some(&tx),
            grove_version,
        )
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
        db.drop_flat_subtree(
            [TEST_LEAF].as_ref(),
            b"flat",
            BackwardsReferences::DontCheck,
            Some(&tx),
            grove_version,
        )
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

    // ── A record the drain cannot resolve ───────────────────────────────

    #[test]
    fn should_reclaim_unrelated_drops_when_an_ancestor_of_a_dropped_path_holds_an_item() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // One flat tree under `[TEST_LEAF]/parent`, and three unrelated ones
        // directly under `[TEST_LEAF]` whose records the drain visits after
        // it.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"parent",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert parent tree");
        seed_flat_tree_at(&db, &[TEST_LEAF, b"parent"], b"nested", 10, grove_version);
        let nested_prefix = prefix_of(&[TEST_LEAF, b"parent", b"nested"]);
        let unrelated_keys = keys_drained_after(TEST_LEAF, nested_prefix, 3);
        for key in &unrelated_keys {
            seed_flat_tree(&db, key, 10, grove_version);
        }

        drop_flat_and_leave_pending(&db, &[TEST_LEAF, b"parent"], b"nested", grove_version);
        for key in &unrelated_keys {
            drop_flat_and_leave_pending(&db, &[TEST_LEAF], key, grove_version);
        }

        // The dropped tree's parent is removed and an item takes its key
        // before the drain runs.
        db.delete([TEST_LEAF].as_ref(), b"parent", None, None, grove_version)
            .unwrap()
            .expect("delete the emptied parent tree");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"parent",
            Element::new_item(b"not a tree".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert an item at the parent's key");

        // Nothing can live below an item, so every record is reclaimed,
        // the one under the item included.
        let report = db.flush_pending_prefix_drops(grove_version).expect("flush");
        assert_eq!(
            report,
            PendingPrefixDropsReport {
                reclaimed_records: 4,
                skipped_live: 0,
                unresolved_records: 0,
                first_unresolved_error: None,
            }
        );
        assert_eq!(data_namespace_key_count(&db, nested_prefix), 0);
        assert!(!record_exists(&db, nested_prefix));
        for key in &unrelated_keys {
            let prefix = prefix_of(&[TEST_LEAF, key]);
            assert_eq!(data_namespace_key_count(&db, prefix), 0);
            assert!(!record_exists(&db, prefix));
        }

        // The item is untouched and the grove is consistent.
        assert_eq!(
            db.get_raw([TEST_LEAF].as_ref().into(), b"parent", None, grove_version)
                .unwrap()
                .expect("item at the parent's key"),
            Element::new_item(b"not a tree".to_vec())
        );
        assert_grove_verifies(&db, grove_version);
    }

    /// `[TEST_LEAF]/outer/inner/nested`, a populated flat tree, dropped
    /// with its record left pending. Returns the prefix of `nested`.
    fn seed_and_drop_nested_two_layers_down(
        db: &TempGroveDb,
        outer: &[u8],
        grove_version: &GroveVersion,
    ) -> [u8; 32] {
        db.insert(
            [TEST_LEAF].as_ref(),
            outer,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert outer tree");
        db.insert(
            [TEST_LEAF, outer].as_ref(),
            b"inner",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert inner tree");
        seed_flat_tree_at(
            db,
            &[TEST_LEAF, outer, b"inner"],
            b"nested",
            10,
            grove_version,
        );
        drop_flat_and_leave_pending(db, &[TEST_LEAF, outer, b"inner"], b"nested", grove_version);
        prefix_of(&[TEST_LEAF, outer, b"inner", b"nested"])
    }

    /// Remove the emptied tree at `parent`/`key` and put an item at its key.
    fn replace_empty_tree_with_item(
        db: &TempGroveDb,
        parent: &[&[u8]],
        key: &[u8],
        grove_version: &GroveVersion,
    ) {
        db.delete(parent, key, None, None, grove_version)
            .unwrap()
            .expect("delete the emptied tree");
        db.insert(
            parent,
            key,
            Element::new_item(b"not a tree".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert an item at the tree's key");
    }

    #[test]
    fn should_reclaim_a_record_when_a_higher_ancestor_of_its_path_holds_an_item() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let nested_prefix = seed_and_drop_nested_two_layers_down(&db, b"outer", grove_version);

        // `inner` and then `outer` are emptied and removed, and an item
        // takes `outer`'s key: the dropped path now runs through an item
        // two layers up.
        db.delete(
            [TEST_LEAF, b"outer"].as_ref(),
            b"inner",
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("delete the emptied inner tree");
        replace_empty_tree_with_item(&db, &[TEST_LEAF], b"outer", grove_version);

        let report = db.flush_pending_prefix_drops(grove_version).expect("flush");
        assert_eq!(
            report,
            PendingPrefixDropsReport {
                reclaimed_records: 1,
                skipped_live: 0,
                unresolved_records: 0,
                first_unresolved_error: None,
            }
        );
        assert_eq!(data_namespace_key_count(&db, nested_prefix), 0);
        assert!(!record_exists(&db, nested_prefix));
        assert_grove_verifies(&db, grove_version);
    }

    #[test]
    fn should_reclaim_a_record_below_an_item_when_a_dropped_ancestor_holds_an_item_too() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let nested_prefix = seed_and_drop_nested_two_layers_down(&db, b"outer", grove_version);

        // An item takes `inner`'s key. Then `outer`, which now holds only
        // that item, is flat-dropped as well and an item takes its key
        // before any drain. `outer`'s storage is not reclaimed (its path is
        // live again), so it still shows `inner` as an item, while `outer`
        // itself can no longer be opened as a tree.
        replace_empty_tree_with_item(&db, &[TEST_LEAF, b"outer"], b"inner", grove_version);
        drop_flat_and_leave_pending(&db, &[TEST_LEAF], b"outer", grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::new_item(b"not a tree".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert an item at outer's key");

        // Read from the root, the dropped path stops at the item at
        // `outer`, so `nested` is reclaimed. `outer`'s own record is skipped
        // as live.
        let report = db.flush_pending_prefix_drops(grove_version).expect("flush");
        assert_eq!(
            report,
            PendingPrefixDropsReport {
                reclaimed_records: 1,
                skipped_live: 1,
                unresolved_records: 0,
                first_unresolved_error: None,
            }
        );
        assert_eq!(data_namespace_key_count(&db, nested_prefix), 0);
        assert!(!record_exists(&db, nested_prefix));
        assert!(record_exists(&db, prefix_of(&[TEST_LEAF, b"outer"])));
        assert_grove_verifies(&db, grove_version);
    }

    #[test]
    fn should_reclaim_a_record_below_an_item_when_a_dropped_ancestor_is_gone() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Pick `outer` so that the drain visits the record of `nested`
        // before the record of `outer`, while `outer`'s storage is still
        // there.
        let outer = (0u32..)
            .map(|i| format!("outer_{i}").into_bytes())
            .find(|outer| {
                prefix_of(&[TEST_LEAF, outer, b"inner", b"nested"]) < prefix_of(&[TEST_LEAF, outer])
            })
            .expect("a key with the wanted drain order");
        let nested_prefix = seed_and_drop_nested_two_layers_down(&db, &outer, grove_version);

        // An item takes `inner`'s key, then `outer` is flat-dropped and
        // nothing takes its place.
        replace_empty_tree_with_item(&db, &[TEST_LEAF, &outer], b"inner", grove_version);
        drop_flat_and_leave_pending(&db, &[TEST_LEAF], &outer, grove_version);

        // `outer` is absent from `[TEST_LEAF]`, so nothing resolves below
        // it and one pass reclaims both records.
        let report = db.flush_pending_prefix_drops(grove_version).expect("flush");
        assert_eq!(
            report,
            PendingPrefixDropsReport {
                reclaimed_records: 2,
                skipped_live: 0,
                unresolved_records: 0,
                first_unresolved_error: None,
            }
        );
        assert_eq!(data_namespace_key_count(&db, nested_prefix), 0);
        assert!(!record_exists(&db, nested_prefix));
        assert!(!record_exists(&db, prefix_of(&[TEST_LEAF, &outer])));
        assert_grove_verifies(&db, grove_version);
    }

    #[test]
    fn should_count_a_record_whose_liveness_check_fails_and_keep_draining() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // `stuck` lives under `[TEST_LEAF]` and is the record the drain
        // visits first; three more under `[ANOTHER_TEST_LEAF]` follow. The
        // item keeps the `[TEST_LEAF]` Merk non-empty after the drop.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"keep",
            Element::new_item(b"kept".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");
        seed_flat_tree(&db, b"stuck", 10, grove_version);
        let stuck_prefix = prefix_of(&[TEST_LEAF, b"stuck"]);
        let later_keys = keys_drained_after(ANOTHER_TEST_LEAF, stuck_prefix, 3);
        for key in &later_keys {
            seed_flat_tree_at(&db, &[ANOTHER_TEST_LEAF], key, 10, grove_version);
        }
        drop_flat_and_leave_pending(&db, &[TEST_LEAF], b"stuck", grove_version);
        for key in &later_keys {
            drop_flat_and_leave_pending(&db, &[ANOTHER_TEST_LEAF], key, grove_version);
        }

        // Simulate a fault in the liveness check of `stuck`: the root node
        // of the `[TEST_LEAF]` Merk no longer decodes, so the read of
        // `[TEST_LEAF]/stuck` fails instead of answering found or not
        // found.
        let Element::Tree(Some(root_key), _) = db
            .get_raw(SubtreePath::empty(), TEST_LEAF, None, grove_version)
            .unwrap()
            .expect("the parent tree's element")
        else {
            panic!("the parent is a non-empty tree");
        };
        let root_node = get_raw_bytes(&db, &[TEST_LEAF], &root_key);
        put_raw(&db, &[TEST_LEAF], &root_key, b"\xffnot a merk node");
        let read = db
            .get_raw([TEST_LEAF].as_ref().into(), b"stuck", None, grove_version)
            .unwrap();
        assert!(
            matches!(
                read,
                Err(ref e) if !matches!(
                    e,
                    Error::PathKeyNotFound(_)
                        | Error::PathNotFound(_)
                        | Error::PathParentLayerNotFound(_)
                        | Error::InvalidParentLayerPath(_)
                        | Error::CorruptedPath(_)
                )
            ),
            "the simulated fault must not read as found or as not found: {read:?}"
        );

        // The faulty record is counted and left alone; the later records
        // are reclaimed.
        let report = db.flush_pending_prefix_drops(grove_version).expect("flush");
        assert_eq!(report.reclaimed_records, 3);
        assert_eq!(report.skipped_live, 0);
        assert_eq!(report.unresolved_records, 1);
        assert!(report.first_unresolved_error.is_some());
        assert!(record_exists(&db, stuck_prefix));
        assert!(data_namespace_key_count(&db, stuck_prefix) > 0);
        for key in &later_keys {
            let prefix = prefix_of(&[ANOTHER_TEST_LEAF, key]);
            assert_eq!(data_namespace_key_count(&db, prefix), 0);
            assert!(!record_exists(&db, prefix));
        }

        // While the fault lasts, every pass checks the record again and
        // reports it again.
        let report = db.flush_pending_prefix_drops(grove_version).expect("flush");
        assert_eq!(report.reclaimed_records, 0);
        assert_eq!(report.unresolved_records, 1);
        assert!(record_exists(&db, stuck_prefix));
        assert!(data_namespace_key_count(&db, stuck_prefix) > 0);

        // Once the fault is gone, the next pass reclaims it.
        put_raw(&db, &[TEST_LEAF], &root_key, &root_node);
        let report = db.flush_pending_prefix_drops(grove_version).expect("flush");
        assert_eq!(
            report,
            PendingPrefixDropsReport {
                reclaimed_records: 1,
                skipped_live: 0,
                unresolved_records: 0,
                first_unresolved_error: None,
            }
        );
        assert!(!record_exists(&db, stuck_prefix));
        assert_eq!(data_namespace_key_count(&db, stuck_prefix), 0);
        assert_grove_verifies(&db, grove_version);
    }

    #[test]
    fn should_skip_a_recreated_path_as_live_while_other_records_drain() {
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
        .expect("insert parent tree");
        seed_flat_tree_at(&db, &[TEST_LEAF, b"parent"], b"nested", 10, grove_version);
        seed_flat_tree(&db, b"recreated", 10, grove_version);
        seed_flat_tree(&db, b"plain", 10, grove_version);
        let nested_prefix = prefix_of(&[TEST_LEAF, b"parent", b"nested"]);
        let recreated_prefix = prefix_of(&[TEST_LEAF, b"recreated"]);
        let plain_prefix = prefix_of(&[TEST_LEAF, b"plain"]);

        drop_flat_and_leave_pending(&db, &[TEST_LEAF, b"parent"], b"nested", grove_version);
        drop_flat_and_leave_pending(&db, &[TEST_LEAF], b"recreated", grove_version);
        drop_flat_and_leave_pending(&db, &[TEST_LEAF], b"plain", grove_version);

        // One dropped tree ends up below an item, another is re-created
        // (a contract violation) before the drain runs.
        db.delete([TEST_LEAF].as_ref(), b"parent", None, None, grove_version)
            .unwrap()
            .expect("delete the emptied parent tree");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"parent",
            Element::new_item(b"not a tree".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert an item at the parent's key");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"recreated",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("recreate tree");
        db.insert(
            [TEST_LEAF, b"recreated"].as_ref(),
            b"fresh",
            Element::new_item(b"fresh_value".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert fresh item");

        // The live path is skipped on every pass; the other two records
        // are reclaimed on the first.
        let report = db.flush_pending_prefix_drops(grove_version).expect("flush");
        assert_eq!(
            report,
            PendingPrefixDropsReport {
                reclaimed_records: 2,
                skipped_live: 1,
                unresolved_records: 0,
                first_unresolved_error: None,
            }
        );
        let report = db.flush_pending_prefix_drops(grove_version).expect("flush");
        assert_eq!(report.reclaimed_records, 0);
        assert_eq!(report.skipped_live, 1);

        assert!(!record_exists(&db, nested_prefix));
        assert_eq!(data_namespace_key_count(&db, nested_prefix), 0);
        assert!(!record_exists(&db, plain_prefix));
        assert_eq!(data_namespace_key_count(&db, plain_prefix), 0);
        assert!(record_exists(&db, recreated_prefix));
        assert_eq!(
            db.get_raw(
                [TEST_LEAF, b"recreated"].as_ref().into(),
                b"fresh",
                None,
                grove_version
            )
            .unwrap()
            .expect("fresh item survives"),
            Element::new_item(b"fresh_value".to_vec())
        );
    }

    #[test]
    fn should_reclaim_an_ordinary_drop_and_report_nothing_unresolved() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        seed_flat_tree(&db, b"flat", 10, grove_version);
        let prefix = prefix_of(&[TEST_LEAF, b"flat"]);
        drop_flat_and_leave_pending(&db, &[TEST_LEAF], b"flat", grove_version);

        let report = db.flush_pending_prefix_drops(grove_version).expect("flush");
        assert_eq!(
            report,
            PendingPrefixDropsReport {
                reclaimed_records: 1,
                skipped_live: 0,
                unresolved_records: 0,
                first_unresolved_error: None,
            }
        );
        assert!(!record_exists(&db, prefix));
        assert_eq!(data_namespace_key_count(&db, prefix), 0);
        assert_grove_verifies(&db, grove_version);
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

        db.drop_flat_subtree(
            [TEST_LEAF].as_ref(),
            b"cidx",
            BackwardsReferences::DontCheck,
            None,
            grove_version,
        )
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
            BackwardsReferences::DontCheck,
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
            )
            .dont_check_for_backwards_references(),
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
        )
        .dont_check_for_backwards_references()];
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
        )
        .dont_check_for_backwards_references()];
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
        )
        .dont_check_for_backwards_references()];
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
        )
        .dont_check_for_backwards_references()];
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

        db.drop_flat_subtree(
            [TEST_LEAF].as_ref(),
            b"flat",
            BackwardsReferences::DontCheck,
            None,
            grove_version,
        )
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

        db.drop_flat_subtree(
            [TEST_LEAF].as_ref(),
            b"flat",
            BackwardsReferences::DontCheck,
            None,
            grove_version,
        )
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
        )
        .dont_check_for_backwards_references()];

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
