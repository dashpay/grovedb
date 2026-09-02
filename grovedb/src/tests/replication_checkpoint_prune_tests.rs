//! What happens to an in-flight sync when the source prunes the snapshot
//! underneath it.
//!
//! At the GroveDB layer a "snapshot" is just a RocksDB checkpoint: a plain
//! directory of hard-linked SSTs, with no pin, lease, or refcount tying it
//! to the sessions reading it. Nothing here stops an operator (or a
//! snapshot-retention policy above this layer) from deleting that
//! directory while a slow consumer is still fetching from it, so the
//! question these tests answer is what the source does next.
//!
//! The safety property is narrow and absolute: the source may keep
//! serving, or it may start failing, but it must never serve a chunk that
//! restores to something other than the app hash the sync was offered.
//! Both permitted outcomes are asserted; the one that actually occurs is
//! then pinned exactly, so a change in behaviour shows up as a test
//! failure rather than as a silent difference in production.

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, path::Path};

    use grovedb_version::version::GroveVersion;
    use tempfile::TempDir;

    use crate::{
        replication::CURRENT_STATE_SYNC_VERSION,
        tests::{make_empty_grovedb, make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, GroveDb,
    };

    /// A source with enough subtrees and entries that the fetch/apply loop
    /// takes many round trips — the deletion has to land *mid-sync* to
    /// probe anything.
    fn multi_round_trip_source(grove_version: &GroveVersion) -> TempGroveDb {
        let source = make_test_grovedb(grove_version);
        for t in 0u8..8 {
            let name = [b's', t];
            source
                .insert(
                    [TEST_LEAF].as_ref(),
                    &name,
                    Element::empty_tree(),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert subtree");
            for i in 0u16..200 {
                source
                    .insert(
                        [TEST_LEAF, &name].as_ref(),
                        &i.to_be_bytes(),
                        Element::new_item(vec![t; 64]),
                        None,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("insert item");
            }
        }
        source
    }

    /// What a sync did when the checkpoint vanished under it.
    #[derive(Debug, PartialEq, Eq)]
    enum PruneOutcome {
        /// The source kept serving from its open file handles and the sync
        /// finished with the correct app hash.
        CompletedCorrectly,
        /// The source (or the client) failed cleanly.
        Failed(String),
    }

    /// Sync `dest` from the checkpoint at `checkpoint_path`, deleting that
    /// directory after `delete_after` successful fetches.
    fn sync_deleting_checkpoint_midway(
        checkpoint_db: &GroveDb,
        checkpoint_path: &Path,
        dest: &TempGroveDb,
        app_hash: [u8; 32],
        delete_after: usize,
        grove_version: &GroveVersion,
    ) -> PruneOutcome {
        let mut session = match dest.start_snapshot_syncing(
            app_hash,
            64,
            CURRENT_STATE_SYNC_VERSION,
            grove_version,
        ) {
            Ok(session) => session,
            Err(e) => return PruneOutcome::Failed(format!("{e}")),
        };

        let mut queue: VecDeque<Vec<u8>> = VecDeque::new();
        queue.push_back(app_hash.to_vec());
        let mut fetches = 0usize;
        let mut deleted = false;

        while let Some(chunk_id) = queue.pop_front() {
            let chunk = match checkpoint_db.fetch_chunk(
                &chunk_id,
                None,
                CURRENT_STATE_SYNC_VERSION,
                grove_version,
            ) {
                Ok(chunk) => chunk,
                Err(e) => return PruneOutcome::Failed(format!("{e}")),
            };
            fetches += 1;
            if !deleted && fetches >= delete_after {
                std::fs::remove_dir_all(checkpoint_path)
                    .expect("the checkpoint directory should be removable while open");
                assert!(
                    !checkpoint_path.exists(),
                    "the checkpoint directory should be gone"
                );
                deleted = true;
            }
            match session.apply_chunk(&chunk_id, &chunk, CURRENT_STATE_SYNC_VERSION, grove_version)
            {
                Ok(more) => queue.extend(more),
                Err(e) => return PruneOutcome::Failed(format!("{e}")),
            }
        }

        assert!(deleted, "the probe never reached the deletion point");
        if !session.is_sync_completed() {
            return PruneOutcome::Failed("sync did not complete".to_string());
        }
        match dest.commit_session(session, grove_version) {
            Ok(()) => PruneOutcome::CompletedCorrectly,
            Err(e) => PruneOutcome::Failed(format!("{e}")),
        }
    }

    /// Deleting the checkpoint directory between `fetch_chunk` calls does
    /// not disturb an in-flight sync.
    ///
    /// This is the POSIX unlink semantics both macOS and Linux give: the
    /// open `GroveDb` holds descriptors to every SST it needs, and an
    /// unlinked file stays readable through an open descriptor until the
    /// last one closes. The directory entry is gone (see
    /// `reopening_a_pruned_checkpoint_silently_creates_an_empty_grove` for
    /// what a *fresh* open of that path does instead), but the
    /// already-open source keeps serving correct chunks, so the sync
    /// completes and the restored root hash matches.
    ///
    /// The operational consequence, and the reason this is pinned rather
    /// than assumed: pruning a snapshot directory is **not** a way to cut
    /// off a slow consumer. Whatever holds the source `GroveDb` open has
    /// to be dropped for the disk space to come back or for the peer to
    /// be disconnected.
    #[test]
    fn deleting_the_checkpoint_mid_sync_keeps_serving_correct_chunks() {
        let grove_version = GroveVersion::latest();
        let source = multi_round_trip_source(grove_version);

        let dir = TempDir::new().expect("temp dir");
        let checkpoint_path = dir.path().join("checkpoint");
        source
            .create_checkpoint(&checkpoint_path)
            .expect("create checkpoint");
        let checkpoint_db = GroveDb::open(&checkpoint_path).expect("open checkpoint db");
        let app_hash = checkpoint_db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("checkpoint root hash");
        assert_eq!(
            app_hash,
            source.root_hash(None, grove_version).unwrap().unwrap()
        );

        let dest = make_empty_grovedb();
        let outcome = sync_deleting_checkpoint_midway(
            &checkpoint_db,
            &checkpoint_path,
            &dest,
            app_hash,
            2,
            grove_version,
        );

        assert_eq!(
            outcome,
            PruneOutcome::CompletedCorrectly,
            "an open source keeps serving from unlinked SSTs; if this now fails, the \
             behaviour changed and the failure must still be clean (never a wrong root hash)"
        );
        assert_eq!(
            dest.root_hash(None, grove_version).unwrap().unwrap(),
            app_hash,
            "the restored root hash must equal the offered app hash"
        );
        let issues = dest
            .verify_grovedb(None, true, false, grove_version)
            .expect("destination verify_grovedb should run");
        assert!(issues.is_empty(), "got: {issues:?}");
    }

    /// The other half of the same fact, and the sharp edge in it.
    ///
    /// A source that opens its snapshot **lazily, per request** does not
    /// fail closed on a pruned checkpoint: `GroveDb::open` runs with
    /// RocksDB's `create_if_missing`, so opening the now-nonexistent path
    /// silently creates a brand-new **empty** grove there. Such a source
    /// would then answer chunk requests for the empty grove rather than
    /// erroring.
    ///
    /// That is not a corruption hole — the client offered a specific
    /// `app_hash`, and an empty grove's root hash is not it, so the sync
    /// fails — but it is a silent recreation, and any caller that treats
    /// "the snapshot directory opened fine" as "the snapshot is still
    /// there" is wrong. Callers must hold the source `GroveDb` open for
    /// the life of a sync (which is what the tutorial pattern and
    /// `run_sync` do) rather than reopening it by path.
    #[test]
    fn reopening_a_pruned_checkpoint_silently_creates_an_empty_grove() {
        let grove_version = GroveVersion::latest();
        let source = make_test_grovedb(grove_version);
        source
            .insert(
                [TEST_LEAF].as_ref(),
                b"k",
                Element::new_item(b"v".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert item");

        let dir = TempDir::new().expect("temp dir");
        let checkpoint_path = dir.path().join("checkpoint");
        source
            .create_checkpoint(&checkpoint_path)
            .expect("create checkpoint");
        let app_hash = GroveDb::open(&checkpoint_path)
            .expect("open checkpoint db")
            .root_hash(None, grove_version)
            .unwrap()
            .expect("checkpoint root hash");

        std::fs::remove_dir_all(&checkpoint_path).expect("remove checkpoint");

        let reopened = GroveDb::open(&checkpoint_path)
            .expect("create_if_missing means a pruned path still 'opens'");
        let reopened_hash = reopened
            .root_hash(None, grove_version)
            .unwrap()
            .expect("root hash of the recreated grove");
        assert_ne!(
            reopened_hash, app_hash,
            "the recreated grove must not claim the pruned snapshot's app hash"
        );
        assert_eq!(
            reopened_hash,
            make_empty_grovedb()
                .root_hash(None, grove_version)
                .unwrap()
                .unwrap(),
            "the reopened path is an empty grove, not the pruned snapshot"
        );

        // And the client-side protection: a session offered the original
        // app hash cannot be satisfied from that empty grove.
        let dest = make_empty_grovedb();
        let outcome = {
            let mut session = dest
                .start_snapshot_syncing(app_hash, 64, CURRENT_STATE_SYNC_VERSION, grove_version)
                .expect("start session");
            reopened
                .fetch_chunk(&app_hash, None, CURRENT_STATE_SYNC_VERSION, grove_version)
                .and_then(|chunk| {
                    session
                        .apply_chunk(&app_hash, &chunk, CURRENT_STATE_SYNC_VERSION, grove_version)
                        .map(|_| ())
                })
        };
        assert!(
            outcome.is_err(),
            "an empty grove must not be able to answer for another grove's app hash"
        );
    }

    /// A pruned checkpoint whose source handle is then dropped: the
    /// destination of a sync that never committed is untouched. Pins that
    /// an abandoned session leaves nothing behind, which is what makes
    /// retrying against another peer safe.
    #[test]
    fn an_abandoned_sync_against_a_pruned_checkpoint_leaves_the_destination_empty() {
        let grove_version = GroveVersion::latest();
        let source = multi_round_trip_source(grove_version);

        let dir = TempDir::new().expect("temp dir");
        let checkpoint_path = dir.path().join("checkpoint");
        source
            .create_checkpoint(&checkpoint_path)
            .expect("create checkpoint");
        let checkpoint_db = GroveDb::open(&checkpoint_path).expect("open checkpoint db");
        let app_hash = checkpoint_db
            .root_hash(None, grove_version)
            .unwrap()
            .expect("checkpoint root hash");

        let dest = make_empty_grovedb();
        let before = dest.root_hash(None, grove_version).unwrap().unwrap();

        {
            let mut session = dest
                .start_snapshot_syncing(app_hash, 64, CURRENT_STATE_SYNC_VERSION, grove_version)
                .expect("start session");
            let chunk = checkpoint_db
                .fetch_chunk(&app_hash, None, CURRENT_STATE_SYNC_VERSION, grove_version)
                .expect("first chunk");
            session
                .apply_chunk(&app_hash, &chunk, CURRENT_STATE_SYNC_VERSION, grove_version)
                .expect("apply first chunk");
            std::fs::remove_dir_all(&checkpoint_path).expect("prune the checkpoint");
            // Session dropped here without committing.
        }
        drop(checkpoint_db);

        assert_eq!(
            dest.root_hash(None, grove_version).unwrap().unwrap(),
            before,
            "an abandoned sync must leave the destination root hash unchanged"
        );
        let issues = dest
            .verify_grovedb(None, true, false, grove_version)
            .expect("destination verify_grovedb should run");
        assert!(issues.is_empty(), "got: {issues:?}");
    }
}
