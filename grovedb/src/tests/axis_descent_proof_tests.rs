//! End-to-end tests for axis-ordered reads embedded in the GroveDBProof
//! V1 envelope (`ProofBytes::IndexedTreeAxisDescent`): round trips for
//! every traversal, cross-checks against the standalone envelopes and
//! the trusted reads, absence-authenticated branched reads, forgery
//! rejections, and the GROVE_V4 gates.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::query::{AxisQuery, IndexAxis};
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
        let standalone =
            GroveDb::verify_indexed_sum_top_k(&standalone_bytes, &[TEST_LEAF, b"psit"], 3, true)
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

        // Range aggregate over the sum axis.
        let pq = PathQuery::new_axis_range_aggregate(psit_path(), IndexAxis::Sum, 0, 40);
        match GroveDb::verify_path_query(&prove(&db, &pq, grove_version), &pq, grove_version)
            .expect("aggregate verifies")
        {
            VerifiedPathQuery::AxisAggregate { root_hash, value } => {
                assert_eq!(root_hash, root);
                let direct = db
                    .indexed_sum_range_aggregate(
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
}
