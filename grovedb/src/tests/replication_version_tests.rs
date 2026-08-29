//! Protocol-version handling of state sync.
//!
//! State sync speaks exactly one protocol version
//! ([`crate::replication::CURRENT_STATE_SYNC_VERSION`]); the constant and
//! the `version` parameters exist so a future incompatible wire change
//! can bump it and old/new peers fail fast with a clear error. These
//! tests pin the properties that make that safe:
//!
//! - Every entry point on both sides — `start_snapshot_syncing` and
//!   `apply_chunk` on the target, `fetch_chunk` on the source — rejects
//!   any other version with a descriptive error naming both versions.
//! - The rejection is clean: a destination that never got past it is left
//!   byte-for-byte untouched and verifiably uncorrupted.
//! - A session refuses chunks applied at a version different from its
//!   own, so one sync can never mix protocol versions midway.

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use grovedb_version::version::GroveVersion;
    use tempfile::TempDir;

    use crate::{
        replication::CURRENT_STATE_SYNC_VERSION,
        tests::{make_empty_grovedb, make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, GroveDb,
    };

    /// A checkpoint of `source`, opened the way a serving peer would.
    struct SourcePeer {
        db: GroveDb,
        _dir: TempDir,
    }

    impl SourcePeer {
        fn new(source: &TempGroveDb) -> Self {
            let dir = TempDir::new().expect("temp dir for checkpoint");
            let path = dir.path().join("checkpoint");
            source
                .create_checkpoint(&path)
                .expect("should create checkpoint");
            SourcePeer {
                db: GroveDb::open(&path).expect("should open checkpoint db"),
                _dir: dir,
            }
        }

        fn app_hash(&self, grove_version: &GroveVersion) -> [u8; 32] {
            self.db
                .root_hash(None, grove_version)
                .unwrap()
                .expect("checkpoint root hash")
        }
    }

    /// Drive a full sync from `peer` into `dest` at `version`.
    fn sync_from_peer(
        peer: &SourcePeer,
        dest: &TempGroveDb,
        version: u16,
        grove_version: &GroveVersion,
    ) -> Result<(), crate::Error> {
        let app_hash = peer.app_hash(grove_version);
        let mut session = dest.start_snapshot_syncing(app_hash, 64, version, grove_version)?;

        let mut queue: VecDeque<Vec<u8>> = VecDeque::new();
        queue.push_back(app_hash.to_vec());

        while let Some(chunk_id) = queue.pop_front() {
            let chunk_data = peer
                .db
                .fetch_chunk(&chunk_id, None, version, grove_version)?;
            let more = session.apply_chunk(&chunk_id, &chunk_data, version, grove_version)?;
            queue.extend(more);
        }

        if !session.is_sync_completed() {
            return Err(crate::Error::InternalError(
                "sync did not complete".to_string(),
            ));
        }
        dest.commit_session(session, grove_version)
    }

    /// A grove covering both transfer modes: Merk chunks and non-Merk
    /// entry replay.
    fn make_source(grove_version: &GroveVersion) -> TempGroveDb {
        let source = make_test_grovedb(grove_version);
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
            .expect("insert subtree");
        for i in 0u8..8 {
            source
                .insert(
                    [TEST_LEAF, b"sub"].as_ref(),
                    &[i],
                    Element::new_item(vec![i; 32]),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert item");
        }
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
            .expect("insert mmr");
        for i in 0u8..4 {
            source
                .mmr_tree_append(
                    [TEST_LEAF].as_ref(),
                    b"mmr",
                    vec![i; 16],
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("append mmr leaf");
        }
        source
    }

    /// Assert the destination is byte-for-byte the pristine empty grove it
    /// was before the failed sync: a rejected sync must never leave a
    /// half-restored subtree behind.
    fn assert_destination_untouched(
        dest: &TempGroveDb,
        before: [u8; 32],
        grove_version: &GroveVersion,
    ) {
        assert_eq!(
            dest.root_hash(None, grove_version).unwrap().unwrap(),
            before,
            "a failed sync must leave the destination root hash unchanged"
        );
        let issues = dest
            .verify_grovedb(None, true, false, grove_version)
            .expect("destination verify_grovedb should run");
        assert!(
            issues.is_empty(),
            "a failed sync must not leave the destination corrupt, got: {issues:?}"
        );
    }

    /// Any version other than the one this build speaks is rejected on
    /// every entry point, on both sides, with a descriptive error naming
    /// the offered and the supported version — and the destination is
    /// left untouched.
    #[test]
    fn unsupported_versions_are_rejected_on_both_sides() {
        let grove_version = GroveVersion::latest();
        let source = make_source(grove_version);
        let peer = SourcePeer::new(&source);
        let dest = make_empty_grovedb();
        let app_hash = peer.app_hash(grove_version);
        let before = dest.root_hash(None, grove_version).unwrap().unwrap();

        for bad in [0u16, CURRENT_STATE_SYNC_VERSION + 1, 99, u16::MAX] {
            // Target side: the session refuses to start.
            // `MultiStateSyncSession` is not `Debug`, so the `Ok` arm has
            // to be destructured rather than `expect_err`ed.
            let Err(err) = dest.start_snapshot_syncing(app_hash, 64, bad, grove_version) else {
                panic!("version {bad} should not have started a session");
            };
            let msg = format!("{err}");
            assert!(
                msg.contains("Unsupported state sync protocol version")
                    && msg.contains(&format!("{bad}"))
                    && msg.contains(&format!("{CURRENT_STATE_SYNC_VERSION}")),
                "version {bad}: the error should name both versions, got {msg}"
            );

            // Source side: the peer refuses to serve.
            let err = peer
                .db
                .fetch_chunk(&app_hash, None, bad, grove_version)
                .expect_err("an unsupported version cannot fetch a chunk");
            assert!(
                format!("{err}").contains("Unsupported state sync protocol version"),
                "version {bad}: got {err}"
            );

            // Target side, mid-session: a started session refuses too.
            let mut session = dest
                .start_snapshot_syncing(app_hash, 64, CURRENT_STATE_SYNC_VERSION, grove_version)
                .expect("start a session at the current version");
            let chunk = peer
                .db
                .fetch_chunk(&app_hash, None, CURRENT_STATE_SYNC_VERSION, grove_version)
                .expect("the peer serves the root chunk at the current version");
            let err = session
                .apply_chunk(&app_hash, &chunk, bad, grove_version)
                .expect_err("an unsupported version cannot apply a chunk");
            assert!(
                format!("{err}").contains("Unsupported state sync protocol version"),
                "version {bad}: got {err}"
            );
        }

        assert_destination_untouched(&dest, before, grove_version);
    }

    /// A session must refuse chunks applied at a version different from
    /// its own even when the wire version itself is supported. Mixing
    /// versions within one session is what this guard exists to stop; it
    /// becomes reachable through the public API the moment a future bump
    /// makes more than one version constructible.
    #[test]
    fn apply_chunk_rejects_a_version_that_differs_from_the_session() {
        let grove_version = GroveVersion::latest();
        let source = make_source(grove_version);
        let peer = SourcePeer::new(&source);
        let dest = make_empty_grovedb();
        let app_hash = peer.app_hash(grove_version);

        // `start_syncing_session` is the raw constructor and does not
        // validate the version — pin the session to a different one so
        // the wire version below passes the supported check but fails
        // the session-consistency check.
        let mut session = dest.start_syncing_session(app_hash, 64, CURRENT_STATE_SYNC_VERSION + 1);
        let chunk = peer
            .db
            .fetch_chunk(&app_hash, None, CURRENT_STATE_SYNC_VERSION, grove_version)
            .expect("the peer serves the root chunk at the current version");

        let err = session
            .apply_chunk(&app_hash, &chunk, CURRENT_STATE_SYNC_VERSION, grove_version)
            .expect_err("a session must reject a chunk applied at a different version");
        assert!(
            format!("{err}").contains("does not match the session's version"),
            "got: {err}"
        );
    }

    /// Sanity anchor for the rejections above: the same grove round-trips
    /// cleanly at the current version, so the failures are about version
    /// handling and not about the fixture.
    #[test]
    fn the_fixture_round_trips_at_the_current_version() {
        let grove_version = GroveVersion::latest();
        let source = make_source(grove_version);
        let peer = SourcePeer::new(&source);
        let dest = make_empty_grovedb();

        sync_from_peer(&peer, &dest, CURRENT_STATE_SYNC_VERSION, grove_version)
            .expect("a sync at the current version must succeed");

        assert_eq!(
            source.root_hash(None, grove_version).unwrap().unwrap(),
            dest.root_hash(None, grove_version).unwrap().unwrap(),
        );
        let issues = dest
            .verify_grovedb(None, true, false, grove_version)
            .expect("destination verify_grovedb should run");
        assert!(issues.is_empty(), "got: {issues:?}");
    }
}
