//! Coverage tests targeting uncovered diff lines of PR #657.

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use grovedb_element::indexed::{compute_avg_fixed_point, IndexAxis};
    use grovedb_merk::{
        element::{
            get::ElementFetchFromStorageExtensions, insert::ElementInsertToStorageExtensions,
        },
        BatchEntry, CryptoHash, Merk,
    };
    use grovedb_path::SubtreePath;
    use grovedb_storage::{
        rocksdb_storage::{PrefixedRocksDbTransactionContext, RocksDbStorage},
        Storage, StorageBatch, StorageContext,
    };
    use grovedb_version::version::GroveVersion;
    use tempfile::TempDir;

    use crate::{
        operations::indexed_tree::make_axis_secondary_key,
        tests::{make_test_grovedb, TEST_LEAF},
        AggregateData, Element, Error, GroveDb,
    };

    const ZERO_HASH: CryptoHash = [0u8; 32];

    // -----------------------------------------------------------------
    // Raw-storage helpers.
    //
    // These follow the corruption-helper precedent already established by
    // `verify_grovedb_indexed_tests::corrupt_pcit_secondary_insert_orphan`
    // and `delete_indexed_tree_tests::seed_stale_secondary_row`: write
    // directly through a `StorageContext` at a derived prefix so the
    // resulting on-disk state is one that no public API can produce.
    // -----------------------------------------------------------------

    fn subtree_path<'a>(path: &'a [&'a [u8]]) -> SubtreePath<'a, &'a [u8]> {
        path.into()
    }

    /// Derived storage prefix of an indexed primary's per-axis secondary
    /// namespace (`Blake3(primary_prefix ‖ axis_tag)`).
    fn axis_secondary_prefix(primary_path: &[&[u8]], axis: IndexAxis) -> [u8; 32] {
        let primary_prefix = RocksDbStorage::build_prefix(subtree_path(primary_path)).unwrap();
        RocksDbStorage::secondary_prefix_for(&primary_prefix, axis.tag()).unwrap()
    }

    /// Write a raw key/value straight into a subtree's data namespace,
    /// bypassing the Merk tree structure entirely. The row is invisible to
    /// tree traversal but IS visible to the raw iteration that
    /// `verify_grovedb`'s content walks use.
    fn raw_put_in_subtree(db: &GroveDb, path: &[&[u8]], key: &[u8], value: &[u8]) {
        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        {
            let ctx = db
                .db
                .get_transactional_storage_context(subtree_path(path), Some(&batch), &tx)
                .unwrap();
            ctx.put(key, value, None, None)
                .unwrap()
                .expect("raw put into subtree namespace");
        }
        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit raw put");
        tx.commit().expect("commit tx");
    }

    /// Same, but into an indexed primary's per-axis secondary namespace.
    /// Insert a row into an axis secondary through the Merk API, so the
    /// stored bytes are a real node the integrity walk can decode. Raw
    /// storage puts are only usable for malformed-key cases, which the walk
    /// reports before it reaches the payload decode.
    fn insert_into_axis_secondary(
        db: &GroveDb,
        primary_path: &[&[u8]],
        axis: IndexAxis,
        key: &[u8],
        payload: Element,
        grove_version: &GroveVersion,
    ) {
        let path_vec: Vec<&[u8]> = primary_path.to_vec();
        let path: SubtreePath<&[u8]> = path_vec.as_slice().into();
        let (parent_path, indexed_key) = path.derive_parent().expect("non-root indexed tree");
        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let secondary_root_key = {
            let parent = db
                .open_transactional_merk_at_path(parent_path, &tx, Some(&batch), grove_version)
                .unwrap()
                .expect("open parent");
            let elem = Element::get(&parent, indexed_key, true, grove_version)
                .unwrap()
                .expect("indexed element");
            match elem.underlying() {
                Element::ProvableCountIndexedTree(_, s, ..)
                | Element::ProvableSumIndexedTree(_, s, ..) => s.clone(),
                Element::ProvableCountProvableSumIndexedTree(_, _, _, axes, _) => axes
                    .iter()
                    .find(|(t, _)| *t == axis.tag())
                    .and_then(|(_, sk)| sk.clone()),
                other => panic!("not an indexed element: {other:?}"),
            }
        };
        {
            let mut secondary_merk = db
                .open_indexed_secondary_at_path(
                    path,
                    axis,
                    secondary_root_key,
                    &tx,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .expect("open secondary");
            payload
                .insert(&mut secondary_merk, key, None, grove_version)
                .unwrap()
                .expect("insert secondary row");
        }
        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit");
        tx.commit().expect("commit tx");
    }

    fn raw_put_in_axis_secondary(
        db: &GroveDb,
        primary_path: &[&[u8]],
        axis: IndexAxis,
        key: &[u8],
        value: &[u8],
    ) {
        let prefix = axis_secondary_prefix(primary_path, axis);
        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        {
            let ctx = db
                .db
                .get_transactional_storage_context_by_subtree_prefix(prefix, Some(&batch), &tx)
                .unwrap();
            ctx.put(key, value, None, None)
                .unwrap()
                .expect("raw put into secondary namespace");
        }
        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit raw put");
        tx.commit().expect("commit tx");
    }

    /// Insert an element straight into a subtree's Merk, bypassing the
    /// grovedb-level insert API (and therefore the indexed-tree secondary
    /// mirroring and key-length validation).
    fn merk_insert_element(
        db: &GroveDb,
        path: &[&[u8]],
        key: &[u8],
        element: Element,
        grove_version: &GroveVersion,
    ) {
        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        {
            let mut merk = db
                .open_transactional_merk_at_path(
                    subtree_path(path),
                    &tx,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .expect("open merk");
            element
                .insert(&mut merk, key, None, grove_version)
                .unwrap()
                .expect("direct merk insert");
        }
        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit merk insert");
        tx.commit().expect("commit tx");
    }

    /// Rewrite the axes list of an existing PCPSIT element in place.
    ///
    /// The constructors and the insert path validate axis tags, but
    /// deserialization does not — so an element carrying a tag outside
    /// `0..=2` is representable on disk and must be rejected by every
    /// consumer that reads the list back. Because the key already exists,
    /// this is a replace: the parent Merk's shape (and therefore its own
    /// root key, held one level further up) is untouched.
    fn corrupt_pcpsit_axes(
        db: &GroveDb,
        parent_path: &[&[u8]],
        key: &[u8],
        axes: Vec<(u8, Option<Vec<u8>>)>,
        grove_version: &GroveVersion,
    ) {
        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        {
            let mut merk = db
                .open_transactional_merk_at_path(
                    subtree_path(parent_path),
                    &tx,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .expect("open parent merk");
            let existing = Element::get(&merk, key, true, grove_version)
                .unwrap()
                .expect("existing element");
            let corrupted = match existing.underlying() {
                Element::ProvableCountProvableSumIndexedTree(root_key, count, sum, _, flags) => {
                    Element::ProvableCountProvableSumIndexedTree(
                        root_key.clone(),
                        *count,
                        *sum,
                        axes,
                        flags.clone(),
                    )
                }
                other => panic!("expected a PCPSIT element, got {other:?}"),
            };
            corrupted
                .insert_count_indexed_subtree(
                    &mut merk,
                    key,
                    ZERO_HASH,
                    ZERO_HASH,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("replace element with corrupt axes");
        }
        db.db
            .commit_multi_context_batch(batch, Some(&tx))
            .unwrap()
            .expect("commit corrupt axes");
        tx.commit().expect("commit tx");
    }

    /// Drive `propagate_changes_with_transaction_with_initial_deferred`
    /// directly, seeding its `merk_cache` with the Merk at `path`. This is
    /// the entry point every mutating operation funnels through; calling it
    /// straight lets a test place the loop in states the public APIs reach
    /// only in combination (or, for the defensive guards, not at all).
    ///
    /// Commits only when propagation succeeded, so a rejected propagation
    /// leaves the database byte-identical.
    fn propagate_from(
        db: &GroveDb,
        path: &[&[u8]],
        initial_deferred: Option<(CryptoHash, Option<Vec<u8>>)>,
        grove_version: &GroveVersion,
    ) -> Result<(), Error> {
        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let result = {
            let sp = subtree_path(path);
            let merk = db
                .open_transactional_merk_at_path(sp.clone(), &tx, Some(&batch), grove_version)
                .unwrap()
                .expect("open merk at propagation start");
            let mut merk_cache: HashMap<
                SubtreePath<&[u8]>,
                Merk<PrefixedRocksDbTransactionContext>,
            > = HashMap::new();
            merk_cache.insert(sp.clone(), merk);
            db.propagate_changes_with_transaction_with_initial_deferred(
                merk_cache,
                sp,
                initial_deferred,
                &tx,
                &batch,
                grove_version,
            )
            .unwrap()
        };
        if result.is_ok() {
            db.db
                .commit_multi_context_batch(batch, Some(&tx))
                .unwrap()
                .expect("commit propagation");
            tx.commit().expect("commit tx");
        }
        result
    }

    fn issue_keys(issues: &crate::VerificationIssues) -> Vec<String> {
        issues
            .keys()
            .map(|p| {
                p.iter()
                    .map(|seg| String::from_utf8_lossy(seg).to_string())
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .collect()
    }

    fn sentinel_path(primary: &[&[u8]], sentinel: &str, item_key: &[u8]) -> Vec<Vec<u8>> {
        let mut p: Vec<Vec<u8>> = primary.iter().map(|s| s.to_vec()).collect();
        p.push(sentinel.as_bytes().to_vec());
        p.push(item_key.to_vec());
        p
    }

    // -----------------------------------------------------------------
    // `GroveDb::open_with_cidx_integrity_check`
    // -----------------------------------------------------------------

    #[test]
    fn open_with_cidx_integrity_check_returns_db_when_consistent() {
        let grove_version = GroveVersion::latest();
        let tmp_dir = TempDir::new().unwrap();

        let root_hash_before = {
            let db = GroveDb::open(tmp_dir.path()).unwrap();
            db.insert(
                crate::tests::common::EMPTY_PATH,
                TEST_LEAF,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create test leaf");
            db.insert(
                [TEST_LEAF].as_ref(),
                b"cidx",
                Element::empty_provable_count_indexed_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create PCIT");
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"a",
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert into PCIT");
            assert!(
                db.verify_grovedb(None, false, true, grove_version)
                    .expect("verify")
                    .is_empty(),
                "fixture must be consistent"
            );
            db.root_hash(None, grove_version).unwrap().unwrap()
        };

        // Re-open through the checking constructor: it must succeed and
        // hand back a database with the same committed root.
        let reopened = GroveDb::open_with_cidx_integrity_check(tmp_dir.path(), grove_version)
            .expect("consistent database must open");
        assert_eq!(
            reopened.root_hash(None, grove_version).unwrap().unwrap(),
            root_hash_before
        );
    }

    #[test]
    fn open_with_cidx_integrity_check_rejects_drifted_secondary() {
        let grove_version = GroveVersion::latest();
        let tmp_dir = TempDir::new().unwrap();

        let issue_count = {
            let db = GroveDb::open(tmp_dir.path()).unwrap();
            db.insert(
                crate::tests::common::EMPTY_PATH,
                TEST_LEAF,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create test leaf");
            db.insert(
                [TEST_LEAF].as_ref(),
                b"cidx",
                Element::empty_provable_count_indexed_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create PCIT");
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                b"a",
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert into PCIT");

            // A secondary row with no primary entry behind it. Written raw,
            // so no Merk root hash moves — the ONLY thing wrong with this
            // database is the content drift the per-entry walk detects.
            insert_into_axis_secondary(
                &db,
                &[TEST_LEAF, b"cidx"],
                IndexAxis::Count,
                &make_axis_secondary_key(IndexAxis::Count, 99, 0, b"ghost"),
                Element::new_item(Vec::new()),
                grove_version,
            );

            let issues = db.verify_grovedb(None, false, true, grove_version).unwrap();
            // Two issues are expected: the content walk's orphan sentinel, and
            // the H1-A chain mismatch — inserting the ghost row through the
            // Merk API moves the secondary's root hash while the parent
            // element still commits to the old one.
            assert!(
                !issues.is_empty(),
                "expected the orphan sentinel, got {:?}",
                issue_keys(&issues)
            );
            assert!(issues.contains_key(&sentinel_path(
                &[TEST_LEAF, b"cidx"],
                "__cidx_secondary_orphan__",
                b"ghost"
            )));
            issues.len()
        };

        // `GroveDb` is not `Debug`, so unwrap the error side by hand.
        let err = match GroveDb::open_with_cidx_integrity_check(tmp_dir.path(), grove_version) {
            Ok(_) => panic!("drifted database must not open"),
            Err(e) => e,
        };
        match err {
            Error::CorruptedData(message) => {
                assert!(
                    message.contains(&format!(
                        "integrity check on open found {issue_count} issue(s)"
                    )),
                    "unexpected message: {message}"
                );
            }
            other => panic!("expected CorruptedData, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // `propagate_changes_with_transaction_with_initial_deferred` guards
    // -----------------------------------------------------------------

    #[test]
    fn propagate_with_deferred_secondary_over_plain_tree_is_rejected() {
        // A deferred secondary means "the child I just came from was an
        // indexed primary", so the element above it must be an indexed
        // element. Hand the loop a deferred secondary above a plain `Tree`
        // and the mismatch must be reported, not silently folded in.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"plain",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create plain tree");
        let root_before = db.root_hash(None, grove_version).unwrap().unwrap();

        let err = propagate_from(
            &db,
            &[TEST_LEAF, b"plain"],
            Some((ZERO_HASH, None)),
            grove_version,
        )
        .expect_err("deferred secondary above a plain tree must be rejected");
        match err {
            Error::CorruptedData(message) => assert!(
                message.contains("expected an indexed-tree element when child_tree is an indexed"),
                "unexpected message: {message}"
            ),
            other => panic!("expected CorruptedData, got {other:?}"),
        }
        assert_eq!(
            db.root_hash(None, grove_version).unwrap().unwrap(),
            root_before,
            "a rejected propagation must not commit"
        );
    }

    #[test]
    fn propagate_at_root_with_unconsumed_deferred_secondary_is_rejected() {
        // Starting at the root leaves the `while let ... derive_parent()`
        // loop with nothing to do, so a deferred secondary handed in by the
        // caller can never be folded into an indexed element. The
        // end-of-loop guard must catch that rather than dropping it.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let root_before = db.root_hash(None, grove_version).unwrap().unwrap();

        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let result = {
            let root_path: SubtreePath<[u8; 0]> = SubtreePath::empty();
            let merk = db
                .open_transactional_merk_at_path(
                    root_path.clone(),
                    &tx,
                    Some(&batch),
                    grove_version,
                )
                .unwrap()
                .expect("open root merk");
            let mut merk_cache: HashMap<
                SubtreePath<[u8; 0]>,
                Merk<PrefixedRocksDbTransactionContext>,
            > = HashMap::new();
            merk_cache.insert(root_path.clone(), merk);
            db.propagate_changes_with_transaction_with_initial_deferred(
                merk_cache,
                root_path,
                Some((ZERO_HASH, Some(b"stale".to_vec()))),
                &tx,
                &batch,
                grove_version,
            )
            .unwrap()
        };

        match result.expect_err("unconsumed deferred secondary must be rejected") {
            Error::CorruptedCodeExecution(message) => assert!(
                message.contains("deferred secondary state was set but never consumed"),
                "unexpected message: {message}"
            ),
            other => panic!("expected CorruptedCodeExecution, got {other:?}"),
        }
        assert_eq!(
            db.root_hash(None, grove_version).unwrap().unwrap(),
            root_before
        );
    }

    // -----------------------------------------------------------------
    // PCPSIT propagation: rebuilding the axes digest from on-disk state
    // -----------------------------------------------------------------

    fn all_axes() -> Vec<(u8, Option<Vec<u8>>)> {
        vec![
            (IndexAxis::Count.tag(), None),
            (IndexAxis::Sum.tag(), None),
            (IndexAxis::Avg.tag(), None),
        ]
    }

    #[test]
    fn propagate_from_pcpsit_primary_rebuilds_axes_digest_from_disk() {
        // When propagation starts AT an indexed primary and no per-axis
        // state was staged by a mirror below it, the PCPSIT branch has to
        // re-read every configured axis's secondary from disk and rebuild
        // the axes digest over their current root hashes. Set up an element
        // whose committed H1-A hash is stale with respect to its primary,
        // then show that this branch heals exactly that binding while
        // leaving the (genuine) content drift reported.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(all_axes())
                .expect("canonical axes"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPSIT");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"a",
            Element::new_item_with_sum_item(b"v".to_vec(), 10),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert row a");
        assert!(db
            .verify_grovedb(None, false, true, grove_version)
            .unwrap()
            .is_empty());

        // Write a second row straight into the primary Merk. The primary's
        // root hash and aggregates move; the parent's PCPSIT element still
        // commits the old ones, and no secondary learns about `b`.
        merk_insert_element(
            &db,
            &[TEST_LEAF, b"pcpsit"],
            b"b",
            Element::new_item_with_sum_item(b"w".to_vec(), 4),
            grove_version,
        );

        let element_path: Vec<Vec<u8>> = vec![TEST_LEAF.to_vec(), b"pcpsit".to_vec()];
        let orphan_paths: Vec<Vec<Vec<u8>>> = [
            "__pcpsit_count_primary_orphan__",
            "__pcpsit_sum_primary_orphan__",
            "__pcpsit_avg_primary_orphan__",
        ]
        .iter()
        .map(|s| sentinel_path(&[TEST_LEAF, b"pcpsit"], s, b"b"))
        .collect();

        let before = db.verify_grovedb(None, false, true, grove_version).unwrap();
        assert!(
            before.contains_key(&element_path),
            "expected a stale H1-A binding at the PCPSIT element, got {:?}",
            issue_keys(&before)
        );
        for p in &orphan_paths {
            assert!(
                before.contains_key(p),
                "expected per-axis orphan sentinels for `b`, got {:?}",
                issue_keys(&before)
            );
        }

        propagate_from(&db, &[TEST_LEAF, b"pcpsit"], None, grove_version)
            .expect("propagation from the primary must succeed");

        let after = db.verify_grovedb(None, false, true, grove_version).unwrap();
        assert!(
            !after.contains_key(&element_path),
            "propagation must re-bind the element to the primary root and the on-disk axes \
             digest, got {:?}",
            issue_keys(&after)
        );
        for p in &orphan_paths {
            assert!(
                after.contains_key(p),
                "propagation must not invent secondary entries, got {:?}",
                issue_keys(&after)
            );
        }
    }

    #[test]
    fn propagate_from_pcpsit_primary_with_unknown_axis_tag_is_rejected() {
        // The axes list is read back from the element, so a tag outside
        // 0..=2 (only plantable by bypassing the constructors) must be
        // rejected rather than silently skipped.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"bad",
            Element::empty_provable_count_provable_sum_indexed_tree(all_axes())
                .expect("canonical axes"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPSIT");
        corrupt_pcpsit_axes(&db, &[TEST_LEAF], b"bad", vec![(9u8, None)], grove_version);

        let err = propagate_from(&db, &[TEST_LEAF, b"bad"], None, grove_version)
            .expect_err("unknown axis tag must be rejected");
        match err {
            Error::CorruptedData(message) => assert!(
                message.contains("invalid axis tag on a PCPSIT element during propagation")
                    && message.contains("unknown axis tag 9"),
                "unexpected message: {message}"
            ),
            other => panic!("expected CorruptedData, got {other:?}"),
        }
    }

    #[test]
    fn propagate_mirror_over_indexed_element_with_unknown_axis_tag_is_rejected() {
        // Same corrupt element, but reached from BELOW: the mirror step
        // reads the axes off the grandparent's indexed element to decide
        // which secondaries to update.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"bad",
            Element::empty_provable_count_provable_sum_indexed_tree(all_axes())
                .expect("canonical axes"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPSIT");
        // A child subtree inside the primary, so propagation can start
        // below it and walk up through the mirror step. Inserted while the
        // element is still well-formed, then the axes are corrupted.
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"bad"].as_ref(),
            b"c",
            Element::empty_count_sum_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert child subtree");
        corrupt_pcpsit_axes(&db, &[TEST_LEAF], b"bad", vec![(9u8, None)], grove_version);

        let err = propagate_from(&db, &[TEST_LEAF, b"bad", b"c"], None, grove_version)
            .expect_err("unknown axis tag must be rejected");
        match err {
            Error::CorruptedData(message) => assert!(
                message.contains("invalid axis tag on an indexed element during propagation")
                    && message.contains("unknown axis tag 9"),
                "unexpected message: {message}"
            ),
            other => panic!("expected CorruptedData, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Batch-path helper: rejecting a non-indexed element
    // -----------------------------------------------------------------

    #[test]
    fn update_count_indexed_item_into_batch_operations_rejects_non_indexed_element() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"plain",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create plain tree");

        let tx = db.start_transaction();
        let batch = StorageBatch::new();
        let merk = db
            .open_transactional_merk_at_path(
                subtree_path(&[TEST_LEAF]),
                &tx,
                Some(&batch),
                grove_version,
            )
            .unwrap()
            .expect("open test leaf merk");

        let mut ops: Vec<BatchEntry<Vec<u8>>> = Vec::new();
        let result = GroveDb::update_indexed_tree_item_preserve_flag_into_batch_operations(
            &merk,
            b"plain".to_vec(),
            None,
            vec![(0u8, ZERO_HASH, None)],
            AggregateData::NoAggregateData,
            ZERO_HASH,
            &mut ops,
            grove_version,
        )
        .unwrap();

        match result.expect_err("a plain Tree is not an indexed element") {
            Error::InvalidPath(message) => assert!(
                message
                    .contains("update_indexed_tree_item_preserve_flag: existing element is not an"),
                "unexpected message: {message}"
            ),
            other => panic!("expected InvalidPath, got {other:?}"),
        }
        assert!(
            ops.is_empty(),
            "a rejected rewrite must not queue batch operations"
        );
    }

    // -----------------------------------------------------------------
    // `verify_indexed_axis_content`: malformed / oversize key sentinels
    // -----------------------------------------------------------------

    #[test]
    fn verify_grovedb_flags_oversize_primary_key_in_pcit() {
        // A primary item key longer than the 247-byte cidx ceiling cannot
        // be produced by the insert API (its derived secondary key would
        // break Merk's 256-byte key limit), so the walk refuses to derive
        // one and reports the key with its length instead.
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
        .expect("create PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert row a");

        // 250 > MAX_CIDX_ITEM_KEY_LEN (247) but still < Merk's 256-byte key
        // ceiling, so the Merk write itself is legal.
        let long_key = vec![7u8; 250];
        assert!(
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"cidx"].as_ref(),
                &long_key,
                Element::new_item(b"v".to_vec()),
                None,
                grove_version,
            )
            .unwrap()
            .is_err(),
            "the public API must refuse an oversize item key"
        );
        merk_insert_element(
            &db,
            &[TEST_LEAF, b"cidx"],
            &long_key,
            Element::new_item(b"v".to_vec()),
            grove_version,
        );

        let issues = db.verify_grovedb(None, false, true, grove_version).unwrap();
        let entry = issues
            .get(&sentinel_path(
                &[TEST_LEAF, b"cidx"],
                "__cidx_primary_key_oversize__",
                &long_key,
            ))
            .unwrap_or_else(|| {
                panic!(
                    "expected __cidx_primary_key_oversize__, got {:?}",
                    issue_keys(&issues)
                )
            });
        assert_eq!(entry.0, ZERO_HASH);
        assert_eq!(entry.1, ZERO_HASH);
        assert_eq!(&entry.2[24..32], &250u64.to_be_bytes());
        // The oversize key is skipped, not indexed: no orphan is claimed
        // for it on top of the length report.
        assert!(!issues.contains_key(&sentinel_path(
            &[TEST_LEAF, b"cidx"],
            "__cidx_primary_orphan__",
            &long_key
        )));
    }

    #[test]
    fn verify_grovedb_flags_malformed_secondary_key_in_pcit() {
        // A secondary row whose key is shorter than the axis sort-key
        // prefix cannot be split into (sort_key, item_key) at all.
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
        .expect("create PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert row a");

        // 3 bytes < the 8-byte count sort-key prefix.
        raw_put_in_axis_secondary(&db, &[TEST_LEAF, b"cidx"], IndexAxis::Count, b"abc", b"x");

        let issues = db.verify_grovedb(None, false, true, grove_version).unwrap();
        let entry = issues
            .get(&sentinel_path(
                &[TEST_LEAF, b"cidx"],
                "__cidx_secondary_malformed_key__",
                b"abc",
            ))
            .unwrap_or_else(|| {
                panic!(
                    "expected __cidx_secondary_malformed_key__, got {:?}",
                    issue_keys(&issues)
                )
            });
        assert_eq!(*entry, (ZERO_HASH, ZERO_HASH, ZERO_HASH));
        // The malformed row is not mistaken for an entry of some item key.
        assert!(
            !issue_keys(&issues)
                .iter()
                .any(|k| k.contains("__cidx_secondary_orphan__")),
            "a malformed row must not also be reported as an orphan: {:?}",
            issue_keys(&issues)
        );
    }

    // -----------------------------------------------------------------
    // `verify_indexed_axis_content`: per-axis decoded mismatch reporting
    // -----------------------------------------------------------------

    #[test]
    fn verify_grovedb_reports_psit_sum_mismatch_with_decoded_sums() {
        // Plant a second row for item `a` under a LOWER sum sort key. The
        // walk takes the first row in key order as the item's index entry,
        // so `a` reads as indexed under sum -3 while the primary says 5,
        // and both values must come back decoded, not as raw key bytes.
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
        .expect("create PSIT");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"a",
            Element::new_sum_item(5),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert row a");

        insert_into_axis_secondary(
            &db,
            &[TEST_LEAF, b"psit"],
            IndexAxis::Sum,
            &make_axis_secondary_key(IndexAxis::Sum, 1, -3, b"a"),
            Element::new_sum_item(-3),
            grove_version,
        );

        let issues = db.verify_grovedb(None, false, true, grove_version).unwrap();
        let entry = issues
            .get(&sentinel_path(
                &[TEST_LEAF, b"psit"],
                "__psit_sum_mismatch__",
                b"a",
            ))
            .unwrap_or_else(|| {
                panic!(
                    "expected __psit_sum_mismatch__, got {:?}",
                    issue_keys(&issues)
                )
            });
        assert_eq!(&entry.1[24..32], &5i64.to_be_bytes(), "expected sum slot");
        assert_eq!(&entry.2[24..32], &(-3i64).to_be_bytes(), "actual sum slot");
        assert_eq!(&entry.1[..24], &[0u8; 24], "sum is right-aligned");
    }

    #[test]
    fn verify_grovedb_reports_pcpsit_avg_mismatch_with_decoded_averages() {
        // Same shape on the avg axis, whose sort key is a 16-byte
        // fixed-point average rather than an 8-byte integer.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(vec![(
                IndexAxis::Avg.tag(),
                None,
            )])
            .expect("canonical axes"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPSIT");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"row",
            Element::new_item_with_sum_item(b"v".to_vec(), 42),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert row");

        insert_into_axis_secondary(
            &db,
            &[TEST_LEAF, b"pcpsit"],
            IndexAxis::Avg,
            &make_axis_secondary_key(IndexAxis::Avg, 1, 1, b"row"),
            Element::new_item_with_sum_item(Vec::new(), 1),
            grove_version,
        );

        let issues = db.verify_grovedb(None, false, true, grove_version).unwrap();
        let entry = issues
            .get(&sentinel_path(
                &[TEST_LEAF, b"pcpsit"],
                "__pcpsit_avg_avg_mismatch__",
                b"row",
            ))
            .unwrap_or_else(|| {
                panic!(
                    "expected __pcpsit_avg_avg_mismatch__, got {:?}",
                    issue_keys(&issues)
                )
            });
        assert_eq!(
            &entry.1[16..32],
            &compute_avg_fixed_point(42, 1).to_be_bytes(),
            "expected avg slot"
        );
        assert_eq!(
            &entry.2[16..32],
            &compute_avg_fixed_point(1, 1).to_be_bytes(),
            "actual avg slot"
        );
        assert_eq!(&entry.1[..16], &[0u8; 16], "avg is right-aligned");
    }

    // -----------------------------------------------------------------
    // `verify_grovedb`: hard errors surfacing out of the indexed arms
    // -----------------------------------------------------------------

    fn assert_undecodable_row_error(err: Error) {
        match err {
            Error::MerkError(_) | Error::ElementError(_) | Error::CorruptedData(_) => {}
            other => panic!("expected a decode failure, got {other:?}"),
        }
    }

    #[test]
    fn verify_grovedb_errors_on_undecodable_row_in_pcit_primary() {
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
        .expect("create PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert row a");
        assert!(db
            .verify_grovedb(None, false, true, grove_version)
            .unwrap()
            .is_empty());

        raw_put_in_subtree(&db, &[TEST_LEAF, b"cidx"], b"zz", b"not-a-tree-node");
        assert_undecodable_row_error(
            db.verify_grovedb(None, false, true, grove_version)
                .expect_err("the content walk must surface the decode failure"),
        );
    }

    #[test]
    fn verify_grovedb_errors_on_undecodable_row_in_psit_primary() {
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
        .expect("create PSIT");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"a",
            Element::new_sum_item(5),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert row a");
        assert!(db
            .verify_grovedb(None, false, true, grove_version)
            .unwrap()
            .is_empty());

        raw_put_in_subtree(&db, &[TEST_LEAF, b"psit"], b"zz", b"not-a-tree-node");
        assert_undecodable_row_error(
            db.verify_grovedb(None, false, true, grove_version)
                .expect_err("the content walk must surface the decode failure"),
        );
    }

    #[test]
    fn verify_grovedb_errors_on_undecodable_row_in_pcpsit_primary() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(all_axes())
                .expect("canonical axes"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPSIT");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"a",
            Element::new_item_with_sum_item(b"v".to_vec(), 10),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert row a");
        assert!(db
            .verify_grovedb(None, false, true, grove_version)
            .unwrap()
            .is_empty());

        raw_put_in_subtree(&db, &[TEST_LEAF, b"pcpsit"], b"zz", b"not-a-tree-node");
        assert_undecodable_row_error(
            db.verify_grovedb(None, false, true, grove_version)
                .expect_err("the content walk must surface the decode failure"),
        );
    }

    #[test]
    fn verify_grovedb_errors_on_undecodable_row_below_pcit_primary() {
        // The bad row lives one level DEEPER than the primary, so the
        // per-entry content walk passes and the failure has to come back
        // out of the recursive descent into the primary instead.
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
        .expect("create PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"cidx"].as_ref(),
            b"sub",
            Element::empty_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert child subtree");
        assert!(db
            .verify_grovedb(None, false, true, grove_version)
            .unwrap()
            .is_empty());

        raw_put_in_subtree(
            &db,
            &[TEST_LEAF, b"cidx", b"sub"],
            b"zz",
            b"not-a-tree-node",
        );
        assert_undecodable_row_error(
            db.verify_grovedb(None, false, true, grove_version)
                .expect_err("the recursive descent must surface the decode failure"),
        );
    }

    #[test]
    fn verify_grovedb_errors_on_undecodable_row_below_psit_primary() {
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
        .expect("create PSIT");
        db.insert_into_provable_sum_indexed_tree(
            [TEST_LEAF, b"psit"].as_ref(),
            b"sub",
            Element::empty_sum_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert child subtree");
        assert!(db
            .verify_grovedb(None, false, true, grove_version)
            .unwrap()
            .is_empty());

        raw_put_in_subtree(
            &db,
            &[TEST_LEAF, b"psit", b"sub"],
            b"zz",
            b"not-a-tree-node",
        );
        assert_undecodable_row_error(
            db.verify_grovedb(None, false, true, grove_version)
                .expect_err("the recursive descent must surface the decode failure"),
        );
    }

    #[test]
    fn verify_grovedb_errors_on_undecodable_row_below_pcpsit_primary() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(all_axes())
                .expect("canonical axes"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPSIT");
        db.insert_into_provable_count_provable_sum_indexed_tree(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            b"sub",
            Element::empty_count_sum_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert child subtree");
        assert!(db
            .verify_grovedb(None, false, true, grove_version)
            .unwrap()
            .is_empty());

        raw_put_in_subtree(
            &db,
            &[TEST_LEAF, b"pcpsit", b"sub"],
            b"zz",
            b"not-a-tree-node",
        );
        assert_undecodable_row_error(
            db.verify_grovedb(None, false, true, grove_version)
                .expect_err("the recursive descent must surface the decode failure"),
        );
    }

    #[test]
    fn verify_grovedb_errors_on_pcpsit_element_with_unknown_axis_tag() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"bad",
            Element::empty_provable_count_provable_sum_indexed_tree(all_axes())
                .expect("canonical axes"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPSIT");
        corrupt_pcpsit_axes(&db, &[TEST_LEAF], b"bad", vec![(9u8, None)], grove_version);

        match db
            .verify_grovedb(None, false, true, grove_version)
            .expect_err("an unknown axis tag must be a hard error")
        {
            Error::CorruptedData(message) => assert!(
                message.contains("invalid axis tag in PCPSIT element")
                    && message.contains("unknown axis tag 9"),
                "unexpected message: {message}"
            ),
            other => panic!("expected CorruptedData, got {other:?}"),
        }
    }
}
