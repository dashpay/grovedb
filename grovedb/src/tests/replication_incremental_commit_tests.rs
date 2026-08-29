//! Bounded-memory (`RestoreCommitMode::Incremental`) restore.
//!
//! The default restore is atomic: nothing reaches the destination until
//! `commit_session` has verified the root hash, which costs a resident
//! copy of the entire state being restored (see
//! `replication_scale_tests`). The incremental mode buys a memory
//! ceiling that does not grow with the state by committing at points the
//! session proves are safe, and pays for it by giving up the rollback.
//!
//! These tests pin the three things that trade depends on: the
//! intermediate commits actually happen and produce the same grove; they
//! never land while an indexed group is only half restored; and a
//! destination left half-restored is detectably poisoned rather than
//! silently plausible.

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use grovedb_version::version::GroveVersion;
    use tempfile::TempDir;

    use crate::{
        replication::{RestoreCommitMode, CURRENT_STATE_SYNC_VERSION},
        tests::{make_empty_grovedb, make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, GroveDb,
    };

    /// One byte, i.e. "a commit is due at every safe point there is".
    ///
    /// Every incremental test here uses it rather than a realistic budget:
    /// the interesting question is never whether a large budget
    /// eventually trips, it is whether the *safety* conditions hold when
    /// it trips as often as it possibly can.
    const COMMIT_AT_EVERY_SAFE_POINT: RestoreCommitMode =
        RestoreCommitMode::Incremental { budget_bytes: 1 };

    /// What a driven restore's session did along the way.
    struct SyncOutcome {
        intermediate_commits: usize,
        /// How many due-and-otherwise-safe commits the session held back
        /// because an indexed group was still in flight.
        commits_deferred_for_open_group: usize,
    }

    /// Checkpoint `source`, restore it into `dest` under `commit_mode`,
    /// and report what the session did.
    ///
    /// `dest` is supplied by the caller rather than created here so tests
    /// that need to close and reopen the restored grove can own its
    /// directory.
    ///
    fn run_incremental_sync_into(
        source: &TempGroveDb,
        dest: &GroveDb,
        grove_version: &GroveVersion,
        subtrees_batch_size: usize,
        commit_mode: RestoreCommitMode,
    ) -> Result<SyncOutcome, crate::Error> {
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

        let mut session = dest.start_snapshot_syncing_with_mode(
            app_hash,
            subtrees_batch_size,
            CURRENT_STATE_SYNC_VERSION,
            commit_mode,
            grove_version,
        )?;

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

        assert!(session.is_sync_completed(), "sync should have completed");
        let intermediate_commits = session.intermediate_commits();
        let commits_deferred_for_open_group = session.commits_deferred_for_open_group();
        dest.commit_session(session, grove_version)?;

        Ok(SyncOutcome {
            intermediate_commits,
            commits_deferred_for_open_group,
        })
    }

    /// [`run_incremental_sync_into`] with a throwaway destination, for the
    /// tests that never reopen it.
    fn run_incremental_sync(
        source: &TempGroveDb,
        grove_version: &GroveVersion,
        subtrees_batch_size: usize,
        commit_mode: RestoreCommitMode,
    ) -> Result<(TempGroveDb, SyncOutcome), crate::Error> {
        let dest = make_empty_grovedb();
        let outcome =
            run_incremental_sync_into(source, &dest, grove_version, subtrees_batch_size, commit_mode)?;
        Ok((dest, outcome))
    }

    /// A grove with enough separate subtrees that a one-byte budget has
    /// many safe points to fire at, plus items under each so the restore
    /// is not degenerate.
    fn multi_subtree_source(grove_version: &GroveVersion) -> TempGroveDb {
        let source = make_test_grovedb(grove_version);
        for tree in 0..6u8 {
            let key = [b'c', b'0' + tree];
            source
                .insert(
                    [TEST_LEAF].as_ref(),
                    &key,
                    Element::empty_tree(),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("create subtree");
            for i in 0..25u32 {
                source
                    .insert(
                        [TEST_LEAF, &key].as_ref(),
                        &i.to_be_bytes(),
                        Element::new_item(vec![tree; 64]),
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

    /// [`multi_subtree_source`] with two indexed trees added, so a
    /// primary-plus-secondaries group is in flight while safe points come
    /// and go around it.
    fn indexed_source(grove_version: &GroveVersion) -> TempGroveDb {
        let source = multi_subtree_source(grove_version);
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
        for k in [b"a" as &[u8], b"b", b"c", b"d"] {
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
            source
                .insert(
                    [TEST_LEAF, b"pcit", k].as_ref(),
                    b"leaf",
                    Element::new_item(k.to_vec()),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("populate PCIT entry");
            source
                .insert_into_provable_sum_indexed_tree(
                    [TEST_LEAF, b"psit"].as_ref(),
                    k,
                    Element::new_sum_item(k[0] as i64),
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert PSIT entry");
        }
        source
    }

    /// The default mode must remain exactly what it was: one transaction,
    /// no early writes, whatever the byte volume.
    #[test]
    fn atomic_mode_takes_no_intermediate_commits() {
        let grove_version = GroveVersion::latest();
        let source = indexed_source(grove_version);

        let (dest, outcome) =
            run_incremental_sync(&source, grove_version, 2, RestoreCommitMode::Atomic)
                .expect("atomic sync should succeed");

        assert_eq!(
            outcome.intermediate_commits, 0,
            "atomic mode must never commit before commit_session"
        );
        assert_eq!(
            dest.root_hash(None, grove_version).unwrap().unwrap(),
            source.root_hash(None, grove_version).unwrap().unwrap(),
        );
        assert!(!dest.has_incomplete_restore().unwrap());
    }

    /// The incremental mode commits repeatedly and still lands on the same
    /// grove.
    #[test]
    fn incremental_mode_commits_early_and_restores_the_same_grove() {
        let grove_version = GroveVersion::latest();
        let source = multi_subtree_source(grove_version);

        let (dest, outcome) =
            run_incremental_sync(&source, grove_version, 64, COMMIT_AT_EVERY_SAFE_POINT)
                .expect("incremental sync should succeed");

        assert!(
            outcome.intermediate_commits > 0,
            "a one-byte budget over a multi-subtree grove must reach at least one safe point; \
             otherwise this test proves nothing about incremental mode"
        );
        assert_eq!(
            dest.root_hash(None, grove_version).unwrap().unwrap(),
            source.root_hash(None, grove_version).unwrap().unwrap(),
            "an incrementally committed restore must produce the same root hash"
        );
        assert_eq!(
            dest
                .get([TEST_LEAF, b"c3"].as_ref(), &7u32.to_be_bytes(), None, grove_version)
                .unwrap()
                .expect("restored item should be readable"),
            Element::new_item(vec![3u8; 64]),
        );
    }

    /// A subtree-count batch boundary is not the only lever any more: the
    /// byte budget must produce commits even when `subtrees_batch_size` is
    /// far larger than the number of subtrees in the grove, which is the
    /// shape Platform state actually has (few, fat subtrees).
    #[test]
    fn byte_budget_commits_when_the_subtree_count_never_reaches_the_batch_size() {
        let grove_version = GroveVersion::latest();
        let source = multi_subtree_source(grove_version);

        // 10_000 subtrees per batch over a grove with well under a dozen:
        // the subtree counter can never trip this.
        let (dest, outcome) =
            run_incremental_sync(&source, grove_version, 10_000, COMMIT_AT_EVERY_SAFE_POINT)
                .expect("incremental sync should succeed");

        assert!(
            outcome.intermediate_commits > 0,
            "the payload budget must be able to close a discovery batch on its own"
        );
        assert_eq!(
            dest.root_hash(None, grove_version).unwrap().unwrap(),
            source.root_hash(None, grove_version).unwrap().unwrap(),
        );
    }

    /// The group-splitting guarantee.
    ///
    /// An indexed subtree is bound to its parent only by the joint check
    /// over the primary's and every secondary's restored root hash, so an
    /// intermediate commit must never land between a group's members.
    ///
    /// With a one-byte budget a commit is due at literally every
    /// boundary, which makes the guard the only thing standing between
    /// the sync and a split group. The proof it is doing real work is
    /// `commits_deferred_for_open_group`: a commit that was due, at a
    /// boundary where nothing else objected, refused purely because a
    /// group was open. If that count were zero the test would be
    /// describing a situation that never arises; asserting it is non-zero
    /// is what stops this from being vacuous. Deleting the
    /// `indexed_groups.is_empty()` clause from
    /// `intermediate_commit_decision` turns every one of those deferrals
    /// into a split-group commit, and `intermediate_commit`'s own hard
    /// refusal then fails the sync outright.
    #[test]
    fn never_splits_an_indexed_group_across_a_commit() {
        let grove_version = GroveVersion::latest();
        let source = indexed_source(grove_version);

        for batch_size in [1usize, 2, 3, 64] {
            let (dest, outcome) =
                run_incremental_sync(&source, grove_version, batch_size, COMMIT_AT_EVERY_SAFE_POINT)
                    .unwrap_or_else(|e| {
                        panic!("incremental sync with batch size {batch_size} should succeed: {e}")
                    });

            assert!(
                outcome.commits_deferred_for_open_group > 0,
                "batch size {batch_size}: no commit was ever held back for an open indexed \
                 group, so this test proves nothing about the guard"
            );
            assert!(
                outcome.intermediate_commits > 0,
                "batch size {batch_size}: no commit was taken at all"
            );
            assert_eq!(
                dest.root_hash(None, grove_version).unwrap().unwrap(),
                source.root_hash(None, grove_version).unwrap().unwrap(),
            );
        }
    }

    /// A restore abandoned after its first intermediate commit leaves the
    /// destination poisoned, and says so.
    #[test]
    fn abandoned_incremental_restore_marks_the_database() {
        let grove_version = GroveVersion::latest();
        let source = multi_subtree_source(grove_version);

        let checkpoint_dir = TempDir::new().expect("temp dir");
        let checkpoint_path = checkpoint_dir.path().join("checkpoint");
        source
            .create_checkpoint(&checkpoint_path)
            .expect("create checkpoint");
        let checkpoint_db = GroveDb::open(&checkpoint_path).expect("open checkpoint");
        let app_hash = checkpoint_db
            .root_hash(None, grove_version)
            .unwrap()
            .unwrap();

        let dest = make_empty_grovedb();
        assert!(
            !dest.has_incomplete_restore().unwrap(),
            "a fresh grove is not mid-restore"
        );

        let mut session = dest
            .start_snapshot_syncing_with_mode(
                app_hash,
                64,
                CURRENT_STATE_SYNC_VERSION,
                COMMIT_AT_EVERY_SAFE_POINT,
                grove_version,
            )
            .expect("start syncing");

        // Drive only until the first intermediate commit, then walk away
        // exactly as a crashed or cancelled restore would.
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
                .expect("fetch chunk");
            let more = session
                .apply_chunk(
                    chunk_id.as_slice(),
                    &chunk_data,
                    CURRENT_STATE_SYNC_VERSION,
                    grove_version,
                )
                .expect("apply chunk");
            if session.intermediate_commits() > 0 {
                break;
            }
            chunk_queue.extend(more);
        }
        assert!(
            session.intermediate_commits() > 0,
            "the restore must have committed something for this test to mean anything"
        );
        drop(session);

        assert!(
            dest.has_incomplete_restore().unwrap(),
            "an abandoned incremental restore must leave the database marked as unusable"
        );
        // Not a root-hash comparison: the root subtree is restored first
        // and already commits to its children's hashes, so `root_hash`
        // matches the target long before the children exist. Absence of a
        // leaf that the completed restore does have is the honest signal.
        assert!(
            matches!(
                dest.get(
                    [TEST_LEAF, b"c5"].as_ref(),
                    &24u32.to_be_bytes(),
                    None,
                    grove_version
                )
                .unwrap(),
                Err(crate::Error::PathParentLayerNotFound(_))
                    | Err(crate::Error::PathKeyNotFound(_))
                    | Err(crate::Error::PathNotFound(_))
            ),
            "the abandoned restore is genuinely incomplete, not accidentally finished"
        );
    }

    /// The marker is scoped to the window it describes: a restore that
    /// runs to a verified commit leaves none behind, and the mark survives
    /// reopening the database while it is set.
    #[test]
    fn completed_incremental_restore_clears_the_marker() {
        let grove_version = GroveVersion::latest();
        let source = multi_subtree_source(grove_version);

        let dest_dir = TempDir::new().expect("temp dir");
        let dest = GroveDb::open(dest_dir.path()).expect("open destination");
        let outcome =
            run_incremental_sync_into(&source, &dest, grove_version, 64, COMMIT_AT_EVERY_SAFE_POINT)
                .expect("incremental sync should succeed");
        assert!(outcome.intermediate_commits > 0);
        assert!(
            !dest.has_incomplete_restore().unwrap(),
            "a verified restore must not leave the destination marked"
        );

        // And the flag is durable, not process state.
        drop(dest);
        let reopened = GroveDb::open(dest_dir.path()).expect("reopen restored grove");
        assert!(!reopened.has_incomplete_restore().unwrap());
    }

    /// The final root hash check still gates the last commit in
    /// incremental mode — losing the rollback must not mean losing the
    /// check.
    #[test]
    fn incremental_mode_still_refuses_a_root_hash_mismatch() {
        let grove_version = GroveVersion::latest();
        let source = multi_subtree_source(grove_version);

        let checkpoint_dir = TempDir::new().expect("temp dir");
        let checkpoint_path = checkpoint_dir.path().join("checkpoint");
        source
            .create_checkpoint(&checkpoint_path)
            .expect("create checkpoint");
        let checkpoint_db = GroveDb::open(&checkpoint_path).expect("open checkpoint");
        let real_hash = checkpoint_db
            .root_hash(None, grove_version)
            .unwrap()
            .unwrap();

        let dest = make_empty_grovedb();
        // Claim a different app hash than the source will actually
        // produce; the chunk stream is genuine, only the target is a lie.
        let mut wrong_hash = real_hash;
        wrong_hash[0] ^= 0xff;

        let mut session = dest
            .start_snapshot_syncing_with_mode(
                real_hash,
                64,
                CURRENT_STATE_SYNC_VERSION,
                COMMIT_AT_EVERY_SAFE_POINT,
                grove_version,
            )
            .expect("start syncing");

        let mut chunk_queue: VecDeque<Vec<u8>> = VecDeque::new();
        chunk_queue.push_back(real_hash.to_vec());
        while let Some(chunk_id) = chunk_queue.pop_front() {
            let chunk_data = checkpoint_db
                .fetch_chunk(
                    chunk_id.as_slice(),
                    None,
                    CURRENT_STATE_SYNC_VERSION,
                    grove_version,
                )
                .expect("fetch chunk");
            let more = session
                .apply_chunk(
                    chunk_id.as_slice(),
                    &chunk_data,
                    CURRENT_STATE_SYNC_VERSION,
                    grove_version,
                )
                .expect("apply chunk");
            chunk_queue.extend(more);
        }
        assert!(session.intermediate_commits() > 0);

        // Rewrite the session's expectation to the wrong hash so the
        // final check has something to reject, exactly as a byzantine
        // source's stream would.
        session.set_app_hash_for_test(wrong_hash);
        let err = dest
            .commit_session(session, grove_version)
            .expect_err("a root hash mismatch must still be refused");
        assert!(
            format!("{err}").contains("root hash mismatch"),
            "unexpected error: {err}"
        );
        assert!(
            dest.has_incomplete_restore().unwrap(),
            "a refused final commit leaves the earlier intermediate commits behind, and the \
             database must stay marked so the caller discards it"
        );
    }
}
