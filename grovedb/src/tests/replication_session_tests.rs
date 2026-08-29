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

    /// Optional in-flight mutation of a commitment tree page:
    /// `(more, aux, entries) -> (more, aux, entries)`. Used by tamper tests.
    type CtPageMutator<'a> =
        &'a dyn Fn(bool, Vec<u8>, Vec<Vec<u8>>) -> (bool, Vec<u8>, Vec<Vec<u8>>);

    /// The single sync driver behind every test in this file: checkpoint the
    /// source (the standard replication pattern — the tutorial does the
    /// same), run the fetch/apply loop with the given subtree batch size,
    /// optionally mutating commitment tree pages in flight, verify
    /// completion, and commit the session.
    fn run_sync(
        source: &TempGroveDb,
        grove_version: &GroveVersion,
        subtrees_batch_size: usize,
        mutate_ct_page: Option<CtPageMutator>,
    ) -> Result<TempGroveDb, crate::Error> {
        use crate::replication::{
            non_merk_sync::{decode_non_merk_page, encode_non_merk_page},
            utils::{decode_global_chunk_id, pack_nested_bytes, unpack_nested_bytes},
        };

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

        let mut session = dest.start_snapshot_syncing(
            app_hash,
            subtrees_batch_size,
            CURRENT_STATE_SYNC_VERSION,
            grove_version,
        )?;

        // Use a queue-based approach as shown in the tutorial
        let mut chunk_queue: VecDeque<Vec<u8>> = VecDeque::new();
        chunk_queue.push_back(app_hash.to_vec());

        while let Some(chunk_id) = chunk_queue.pop_front() {
            let mut chunk_data = checkpoint_db.fetch_chunk(
                chunk_id.as_slice(),
                None,
                CURRENT_STATE_SYNC_VERSION,
                grove_version,
            )?;

            if let Some(mutate) = mutate_ct_page {
                // Mirror apply_chunk's unpacking to find commitment tree
                // pages and run them through the mutator.
                let global_ids: Vec<Vec<u8>> = if chunk_id.as_slice() == app_hash.as_slice() {
                    vec![chunk_id.clone()]
                } else {
                    unpack_nested_bytes(&chunk_id)?
                };
                let global_data = unpack_nested_bytes(&chunk_data)?;
                assert_eq!(global_ids.len(), global_data.len());
                let mut mutated_globals = Vec::with_capacity(global_data.len());
                for (gid, gdata) in global_ids.iter().zip(global_data) {
                    let (_, _, tree_type, _) = decode_global_chunk_id(gid, &app_hash)?;
                    if matches!(
                        tree_type,
                        grovedb_merk::tree_type::TreeType::CommitmentTree(_)
                    ) {
                        let pages = unpack_nested_bytes(&gdata)?;
                        let mut mutated_pages = Vec::with_capacity(pages.len());
                        for page in pages {
                            let (more, aux, entries) = decode_non_merk_page(&page)?;
                            let (more, aux, entries) = mutate(more, aux, entries);
                            mutated_pages.push(encode_non_merk_page(more, aux, entries)?);
                        }
                        mutated_globals.push(pack_nested_bytes(mutated_pages)?);
                    } else {
                        mutated_globals.push(gdata);
                    }
                }
                chunk_data = pack_nested_bytes(mutated_globals)?;
            }

            let more_ids = session.apply_chunk(
                chunk_id.as_slice(),
                &chunk_data,
                CURRENT_STATE_SYNC_VERSION,
                grove_version,
            )?;

            chunk_queue.extend(more_ids);
        }

        if !session.is_sync_completed() {
            return Err(crate::Error::InternalError(
                "sync did not complete".to_string(),
            ));
        }

        dest.commit_session(session, grove_version)?;
        Ok(dest)
    }

    /// Helper: perform a full state sync from source to destination,
    /// panicking on any error.
    ///
    /// Returns the destination TempGroveDb after committing the session.
    fn sync_source_to_destination(
        source: &TempGroveDb,
        grove_version: &GroveVersion,
    ) -> TempGroveDb {
        run_sync(source, grove_version, 64, None).expect("state sync should succeed")
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
        run_sync(source, grove_version, 64, None).map(|_| ())
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
        let (merk, root_key, tree_type, _element) = source
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

    /// Full state-sync round trip for a POPULATED CommitmentTree (issue
    /// #785, Phase 1). Uses chunk_power 2 (epoch of 4) with 6 notes so the
    /// payload spans a compacted chunk blob AND the current buffer, plus
    /// the Sinsemilla frontier.
    #[test]
    fn state_sync_populated_commitment_tree_round_trip() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"ct",
                Element::empty_commitment_tree(2).expect("valid chunk power"),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert commitment tree");
        // A sibling item so the parent subtree holds mixed content.
        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"sibling",
                Element::new_item(b"item next to the ct".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sibling item");

        for i in 1u8..=6 {
            source
                .commitment_tree_insert_raw(
                    [TEST_LEAF].as_ref(),
                    b"ct",
                    [i; 32],
                    [i.wrapping_add(100); 32],
                    [i.wrapping_add(200); 32],
                    vec![i; 216],
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert commitment tree note");
        }

        let source_root_hash = source
            .root_hash(None, grove_version)
            .unwrap()
            .expect("source root hash");
        let source_anchor = source
            .commitment_tree_anchor([TEST_LEAF].as_ref(), b"ct", None, grove_version)
            .unwrap()
            .expect("source anchor");

        let dest = sync_source_to_destination(&source, grove_version);

        let dest_root_hash = dest
            .root_hash(None, grove_version)
            .unwrap()
            .expect("dest root hash");
        assert_eq!(source_root_hash, dest_root_hash, "app hash must match");

        // The anchor (recomputed from the transferred frontier) matches.
        let dest_anchor = dest
            .commitment_tree_anchor([TEST_LEAF].as_ref(), b"ct", None, grove_version)
            .unwrap()
            .expect("dest anchor must be readable");
        assert_eq!(source_anchor, dest_anchor, "anchor must match");

        // Every note value survives, both in the compacted chunk (positions
        // 0..4) and in the buffer (positions 4..6).
        for pos in 0u64..6 {
            let source_value = source
                .commitment_tree_get_value([TEST_LEAF].as_ref(), b"ct", pos, None, grove_version)
                .unwrap()
                .expect("source note value")
                .expect("source note value present");
            let dest_value = dest
                .commitment_tree_get_value([TEST_LEAF].as_ref(), b"ct", pos, None, grove_version)
                .unwrap()
                .expect("dest note value")
                .expect("dest note value present");
            assert_eq!(source_value, dest_value, "note {pos} must match");
        }

        // The destination passes a full integrity check.
        let dest_issues = dest
            .verify_grovedb(None, true, false, grove_version)
            .expect("dest verify_grovedb should run");
        assert!(
            dest_issues.is_empty(),
            "destination must verify clean, got: {:?}",
            dest_issues
        );

        // The restored subtree is fully usable for future writes: appending
        // the same note on both sides keeps the states identical.
        for db in [&source, &dest] {
            db.commitment_tree_insert_raw(
                [TEST_LEAF].as_ref(),
                b"ct",
                // Small repeated bytes stay below the Pallas field modulus.
                [7u8; 32],
                [8u8; 32],
                [9u8; 32],
                vec![77u8; 216],
                None,
                grove_version,
            )
            .unwrap()
            .expect("post-sync append");
        }
        assert_eq!(
            source.root_hash(None, grove_version).unwrap().unwrap(),
            dest.root_hash(None, grove_version).unwrap().unwrap(),
            "post-sync appends must produce identical states"
        );
    }

    /// A peer speaking the pre-#785 protocol requests an append-only
    /// subtree the old way — with no page cursor in the global chunk id.
    /// The source must reject that request descriptively instead of trying
    /// (and opaquely failing) to build a Merk chunk producer.
    #[test]
    fn fetch_chunk_rejects_append_only_request_without_page_cursor() {
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
        let (merk, root_key, tree_type, _element) = source
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
        // No nested chunk ids — the shape an old peer would send.
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
            .expect_err("cursor-less append-only chunk request must be rejected");
        assert_not_supported_append_only(&err, "source-side fetch_chunk");
    }

    /// The MMR page cursor's `state` (the mmr_size) is peer-controlled. A
    /// non-canonical size must be rejected before it drives any leaf →
    /// position arithmetic: `state = u64::MAX` yields a leaf count of
    /// `2^63`, and `start = 2^63 - 1` would then overflow `leaf_to_pos`
    /// (a debug-build panic, a wrapped position in release).
    #[test]
    fn fetch_chunk_rejects_non_canonical_mmr_cursor() {
        use crate::replication::{
            non_merk_sync::NonMerkChunkId,
            utils::{encode_global_chunk_id, pack_nested_bytes},
        };

        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"mmr",
                Element::empty_mmr_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert mmr tree");
        for i in 0u8..3 {
            source
                .mmr_tree_append(
                    [TEST_LEAF].as_ref(),
                    b"mmr",
                    vec![i; 8],
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("append mmr leaf");
        }

        let tx = source.start_transaction();
        let (merk, root_key, tree_type, _element) = source
            .open_merk_for_replication([TEST_LEAF, b"mmr"].as_ref().into(), &tx, grove_version)
            .expect("open mmr merk for replication");
        drop(merk);
        let mmr_path: &[&[u8]] = &[TEST_LEAF, b"mmr"];
        let prefix = grovedb_storage::rocksdb_storage::RocksDbStorage::build_prefix(
            mmr_path.as_ref().into(),
        )
        .unwrap();

        let fetch = |id: NonMerkChunkId| -> Result<Vec<u8>, crate::Error> {
            let global_chunk_id =
                encode_global_chunk_id(prefix, root_key.clone(), tree_type, vec![id.encode()])?;
            let packed = pack_nested_bytes(vec![global_chunk_id])?;
            source.fetch_chunk(
                packed.as_slice(),
                Some(&tx),
                CURRENT_STATE_SYNC_VERSION,
                grove_version,
            )
        };

        // Sanity: the honest cursor (3 leaves → mmr_size 4) serves the page.
        fetch(NonMerkChunkId {
            start: 0,
            state: 4,
            param: 0,
        })
        .expect("honest cursor must be served");

        for (state, start) in [
            (u64::MAX, (1u64 << 63) - 1),
            (u64::MAX, 0),
            ((1u64 << 63) + 1, 0),
            (5, 0),
            (2, 1),
        ] {
            let err = fetch(NonMerkChunkId {
                start,
                state,
                param: 0,
            })
            .expect_err("non-canonical mmr size in cursor must be rejected");
            assert!(
                format!("{err:?}").contains("not a valid MMR size"),
                "state {state} start {start}: got {err:?}"
            );
        }

        // A canonical-but-wrong size (2^63 = 2^62 + 1 leaves) passes the
        // shape check and then fails as a bounded read of a missing leaf —
        // never a panic, never an unrelated position.
        let err = fetch(NonMerkChunkId {
            start: 0,
            state: 1u64 << 63,
            param: 0,
        })
        .expect_err("oversized canonical mmr size must fail as a missing-leaf read");
        assert!(
            format!("{err:?}").contains("missing MMR leaf"),
            "got {err:?}"
        );
    }

    /// `PrivateDocumentStore` also uses non-Merk data storage but has no
    /// entry-replay arm yet. An EMPTY one must keep syncing through the
    /// ordinary Merk path (exactly as before the append-only work), and the
    /// restored store must be usable afterwards.
    #[test]
    fn state_sync_empty_private_document_store_round_trip() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"docs",
                Element::empty_private_document_store(16, 2).expect("valid config"),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert empty private document store");

        let dest = sync_source_to_destination(&source, grove_version);

        assert_eq!(
            source.root_hash(None, grove_version).unwrap().unwrap(),
            dest.root_hash(None, grove_version).unwrap().unwrap(),
        );
        let dest_issues = dest
            .verify_grovedb(None, true, false, grove_version)
            .expect("dest verify_grovedb should run");
        assert!(dest_issues.is_empty(), "got: {:?}", dest_issues);

        for db in [&source, &dest] {
            db.private_document_store_insert(
                [TEST_LEAF].as_ref(),
                b"docs",
                vec![9u8; 16],
                None,
                grove_version,
            )
            .unwrap()
            .expect("post-sync document insert");
        }
        assert_eq!(
            source.root_hash(None, grove_version).unwrap().unwrap(),
            dest.root_hash(None, grove_version).unwrap().unwrap(),
            "post-sync inserts must produce identical states"
        );
    }

    /// A POPULATED `PrivateDocumentStore` cannot be transferred yet: the
    /// target rejects it descriptively at discovery (never a silent
    /// truncation to an empty store), and the source rejects a chunk request
    /// for it descriptively too.
    #[test]
    fn state_sync_rejects_populated_private_document_store_up_front() {
        use crate::replication::utils::{encode_global_chunk_id, pack_nested_bytes};

        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"docs",
                Element::empty_private_document_store(16, 2).expect("valid config"),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert private document store");
        source
            .private_document_store_insert(
                [TEST_LEAF].as_ref(),
                b"docs",
                vec![1u8; 16],
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert document");

        let err = try_sync_source_to_destination(&source, grove_version)
            .expect_err("state sync of a DB containing a populated PDS must fail up-front");
        let msg = format!("{err:?}");
        assert!(
            matches!(err, crate::Error::NotSupported(_)) && msg.contains("populated"),
            "target-side: expected descriptive NotSupported, got: {msg}"
        );

        // Source side: the Merk-path request shape for this subtree.
        let tx = source.start_transaction();
        let (merk, root_key, tree_type, _element) = source
            .open_merk_for_replication([TEST_LEAF, b"docs"].as_ref().into(), &tx, grove_version)
            .expect("open pds merk for replication");
        drop(merk);
        let pds_path: &[&[u8]] = &[TEST_LEAF, b"docs"];
        let prefix = grovedb_storage::rocksdb_storage::RocksDbStorage::build_prefix(
            pds_path.as_ref().into(),
        )
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
            .expect_err("populated PDS chunk request must be rejected");
        let msg = format!("{err:?}");
        assert!(
            matches!(err, crate::Error::NotSupported(_)) && msg.contains("populated"),
            "source-side: expected descriptive NotSupported, got: {msg}"
        );
    }

    /// An EMPTY CommitmentTree state-syncs cleanly: it has no payload
    /// entries, so the entry-replay path transfers a single empty page and
    /// verification reduces to the empty-tree state-root convention.
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

    /// Round trip for a populated MmrTree.
    #[test]
    fn state_sync_populated_mmr_tree_round_trip() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"mmr",
                Element::empty_mmr_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert mmr tree");
        for i in 0u8..3 {
            source
                .mmr_tree_append(
                    [TEST_LEAF].as_ref(),
                    b"mmr",
                    format!("leaf-{i}").into_bytes(),
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("append mmr leaf");
        }

        let dest = sync_source_to_destination(&source, grove_version);

        assert_eq!(
            source.root_hash(None, grove_version).unwrap().unwrap(),
            dest.root_hash(None, grove_version).unwrap().unwrap(),
        );
        for i in 0u64..3 {
            assert_eq!(
                dest.mmr_tree_get_value([TEST_LEAF].as_ref(), b"mmr", i, None, grove_version)
                    .unwrap()
                    .expect("dest mmr leaf"),
                Some(format!("leaf-{i}").into_bytes()),
                "mmr leaf {i} must survive the sync"
            );
        }
        let dest_issues = dest
            .verify_grovedb(None, true, false, grove_version)
            .expect("dest verify_grovedb should run");
        assert!(dest_issues.is_empty(), "got: {:?}", dest_issues);
    }

    /// Round trip for a populated BulkAppendTree spanning multiple
    /// compacted chunks plus the buffer (chunk_power 2 → epoch of 4;
    /// 10 values → 2 chunk blobs + 2 buffer entries).
    #[test]
    fn state_sync_populated_bulk_append_tree_round_trip() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"bulk",
                Element::empty_bulk_append_tree(2).expect("valid chunk power"),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert bulk append tree");
        for i in 0u8..10 {
            source
                .bulk_append(
                    [TEST_LEAF].as_ref(),
                    b"bulk",
                    format!("value-{i}").into_bytes(),
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("bulk append");
        }

        let dest = sync_source_to_destination(&source, grove_version);

        assert_eq!(
            source.root_hash(None, grove_version).unwrap().unwrap(),
            dest.root_hash(None, grove_version).unwrap().unwrap(),
        );
        for i in 0u64..10 {
            assert_eq!(
                dest.bulk_get_value([TEST_LEAF].as_ref(), b"bulk", i, None, grove_version)
                    .unwrap()
                    .expect("dest bulk value"),
                Some(format!("value-{i}").into_bytes()),
                "bulk value {i} must survive the sync"
            );
        }
        let dest_issues = dest
            .verify_grovedb(None, true, false, grove_version)
            .expect("dest verify_grovedb should run");
        assert!(dest_issues.is_empty(), "got: {:?}", dest_issues);
    }

    /// Round trip for a populated DenseAppendOnlyFixedSizeTree.
    #[test]
    fn state_sync_populated_dense_tree_round_trip() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"dense",
                Element::empty_dense_tree(4),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert dense tree");
        for i in 0u8..3 {
            source
                .dense_tree_insert(
                    [TEST_LEAF].as_ref(),
                    b"dense",
                    vec![i; 32],
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("dense insert");
        }

        let dest = sync_source_to_destination(&source, grove_version);

        assert_eq!(
            source.root_hash(None, grove_version).unwrap().unwrap(),
            dest.root_hash(None, grove_version).unwrap().unwrap(),
        );
        for i in 0u16..3 {
            assert_eq!(
                dest.dense_tree_get([TEST_LEAF].as_ref(), b"dense", i, None, grove_version)
                    .unwrap()
                    .expect("dest dense value"),
                Some(vec![i as u8; 32]),
                "dense value {i} must survive the sync"
            );
        }
        let dest_issues = dest
            .verify_grovedb(None, true, false, grove_version)
            .expect("dest verify_grovedb should run");
        assert!(dest_issues.is_empty(), "got: {:?}", dest_issues);
    }

    /// Entry payloads larger than the page byte budget force the transfer
    /// across multiple pages; the multi-page path must round-trip too.
    #[test]
    fn state_sync_mmr_tree_multi_page_round_trip() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"mmr_big",
                Element::empty_mmr_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert mmr tree");
        // Size each leaf from the page budget so any two leaves exceed one
        // page: the transfer is guaranteed to split into at least two pages
        // even if MAX_PAGE_BYTES is raised later.
        let leaf_size = crate::replication::non_merk_sync::MAX_PAGE_BYTES / 2 + 1;
        for i in 0u8..4 {
            source
                .mmr_tree_append(
                    [TEST_LEAF].as_ref(),
                    b"mmr_big",
                    vec![i; leaf_size],
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("append big mmr leaf");
        }

        let dest = sync_source_to_destination(&source, grove_version);

        assert_eq!(
            source.root_hash(None, grove_version).unwrap().unwrap(),
            dest.root_hash(None, grove_version).unwrap().unwrap(),
        );
        for i in 0u64..4 {
            assert_eq!(
                dest.mmr_tree_get_value([TEST_LEAF].as_ref(), b"mmr_big", i, None, grove_version)
                    .unwrap()
                    .expect("dest mmr leaf"),
                Some(vec![i as u8; leaf_size]),
                "big mmr leaf {i} must survive the sync"
            );
        }
        let dest_issues = dest
            .verify_grovedb(None, true, false, grove_version)
            .expect("dest verify_grovedb should run");
        assert!(dest_issues.is_empty(), "got: {:?}", dest_issues);
    }

    /// Drive the full sync loop while mutating the wire bytes of commitment
    /// tree pages. Every mutation must be rejected before the session can
    /// complete — the target recomputes the state root from the replayed
    /// payload and checks it against the parent binding.
    fn try_sync_with_ct_page_mutation(
        source: &TempGroveDb,
        grove_version: &GroveVersion,
        mutate_page: CtPageMutator,
    ) -> Result<(), crate::Error> {
        run_sync(source, grove_version, 64, Some(mutate_page)).map(|_| ())
    }

    /// Byzantine-source coverage: any tampering with commitment tree wire
    /// bytes — a flipped entry byte, a stripped frontier, a dropped entry —
    /// must fail the sync instead of committing corrupt state.
    #[test]
    fn state_sync_commitment_tree_tampered_pages_rejected() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"ct",
                Element::empty_commitment_tree(2).expect("valid chunk power"),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert commitment tree");
        for i in 1u8..=6 {
            source
                .commitment_tree_insert_raw(
                    [TEST_LEAF].as_ref(),
                    b"ct",
                    [i; 32],
                    [i.wrapping_add(100); 32],
                    [i.wrapping_add(200); 32],
                    vec![i; 216],
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert commitment tree note");
        }

        // Sanity: with the identity mutation the sync completes.
        try_sync_with_ct_page_mutation(&source, grove_version, &|more, aux, entries| {
            (more, aux, entries)
        })
        .expect("un-tampered sync must succeed");

        // 1. Flip one byte of one entry: the replayed payload no longer
        //    hashes to the bound state root.
        let err =
            try_sync_with_ct_page_mutation(&source, grove_version, &|more, aux, mut entries| {
                if let Some(first) = entries.first_mut() {
                    first[0] ^= 0x01;
                }
                (more, aux, entries)
            })
            .expect_err("flipped entry byte must be rejected");
        assert!(
            format!("{err:?}").contains("state root mismatch after replay"),
            "expected state-root rejection, got: {err:?}"
        );

        // 2. Strip the frontier from the first page.
        let err = try_sync_with_ct_page_mutation(&source, grove_version, &|more, _aux, entries| {
            (more, Vec::new(), entries)
        })
        .expect_err("stripped frontier must be rejected");
        assert!(
            format!("{err:?}").contains("missing the frontier"),
            "expected missing-frontier rejection, got: {err:?}"
        );

        // 3. Tamper with the frontier bytes: the recomputed sinsemilla root
        //    diverges from the one bound into ct_state.
        let err =
            try_sync_with_ct_page_mutation(&source, grove_version, &|more, mut aux, entries| {
                if !aux.is_empty() {
                    let last = aux.len() - 1;
                    aux[last] ^= 0x01;
                }
                (more, aux, entries)
            })
            .expect_err("tampered frontier must be rejected");
        let msg = format!("{err:?}");
        assert!(
            msg.contains("state root mismatch after replay")
                || msg.contains("commitment tree frontier is invalid")
                || msg.contains("cannot open commitment tree")
                || msg.contains("cannot compute commitment tree state root"),
            "expected frontier-integrity rejection, got: {msg}"
        );

        // 4. Drop the last entry while still claiming the page is final.
        let err =
            try_sync_with_ct_page_mutation(&source, grove_version, &|more, aux, mut entries| {
                if !more {
                    entries.pop();
                }
                (more, aux, entries)
            })
            .expect_err("dropped entry must be rejected");
        assert!(
            format!("{err:?}").contains("replay incomplete"),
            "expected incomplete-replay rejection, got: {err:?}"
        );
    }

    /// A Byzantine source can pad the serialized frontier with trailing
    /// bytes: `CommitmentFrontier::deserialize` tolerates them, so the
    /// Sinsemilla root — and therefore the bound state root — is unchanged.
    /// The target must still reject the page: it stores the frontier bytes
    /// verbatim, and their length feeds the V4 storage-cost accounting of
    /// every later frontier save, so accepting padded bytes would make the
    /// synced node's fee computation diverge from the network's.
    #[test]
    fn state_sync_commitment_tree_rejects_non_canonical_frontier() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"ct",
                Element::empty_commitment_tree(2).expect("valid chunk power"),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert commitment tree");
        for i in 1u8..=3 {
            source
                .commitment_tree_insert_raw(
                    [TEST_LEAF].as_ref(),
                    b"ct",
                    [i; 32],
                    [i.wrapping_add(100); 32],
                    [i.wrapping_add(200); 32],
                    vec![i; 216],
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert commitment tree note");
        }

        // Padded frontier: decodes to the genuine frontier, different bytes.
        let err =
            try_sync_with_ct_page_mutation(&source, grove_version, &|more, mut aux, entries| {
                if !aux.is_empty() {
                    aux.push(0x00);
                }
                (more, aux, entries)
            })
            .expect_err("padded frontier must be rejected");
        assert!(
            format!("{err:?}").contains("not canonically encoded"),
            "expected canonical-encoding rejection, got: {err:?}"
        );

        // Garbage that does not decode at all is rejected too.
        let err = try_sync_with_ct_page_mutation(&source, grove_version, &|more, aux, entries| {
            let aux = if aux.is_empty() {
                aux
            } else {
                vec![0x07, 0x07, 0x07]
            };
            (more, aux, entries)
        })
        .expect_err("undecodable frontier must be rejected");
        assert!(
            format!("{err:?}").contains("frontier is invalid"),
            "expected frontier-decoding rejection, got: {err:?}"
        );
    }

    /// An EMPTY commitment tree never has a stored frontier, so an honest
    /// source sends an empty aux section. A Byzantine source planting one
    /// must be rejected: the empty-tree state root is a constant that would
    /// never look at the planted bytes, yet the target's next append would
    /// load them.
    #[test]
    fn state_sync_empty_commitment_tree_rejects_planted_frontier() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"ct",
                Element::empty_commitment_tree(2).expect("valid chunk power"),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert empty commitment tree");

        // `[0x00]` is the canonical serialization of an EMPTY frontier: a
        // perfectly well-formed value that still must not be accepted.
        let err = try_sync_with_ct_page_mutation(&source, grove_version, &|more, _aux, entries| {
            (more, vec![0x00], entries)
        })
        .expect_err("planted frontier on an empty commitment tree must be rejected");
        assert!(
            format!("{err:?}").contains("must not carry a frontier"),
            "expected planted-frontier rejection, got: {err:?}"
        );
    }

    /// Non-Merk subtrees interleaved with the subtree-batch boundary:
    /// a batch size of 1 forces a transaction swap after every completed
    /// subtree, including append-only ones.
    #[test]
    fn state_sync_non_merk_trees_with_batch_size_one() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"ct",
                Element::empty_commitment_tree(2).expect("valid chunk power"),
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
                vec![9u8; 216],
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert commitment tree note");
        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"mmr",
                Element::empty_mmr_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert mmr tree");
        source
            .mmr_tree_append(
                [TEST_LEAF].as_ref(),
                b"mmr",
                b"leaf".to_vec(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("append mmr leaf");
        source
            .insert(
                [ANOTHER_TEST_LEAF].as_ref(),
                b"dense",
                Element::empty_dense_tree(4),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert dense tree");
        source
            .dense_tree_insert(
                [ANOTHER_TEST_LEAF].as_ref(),
                b"dense",
                vec![5u8; 32],
                None,
                grove_version,
            )
            .unwrap()
            .expect("dense insert");

        // Same sync loop as the shared driver but with subtrees_batch_size
        // of 1, exercising set_new_transaction between subtrees.
        let dest = run_sync(&source, grove_version, 1, None)
            .expect("state sync with batch size 1 should succeed");

        assert_eq!(
            source.root_hash(None, grove_version).unwrap().unwrap(),
            dest.root_hash(None, grove_version).unwrap().unwrap(),
        );
        let dest_issues = dest
            .verify_grovedb(None, true, false, grove_version)
            .expect("dest verify_grovedb should run");
        assert!(dest_issues.is_empty(), "got: {:?}", dest_issues);
    }

    /// Empty append-only trees of every type survive state sync: nothing
    /// to transfer, and verification reduces to the empty-tree state-root
    /// conventions (NULL_HASH for MMR / bulk / dense).
    #[test]
    fn state_sync_empty_non_merk_trees_round_trip() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);

        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"mmr_empty",
                Element::empty_mmr_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert empty mmr tree");
        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"bulk_empty",
                Element::empty_bulk_append_tree(2).expect("valid chunk power"),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert empty bulk tree");
        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"dense_empty",
                Element::empty_dense_tree(4),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert empty dense tree");

        let dest = sync_source_to_destination(&source, grove_version);

        assert_eq!(
            source.root_hash(None, grove_version).unwrap().unwrap(),
            dest.root_hash(None, grove_version).unwrap().unwrap(),
        );
        let dest_issues = dest
            .verify_grovedb(None, true, false, grove_version)
            .expect("dest verify_grovedb should run");
        assert!(dest_issues.is_empty(), "got: {:?}", dest_issues);
    }

    /// Direct misuse/malformed-input coverage for the non-Merk restorer:
    /// every wire-level validation must reject before touching storage.
    #[test]
    fn non_merk_restorer_rejects_malformed_input() {
        use crate::replication::non_merk_sync::{
            encode_non_merk_page, NonMerkChunkId, NonMerkRestorer,
        };

        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let tx = db.start_transaction();
        let path: Vec<Vec<u8>> = vec![TEST_LEAF.to_vec(), b"ct".to_vec()];

        // Non append-only elements are rejected outright.
        let err = NonMerkRestorer::new(Element::new_item(b"nope".to_vec()), [0u8; 32], [1u8; 32])
            .expect_err("item element must be rejected");
        assert!(
            format!("{err:?}").contains("non append-only"),
            "got: {err:?}"
        );

        // A commitment tree declaring 3 entries with chunk_power 2.
        let mut restorer =
            NonMerkRestorer::new(Element::CommitmentTree(3, 2, None), [0u8; 32], [1u8; 32])
                .expect("valid CT element");
        let frontier = b"opaque frontier bytes".to_vec();

        // Malformed cursor: wrong length.
        let page = encode_non_merk_page(false, frontier.clone(), vec![b"e".to_vec()])
            .expect("encode page");
        let err = restorer
            .apply_page(&db, &tx, &path, &[0u8; 3], &page, grove_version)
            .expect_err("short chunk id must be rejected");
        assert!(format!("{err:?}").contains("17 bytes"), "got: {err:?}");

        // Out-of-order cursor: wrong start position.
        let bad_id = NonMerkChunkId {
            start: 1,
            state: 3,
            param: 2,
        }
        .encode();
        let err = restorer
            .apply_page(&db, &tx, &path, &bad_id, &page, grove_version)
            .expect_err("out-of-order cursor must be rejected");
        assert!(format!("{err:?}").contains("out of order"), "got: {err:?}");

        let good_id = restorer.initial_chunk_id();

        // Empty page data cannot even be decoded.
        let err = restorer
            .apply_page(&db, &tx, &path, &good_id, &[], grove_version)
            .expect_err("empty page must be rejected");
        assert!(
            format!("{err:?}").contains("missing more-flag"),
            "got: {err:?}"
        );

        // A page claiming more data but carrying no entries would loop
        // forever; it must be rejected.
        let page = encode_non_merk_page(true, frontier.clone(), vec![]).expect("encode page");
        let err = restorer
            .apply_page(&db, &tx, &path, &good_id, &page, grove_version)
            .expect_err("more-without-entries must be rejected");
        assert!(
            format!("{err:?}").contains("carries no entries"),
            "got: {err:?}"
        );

        // More entries than the element declares.
        let too_many: Vec<Vec<u8>> = (0u8..4).map(|i| vec![i; 8]).collect();
        let page = encode_non_merk_page(false, frontier.clone(), too_many).expect("encode page");
        let err = restorer
            .apply_page(&db, &tx, &path, &good_id, &page, grove_version)
            .expect_err("entry overflow must be rejected");
        assert!(format!("{err:?}").contains("overflows"), "got: {err:?}");

        // A populated commitment tree page 0 without the frontier.
        let page =
            encode_non_merk_page(false, Vec::new(), vec![b"e".to_vec()]).expect("encode page");
        let err = restorer
            .apply_page(&db, &tx, &path, &good_id, &page, grove_version)
            .expect_err("missing frontier must be rejected");
        assert!(
            format!("{err:?}").contains("missing the frontier"),
            "got: {err:?}"
        );

        // Finalizing before all entries arrived is rejected.
        let err = restorer
            .finalize(&db, &tx, &path, grove_version)
            .expect_err("incomplete replay must be rejected");
        assert!(
            format!("{err:?}").contains("replay incomplete"),
            "got: {err:?}"
        );

        // Aux data is only valid on a commitment tree's first page; any
        // other tree type must reject it.
        let mmr_path: Vec<Vec<u8>> = vec![TEST_LEAF.to_vec(), b"mmr".to_vec()];
        let mut mmr_restorer =
            NonMerkRestorer::new(Element::MmrTree(0, None), [0u8; 32], [1u8; 32])
                .expect("valid MMR element");
        let page = encode_non_merk_page(false, b"bogus aux".to_vec(), vec![]).expect("encode page");
        let err = mmr_restorer
            .apply_page(
                &db,
                &tx,
                &mmr_path,
                &mmr_restorer.initial_chunk_id(),
                &page,
                grove_version,
            )
            .expect_err("aux on a non-CT page must be rejected");
        assert!(
            format!("{err:?}").contains("unexpected aux"),
            "got: {err:?}"
        );

        // A page arriving after the final page is rejected.
        let dense_path: Vec<Vec<u8>> = vec![TEST_LEAF.to_vec(), b"dense".to_vec()];
        let mut dense_restorer = NonMerkRestorer::new(
            Element::DenseAppendOnlyFixedSizeTree(0, 4, None),
            [0u8; 32],
            [1u8; 32],
        )
        .expect("valid dense element");
        let final_page = encode_non_merk_page(false, Vec::new(), vec![]).expect("encode page");
        let dense_id = dense_restorer.initial_chunk_id();
        dense_restorer
            .apply_page(&db, &tx, &dense_path, &dense_id, &final_page, grove_version)
            .expect("final page applies");
        let err = dense_restorer
            .apply_page(&db, &tx, &dense_path, &dense_id, &final_page, grove_version)
            .expect_err("page after final must be rejected");
        assert!(
            format!("{err:?}").contains("after the final page"),
            "got: {err:?}"
        );
    }

    /// A populated bidirectional-reference graph round-trips through state
    /// sync: the chunk producer emits a `BidirectionalReference` stored in
    /// a normal tree as a `KVValueHash` node (via the `KvRefValueHash`
    /// mapping), which the restorer must accept under the plain-reference
    /// trust model — its value hash embeds the resolved end-of-chain hash,
    /// which is not locally derivable. The item variants restore through
    /// the recompute-checked `KVValueHashFeatureType` path.
    #[test]
    fn state_sync_populated_bidirectional_reference_graph_round_trip() {
        use crate::{
            bidirectional_references::BidirectionalReference, reference_path::ReferencePathType,
        };

        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);
        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"value",
                Element::new_item_allowing_bidirectional_references(b"hello".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .unwrap();
        for (key, target) in [(b"r1".as_slice(), b"value".as_slice()), (b"r2", b"r1")] {
            source
                .insert(
                    [TEST_LEAF].as_ref(),
                    key,
                    Element::BidirectionalReference(BidirectionalReference {
                        forward_reference_path: ReferencePathType::SiblingReference(
                            target.to_vec(),
                        ),
                        backward_references: Vec::new(),
                        cascade_on_update: true,
                        max_hop: None,
                        flags: None,
                    }),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .unwrap();
        }

        let source_root_hash = source.root_hash(None, grove_version).unwrap().unwrap();
        let dest = sync_source_to_destination(&source, grove_version);
        assert_eq!(
            source_root_hash,
            dest.root_hash(None, grove_version).unwrap().unwrap(),
            "destination root hash should match source after full sync"
        );
        assert!(
            dest.verify_grovedb(None, true, true, grove_version)
                .unwrap()
                .is_empty(),
            "the restored graph must verify, referrer lists included"
        );
    }
}
