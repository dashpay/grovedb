//! Cross-version and source-availability behaviour of state sync.
//!
//! The existing session suite covers the **v1 client against a v2 source**
//! direction (`state_sync_v1_client_round_trip_against_v2_source`). These
//! tests pin the reverse — a **v2 client against a source that only
//! speaks v1** — which is the direction a network actually takes during a
//! rollout: new nodes come up first and have to fetch from peers that have
//! not upgraded yet.
//!
//! Three things are worth pinning there, and none of them were covered:
//!
//! - For a grove without indexed trees the two protocol versions produce
//!   *the same bytes*, so a v2 client can sync from a v1 peer. That is a
//!   compatibility guarantee, not an accident, and a future wire change
//!   would silently break it without a test.
//! - For a grove *with* indexed trees the v1 source refuses, and the
//!   client must surface that refusal as a descriptive error while
//!   leaving the destination exactly as it was — the atomic-restore
//!   invariant applies to a failed sync too.
//! - A peer that refuses version 2 outright (the old build's equality
//!   check against its own `CURRENT_STATE_SYNC_VERSION`) must produce the
//!   same clean outcome.

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

    /// The protocol version a not-yet-upgraded peer speaks.
    const V1: u16 = 1;
    /// The protocol version this build's client starts a session at.
    const V2: u16 = 2;

    /// How the simulated remote peer answers a chunk request.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum SourceBehaviour {
        /// The peer serves version 1 semantics and rejects anything else,
        /// exactly as a build predating dual-version serving did: its
        /// check was equality against its own `CURRENT_STATE_SYNC_VERSION`
        /// (then 1), not membership in a supported set.
        V1Only,
        /// The peer refuses every request outright, whatever the version.
        RefuseEverything,
    }

    /// One simulated remote peer: a checkpoint plus a serving policy.
    struct SimulatedPeer {
        db: GroveDb,
        behaviour: SourceBehaviour,
        _dir: TempDir,
    }

    impl SimulatedPeer {
        fn new(source: &TempGroveDb, behaviour: SourceBehaviour) -> Self {
            let dir = TempDir::new().expect("temp dir for checkpoint");
            let path = dir.path().join("checkpoint");
            source
                .create_checkpoint(&path)
                .expect("should create checkpoint");
            SimulatedPeer {
                db: GroveDb::open(&path).expect("should open checkpoint db"),
                behaviour,
                _dir: dir,
            }
        }

        fn app_hash(&self, grove_version: &GroveVersion) -> [u8; 32] {
            self.db
                .root_hash(None, grove_version)
                .unwrap()
                .expect("checkpoint root hash")
        }

        /// Answer a chunk request the way this peer's build would.
        fn fetch_chunk(
            &self,
            chunk_id: &[u8],
            requested_version: u16,
            grove_version: &GroveVersion,
        ) -> Result<Vec<u8>, crate::Error> {
            match self.behaviour {
                SourceBehaviour::RefuseEverything => Err(crate::Error::CorruptedData(
                    "Unsupported state sync protocol version".to_string(),
                )),
                SourceBehaviour::V1Only => {
                    if requested_version != V1 {
                        // What an old build did with a v2 request.
                        return Err(crate::Error::CorruptedData(
                            "Unsupported state sync protocol version".to_string(),
                        ));
                    }
                    self.db.fetch_chunk(chunk_id, None, V1, grove_version)
                }
            }
        }
    }

    /// Drive a full sync from `peer` into `dest`, with the client speaking
    /// `client_version` in its session and asking the peer for
    /// `requested_version` on the wire.
    ///
    /// Splitting the two is the whole point: a v2 client talking to a v1
    /// peer negotiates *down* to v1 on the wire, and the interesting
    /// question is what its v2 session does with the answers.
    fn sync_from_peer(
        peer: &SimulatedPeer,
        dest: &TempGroveDb,
        client_version: u16,
        requested_version: u16,
        grove_version: &GroveVersion,
    ) -> Result<(), crate::Error> {
        let app_hash = peer.app_hash(grove_version);
        let mut session =
            dest.start_snapshot_syncing(app_hash, 64, client_version, grove_version)?;

        let mut queue: VecDeque<Vec<u8>> = VecDeque::new();
        queue.push_back(app_hash.to_vec());

        while let Some(chunk_id) = queue.pop_front() {
            let chunk_data = peer.fetch_chunk(&chunk_id, requested_version, grove_version)?;
            let more =
                session.apply_chunk(&chunk_id, &chunk_data, client_version, grove_version)?;
            queue.extend(more);
        }

        if !session.is_sync_completed() {
            return Err(crate::Error::InternalError(
                "sync did not complete".to_string(),
            ));
        }
        dest.commit_session(session, grove_version)
    }

    /// A grove with no indexed trees, but covering both transfer modes a
    /// v1 peer does support: Merk chunks and non-Merk entry replay.
    fn source_without_indexed_trees(grove_version: &GroveVersion) -> TempGroveDb {
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

    /// The same, plus a populated `ProvableCountIndexedTree` — the one
    /// thing a v1 peer cannot serve.
    fn source_with_indexed_tree(grove_version: &GroveVersion) -> TempGroveDb {
        let source = source_without_indexed_trees(grove_version);
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
        for k in [b"a".as_ref(), b"b".as_ref()] {
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
        }
        source
    }

    /// Assert the destination is byte-for-byte the pristine empty grove it
    /// was before the failed sync: a rejected sync must roll its whole
    /// transaction back, never leave a half-restored subtree behind.
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

    // ---------- v2 client against a v1-only source ----------

    /// The compatibility guarantee: for a grove with no indexed trees the
    /// v1 and v2 wire formats coincide, so a v2 client can complete a sync
    /// against a peer that has not upgraded.
    #[test]
    fn v2_client_syncs_from_a_v1_only_source_without_indexed_trees() {
        let grove_version = GroveVersion::latest();
        let source = source_without_indexed_trees(grove_version);
        let peer = SimulatedPeer::new(&source, SourceBehaviour::V1Only);
        let dest = make_empty_grovedb();

        sync_from_peer(&peer, &dest, V2, V1, grove_version)
            .expect("a v2 client must be able to sync a non-indexed grove from a v1 peer");

        assert_eq!(
            source.root_hash(None, grove_version).unwrap().unwrap(),
            dest.root_hash(None, grove_version).unwrap().unwrap(),
        );
        let issues = dest
            .verify_grovedb(None, true, false, grove_version)
            .expect("destination verify_grovedb should run");
        assert!(issues.is_empty(), "got: {issues:?}");
    }

    /// The refusal path: the v2 client reaches the indexed subtree, asks
    /// the v1 peer for its header page, and the peer refuses. The client
    /// must surface a descriptive `NotSupported` — not a root-hash
    /// mismatch three layers later — and must leave the destination
    /// untouched.
    #[test]
    fn v2_client_against_a_v1_only_source_fails_descriptively_on_an_indexed_tree() {
        let grove_version = GroveVersion::latest();
        let source = source_with_indexed_tree(grove_version);
        let peer = SimulatedPeer::new(&source, SourceBehaviour::V1Only);
        let dest = make_empty_grovedb();
        let before = dest.root_hash(None, grove_version).unwrap().unwrap();

        let err = sync_from_peer(&peer, &dest, V2, V1, grove_version)
            .expect_err("a v1 peer cannot serve an indexed tree to a v2 client");

        assert!(
            matches!(err, crate::Error::NotSupported(_)),
            "expected Error::NotSupported, got: {err:?}"
        );
        let msg = format!("{err}");
        assert!(
            msg.contains("indexed") && msg.contains("protocol version 2"),
            "the error should name indexed trees and the required version, got: {msg}"
        );
        assert_destination_untouched(&dest, before, grove_version);
    }

    /// A peer that refuses version 2 outright — the equality check an
    /// old build performed against its own current version. The client
    /// must fail on the very first request with the peer's own message
    /// and commit nothing.
    #[test]
    fn v2_client_against_a_source_refusing_v2_fails_on_the_first_request() {
        let grove_version = GroveVersion::latest();
        let source = source_without_indexed_trees(grove_version);
        let peer = SimulatedPeer::new(&source, SourceBehaviour::V1Only);
        let dest = make_empty_grovedb();
        let before = dest.root_hash(None, grove_version).unwrap().unwrap();

        // The client does NOT negotiate down: it asks for v2 and the v1
        // peer rejects it.
        let err = sync_from_peer(&peer, &dest, V2, V2, grove_version)
            .expect_err("a v1-only peer must reject a v2 chunk request");
        assert!(
            format!("{err}").contains("Unsupported state sync protocol version"),
            "got: {err}"
        );
        assert_destination_untouched(&dest, before, grove_version);
    }

    /// A peer that refuses everything: the failure surfaces immediately
    /// and nothing is written.
    #[test]
    fn v2_client_against_a_source_refusing_everything_commits_nothing() {
        let grove_version = GroveVersion::latest();
        let source = source_without_indexed_trees(grove_version);
        let peer = SimulatedPeer::new(&source, SourceBehaviour::RefuseEverything);
        let dest = make_empty_grovedb();
        let before = dest.root_hash(None, grove_version).unwrap().unwrap();

        sync_from_peer(&peer, &dest, V2, V2, grove_version)
            .expect_err("a peer refusing every request cannot complete a sync");
        assert_destination_untouched(&dest, before, grove_version);
    }

    // ---------- session-version guards on the client ----------

    /// A v2 session must refuse chunks presented as v1 even when the bytes
    /// would have been valid. Mixing versions within one session is what
    /// this guard exists to stop.
    #[test]
    fn apply_chunk_rejects_a_version_that_differs_from_the_session() {
        let grove_version = GroveVersion::latest();
        let source = source_without_indexed_trees(grove_version);
        let peer = SimulatedPeer::new(&source, SourceBehaviour::V1Only);
        let dest = make_empty_grovedb();
        let app_hash = peer.app_hash(grove_version);

        let mut session = dest
            .start_snapshot_syncing(app_hash, 64, V2, grove_version)
            .expect("start a v2 session");
        let chunk = peer
            .fetch_chunk(&app_hash, V1, grove_version)
            .expect("v1 peer serves the root chunk");

        let err = session
            .apply_chunk(&app_hash, &chunk, V1, grove_version)
            .expect_err("a v2 session must reject a chunk applied as v1");
        assert!(
            format!("{err}").contains("does not match the session's version"),
            "got: {err}"
        );
    }

    /// Versions outside the supported set are rejected before the session
    /// version is even consulted, on both the client and the source.
    #[test]
    fn unsupported_versions_are_rejected_on_both_sides() {
        let grove_version = GroveVersion::latest();
        let source = source_without_indexed_trees(grove_version);
        let peer = SimulatedPeer::new(&source, SourceBehaviour::V1Only);
        let dest = make_empty_grovedb();
        let app_hash = peer.app_hash(grove_version);

        for bad in [0u16, 3, u16::MAX] {
            // `MultiStateSyncSession` is not `Debug`, so the `Ok` arm has
            // to be destructured rather than `expect_err`ed.
            let Err(err) = dest.start_snapshot_syncing(app_hash, 64, bad, grove_version) else {
                panic!("version {bad} should not have started a session");
            };
            assert!(
                format!("{err}").contains("Unsupported state sync protocol version"),
                "version {bad}: got {err}"
            );

            let err = peer
                .db
                .fetch_chunk(&app_hash, None, bad, grove_version)
                .expect_err("an unsupported version cannot fetch a chunk");
            assert!(
                format!("{err}").contains("Unsupported state sync protocol version"),
                "version {bad}: got {err}"
            );
        }

        // And on an already-started v2 session.
        let mut session = dest
            .start_snapshot_syncing(app_hash, 64, V2, grove_version)
            .expect("start a v2 session");
        let chunk = peer
            .fetch_chunk(&app_hash, V1, grove_version)
            .expect("v1 peer serves the root chunk");
        let err = session
            .apply_chunk(&app_hash, &chunk, 3, grove_version)
            .expect_err("an unsupported version cannot apply a chunk");
        assert!(
            format!("{err}").contains("Unsupported state sync protocol version"),
            "got: {err}"
        );
    }

    /// Sanity anchor for the pair above: at matching versions the same
    /// grove round-trips at v2 as well as at v1, so the failures above are
    /// about version handling and not about the fixture.
    #[test]
    fn the_same_grove_round_trips_at_both_supported_versions() {
        let grove_version = GroveVersion::latest();
        let source = source_without_indexed_trees(grove_version);
        let expected = source.root_hash(None, grove_version).unwrap().unwrap();

        for version in [V1, CURRENT_STATE_SYNC_VERSION] {
            let peer = SimulatedPeer::new(&source, SourceBehaviour::V1Only);
            let dest = make_empty_grovedb();
            // `V1Only` serves v1 bytes; the client runs its session at
            // `version`, which is exactly the mixed-version case for v2.
            sync_from_peer(&peer, &dest, version, V1, grove_version)
                .unwrap_or_else(|e| panic!("sync at client version {version} failed: {e}"));
            assert_eq!(
                dest.root_hash(None, grove_version).unwrap().unwrap(),
                expected,
                "client version {version} restored a different root hash"
            );
        }
    }
}
