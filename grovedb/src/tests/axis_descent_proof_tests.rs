//! End-to-end tests for axis-ordered reads embedded in the GroveDBProof
//! V1 envelope (`ProofBytes::IndexedTreeAxisDescent`): round trips for
//! every traversal, cross-checks against the standalone envelopes and
//! the trusted reads, absence-authenticated branched reads, forgery
//! rejections, and the GROVE_V4 gates.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::query::{AggregateFold, AxisQuery, IndexAxis};
    use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

    use crate::{
        operations::proof::{
            indexed_axis::AxisEntries, AxisDescentProof, GroveDBProof, LayerProof, ProofBytes,
            VerifiedPathQuery,
        },
        tests::{make_test_grovedb, TEST_LEAF},
        Element, Error, GroveDb, PathQuery,
    };

    // -----------------------------------------------------------------
    // Fixtures (same shapes as the standalone-envelope suites)
    // -----------------------------------------------------------------

    const PSIT_ENTRIES: &[(&[u8], i64)] = &[
        (b"alice", 40),
        (b"bob", -10),
        (b"carol", 25),
        (b"dave", 40),
        (b"erin", 5),
    ];

    fn build_psit(db: &GroveDb, grove_version: &GroveVersion, entries: &[(&[u8], i64)]) {
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
        for (k, s) in entries {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                k,
                Element::new_sum_item(*s),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PSIT entry");
        }
    }

    fn build_pcpsit(db: &GroveDb, grove_version: &GroveVersion, entries: &[(&[u8], i64)]) {
        // All three axes so the queried-axis digest reconstruction
        // exercises other_axes_root_hashes.
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![(0, None), (1, None), (2, None)];
        let elem =
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("axes canonical");
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            elem,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPSIT");
        for (k, sum) in entries {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(b"v".to_vec(), *sum),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCPSIT entry");
        }
    }

    fn build_branched_psits(
        db: &GroveDb,
        grove_version: &GroveVersion,
        branches: &[(&[u8], &[(&[u8], i64)])],
    ) {
        for (branch, entries) in branches {
            db.insert(
                [TEST_LEAF].as_ref(),
                branch,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create branch tree");
            db.insert(
                [TEST_LEAF, branch].as_ref(),
                b"scores",
                Element::empty_provable_sum_indexed_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create branch PSIT");
            for (k, s) in *entries {
                db.insert_into_provable_sum_indexed_tree(
                    [TEST_LEAF, branch, b"scores".as_slice()].as_ref(),
                    k,
                    Element::new_sum_item(*s),
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert branch PSIT entry");
            }
        }
    }

    fn psit_path() -> Vec<Vec<u8>> {
        vec![TEST_LEAF.to_vec(), b"psit".to_vec()]
    }

    fn root_hash(db: &GroveDb, grove_version: &GroveVersion) -> [u8; 32] {
        db.root_hash(None, grove_version).unwrap().expect("root")
    }

    fn prove(db: &GroveDb, path_query: &PathQuery, grove_version: &GroveVersion) -> Vec<u8> {
        db.prove_query(path_query, None, grove_version)
            .unwrap()
            .expect("prove axis path query")
    }

    fn entries_as_sum(entries: &AxisEntries) -> &[(i64, Vec<u8>)] {
        match entries {
            AxisEntries::Sum(entries) => entries,
            other => panic!("expected sum entries, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Round trips
    // -----------------------------------------------------------------

    #[test]
    fn top_k_round_trip_matches_reads_and_standalone_envelope() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);

        let path_query = PathQuery::new_axis_top_k(psit_path(), IndexAxis::Sum, 3, 0, true);
        let proof = prove(&db, &path_query, grove_version);

        let verified = GroveDb::verify_path_query(&proof, &path_query, grove_version)
            .expect("embedded top-k proof must verify");
        let VerifiedPathQuery::AxisEntries {
            root_hash: verified_root,
            entries,
            skipped,
        } = verified
        else {
            panic!("expected AxisEntries");
        };
        assert_eq!(verified_root, root_hash(&db, grove_version));
        assert_eq!(skipped, Some(0));

        // Equal to the trusted read...
        let direct = db
            .indexed_sum_top_k_paginated(
                [TEST_LEAF, b"psit"].as_ref(),
                3,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("direct read");
        assert_eq!(entries_as_sum(&entries), direct.as_slice());

        // ...and to the standalone envelope over the same state.
        let standalone_bytes = db
            .prove_indexed_sum_top_k([TEST_LEAF, b"psit"].as_ref(), 3, true, None, grove_version)
            .unwrap()
            .expect("standalone prove");
        let standalone = GroveDb::verify_indexed_sum_top_k(
            &standalone_bytes,
            &[TEST_LEAF, b"psit"],
            3,
            true,
            grove_version,
        )
        .expect("standalone verify");
        assert_eq!(standalone.root_hash, verified_root);
        assert_eq!(
            entries_as_sum(&standalone.entries),
            entries_as_sum(&entries)
        );
    }

    #[test]
    fn paginated_bounded_rank_and_aggregate_round_trips() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);
        let root = root_hash(&db, grove_version);

        // Paginated top-k: skip 2, take 2, descending.
        let pq = PathQuery::new_axis_top_k(psit_path(), IndexAxis::Sum, 2, 2, true);
        match GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
            .expect("paginated verifies")
        {
            VerifiedPathQuery::AxisEntries {
                root_hash,
                entries,
                skipped,
            } => {
                assert_eq!(root_hash, root);
                assert_eq!(skipped, Some(2));
                let direct = db
                    .indexed_sum_top_k_paginated(
                        [TEST_LEAF, b"psit"].as_ref(),
                        2,
                        2,
                        true,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("direct");
                assert_eq!(entries_as_sum(&entries), direct.as_slice());
            }
            other => panic!("expected AxisEntries, got {other:?}"),
        }

        // Bounded: sums in [0, 40] ascending, capped.
        let pq = PathQuery::new_axis_bounded(psit_path(), IndexAxis::Sum, 0, 40, 10, false);
        match GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
            .expect("bounded verifies")
        {
            VerifiedPathQuery::AxisEntries {
                root_hash,
                entries,
                skipped,
            } => {
                assert_eq!(root_hash, root);
                assert_eq!(skipped, None);
                let direct = db
                    .indexed_sum_range(
                        [TEST_LEAF, b"psit"].as_ref(),
                        0,
                        40,
                        false,
                        10,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("direct");
                assert_eq!(entries_as_sum(&entries), direct.as_slice());
            }
            other => panic!("expected AxisEntries, got {other:?}"),
        }

        // Rank of key, both directions, cross-checked against the
        // standalone rank prover.
        for descending in [true, false] {
            let pq = PathQuery::new_axis_rank_of_key(
                psit_path(),
                IndexAxis::Sum,
                b"carol".to_vec(),
                descending,
            );
            let (_, expected_rank) = db
                .prove_indexed_axis_rank_of_key(
                    [TEST_LEAF, b"psit"].as_ref(),
                    IndexAxis::Sum,
                    b"carol",
                    descending,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("standalone rank");
            match GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
                .expect("rank verifies")
            {
                VerifiedPathQuery::AxisRank { root_hash, rank } => {
                    assert_eq!(root_hash, root);
                    assert_eq!(rank, expected_rank, "descending={descending}");
                }
                other => panic!("expected AxisRank, got {other:?}"),
            }
        }

        // Aggregate over the value range over the sum axis.
        let pq = PathQuery::new_axis_aggregate_over_value_range(
            psit_path(),
            IndexAxis::Sum,
            0,
            40,
            AggregateFold::Total,
        );
        match GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
            .expect("aggregate verifies")
        {
            VerifiedPathQuery::AxisAggregate { root_hash, value } => {
                assert_eq!(root_hash, root);
                let direct = db
                    .indexed_sum_aggregate_over_value_range(
                        [TEST_LEAF, b"psit"].as_ref(),
                        0,
                        40,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("direct");
                assert_eq!(value, direct as i128);
            }
            other => panic!("expected AxisAggregate, got {other:?}"),
        }
    }

    #[test]
    fn pcpsit_multi_axis_round_trip_exercises_axes_digest() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(&db, grove_version, PSIT_ENTRIES);
        let root = root_hash(&db, grove_version);

        // Query the SUM axis of a three-axis PCPSIT: the verifier must
        // rebuild the axes digest from the two other axes' carried
        // roots plus the RECOMPUTED sum-secondary root.
        let path = vec![TEST_LEAF.to_vec(), b"pcpsit".to_vec()];
        let pq = PathQuery::new_axis_top_k(path, IndexAxis::Sum, 2, 0, true);
        match GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
            .expect("PCPSIT axis proof verifies")
        {
            VerifiedPathQuery::AxisEntries {
                root_hash, entries, ..
            } => {
                assert_eq!(root_hash, root);
                let direct = db
                    .indexed_sum_top_k_paginated(
                        [TEST_LEAF, b"pcpsit"].as_ref(),
                        2,
                        0,
                        true,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("direct");
                assert_eq!(entries_as_sum(&entries), direct.as_slice());
            }
            other => panic!("expected AxisEntries, got {other:?}"),
        }
    }

    #[test]
    fn branched_round_trip_authenticates_absence() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_branched_psits(
            &db,
            grove_version,
            &[
                (b"alice", &[(b"m1", 10), (b"m2", 30)]),
                (b"carol", &[(b"m1", 7)]),
            ],
        );
        let root = root_hash(&db, grove_version);

        let pq = PathQuery::new_branched_axis(
            vec![TEST_LEAF.to_vec()],
            vec![b"alice".to_vec(), b"bob".to_vec(), b"carol".to_vec()],
            vec![b"scores".to_vec()],
            AxisQuery::top_k(IndexAxis::Sum, 2, 0, true),
        );
        let proof = prove(&db, &pq, grove_version);
        match GroveDb::verify_path_query(&proof, &pq, grove_version)
            .expect("branched proof verifies")
        {
            VerifiedPathQuery::BranchedAxisEntries {
                root_hash,
                branches,
            } => {
                assert_eq!(root_hash, root);
                assert_eq!(branches.len(), 3);
                for (branch_key, entries) in &branches {
                    match branch_key.as_slice() {
                        b"bob" => assert!(entries.is_none(), "bob is proven absent"),
                        present => {
                            let direct = db
                                .indexed_sum_top_k_paginated(
                                    [TEST_LEAF, present, b"scores".as_slice()].as_ref(),
                                    2,
                                    0,
                                    true,
                                    None,
                                    grove_version,
                                )
                                .unwrap()
                                .expect("direct");
                            assert_eq!(
                                entries_as_sum(entries.as_ref().expect("present")),
                                direct.as_slice()
                            );
                        }
                    }
                }
            }
            other => panic!("expected BranchedAxisEntries, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Forgeries
    // -----------------------------------------------------------------

    /// Decode → mutate the terminal axis-descent payload → re-encode.
    fn tamper_axis_payload(proof: &[u8], mutate: impl FnOnce(&mut AxisDescentProof)) -> Vec<u8> {
        let config = bincode::config::standard().with_big_endian();
        let (mut decoded, _): (GroveDBProof, usize) =
            bincode::decode_from_slice(proof, config).expect("decode envelope");
        let GroveDBProof::V1(ref mut v1) = decoded else {
            panic!("expected V1 envelope");
        };
        fn find_descent(layer: &mut LayerProof) -> Option<&mut LayerProof> {
            if matches!(layer.merk_proof, ProofBytes::IndexedTreeAxisDescent(_)) {
                return Some(layer);
            }
            layer.lower_layers.values_mut().find_map(find_descent)
        }
        let descent = find_descent(&mut v1.root_layer).expect("envelope has an axis descent");
        let ProofBytes::IndexedTreeAxisDescent(bytes) = &descent.merk_proof else {
            unreachable!();
        };
        let mut payload = AxisDescentProof::decode_canonical(bytes).expect("decode payload");
        mutate(&mut payload);
        descent.merk_proof =
            ProofBytes::IndexedTreeAxisDescent(payload.encode_canonical().expect("re-encode"));
        bincode::encode_to_vec(&decoded, config).expect("re-encode envelope")
    }

    #[test]
    fn forged_payload_fields_are_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);
        let pq = PathQuery::new_axis_top_k(psit_path(), IndexAxis::Sum, 3, 0, true);
        let proof = prove(&db, &pq, grove_version);

        // Sanity: untampered verifies.
        GroveDb::verify_path_query(&proof, &pq, grove_version).expect("untampered verifies");

        // Forged primary root: part of the combine_hash_three preimage.
        let tampered = tamper_axis_payload(&proof, |payload| {
            payload.primary_root_hash[0] ^= 0xff;
        });
        GroveDb::verify_path_query(&tampered, &pq, grove_version)
            .expect_err("forged primary root must fail the parent binding");

        // Relabeled axis tag: must not survive.
        let tampered = tamper_axis_payload(&proof, |payload| {
            payload.axis_tag = IndexAxis::Count.tag();
        });
        GroveDb::verify_path_query(&tampered, &pq, grove_version)
            .expect_err("relabeled axis tag must be rejected");

        // Tampered secondary proof bytes: the recomputed secondary root
        // changes, so the binding fails even though the bytes decode.
        let tampered = tamper_axis_payload(&proof, |payload| {
            let len = payload.secondary_proof.len();
            payload.secondary_proof[len / 2] ^= 0x01;
        });
        GroveDb::verify_path_query(&tampered, &pq, grove_version)
            .expect_err("tampered secondary proof must be rejected");

        // Smuggled other-axes list on a single-secondary target.
        let tampered = tamper_axis_payload(&proof, |payload| {
            payload.other_axes_root_hashes.push((0, [7u8; 32]));
        });
        GroveDb::verify_path_query(&tampered, &pq, grove_version)
            .expect_err("other-axes on a PCIT/PSIT target must be rejected");
    }

    #[test]
    fn lying_about_the_rank_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);
        let pq =
            PathQuery::new_axis_rank_of_key(psit_path(), IndexAxis::Sum, b"carol".to_vec(), true);
        let proof = prove(&db, &pq, grove_version);
        GroveDb::verify_path_query(&proof, &pq, grove_version).expect("honest rank verifies");

        // Claiming a different rank: the count commitments in the
        // carried secondary proof attest the true skip, so a lying echo
        // cannot verify.
        let tampered = tamper_axis_payload(&proof, |payload| {
            payload.rank = Some(payload.rank.expect("rank present") + 1);
        });
        GroveDb::verify_path_query(&tampered, &pq, grove_version)
            .expect_err("a lying rank echo must be rejected");
    }

    #[test]
    fn hiding_a_present_branch_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_branched_psits(
            &db,
            grove_version,
            &[(b"alice", &[(b"m1", 10)]), (b"carol", &[(b"m1", 7)])],
        );
        let pq = PathQuery::new_branched_axis(
            vec![TEST_LEAF.to_vec()],
            vec![b"alice".to_vec(), b"carol".to_vec()],
            vec![b"scores".to_vec()],
            AxisQuery::top_k(IndexAxis::Sum, 1, 0, true),
        );
        let proof = prove(&db, &pq, grove_version);
        GroveDb::verify_path_query(&proof, &pq, grove_version).expect("honest branched verifies");

        // Strip carol's whole branch descent: the branching layer still
        // proves carol PRESENT, so the verifier must reject rather than
        // reporting her branch as absent.
        let config = bincode::config::standard().with_big_endian();
        let (mut decoded, _): (GroveDBProof, usize) =
            bincode::decode_from_slice(&proof, config).expect("decode envelope");
        let GroveDBProof::V1(ref mut v1) = decoded else {
            panic!("expected V1 envelope");
        };
        fn strip_key(layer: &mut LayerProof, key: &[u8]) -> bool {
            if layer.lower_layers.remove(key).is_some() {
                return true;
            }
            layer
                .lower_layers
                .values_mut()
                .any(|lower| strip_key(lower, key))
        }
        assert!(
            strip_key(&mut v1.root_layer, b"carol".as_slice()),
            "fixture must contain carol's branch layer"
        );
        let tampered = bincode::encode_to_vec(&decoded, config).expect("re-encode");
        GroveDb::verify_path_query(&tampered, &pq, grove_version)
            .expect_err("hiding a present branch must be rejected");
    }

    #[test]
    fn hiding_a_present_but_empty_branch_is_rejected() {
        // Audit finding: a branch whose indexed tree exists but is EMPTY
        // fails `is_non_empty_tree`, so before the axis check was hoisted
        // above the lower-layer lookup, stripping its axis layer slipped
        // every guard and the verifier endorsed a false `None`
        // ("proven absent") slot under the genuine root hash.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_branched_psits(
            &db,
            grove_version,
            &[
                (b"alice", &[] as &[(&[u8], i64)]), // created, NO entries
                (b"carol", &[(b"m1", 7)]),
            ],
        );
        let pq = PathQuery::new_branched_axis(
            vec![TEST_LEAF.to_vec()],
            vec![b"alice".to_vec(), b"carol".to_vec()],
            vec![b"scores".to_vec()],
            AxisQuery::top_k(IndexAxis::Sum, 1, 0, true),
        );
        let proof = prove(&db, &pq, grove_version);

        // Honest verification reports alice PRESENT with empty entries,
        // never absent.
        match GroveDb::verify_path_query(&proof, &pq, grove_version)
            .expect("honest empty-branch proof verifies")
        {
            VerifiedPathQuery::BranchedAxisEntries { branches, .. } => {
                let alice = branches
                    .iter()
                    .find(|(key, _)| key == b"alice")
                    .expect("alice slot");
                assert!(
                    matches!(&alice.1, Some(entries) if entries.is_empty()),
                    "an existing empty indexed tree must report Some(empty), got {:?}",
                    alice.1
                );
            }
            other => panic!("expected BranchedAxisEntries, got {other:?}"),
        }

        // Stripping alice's axis layer must be rejected, not endorsed as
        // absence.
        let config = bincode::config::standard().with_big_endian();
        let (mut decoded, _): (GroveDBProof, usize) =
            bincode::decode_from_slice(&proof, config).expect("decode envelope");
        let GroveDBProof::V1(ref mut v1) = decoded else {
            panic!("expected V1 envelope");
        };
        fn strip_key(layer: &mut LayerProof, key: &[u8]) -> bool {
            if layer.lower_layers.remove(key).is_some() {
                return true;
            }
            layer
                .lower_layers
                .values_mut()
                .any(|lower| strip_key(lower, key))
        }
        assert!(strip_key(&mut v1.root_layer, b"scores".as_slice()));
        let tampered = bincode::encode_to_vec(&decoded, config).expect("re-encode");
        GroveDb::verify_path_query(&tampered, &pq, grove_version)
            .expect_err("hiding a present-but-empty branch must be rejected");
    }

    #[test]
    fn empty_indexed_tree_round_trips_for_every_traversal() {
        // Audit finding: the Bounded traversal hard-errored on an empty
        // secondary (Merk::prove refuses empty trees) while every other
        // traversal — and the trusted read — handled it. All four must
        // round-trip against a freshly created, never-populated indexed
        // tree.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, &[]);
        let root = root_hash(&db, grove_version);

        // Bounded (the previously-broken one).
        let pq = PathQuery::new_axis_bounded(psit_path(), IndexAxis::Sum, 0, 100, 5, true);
        match GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
            .expect("bounded over an empty secondary verifies")
        {
            VerifiedPathQuery::AxisEntries {
                root_hash, entries, ..
            } => {
                assert_eq!(root_hash, root);
                assert!(entries.is_empty());
            }
            other => panic!("expected AxisEntries, got {other:?}"),
        }

        // TopK.
        let pq = PathQuery::new_axis_top_k(psit_path(), IndexAxis::Sum, 3, 0, true);
        match GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
            .expect("top-k over an empty secondary verifies")
        {
            VerifiedPathQuery::AxisEntries {
                root_hash, entries, ..
            } => {
                assert_eq!(root_hash, root);
                assert!(entries.is_empty());
            }
            other => panic!("expected AxisEntries, got {other:?}"),
        }

        // AggregateOverValueRange.
        let pq = PathQuery::new_axis_aggregate_over_value_range(
            psit_path(),
            IndexAxis::Sum,
            0,
            100,
            AggregateFold::Total,
        );
        match GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
            .expect("range aggregate over an empty secondary verifies")
        {
            VerifiedPathQuery::AxisAggregate { root_hash, value } => {
                assert_eq!(root_hash, root);
                assert_eq!(value, 0);
            }
            other => panic!("expected AxisAggregate, got {other:?}"),
        }

        // RankOfKey errors cleanly (the key cannot exist in an empty
        // tree) rather than producing a bogus proof.
        let pq =
            PathQuery::new_axis_rank_of_key(psit_path(), IndexAxis::Sum, b"ghost".to_vec(), true);
        db.prove_query(&pq, None, grove_version)
            .unwrap()
            .expect_err("rank of a key in an empty indexed tree must error");
    }

    #[test]
    fn duplicate_branch_keys_are_rejected_at_classification() {
        // Audit finding: duplicate branch keys would produce
        // contradictory per-branch rows (one Some, one None) from a
        // single valid proof. The constructor dedups via `insert_key`,
        // so hand-build the malformed query the way a hostile or buggy
        // caller could.
        use grovedb_merk::proofs::{
            query::{query_item::QueryItem, ReadMode},
            Query,
        };
        let mut terminal = Query::new();
        terminal.read_mode = Some(Box::new(ReadMode::Axis(AxisQuery::top_k(
            IndexAxis::Sum,
            1,
            0,
            true,
        ))));
        let mut branching = Query::new();
        branching.items.push(QueryItem::Key(b"alice".to_vec()));
        branching.items.push(QueryItem::Key(b"alice".to_vec()));
        branching.set_subquery_path(vec![b"scores".to_vec()]);
        branching.set_subquery(terminal);
        let pq = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], branching);
        match pq.classify() {
            Err(Error::InvalidQuery(message)) => {
                assert!(message.contains("same branch key"), "got: {message}")
            }
            other => panic!("duplicate branch keys must be rejected, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Gates
    // -----------------------------------------------------------------

    #[test]
    fn axis_descents_are_gated_to_grove_v4_on_both_sides() {
        let v3 = &GROVE_VERSIONS[2];
        assert_eq!(v3.protocol_version, 3);
        let v4 = GroveVersion::latest();

        let db = make_test_grovedb(v4);
        build_psit(&db, v4, PSIT_ENTRIES);
        let pq = PathQuery::new_axis_top_k(psit_path(), IndexAxis::Sum, 2, 0, true);

        // V3 prover refuses.
        match db.prove_query(&pq, None, v3).unwrap() {
            Err(Error::NotSupported(_)) => {}
            other => panic!("V3 prover must refuse axis shapes, got {other:?}"),
        }

        // V3 verifier rejects a genuine V4 proof.
        let proof = prove(&db, &pq, v4);
        match GroveDb::verify_path_query(&proof, &pq, v3) {
            Err(Error::NotSupported(_)) => {}
            other => panic!("V3 verifier must reject axis shapes, got {other:?}"),
        }

        // A V0 envelope (grove v1 prover, plain query) fed to an
        // axis-shaped verification is rejected as the wrong envelope.
        let v1 = &GROVE_VERSIONS[0];
        let plain = PathQuery::new_single_key(vec![TEST_LEAF.to_vec()], b"psit".to_vec());
        let v0_proof = db
            .prove_query(&plain, None, v1)
            .unwrap()
            .expect("V0 proof for a plain query");
        match GroveDb::verify_path_query(&v0_proof, &pq, v4) {
            Err(Error::NotSupported(_)) | Err(Error::InvalidProof(..)) => {}
            other => panic!("V0 envelope + axis query must be rejected, got {other:?}"),
        }
    }

    #[test]
    fn verify_path_query_serves_key_selection_too() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"item",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");
        let pq = PathQuery::new_single_key(vec![TEST_LEAF.to_vec()], b"item".to_vec());
        let proof = prove(&db, &pq, grove_version);
        match GroveDb::verify_path_query(&proof, &pq, grove_version)
            .expect("key selection verifies through the unified entry")
        {
            VerifiedPathQuery::Elements {
                root_hash,
                elements,
            } => {
                assert_eq!(root_hash, root_hash_of(&db, grove_version));
                assert_eq!(elements.len(), 1);
            }
            other => panic!("expected Elements, got {other:?}"),
        }
    }

    fn root_hash_of(db: &GroveDb, grove_version: &GroveVersion) -> [u8; 32] {
        db.root_hash(None, grove_version).unwrap().expect("root")
    }

    // -----------------------------------------------------------------
    // The other two axes through the embedded envelope
    //
    // Every traversal fans out per axis on BOTH sides — the prover picks
    // a secondary-proof builder per axis, the verifier picks a decoder —
    // so a sum-only suite leaves two thirds of each fan-out unexecuted.
    // -----------------------------------------------------------------

    /// Build a PCIT at `[TEST_LEAF, b"pcit"]` whose entries carry the
    /// given counts. Counts are DERIVED: each child is a provable count
    /// tree populated with `c` items.
    fn build_pcit(db: &GroveDb, grove_version: &GroveVersion, entries: &[(&[u8], u64)]) {
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcit",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCIT");
        for (key, count) in entries {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"pcit"].as_ref(),
                key,
                Element::empty_provable_count_tree(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCIT child");
            for i in 0..*count {
                db.insert(
                    [TEST_LEAF, b"pcit", key].as_ref(),
                    &i.to_be_bytes(),
                    Element::new_item(b"v".to_vec()),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("populate PCIT child");
            }
        }
    }

    fn pcit_path() -> Vec<Vec<u8>> {
        vec![TEST_LEAF.to_vec(), b"pcit".to_vec()]
    }

    #[test]
    fn count_axis_traversals_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(
            &db,
            grove_version,
            &[(b"alpha", 3), (b"beta", 1), (b"gamma", 5)],
        );
        let root = root_hash(&db, grove_version);
        let path = [TEST_LEAF, b"pcit"];

        // Aggregate over the value range on the COUNT axis: a different prover builder
        // and a different verifier decoder from the sum axis.
        let pq = PathQuery::new_axis_aggregate_over_value_range(
            pcit_path(),
            IndexAxis::Count,
            2,
            10,
            AggregateFold::Population,
        );
        match GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
            .expect("count range aggregate verifies")
        {
            VerifiedPathQuery::AxisAggregate { root_hash, value } => {
                assert_eq!(root_hash, root);
                let direct = db
                    .indexed_count_aggregate_over_value_range(
                        path.as_ref(),
                        2,
                        10,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("direct count range aggregate");
                assert_eq!(value, direct as i128);
                // A count-axis range aggregate counts the ENTRIES whose
                // count value lands in `[lo, hi]` — alpha (3) and gamma
                // (5) — not the sum of those counts. beta (1) is below
                // the range.
                assert_eq!(value, 2);
            }
            other => panic!("expected AxisAggregate, got {other:?}"),
        }

        // Paginated page and rank on the count axis: the paginated
        // decoder and `first_original_key` both fan out per axis.
        let pq = PathQuery::new_axis_top_k(pcit_path(), IndexAxis::Count, 2, 0, true);
        match GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
            .expect("count top-k verifies")
        {
            VerifiedPathQuery::AxisEntries { entries, .. } => {
                let direct = db
                    .indexed_count_top_k_paginated(path.as_ref(), 2, 0, true, None, grove_version)
                    .unwrap()
                    .expect("direct count top-k");
                assert_eq!(entries, AxisEntries::Count(direct));
            }
            other => panic!("expected AxisEntries, got {other:?}"),
        }

        let pq =
            PathQuery::new_axis_rank_of_key(pcit_path(), IndexAxis::Count, b"alpha".to_vec(), true);
        match GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
            .expect("count rank verifies")
        {
            // gamma (5) outranks alpha (3) descending.
            VerifiedPathQuery::AxisRank { rank, .. } => assert_eq!(rank, 1),
            other => panic!("expected AxisRank, got {other:?}"),
        }
    }

    #[test]
    fn avg_axis_traversals_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(&db, grove_version, PSIT_ENTRIES);
        let pcpsit_path = vec![TEST_LEAF.to_vec(), b"pcpsit".to_vec()];
        let path = [TEST_LEAF, b"pcpsit"];

        // Bounded over the whole i128 domain (the avg axis is unclamped).
        let pq = PathQuery::new_axis_bounded(
            pcpsit_path.clone(),
            IndexAxis::Avg,
            i128::MIN,
            i128::MAX,
            10,
            false,
        );
        match GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
            .expect("avg bounded verifies")
        {
            VerifiedPathQuery::AxisEntries { entries, .. } => {
                let direct = db
                    .indexed_avg_range(
                        path.as_ref(),
                        i128::MIN,
                        i128::MAX,
                        false,
                        10,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("direct avg range");
                assert_eq!(entries, AxisEntries::Avg(direct));
            }
            other => panic!("expected AxisEntries, got {other:?}"),
        }

        // Rank on the avg axis: `first_original_key` must read the Avg
        // variant, not fall through to another axis's tuple shape.
        let pq =
            PathQuery::new_axis_rank_of_key(pcpsit_path, IndexAxis::Avg, b"alice".to_vec(), true);
        let verified =
            GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
                .expect("avg rank verifies");
        let VerifiedPathQuery::AxisRank { rank, .. } = verified else {
            panic!("expected AxisRank, got {verified:?}");
        };
        // Cross-check against the standalone envelope over the same
        // state: two wire paths, one answer.
        let (standalone_bytes, standalone_rank) = db
            .prove_indexed_axis_rank_of_key(
                path.as_ref(),
                IndexAxis::Avg,
                b"alice",
                true,
                None,
                grove_version,
            )
            .unwrap()
            .expect("standalone avg rank prove");
        GroveDb::verify_indexed_axis_rank_of_key(
            &standalone_bytes,
            &path,
            IndexAxis::Avg,
            b"alice",
            standalone_rank,
            true,
            grove_version,
        )
        .expect("standalone avg rank verify");
        assert_eq!(rank, standalone_rank);
    }

    #[test]
    fn bounded_over_an_empty_secondary_round_trips_on_every_axis() {
        // The empty-secondary convention resolves to `NULL_HASH` plus an
        // axis-typed empty entry list — one constructor per axis.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[]);
        build_pcpsit(&db, grove_version, &[]);
        let pcpsit_path = vec![TEST_LEAF.to_vec(), b"pcpsit".to_vec()];

        for (path, axis, expected) in [
            (
                pcit_path(),
                IndexAxis::Count,
                AxisEntries::Count(Vec::new()),
            ),
            (
                pcpsit_path.clone(),
                IndexAxis::Sum,
                AxisEntries::Sum(Vec::new()),
            ),
            (pcpsit_path, IndexAxis::Avg, AxisEntries::Avg(Vec::new())),
        ] {
            let pq = PathQuery::new_axis_bounded(path, axis, 0, 100, 5, true);
            match GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
                .expect("bounded over an empty secondary verifies")
            {
                VerifiedPathQuery::AxisEntries { entries, .. } => {
                    assert_eq!(entries, expected, "axis {axis:?}")
                }
                other => panic!("expected AxisEntries for {axis:?}, got {other:?}"),
            }
        }
    }

    // -----------------------------------------------------------------
    // Structural forgeries of the axis layer itself
    // -----------------------------------------------------------------

    /// Decode → mutate the terminal axis-descent LAYER → re-encode.
    fn tamper_axis_layer(proof: &[u8], mutate: impl FnOnce(&mut LayerProof)) -> Vec<u8> {
        let config = bincode::config::standard().with_big_endian();
        let (mut decoded, _): (GroveDBProof, usize) =
            bincode::decode_from_slice(proof, config).expect("decode envelope");
        let GroveDBProof::V1(ref mut v1) = decoded else {
            panic!("expected V1 envelope");
        };
        fn find_descent(layer: &mut LayerProof) -> Option<&mut LayerProof> {
            if matches!(layer.merk_proof, ProofBytes::IndexedTreeAxisDescent(_)) {
                return Some(layer);
            }
            layer.lower_layers.values_mut().find_map(find_descent)
        }
        mutate(find_descent(&mut v1.root_layer).expect("envelope has an axis descent"));
        bincode::encode_to_vec(&decoded, config).expect("re-encode envelope")
    }

    #[test]
    fn a_malformed_axis_layer_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);
        let pq = PathQuery::new_axis_top_k(psit_path(), IndexAxis::Sum, 3, 0, true);
        let proof = prove(&db, &pq, grove_version);
        GroveDb::verify_path_query(&proof, &pq, grove_version).expect("untampered verifies");

        // A plain merk proof where the query says "axis read": the
        // verifier must not fall back to a key-selecting descent into
        // the primary, which would answer a different question under
        // the same root hash.
        let swapped = tamper_axis_layer(&proof, |layer| {
            layer.merk_proof = ProofBytes::Merk(Vec::new());
        });
        match GroveDb::verify_path_query(&swapped, &pq, grove_version) {
            Err(Error::InvalidProof(_, message)) => {
                assert!(message.contains("IndexedTreeAxisDescent"), "got: {message}")
            }
            other => panic!("a non-axis layer under an axis query must be rejected: {other:?}"),
        }

        // An axis descent is terminal; smuggling lower layers under it
        // must not be silently ignored.
        let with_lower = tamper_axis_layer(&proof, |layer| {
            layer
                .lower_layers
                .insert(b"smuggled".to_vec(), layer.clone());
        });
        match GroveDb::verify_path_query(&with_lower, &pq, grove_version) {
            Err(Error::InvalidProof(_, message)) => {
                assert!(message.contains("terminal"), "got: {message}")
            }
            other => panic!("lower layers under an axis descent must be rejected: {other:?}"),
        }

        // A rank echo on a traversal that has no rank: the field only
        // exists for RankOfKey, so carrying it elsewhere is malformed.
        let stray_rank = tamper_axis_payload(&proof, |payload| payload.rank = Some(0));
        match GroveDb::verify_path_query(&stray_rank, &pq, grove_version) {
            Err(Error::InvalidProof(_, message)) => {
                assert!(message.contains("not RankOfKey"), "got: {message}")
            }
            other => panic!("a stray rank echo must be rejected: {other:?}"),
        }
    }

    #[test]
    fn a_rank_proof_cannot_be_stripped_or_replayed_for_another_key() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);
        let pq =
            PathQuery::new_axis_rank_of_key(psit_path(), IndexAxis::Sum, b"carol".to_vec(), true);
        let proof = prove(&db, &pq, grove_version);
        GroveDb::verify_path_query(&proof, &pq, grove_version).expect("honest rank verifies");

        // Dropping the rank: a RankOfKey traversal has nothing to
        // attest without it.
        let no_rank = tamper_axis_payload(&proof, |payload| payload.rank = None);
        match GroveDb::verify_path_query(&no_rank, &pq, grove_version) {
            Err(Error::InvalidProof(_, message)) => {
                assert!(message.contains("must carry the rank"), "got: {message}")
            }
            other => panic!("a rank proof without a rank must be rejected: {other:?}"),
        }

        // Replaying carol's rank proof as an answer about erin: the
        // count commitments still attest the skip honestly, so the
        // entry AT that rank is carol — not the key that was asked
        // about. The verifier must catch the substitution.
        let other_key =
            PathQuery::new_axis_rank_of_key(psit_path(), IndexAxis::Sum, b"erin".to_vec(), true);
        match GroveDb::verify_path_query(&proof, &other_key, grove_version) {
            Err(Error::InvalidProof(_, message)) => {
                assert!(message.contains("not the queried key"), "got: {message}")
            }
            other => panic!("a replayed rank proof must be rejected: {other:?}"),
        }

        // Overstating the rank past the end of the walk: the count
        // commitments attest fewer skipped entries than claimed.
        let overstated = tamper_axis_payload(&proof, |payload| payload.rank = Some(99));
        match GroveDb::verify_path_query(&overstated, &pq, grove_version) {
            Err(Error::InvalidProof(_, message)) => assert!(
                message.contains("attest") || message.contains("count-offset proof failed"),
                "got: {message}"
            ),
            other => panic!("an overstated rank must be rejected: {other:?}"),
        }
    }

    #[test]
    fn axis_descent_bytes_never_satisfy_a_key_selection_query() {
        // The axis-descent variant is meaningful only where the query
        // says "axis read". Reaching the ordinary layer verifier with
        // it — e.g. by feeding an axis proof to a plain query over the
        // same path — must hard-error rather than being treated as an
        // empty or opaque merk proof.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);
        let axis_pq = PathQuery::new_axis_top_k(psit_path(), IndexAxis::Sum, 3, 0, true);
        let axis_proof = prove(&db, &axis_pq, grove_version);

        let mut plain = grovedb_merk::proofs::Query::new();
        plain.insert_all();
        let mut outer = grovedb_merk::proofs::Query::new();
        outer.insert_key(b"psit".to_vec());
        outer.set_subquery(plain);
        let plain_pq = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], outer);

        match GroveDb::verify_query(&axis_proof, &plain_pq, grove_version) {
            Err(Error::InvalidProof(..)) | Err(Error::NotSupported(_)) => {}
            other => panic!("axis-descent bytes must not satisfy a key selection: {other:?}"),
        }
    }

    #[test]
    fn axis_descent_payload_decoding_is_canonical() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);
        let pq = PathQuery::new_axis_top_k(psit_path(), IndexAxis::Sum, 3, 0, true);
        let proof = prove(&db, &pq, grove_version);

        // Pull the honest payload bytes out of the envelope.
        let config = bincode::config::standard().with_big_endian();
        let (decoded, _): (GroveDBProof, usize) =
            bincode::decode_from_slice(&proof, config).expect("decode envelope");
        let GroveDBProof::V1(v1) = decoded else {
            panic!("expected V1 envelope");
        };
        fn find_descent(layer: &LayerProof) -> Option<&Vec<u8>> {
            if let ProofBytes::IndexedTreeAxisDescent(bytes) = &layer.merk_proof {
                return Some(bytes);
            }
            layer.lower_layers.values().find_map(find_descent)
        }
        let bytes = find_descent(&v1.root_layer).expect("axis descent present");
        AxisDescentProof::decode_canonical(bytes).expect("honest payload decodes");

        // Trailing bytes are a distinct encoding of the same payload —
        // canonical decoding rejects them rather than ignoring them.
        let mut padded = bytes.clone();
        padded.push(0);
        match AxisDescentProof::decode_canonical(&padded) {
            Err(Error::CorruptedData(message)) => {
                assert!(message.contains("trailing"), "got: {message}")
            }
            other => panic!("trailing bytes must be rejected, got {other:?}"),
        }

        // Garbage does not decode at all.
        AxisDescentProof::decode_canonical(&[0xff, 0xff, 0xff, 0xff])
            .expect_err("garbage must not decode");

        // The Display impl degrades to a byte count for payloads it
        // cannot decode, rather than panicking inside a debug print.
        let rendered = format!("{}", ProofBytes::IndexedTreeAxisDescent(vec![0xff; 3]));
        assert!(rendered.contains('3'), "got: {rendered}");
    }

    // -----------------------------------------------------------------
    // Prover-side refusals
    // -----------------------------------------------------------------

    #[test]
    fn a_single_path_axis_read_without_a_target_fails_generation() {
        // Review finding (P2): the generic walk produces no axis descent
        // when the queried path is missing or is not an indexed tree,
        // and used to return `Ok` with an ordinary layer — a proof the
        // verifier could only reject as "got 0 axis layers". The prover
        // must refuse to emit it instead.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);

        // A present element that is an ordinary tree...
        let plain_target =
            PathQuery::new_axis_top_k(vec![TEST_LEAF.to_vec()], IndexAxis::Sum, 2, 0, true);
        // ...and a path that names nothing at all.
        let missing_target = PathQuery::new_axis_top_k(
            vec![TEST_LEAF.to_vec(), b"ghost".to_vec()],
            IndexAxis::Sum,
            2,
            0,
            true,
        );

        for pq in [plain_target, missing_target] {
            match db.prove_query(&pq, None, grove_version).unwrap() {
                Err(Error::InvalidPath(message)) => assert!(
                    message.contains("exactly one axis descent"),
                    "got: {message}"
                ),
                other => panic!("an axis read with no indexed target must be refused: {other:?}"),
            }
        }
    }

    #[test]
    fn a_branch_whose_axis_terminal_is_not_indexed_is_never_reported_absent() {
        // Review finding (P1): the verifier resolved the axis read only
        // AFTER filtering on `is_indexed_tree`, so a present ORDINARY
        // tree at a branch's axis terminal fell through to the normal
        // descent — which yields no trio for it. The branched arm then
        // saw neither an axis layer nor a present element and endorsed
        // the branch as `None`, i.e. proven absent, under a genuine root
        // hash, while the trusted read rejects the same state outright.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // carol is indexed as usual; alice's `scores` is an ordinary
        // tree, so her axis terminal has no secondary index at all.
        build_branched_psits(&db, grove_version, &[(b"carol", &[(b"m1", 7)])]);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"alice",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create alice branch");
        db.insert(
            [TEST_LEAF, b"alice"].as_ref(),
            b"scores",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create alice's UNindexed scores tree");

        let pq = PathQuery::new_branched_axis(
            vec![TEST_LEAF.to_vec()],
            vec![b"alice".to_vec(), b"carol".to_vec()],
            vec![b"scores".to_vec()],
            AxisQuery::top_k(IndexAxis::Sum, 1, 0, true),
        );

        // The trusted read refuses the same state, so a verified answer
        // of "alice is absent" would be a proof/read disagreement.
        db.run_path_query(
            &pq,
            true,
            true,
            true,
            crate::query_result_type::QueryResultType::QueryKeyElementPairResultType,
            None,
            grove_version,
        )
        .unwrap()
        .expect_err("the trusted read rejects an unindexed axis terminal");

        // Whatever the prover emits, verification must not endorse
        // alice as proven-absent.
        match db.prove_query(&pq, None, grove_version).unwrap() {
            Err(_) => {}
            Ok(proof) => match GroveDb::verify_path_query(&proof, &pq, grove_version) {
                Err(Error::InvalidProof(_, message)) => {
                    assert!(message.contains("non-indexed"), "got: {message}")
                }
                other => panic!(
                    "an unindexed axis terminal must never verify as an absent branch: \
                     {other:?}"
                ),
            },
        }
    }

    #[test]
    fn the_v0_envelope_and_malformed_read_modes_are_refused_at_the_prover() {
        let v1 = &GROVE_VERSIONS[0];
        assert_eq!(v1.protocol_version, 1, "GROVE_VERSIONS[0] must be V1");
        let v4 = GroveVersion::latest();
        let db = make_test_grovedb(v4);
        build_psit(&db, v4, PSIT_ENTRIES);

        // Grove v1 emits V0 envelopes, which have no axis-descent
        // variant at all — refuse before touching the tree.
        let pq = PathQuery::new_axis_top_k(psit_path(), IndexAxis::Sum, 2, 0, true);
        match db.prove_query(&pq, None, v1).unwrap() {
            Err(Error::NotSupported(message)) => {
                assert!(message.contains("V1 proof envelopes"), "got: {message}")
            }
            other => panic!("the V0 envelope must refuse axis shapes: {other:?}"),
        }

        // A read-mode query that does not classify at all fails closed
        // at the prover instead of being misread as key selection.
        use grovedb_merk::proofs::{query::ReadMode, Query as MerkQuery};
        let mut malformed = MerkQuery::new();
        malformed.read_mode = Some(Box::new(ReadMode::Axis(AxisQuery::top_k(
            IndexAxis::Sum,
            1,
            0,
            true,
        ))));
        // Items alongside an axis read: the axis grammar forbids it.
        malformed.insert_key(b"alice".to_vec());
        let pq = PathQuery::new_unsized(psit_path(), malformed);
        match db.prove_query(&pq, None, v4).unwrap() {
            Err(Error::InvalidQuery(_)) => {}
            other => panic!("a malformed read-mode query must fail closed: {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Read-mode resolution — the one function both sides use to decide
    // which layers are axis reads
    // -----------------------------------------------------------------

    #[test]
    fn axis_read_resolution_agrees_on_every_position() {
        use grovedb_merk::proofs::{
            query::{query_item::QueryItem, ReadMode},
            Query as MerkQuery,
        };
        let mut terminal = MerkQuery::new();
        terminal.read_mode = Some(Box::new(ReadMode::Axis(AxisQuery::top_k(
            IndexAxis::Sum,
            1,
            0,
            true,
        ))));
        // A conditional branch for `alice` and a default branch for the
        // rest, both reaching the terminal through a two-segment
        // subquery path.
        let mut branching = MerkQuery::new();
        branching.insert_key(b"alice".to_vec());
        branching.insert_key(b"bob".to_vec());
        branching.set_subquery_path(vec![b"scores".to_vec(), b"y2026".to_vec()]);
        branching.set_subquery(terminal.clone());
        branching.add_conditional_subquery(
            QueryItem::Key(b"alice".to_vec()),
            Some(vec![b"scores".to_vec(), b"y2026".to_vec()]),
            Some(terminal),
        );
        let pq = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], branching);

        let p =
            |segments: &[&[u8]]| -> Option<bool> { pq.axis_read_at_path(segments).map(|_| true) };

        // The terminal node, reached through the conditional branch...
        assert_eq!(
            p(&[TEST_LEAF, b"alice", b"scores", b"y2026"]),
            Some(true),
            "conditional branch must resolve to the axis terminal"
        );
        // ...and through the default one.
        assert_eq!(
            p(&[TEST_LEAF, b"bob", b"scores", b"y2026"]),
            Some(true),
            "default branch must resolve to the axis terminal"
        );
        // Above the query root: nothing can carry a read mode.
        assert_eq!(p(&[]), None);
        // Diverging from the query's own path.
        assert_eq!(p(&[b"elsewhere", b"alice", b"scores", b"y2026"]), None);
        // Mid-subquery_path: a read mode lives on a query node, never
        // between two path segments.
        assert_eq!(p(&[TEST_LEAF, b"bob", b"scores"]), None);
        // The subquery path exists but diverges at its last segment.
        assert_eq!(p(&[TEST_LEAF, b"bob", b"scores", b"y2025"]), None);
        // Past the terminal: the axis descent has nothing below it.
        assert_eq!(
            p(&[TEST_LEAF, b"bob", b"scores", b"y2026", b"deeper"]),
            None
        );
        // The branching level itself is a key selection, not an axis
        // read.
        assert_eq!(p(&[TEST_LEAF]), None);
    }

    // -----------------------------------------------------------------
    // The explicit fold (issue #806): the 2x2 (axis x fold) matrix
    // -----------------------------------------------------------------

    #[test]
    fn sum_population_over_value_range_round_trips() {
        // The band [0, 40] over PSIT_ENTRIES' sums [40, -10, 25, 40, 5]
        // selects 40, 25, 40, 5: Population = 4, Total = 110. The two
        // folds are different questions over the same band, and each
        // must round-trip to its own answer.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);
        let root = root_hash(&db, grove_version);

        let population_pq = PathQuery::new_axis_aggregate_over_value_range(
            psit_path(),
            IndexAxis::Sum,
            0,
            40,
            AggregateFold::Population,
        );
        match GroveDb::verify_path_query(
            &prove(&db, &population_pq, grove_version),
            &population_pq,
            grove_version,
        )
        .expect("sum-axis population verifies")
        {
            VerifiedPathQuery::AxisAggregate { root_hash, value } => {
                assert_eq!(root_hash, root);
                // alice and dave BOTH sit at 40: population counts
                // entries, not distinct values — 4, not 3.
                assert_eq!(value, 4);
                // ...equal to the trusted read over the same state.
                let direct = db
                    .indexed_sum_population_over_value_range(
                        [TEST_LEAF, b"psit"].as_ref(),
                        0,
                        40,
                        None,
                        grove_version,
                    )
                    .unwrap()
                    .expect("trusted population read");
                assert_eq!(value, direct as i128);
            }
            other => panic!("expected AxisAggregate, got {other:?}"),
        }

        // The Total fold over the same band answers 110, not 4.
        let total_pq = PathQuery::new_axis_aggregate_over_value_range(
            psit_path(),
            IndexAxis::Sum,
            0,
            40,
            AggregateFold::Total,
        );
        match GroveDb::verify_path_query(
            &prove(&db, &total_pq, grove_version),
            &total_pq,
            grove_version,
        )
        .expect("sum-axis total verifies")
        {
            VerifiedPathQuery::AxisAggregate { value, .. } => assert_eq!(value, 110),
            other => panic!("expected AxisAggregate, got {other:?}"),
        }
    }

    #[test]
    fn the_fold_lives_in_the_query_not_the_embedded_proof() {
        // On a PCPS secondary both walkers commit the SAME dual
        // (count, sum) node flavors — the node hash is
        // `node_hash_with_count_and_sum`, so every subtree commitment
        // carries both aggregates regardless of which fold the prover
        // was asked for. The embedded payload therefore does not (and
        // could not meaningfully) assert a fold; the query is the
        // verifier's sole source of it, per the query-as-input
        // principle every embedded shape follows.
        //
        // The security property to pin is NOT rejection — it is that
        // cross-feeding a proof built for one fold to a query asking
        // the other yields that other fold's CORRECT answer, recomputed
        // from the hash-bound commitments under the genuine root. A
        // prover cannot use fold confusion to make either question
        // report a wrong number.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);
        let root = root_hash(&db, grove_version);

        let population_pq = PathQuery::new_axis_aggregate_over_value_range(
            psit_path(),
            IndexAxis::Sum,
            0,
            40,
            AggregateFold::Population,
        );
        let total_pq = PathQuery::new_axis_aggregate_over_value_range(
            psit_path(),
            IndexAxis::Sum,
            0,
            40,
            AggregateFold::Total,
        );
        let population_proof = prove(&db, &population_pq, grove_version);
        let total_proof = prove(&db, &total_pq, grove_version);

        // Cross-fed in both directions, each query still gets its own
        // correct answer, bound to the genuine root.
        match GroveDb::verify_path_query(&population_proof, &total_pq, grove_version)
            .expect("a dual-aggregate proof answers the total question too")
        {
            VerifiedPathQuery::AxisAggregate { root_hash, value } => {
                assert_eq!(root_hash, root);
                assert_eq!(value, 110, "the TOTAL, not the population");
            }
            other => panic!("expected AxisAggregate, got {other:?}"),
        }
        match GroveDb::verify_path_query(&total_proof, &population_pq, grove_version)
            .expect("a dual-aggregate proof answers the population question too")
        {
            VerifiedPathQuery::AxisAggregate { root_hash, value } => {
                assert_eq!(root_hash, root);
                assert_eq!(value, 4, "the POPULATION, not the total");
            }
            other => panic!("expected AxisAggregate, got {other:?}"),
        }
    }

    #[test]
    fn count_total_is_refused_by_name_until_the_secondary_is_sum_bearing() {
        // (Count, Total) is the missing cell of the matrix until issue
        // #806's secondary upgrade lands: every surface must refuse it
        // by name, not silently answer the population instead.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(&db, grove_version, PSIT_ENTRIES);
        let pcpsit_path = vec![TEST_LEAF.to_vec(), b"pcpsit".to_vec()];

        let pq = PathQuery::new_axis_aggregate_over_value_range(
            pcpsit_path,
            IndexAxis::Count,
            0,
            10,
            AggregateFold::Total,
        );
        // The prover refuses...
        match db.prove_query(&pq, None, grove_version).unwrap() {
            Err(Error::NotSupported(message)) => {
                assert!(message.contains("806"), "got: {message}")
            }
            other => panic!("count+Total proving must be refused, got {other:?}"),
        }
        // ...the trusted read refuses...
        match db
            .run_path_query(
                &pq,
                true,
                true,
                true,
                crate::query_result_type::QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
        {
            Err(Error::NotSupported(message)) => {
                assert!(message.contains("806"), "got: {message}")
            }
            other => panic!("count+Total reading must be refused, got {other:?}"),
        }
        // ...and the standalone family refuses on both sides.
        db.prove_indexed_axis_aggregate_over_value_range(
            [TEST_LEAF, b"pcpsit"].as_ref(),
            IndexAxis::Count,
            0,
            10,
            AggregateFold::Total,
            None,
            grove_version,
        )
        .unwrap()
        .expect_err("standalone count+Total proving must be refused");
        GroveDb::verify_indexed_axis_aggregate_over_value_range(
            &[0u8; 4],
            &[TEST_LEAF, b"pcpsit"],
            IndexAxis::Count,
            0,
            10,
            AggregateFold::Total,
            grove_version,
        )
        .expect_err("standalone count+Total verification must be refused");

        // The refusal must also fire in the VERIFIERS' own arms, not
        // just upstream of them. Embedded: a genuine count+Population
        // proof against a count+Total query reaches the descent
        // verifier's (Count, Total) arm — the query is where the fold
        // lives, so this is the exact shape a confused (or hostile)
        // client would produce.
        let population_pq = PathQuery::new_axis_aggregate_over_value_range(
            vec![TEST_LEAF.to_vec(), b"pcpsit".to_vec()],
            IndexAxis::Count,
            0,
            10,
            AggregateFold::Population,
        );
        let population_proof = prove(&db, &population_pq, grove_version);
        match GroveDb::verify_path_query(&population_proof, &pq, grove_version) {
            Err(Error::NotSupported(message)) => {
                assert!(message.contains("806"), "got: {message}")
            }
            other => panic!("the descent verifier must refuse count+Total, got {other:?}"),
        }

        // Standalone: relabel a genuine count+Population envelope's
        // fold echo to Total. The echo check passes (expected == echo),
        // so the inner dispatch's (Count, Total) rejection is what must
        // stop it.
        let standalone = db
            .prove_indexed_axis_aggregate_over_value_range(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                IndexAxis::Count,
                0,
                10,
                AggregateFold::Population,
                None,
                grove_version,
            )
            .unwrap()
            .expect("standalone count+Population prove");
        let config = bincode::config::standard().with_limit::<{ 16 * 1024 * 1024 }>();
        let (mut envelope, _): (
            crate::operations::proof::indexed_axis::IndexedAxisAggregateProof,
            usize,
        ) = bincode::decode_from_slice(&standalone, config).expect("decode envelope");
        envelope.fold_tag = AggregateFold::Total.tag();
        let relabeled = bincode::encode_to_vec(&envelope, config).expect("re-encode");
        match GroveDb::verify_indexed_axis_aggregate_over_value_range(
            &relabeled,
            &[TEST_LEAF, b"pcpsit"],
            IndexAxis::Count,
            0,
            10,
            AggregateFold::Total,
            grove_version,
        ) {
            Err(Error::NotSupported(message)) => {
                assert!(message.contains("806"), "got: {message}")
            }
            other => panic!(
                "a relabeled count envelope must hit the inner count+Total refusal: {other:?}"
            ),
        }
    }

    #[test]
    fn a_forged_fold_echo_in_the_standalone_envelope_is_rejected() {
        // The standalone envelope ECHOES the fold; the verifier must
        // authenticate the echo against the caller's expected fold, so a
        // relabeled envelope cannot pass one fold's proof off as the
        // other's answer.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);

        let bytes = db
            .prove_indexed_axis_aggregate_over_value_range(
                [TEST_LEAF, b"psit"].as_ref(),
                IndexAxis::Sum,
                0,
                40,
                AggregateFold::Population,
                None,
                grove_version,
            )
            .unwrap()
            .expect("standalone population prove");
        // Honest verification answers the population...
        let result = GroveDb::verify_indexed_axis_aggregate_over_value_range(
            &bytes,
            &[TEST_LEAF, b"psit"],
            IndexAxis::Sum,
            0,
            40,
            AggregateFold::Population,
            grove_version,
        )
        .expect("honest fold verifies");
        assert_eq!(result.aggregate, 4);
        // ...and the same bytes must not answer the Total question.
        match GroveDb::verify_indexed_axis_aggregate_over_value_range(
            &bytes,
            &[TEST_LEAF, b"psit"],
            IndexAxis::Sum,
            0,
            40,
            AggregateFold::Total,
            grove_version,
        ) {
            Err(Error::CorruptedData(message)) => {
                assert!(message.contains("fold mismatch"), "got: {message}")
            }
            other => panic!("a fold-mismatched envelope must be rejected, got {other:?}"),
        }
    }
}
