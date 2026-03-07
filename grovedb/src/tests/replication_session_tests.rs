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

        dest.commit_session(session)
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
}
