//! Replication session round-trip tests

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use grovedb_version::version::GroveVersion;
    use tempfile::TempDir;

    use crate::{
        replication::CURRENT_STATE_SYNC_VERSION,
        tests::{make_empty_grovedb, make_test_grovedb, TempGroveDb, ANOTHER_TEST_LEAF, TEST_LEAF},
        Element, GroveDb,
    };

    /// Helper: perform a full state sync from source to destination using
    /// a checkpoint of the source (mirrors the tutorial/production pattern).
    ///
    /// Returns the destination TempGroveDb after committing the session.
    fn sync_source_to_destination(
        source: &TempGroveDb,
        grove_version: &GroveVersion,
    ) -> TempGroveDb {
        // Create a checkpoint from the source -- this is the standard pattern
        // for replication (the tutorial does the same).
        let checkpoint_dir = TempDir::new().expect("should create temp dir for checkpoint");
        let checkpoint_path = checkpoint_dir.path().join("checkpoint");
        source
            .create_checkpoint(&checkpoint_path)
            .expect("should create checkpoint");
        let checkpoint_db = GroveDb::open(&checkpoint_path).expect("should open checkpoint db");

        let app_hash = checkpoint_db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("checkpoint root hash should be available");

        let dest = make_empty_grovedb();

        let mut session = dest
            .start_snapshot_syncing(app_hash, 64, CURRENT_STATE_SYNC_VERSION, grove_version)
            .expect("should start snapshot syncing");

        // Use a queue-based approach as shown in the tutorial
        let mut chunk_queue: VecDeque<Vec<u8>> = VecDeque::new();
        chunk_queue.push_back(app_hash.to_vec());

        while let Some(chunk_id) = chunk_queue.pop_front() {
            let chunk_data = checkpoint_db
                .fetch_chunk(
                    chunk_id.as_slice(),
                    None,
                    CURRENT_STATE_SYNC_VERSION,
                    grove_version,
                )
                .expect("should fetch chunk from checkpoint");

            let more_ids = session
                .apply_chunk(
                    chunk_id.as_slice(),
                    &chunk_data,
                    CURRENT_STATE_SYNC_VERSION,
                    grove_version,
                )
                .expect("should apply chunk to destination");

            chunk_queue.extend(more_ids);
        }

        assert!(
            session.is_sync_completed(),
            "sync should be completed after all chunks are applied"
        );

        dest.commit_session(session, grove_version)
            .expect("should commit sync session");

        dest
    }

    #[test]
    fn start_snapshot_syncing_returns_session() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        // Insert an item so the tree is non-trivial
        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"key1",
                Element::new_item(b"value1".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item into source");

        let app_hash = source
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        let dest = make_empty_grovedb();
        let session = dest
            .start_snapshot_syncing(app_hash, 10, CURRENT_STATE_SYNC_VERSION, grove_version)
            .expect("start_snapshot_syncing should return a session");

        // The session should not be completed yet (no chunks applied)
        assert!(
            !session.is_sync_completed(),
            "session should not be completed immediately after creation"
        );
    }

    #[test]
    fn start_snapshot_syncing_zero_batch_size_error() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        let app_hash = source
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        let dest = make_empty_grovedb();
        let result =
            dest.start_snapshot_syncing(app_hash, 0, CURRENT_STATE_SYNC_VERSION, grove_version);

        let err = result
            .err()
            .expect("start_snapshot_syncing with batch_size=0 should return an error");
        let err_msg = format!("{:?}", err);
        assert!(
            err_msg.contains("zero"),
            "error message should mention zero, got: {}",
            err_msg
        );
    }

    #[test]
    fn start_snapshot_syncing_unsupported_version_error() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        let app_hash = source
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        let dest = make_empty_grovedb();
        // Use version 0, which is not CURRENT_STATE_SYNC_VERSION (1)
        let err = dest
            .start_snapshot_syncing(app_hash, 10, 0, grove_version)
            .err()
            .expect("start_snapshot_syncing with unsupported version should return an error");
        let err_msg = format!("{:?}", err);
        assert!(
            err_msg.contains("Unsupported"),
            "error message should mention unsupported version, got: {}",
            err_msg
        );
    }

    #[test]
    fn full_round_trip_single_tree() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        // Insert several items into a single subtree
        for i in 0..5u8 {
            let key = format!("key{}", i);
            let value = format!("value{}", i);
            source
                .insert(
                    [TEST_LEAF].as_ref(),
                    key.as_bytes(),
                    Element::new_item(value.into_bytes()),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("should insert item into source");
        }

        let source_root_hash = source
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get source root hash");

        let dest = sync_source_to_destination(&source, grove_version);

        let dest_root_hash = dest
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get destination root hash");

        assert_eq!(
            source_root_hash, dest_root_hash,
            "destination root hash should match source after full sync"
        );
    }

    #[test]
    fn full_round_trip_nested_trees() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        // Create nested tree structure:
        //   root -> test_leaf -> inner_tree -> items
        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"inner_tree",
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert inner tree");

        source
            .insert(
                [TEST_LEAF, b"inner_tree"].as_ref(),
                b"nested_key1",
                Element::new_item(b"nested_value1".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert nested item 1");

        source
            .insert(
                [TEST_LEAF, b"inner_tree"].as_ref(),
                b"nested_key2",
                Element::new_item(b"nested_value2".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert nested item 2");

        // Also insert items in another_test_leaf
        source
            .insert(
                [ANOTHER_TEST_LEAF].as_ref(),
                b"other_key",
                Element::new_item(b"other_value".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item in another test leaf");

        let source_root_hash = source
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get source root hash");

        let dest = sync_source_to_destination(&source, grove_version);

        let dest_root_hash = dest
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get destination root hash");

        assert_eq!(
            source_root_hash, dest_root_hash,
            "destination root hash should match source after syncing nested trees"
        );

        // Verify nested items are readable in destination
        let elem1 = dest
            .get(
                [TEST_LEAF, b"inner_tree"].as_ref(),
                b"nested_key1",
                None,
                grove_version,
            )
            .unwrap()
            .expect("should read nested_key1 from destination");
        assert_eq!(
            elem1,
            Element::new_item(b"nested_value1".to_vec()),
            "nested_key1 value should match"
        );

        let elem2 = dest
            .get(
                [TEST_LEAF, b"inner_tree"].as_ref(),
                b"nested_key2",
                None,
                grove_version,
            )
            .unwrap()
            .expect("should read nested_key2 from destination");
        assert_eq!(
            elem2,
            Element::new_item(b"nested_value2".to_vec()),
            "nested_key2 value should match"
        );

        let other_elem = dest
            .get(
                [ANOTHER_TEST_LEAF].as_ref(),
                b"other_key",
                None,
                grove_version,
            )
            .unwrap()
            .expect("should read other_key from destination");
        assert_eq!(
            other_elem,
            Element::new_item(b"other_value".to_vec()),
            "other_key value should match"
        );
    }

    #[test]
    fn apply_chunk_wrong_version_error() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"key1",
                Element::new_item(b"value1".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item");

        let app_hash = source
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        let dest = make_empty_grovedb();
        let mut session = dest
            .start_snapshot_syncing(app_hash, 10, CURRENT_STATE_SYNC_VERSION, grove_version)
            .expect("should start snapshot syncing");

        let root_chunk_data = source
            .fetch_chunk(&app_hash, None, CURRENT_STATE_SYNC_VERSION, grove_version)
            .expect("should fetch root chunk");

        // Apply chunk with wrong version (version 0 instead of 1)
        let result = session.apply_chunk(
            &app_hash,
            &root_chunk_data,
            0, // wrong version
            grove_version,
        );

        assert!(
            result.is_err(),
            "apply_chunk with wrong version should return an error"
        );
        let err = result.unwrap_err();
        let err_msg = format!("{:?}", err);
        assert!(
            err_msg.contains("Unsupported"),
            "error message should mention unsupported version, got: {}",
            err_msg
        );
    }

    #[test]
    fn is_sync_completed_transitions() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        // Insert a single item to keep the tree small
        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"key1",
                Element::new_item(b"value1".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item");

        let app_hash = source
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        let dest = make_empty_grovedb();
        let mut session = dest
            .start_snapshot_syncing(app_hash, 64, CURRENT_STATE_SYNC_VERSION, grove_version)
            .expect("should start snapshot syncing");

        // Initially, sync should NOT be completed
        assert!(
            !session.is_sync_completed(),
            "sync should not be completed before any chunks are applied"
        );

        // Fetch and apply root chunk
        let root_chunk_data = source
            .fetch_chunk(&app_hash, None, CURRENT_STATE_SYNC_VERSION, grove_version)
            .expect("should fetch root chunk");

        let mut next_chunk_ids = session
            .apply_chunk(
                &app_hash,
                &root_chunk_data,
                CURRENT_STATE_SYNC_VERSION,
                grove_version,
            )
            .expect("should apply root chunk");

        // Continue applying all remaining chunks
        while !next_chunk_ids.is_empty() {
            let mut new_next_chunk_ids: Vec<Vec<u8>> = Vec::new();
            for packed_chunk_id in &next_chunk_ids {
                let chunk_data = source
                    .fetch_chunk(
                        packed_chunk_id,
                        None,
                        CURRENT_STATE_SYNC_VERSION,
                        grove_version,
                    )
                    .expect("should fetch chunk");

                let more_ids = session
                    .apply_chunk(
                        packed_chunk_id,
                        &chunk_data,
                        CURRENT_STATE_SYNC_VERSION,
                        grove_version,
                    )
                    .expect("should apply chunk");

                new_next_chunk_ids.extend(more_ids);
            }
            next_chunk_ids = new_next_chunk_ids;
        }

        // After all chunks applied, sync should be completed
        assert!(
            session.is_sync_completed(),
            "sync should be completed after all chunks are applied"
        );
    }

    #[test]
    fn commit_session_destination_readable() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        // Build a meaningful data set in source
        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"alpha",
                Element::new_item(b"alpha_value".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert alpha");

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"beta",
                Element::new_item(b"beta_value".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert beta");

        source
            .insert(
                [ANOTHER_TEST_LEAF].as_ref(),
                b"gamma",
                Element::new_item(b"gamma_value".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert gamma");

        // Create a nested subtree with items
        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"sub",
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert subtree");

        source
            .insert(
                [TEST_LEAF, b"sub"].as_ref(),
                b"delta",
                Element::new_item(b"delta_value".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert delta in subtree");

        // Perform full sync
        let dest = sync_source_to_destination(&source, grove_version);

        // Verify every item is readable and correct in the destination
        let alpha = dest
            .get([TEST_LEAF].as_ref(), b"alpha", None, grove_version)
            .unwrap()
            .expect("should read alpha from destination");
        assert_eq!(
            alpha,
            Element::new_item(b"alpha_value".to_vec()),
            "alpha value should match"
        );

        let beta = dest
            .get([TEST_LEAF].as_ref(), b"beta", None, grove_version)
            .unwrap()
            .expect("should read beta from destination");
        assert_eq!(
            beta,
            Element::new_item(b"beta_value".to_vec()),
            "beta value should match"
        );

        let gamma = dest
            .get([ANOTHER_TEST_LEAF].as_ref(), b"gamma", None, grove_version)
            .unwrap()
            .expect("should read gamma from destination");
        assert_eq!(
            gamma,
            Element::new_item(b"gamma_value".to_vec()),
            "gamma value should match"
        );

        let delta = dest
            .get([TEST_LEAF, b"sub"].as_ref(), b"delta", None, grove_version)
            .unwrap()
            .expect("should read delta from destination");
        assert_eq!(
            delta,
            Element::new_item(b"delta_value".to_vec()),
            "delta value should match"
        );

        // Root hashes should match
        let source_hash = source
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get source root hash");
        let dest_hash = dest
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get destination root hash");
        assert_eq!(
            source_hash, dest_hash,
            "root hashes should match after commit"
        );
    }

    #[test]
    fn sync_with_empty_subtree_succeeds() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        // Insert a subtree with no items — it will be genuinely empty
        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"empty_child",
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert empty subtree");

        // Also insert a non-empty sibling so the tree is non-trivial
        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"item",
                Element::new_item(b"val".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("should insert item");

        // Full sync should succeed (exercises the is_subtree_empty path)
        let dest = sync_source_to_destination(&source, grove_version);

        let source_hash = source
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get source hash");
        let dest_hash = dest
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get dest hash");
        assert_eq!(source_hash, dest_hash);
    }

    #[test]
    fn is_sync_completed_returns_false_before_any_sync() {
        let grove_version = GroveVersion::latest();
        let dest = make_empty_grovedb();
        let session = crate::replication::MultiStateSyncSession::new(&dest, [0u8; 32], 64);
        assert!(
            !session.is_sync_completed(),
            "is_sync_completed should return false when no sync has ever started"
        );
    }

    #[test]
    fn fetch_chunk_unsupported_version_error() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        let app_hash = source
            .root_hash(None, grove_version)
            .unwrap()
            .expect("should get root hash");

        let result = source.fetch_chunk(
            &app_hash,
            None,
            0, // unsupported version
            grove_version,
        );

        assert!(
            result.is_err(),
            "fetch_chunk with unsupported version should return an error"
        );
        let err = result.unwrap_err();
        let err_msg = format!("{:?}", err);
        assert!(
            err_msg.contains("Unsupported"),
            "error message should mention unsupported version, got: {}",
            err_msg
        );
    }

    #[test]
    fn commit_session_rejects_incomplete_session() {
        let grove_version = GroveVersion::latest();
        let dest = make_empty_grovedb();

        // Create a session that has never synced any chunks
        let session = crate::replication::MultiStateSyncSession::new(&dest, [0xAB; 32], 64);

        // commit() should reject because the session is incomplete
        let err = dest
            .commit_session(session, grove_version)
            .expect_err("commit should fail for incomplete session");
        match err {
            crate::Error::CorruptedData(message) => assert!(
                message.contains("incomplete"),
                "error should mention incomplete, got: {}",
                message
            ),
            other => panic!("expected CorruptedData, got: {:?}", other),
        }
    }

    // ---------- Indexed-tree state-sync rejection ----------
    //
    // State sync cannot yet handle indexed trees: their primaries commit a
    // three-input `combine_hash_three` (the restorer only knows the
    // two-input combine), and their axis secondary namespaces are never
    // enumerated during discovery. Rather than failing midway with an
    // opaque "chunk doesn't match expected root hash", both the source
    // side (`fetch_chunk`) and the target side (discovery in
    // `discover_new_subtrees_metadata`) now reject up-front with a
    // descriptive `Error::NotSupported`.

    fn assert_not_supported_indexed(err: &crate::Error, context: &str) {
        let msg = format!("{err:?}");
        assert!(
            matches!(err, crate::Error::NotSupported(_)),
            "{context}: expected Error::NotSupported, got: {msg}"
        );
        assert!(
            msg.contains("indexed"),
            "{context}: error should mention indexed trees, got: {msg}"
        );
    }

    /// Drive the full source->destination sync loop (mirroring
    /// `sync_source_to_destination`) but return the first error instead of
    /// panicking, so the test can assert on it.
    fn try_sync_source_to_destination(
        source: &TempGroveDb,
        grove_version: &GroveVersion,
    ) -> Result<(), crate::Error> {
        let checkpoint_dir = TempDir::new().expect("should create temp dir for checkpoint");
        let checkpoint_path = checkpoint_dir.path().join("checkpoint");
        source
            .create_checkpoint(&checkpoint_path)
            .expect("should create checkpoint");
        let checkpoint_db = GroveDb::open(&checkpoint_path).expect("should open checkpoint db");

        let app_hash = checkpoint_db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("checkpoint root hash should be available");

        let dest = make_empty_grovedb();
        let mut session =
            dest.start_snapshot_syncing(app_hash, 64, CURRENT_STATE_SYNC_VERSION, grove_version)?;

        let mut chunk_queue: VecDeque<Vec<u8>> = VecDeque::new();
        chunk_queue.push_back(app_hash.to_vec());

        while let Some(chunk_id) = chunk_queue.pop_front() {
            let chunk_data = checkpoint_db.fetch_chunk(
                chunk_id.as_slice(),
                None,
                CURRENT_STATE_SYNC_VERSION,
                grove_version,
            )?;
            let more_ids = session.apply_chunk(
                chunk_id.as_slice(),
                &chunk_data,
                CURRENT_STATE_SYNC_VERSION,
                grove_version,
            )?;
            chunk_queue.extend(more_ids);
        }
        Ok(())
    }

    #[test]
    fn state_sync_rejects_populated_pcit_up_front() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcit",
                Element::empty_provable_count_indexed_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create PCIT");
        // Children enter EMPTY and are populated so their counts are
        // DERIVED. All state sync needs is a populated PCIT; how the
        // aggregate was produced is irrelevant to the rejection.
        for (k, c) in &[(b"a" as &[u8], 3u64), (b"b" as &[u8], 7u64)] {
            source
                .insert_into_count_indexed_tree(
                    [TEST_LEAF, b"pcit"].as_ref(),
                    k,
                    Element::empty_provable_count_tree(),
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert PCIT entry");
            for i in 0..*c {
                source
                    .insert(
                        [TEST_LEAF, b"pcit", k].as_ref(),
                        &i.to_be_bytes(),
                        Element::new_item(vec![]),
                        None,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("derive PCIT entry count");
            }
        }

        let err = try_sync_source_to_destination(&source, grove_version)
            .expect_err("state sync of a DB containing a populated PCIT must fail up-front");
        assert_not_supported_indexed(&err, "PCIT sync");
    }

    #[test]
    fn state_sync_rejects_populated_psit_up_front() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"psit",
                Element::empty_provable_sum_indexed_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create PSIT");
        for (k, s) in &[(b"a" as &[u8], 4i64), (b"b" as &[u8], -2i64)] {
            source
                .insert_into_provable_sum_indexed_tree(
                    [TEST_LEAF, b"psit"].as_ref(),
                    k,
                    Element::new_sum_item(*s),
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert PSIT entry");
        }

        let err = try_sync_source_to_destination(&source, grove_version)
            .expect_err("state sync of a DB containing a populated PSIT must fail up-front");
        assert_not_supported_indexed(&err, "PSIT sync");
    }

    #[test]
    fn fetch_chunk_source_side_rejects_indexed_tree_chunk() {
        // Directly exercise the source-side `fetch_chunk` rejection: build
        // the global chunk id for the PCIT subtree's own prefix and ask
        // the source to produce it. The source must reject with
        // NotSupported rather than emitting a chunk.
        use crate::replication::utils::{encode_global_chunk_id, pack_nested_bytes};

        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"pcit",
                Element::empty_provable_count_indexed_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create PCIT");
        // Empty child plus one item inside it: a non-empty PCIT whose
        // count is DERIVED, which is all fetch_chunk needs to reject.
        source
            .insert_into_count_indexed_tree(
                [TEST_LEAF, b"pcit"].as_ref(),
                b"a",
                Element::empty_provable_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCIT entry");
        source
            .insert(
                [TEST_LEAF, b"pcit", b"a"].as_ref(),
                b"row",
                Element::new_item(b"v".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("derive PCIT entry count");

        // Read the PCIT element to get its root key and confirm tree type.
        let tx = source.start_transaction();
        let (merk, root_key, tree_type) = source
            .open_merk_for_replication([TEST_LEAF, b"pcit"].as_ref().into(), &tx, grove_version)
            .expect("open pcit merk for replication");
        drop(merk);
        assert!(
            tree_type.is_indexed_primary(),
            "sanity: opened tree must be an indexed primary, got {tree_type:?}"
        );

        let pcit_path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let prefix = grovedb_storage::rocksdb_storage::RocksDbStorage::build_prefix(
            pcit_path.as_ref().into(),
        )
        .unwrap();
        let global_chunk_id =
            encode_global_chunk_id(prefix, root_key, tree_type, vec![]).expect("encode chunk id");
        // fetch_chunk unpacks its input as nested bytes when the length
        // differs from the root-hash length, then decodes each element as
        // a global chunk id. Pack the single id the same way the wire
        // protocol does.
        let packed = pack_nested_bytes(vec![global_chunk_id]).expect("pack chunk id");

        let err = source
            .fetch_chunk(
                packed.as_slice(),
                Some(&tx),
                CURRENT_STATE_SYNC_VERSION,
                grove_version,
            )
            .expect_err("source-side fetch_chunk of an indexed tree must be rejected");
        assert_not_supported_indexed(&err, "source-side fetch_chunk");
    }

    fn assert_not_supported_append_only(err: &crate::Error, context: &str) {
        let msg = format!("{err:?}");
        assert!(
            matches!(err, crate::Error::NotSupported(_)),
            "{context}: expected Error::NotSupported, got: {msg}"
        );
        assert!(
            msg.contains("append-only"),
            "{context}: error should mention append-only trees, got: {msg}"
        );
    }

    /// State sync of a DB containing a POPULATED CommitmentTree is
    /// rejected up-front with a descriptive `NotSupported` (issue #785,
    /// Phase 0 guard).
    ///
    /// The CT's payload (Sinsemilla frontier + BulkAppendTree chunks + MMR
    /// overlay) lives in the data namespace as raw non-Merk entries while
    /// its Merk is always empty (root_key = None), so a Merk-chunk restore
    /// cannot transfer it. Before the guard, the source failed later with
    /// an opaque `CorruptedData("... cannot create chunk producer for
    /// empty Merk")` instead.
    ///
    /// (An EMPTY CommitmentTree syncs fine — no payload entries exist yet;
    /// see `state_sync_empty_commitment_tree_succeeds` below.)
    #[test]
    fn state_sync_rejects_populated_commitment_tree_up_front() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"ct",
                Element::empty_commitment_tree(4).expect("valid chunk power"),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert commitment tree");

        // Populate with three notes.
        for i in 1u8..=3 {
            source
                .commitment_tree_insert_raw(
                    [TEST_LEAF].as_ref(),
                    b"ct",
                    [i; 32],
                    [i.wrapping_add(100); 32],
                    [i.wrapping_add(200); 32],
                    vec![0u8; 216],
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert commitment tree note");
        }

        // Source is healthy: anchor readable, verify_grovedb clean.
        source
            .commitment_tree_anchor([TEST_LEAF].as_ref(), b"ct", None, grove_version)
            .unwrap()
            .expect("source anchor should be readable");
        let source_issues = source
            .verify_grovedb(None, true, false, grove_version)
            .expect("source verify_grovedb should run");
        assert!(
            source_issues.is_empty(),
            "source must verify clean, got: {:?}",
            source_issues
        );

        // State sync is rejected up-front by the target-side discovery
        // guard when the parent subtree completes.
        let err = try_sync_source_to_destination(&source, grove_version)
            .expect_err("state sync of a DB containing a populated CommitmentTree must fail");
        assert_not_supported_append_only(&err, "populated CT sync");
    }

    /// Source-side counterpart: a direct `fetch_chunk` for a populated
    /// CommitmentTree's own prefix must be rejected with the same
    /// descriptive `NotSupported` (issue #785, Phase 0 guard) rather than
    /// the opaque chunk-producer `CorruptedData` it produced before.
    #[test]
    fn fetch_chunk_source_side_rejects_populated_commitment_tree_chunk() {
        use crate::replication::utils::{encode_global_chunk_id, pack_nested_bytes};

        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"ct",
                Element::empty_commitment_tree(4).expect("valid chunk power"),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert commitment tree");
        source
            .commitment_tree_insert_raw(
                [TEST_LEAF].as_ref(),
                b"ct",
                [1u8; 32],
                [2u8; 32],
                [3u8; 32],
                vec![0u8; 216],
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert commitment tree note");

        let tx = source.start_transaction();
        let (merk, root_key, tree_type) = source
            .open_merk_for_replication([TEST_LEAF, b"ct"].as_ref().into(), &tx, grove_version)
            .expect("open ct merk for replication");
        drop(merk);
        assert!(
            tree_type.uses_non_merk_data_storage(),
            "sanity: opened tree must be a non-Merk data tree, got {tree_type:?}"
        );

        let ct_path: &[&[u8]] = &[TEST_LEAF, b"ct"];
        let prefix =
            grovedb_storage::rocksdb_storage::RocksDbStorage::build_prefix(ct_path.as_ref().into())
                .unwrap();
        let global_chunk_id =
            encode_global_chunk_id(prefix, root_key, tree_type, vec![]).expect("encode chunk id");
        let packed = pack_nested_bytes(vec![global_chunk_id]).expect("pack chunk id");

        let err = source
            .fetch_chunk(
                packed.as_slice(),
                Some(&tx),
                CURRENT_STATE_SYNC_VERSION,
                grove_version,
            )
            .expect_err("source-side fetch_chunk of a populated CT must be rejected");
        assert_not_supported_append_only(&err, "source-side fetch_chunk");
    }

    /// Companion to the test above: an EMPTY CommitmentTree state-syncs
    /// without error today, because no payload entries exist yet under its
    /// prefix — `is_empty_tree()` is true, the source returns an empty
    /// chunk, and the destination accepts the NULL-root subtree. The CT
    /// element itself is restored byte-for-byte via the parent Merk.
    ///
    /// This is exactly why a naive "return empty chunk / skip" fix for the
    /// populated case would be dangerous: the destination would accept the
    /// sync (parent Merk and app hash restore fine — nothing recomputes the
    /// ct_state root from payload during restore) while the frontier and
    /// note data are silently missing.
    #[test]
    fn state_sync_empty_commitment_tree_succeeds() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"ct_empty",
                Element::empty_commitment_tree(4).expect("valid chunk power"),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert commitment tree");

        let source_root_hash = source
            .root_hash(None, grove_version)
            .unwrap()
            .expect("source root hash");

        let dest = sync_source_to_destination(&source, grove_version);

        let dest_root_hash = dest
            .root_hash(None, grove_version)
            .unwrap()
            .expect("dest root hash");
        assert_eq!(source_root_hash, dest_root_hash);

        // The empty CT element survives and is intact on the destination.
        let elem = dest
            .get([TEST_LEAF].as_ref(), b"ct_empty", None, grove_version)
            .unwrap()
            .expect("CT element must exist on destination");
        match elem.underlying() {
            Element::CommitmentTree(total_count, _, _) => {
                assert_eq!(*total_count, 0);
            }
            other => panic!("expected CommitmentTree element, got {:?}", other),
        }

        let dest_issues = dest
            .verify_grovedb(None, true, false, grove_version)
            .expect("dest verify_grovedb should run");
        assert!(
            dest_issues.is_empty(),
            "empty-CT destination must verify clean, got: {:?}",
            dest_issues
        );
    }

    /// The guard is family-wide: every populated tree type that stores
    /// payload as non-Merk data-namespace entries (MmrTree, BulkAppendTree,
    /// DenseAppendOnlyFixedSizeTree — same storage model as
    /// CommitmentTree) is rejected up-front the same way.
    #[test]
    fn state_sync_rejects_all_populated_non_merk_trees_up_front() {
        let grove_version = GroveVersion::latest();

        // (key, element, populate)
        let cases: Vec<(
            &[u8],
            Element,
            Box<dyn Fn(&TempGroveDb) -> Result<(), crate::Error>>,
        )> = vec![
            (
                b"mmr".as_ref(),
                Element::empty_mmr_tree(),
                Box::new(|db: &TempGroveDb| {
                    db.mmr_tree_append(
                        [TEST_LEAF].as_ref(),
                        b"mmr",
                        b"leaf-1".to_vec(),
                        None,
                        GroveVersion::latest(),
                    )
                    .unwrap()
                    .map(|_| ())
                }),
            ),
            (
                b"bulk".as_ref(),
                Element::empty_bulk_append_tree(4).expect("valid chunk power"),
                Box::new(|db: &TempGroveDb| {
                    db.bulk_append(
                        [TEST_LEAF].as_ref(),
                        b"bulk",
                        b"value-1".to_vec(),
                        None,
                        GroveVersion::latest(),
                    )
                    .unwrap()
                    .map(|_| ())
                }),
            ),
            (
                b"dense".as_ref(),
                Element::empty_dense_tree(4),
                Box::new(|db: &TempGroveDb| {
                    db.dense_tree_insert(
                        [TEST_LEAF].as_ref(),
                        b"dense",
                        vec![7u8; 32],
                        None,
                        GroveVersion::latest(),
                    )
                    .unwrap()
                    .map(|_| ())
                }),
            ),
        ];

        for (key, element, populate) in cases {
            let source = make_test_grovedb(grove_version);
            source
                .insert(
                    [TEST_LEAF].as_ref(),
                    key,
                    element,
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert non-Merk tree element");
            populate(&source).expect("populate non-Merk tree");

            let err = try_sync_source_to_destination(&source, grove_version)
                .expect_err("state sync of a DB containing a populated non-Merk tree must fail");
            assert_not_supported_append_only(
                &err,
                &format!("populated {} sync", String::from_utf8_lossy(key)),
            );
        }
    }
}
