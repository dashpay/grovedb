//! Regression tests for issue #858: a reference whose terminal is a
//! `NonCounted`-wrapped item must commit to the terminal's **stored**
//! bytes (wrapper included) on every path.
//!
//! Before the fix the batch reference resolver hashed the stored bytes
//! while direct insert, `verify_grovedb` and the V1 prover resolved the
//! terminal through the presentation-oriented `follow_reference`, which
//! looks through the wrapper first. The same reference therefore
//! committed different roots depending on how it was written, and a
//! batch-written reference failed both the integrity walk and proof
//! verification.
//!
//! `GROVE_V3` is live, so the direct-insert change is gated to
//! `GROVE_V4` (`add_element_on_transaction: 2`); the read-side paths
//! (integrity walk, V1 prover) select the representation matching each
//! reference's stored commitment, including when reading V3 data under V4.

#[cfg(test)]
mod tests {
    use grovedb_merk::tree::{combine_hash, value_hash};
    use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

    use crate::{
        batch::QualifiedGroveDbOp,
        reference_path::ReferencePathType,
        tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
        Element, GroveDb, PathQuery, Query, SizedQuery,
    };

    const COUNT_TREE: &[u8] = b"ct";
    const TARGET: &[u8] = b"target";
    const REF: &[u8] = b"ref";
    const TARGET_VALUE: &[u8] = b"target-value";

    fn wrapped_item() -> Element {
        Element::new_non_counted(Element::new_item(TARGET_VALUE.to_vec()))
            .expect("NonCounted(Item) is a valid wrapper")
    }

    fn reference_to_target() -> Element {
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            COUNT_TREE.to_vec(),
            TARGET.to_vec(),
        ]))
    }

    /// `TEST_LEAF/ct` is a `CountTree` holding `NonCounted(Item)` at
    /// `target`. The wrapper is only admitted under a count-bearing
    /// parent, which is why the terminal lives in a count tree.
    fn db_with_wrapped_target(grove_version: &GroveVersion) -> TempGroveDb {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            COUNT_TREE,
            Element::empty_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert count tree");
        db.insert(
            [TEST_LEAF, COUNT_TREE].as_ref(),
            TARGET,
            wrapped_item(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert NonCounted(Item) target");
        db
    }

    fn insert_reference_directly(db: &TempGroveDb, grove_version: &GroveVersion) {
        db.insert(
            [TEST_LEAF].as_ref(),
            REF,
            reference_to_target(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("direct insert of reference");
    }

    fn insert_reference_in_batch(db: &TempGroveDb, grove_version: &GroveVersion) {
        let op = QualifiedGroveDbOp::insert_or_replace_op(
            vec![TEST_LEAF.to_vec()],
            REF.to_vec(),
            reference_to_target(),
        );
        db.apply_batch(vec![op], None, None, grove_version)
            .unwrap()
            .expect("batch insert of reference");
    }

    fn root(db: &TempGroveDb, grove_version: &GroveVersion) -> [u8; 32] {
        db.root_hash(None, grove_version)
            .unwrap()
            .expect("root hash")
    }

    /// Commitment a reference to the wrapped target must carry: the
    /// reference's own value hash combined with the hash of the
    /// terminal's STORED bytes (wrapper included).
    fn expected_reference_value_hash(grove_version: &GroveVersion) -> [u8; 32] {
        let ref_bytes = reference_to_target()
            .serialize(grove_version)
            .expect("serialize reference");
        let stored_target_bytes = wrapped_item()
            .serialize(grove_version)
            .expect("serialize wrapped target");
        combine_hash(
            &value_hash(&ref_bytes).unwrap(),
            &value_hash(&stored_target_bytes).unwrap(),
        )
        .unwrap()
    }

    fn stored_reference_value_hash(db: &TempGroveDb, grove_version: &GroveVersion) -> [u8; 32] {
        let tx = db.start_transaction();
        let merk = db
            .open_transactional_merk_at_path([TEST_LEAF].as_ref().into(), &tx, None, grove_version)
            .unwrap()
            .expect("open TEST_LEAF merk");
        merk.get_value_hash(
            REF,
            true,
            None::<&fn(&[u8], &GroveVersion) -> _>,
            grove_version,
        )
        .unwrap()
        .expect("read reference value hash")
        .expect("reference must exist")
    }

    fn query_for_ref() -> PathQuery {
        let mut query = Query::new();
        query.insert_key(REF.to_vec());
        PathQuery::new(vec![TEST_LEAF.to_vec()], SizedQuery::new(query, None, None))
    }

    /// Prove the reference and check the proof against the live root.
    /// Returns the bytes the verifier surfaced for the reference row.
    fn prove_and_verify_ref(db: &TempGroveDb, grove_version: &GroveVersion) -> Vec<u8> {
        let path_query = query_for_ref();
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove reference");
        let (proved_root, results) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).expect("verify proof");
        assert_eq!(
            proved_root,
            root(db, grove_version),
            "proof must bind the live root"
        );
        assert_eq!(results.len(), 1, "exactly the reference row");
        assert_eq!(results[0].key, REF.to_vec());
        results[0].value.clone()
    }

    #[test]
    fn direct_and_batch_reference_to_wrapped_terminal_commit_the_same_root() {
        let grove_version = GroveVersion::latest();

        let direct = db_with_wrapped_target(grove_version);
        insert_reference_directly(&direct, grove_version);

        let batched = db_with_wrapped_target(grove_version);
        insert_reference_in_batch(&batched, grove_version);

        assert_eq!(
            root(&direct, grove_version),
            root(&batched, grove_version),
            "direct and batch writes of the same reference must commit the same root"
        );

        let expected = expected_reference_value_hash(grove_version);
        assert_eq!(
            stored_reference_value_hash(&direct, grove_version),
            expected,
            "direct insert must bind the terminal's stored (wrapped) bytes"
        );
        assert_eq!(
            stored_reference_value_hash(&batched, grove_version),
            expected,
            "batch insert must bind the terminal's stored (wrapped) bytes"
        );
    }

    #[test]
    fn integrity_walk_accepts_references_to_wrapped_terminals_from_both_paths() {
        let grove_version = GroveVersion::latest();

        let direct = db_with_wrapped_target(grove_version);
        insert_reference_directly(&direct, grove_version);
        let batched = db_with_wrapped_target(grove_version);
        insert_reference_in_batch(&batched, grove_version);

        for (label, db) in [("direct", &direct), ("batch", &batched)] {
            let issues = db
                .verify_grovedb(None, true, true, grove_version)
                .expect("verify_grovedb runs");
            assert!(
                issues.is_empty(),
                "{label}-written reference to a wrapped terminal must pass verify_grovedb, got \
                 {issues:?}"
            );
        }
    }

    #[test]
    fn v1_proof_round_trips_a_reference_to_a_wrapped_terminal_from_both_paths() {
        let grove_version = GroveVersion::latest();

        let direct = db_with_wrapped_target(grove_version);
        insert_reference_directly(&direct, grove_version);
        let batched = db_with_wrapped_target(grove_version);
        insert_reference_in_batch(&batched, grove_version);

        for (label, db) in [("direct", &direct), ("batch", &batched)] {
            let surfaced = prove_and_verify_ref(db, grove_version);
            // The proof embeds the terminal exactly as stored (that is
            // what the reference node's hash binds); consumers look
            // through the wrapper the same way the trusted read does.
            let element =
                Element::deserialize(&surfaced, grove_version).expect("surfaced bytes decode");
            assert_eq!(
                element,
                wrapped_item(),
                "{label}: proof must surface the terminal's stored bytes"
            );
            assert_eq!(
                element.into_underlying(),
                Element::new_item(TARGET_VALUE.to_vec()),
                "{label}: looked-through terminal is the item"
            );
        }

        // The trusted read still presents the looked-through value.
        let got = direct
            .get([TEST_LEAF].as_ref(), REF, None, grove_version)
            .unwrap()
            .expect("get through reference");
        assert_eq!(got, Element::new_item(TARGET_VALUE.to_vec()));
    }

    /// A chain that hops through a `NonCounted(Reference)` and lands on
    /// a `NonCounted(Item)`: the wrapper on the intermediate hop is
    /// looked through for chain-following, the wrapper on the terminal
    /// is part of the commitment.
    #[test]
    fn chain_through_wrapped_hop_to_wrapped_terminal_agrees_across_paths() {
        let grove_version = GroveVersion::latest();

        let setup = |db: &TempGroveDb| {
            let hop = Element::new_non_counted(reference_to_target()).expect("wrap reference");
            db.insert(
                [TEST_LEAF, COUNT_TREE].as_ref(),
                b"hop",
                hop,
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert wrapped hop");
        };
        let ref_to_hop = || {
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                COUNT_TREE.to_vec(),
                b"hop".to_vec(),
            ]))
        };

        let direct = db_with_wrapped_target(grove_version);
        setup(&direct);
        direct
            .insert(
                [TEST_LEAF].as_ref(),
                REF,
                ref_to_hop(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("direct insert of chained reference");

        let batched = db_with_wrapped_target(grove_version);
        setup(&batched);
        batched
            .apply_batch(
                vec![QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec()],
                    REF.to_vec(),
                    ref_to_hop(),
                )],
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("batch insert of chained reference");

        assert_eq!(root(&direct, grove_version), root(&batched, grove_version));
        for (label, db) in [("direct", &direct), ("batch", &batched)] {
            let issues = db
                .verify_grovedb(None, true, true, grove_version)
                .expect("verify_grovedb runs");
            assert!(issues.is_empty(), "{label}: {issues:?}");
            let surfaced = prove_and_verify_ref(db, grove_version);
            assert_eq!(
                Element::deserialize(&surfaced, grove_version)
                    .expect("decode")
                    .into_underlying(),
                Element::new_item(TARGET_VALUE.to_vec()),
                "{label}: chain resolves to the item"
            );
        }
    }

    /// Offset-paginated proofs over a `ProvableCountTree` of references
    /// whose terminals are wrapped: the rows surface the stored
    /// (wrapped) target bytes and the verifier accepts them because they
    /// were resolved through a reference.
    #[test]
    fn count_offset_proof_round_trips_references_to_wrapped_terminals() {
        let grove_version = GroveVersion::latest();
        let db = db_with_wrapped_target(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"refs",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert provable count tree");
        // Batch-written rows: before the fix these carried the stored
        // (wrapped) commitment while the prover embedded the
        // looked-through terminal, so the proof failed to verify.
        let ops = [b"a", b"b", b"c", b"d"]
            .into_iter()
            .map(|key| {
                QualifiedGroveDbOp::insert_or_replace_op(
                    vec![TEST_LEAF.to_vec(), b"refs".to_vec()],
                    key.to_vec(),
                    reference_to_target(),
                )
            })
            .collect();
        db.apply_batch(ops, None, None, grove_version)
            .unwrap()
            .expect("batch insert reference rows");

        let mut query = Query::new();
        query.insert_range_inclusive(b"a".to_vec()..=b"d".to_vec());
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"refs".to_vec()],
            SizedQuery::new(query, Some(2), Some(1)),
        );
        let proof = db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove offset-paginated reference rows");
        let (proved_root, results) =
            GroveDb::verify_query_raw(&proof, &path_query, grove_version).expect("verify");
        assert_eq!(proved_root, root(&db, grove_version));
        let keys: Vec<Vec<u8>> = results.iter().map(|r| r.key.clone()).collect();
        assert_eq!(keys, vec![b"b".to_vec(), b"c".to_vec()]);
        for row in results {
            assert_eq!(
                Element::deserialize(&row.value, grove_version)
                    .expect("decode")
                    .into_underlying(),
                Element::new_item(TARGET_VALUE.to_vec())
            );
        }
    }

    /// Consensus pin: `GROVE_V3` is live, so its direct insert keeps the
    /// legacy commitment (hash of the looked-through terminal) and still
    /// disagrees with the batch path. Both commitments must remain readable
    /// and provable, including after upgrading to GROVE_V4.
    #[test]
    fn grove_v3_direct_insert_keeps_the_legacy_unwrapped_commitment() {
        let grove_version = &GROVE_V3;

        let direct = db_with_wrapped_target(grove_version);
        insert_reference_directly(&direct, grove_version);
        let batched = db_with_wrapped_target(grove_version);
        insert_reference_in_batch(&batched, grove_version);

        let ref_bytes = reference_to_target()
            .serialize(grove_version)
            .expect("serialize reference");
        let unwrapped_target_bytes = Element::new_item(TARGET_VALUE.to_vec())
            .serialize(grove_version)
            .expect("serialize inner item");
        let legacy = combine_hash(
            &value_hash(&ref_bytes).unwrap(),
            &value_hash(&unwrapped_target_bytes).unwrap(),
        )
        .unwrap();

        assert_eq!(
            stored_reference_value_hash(&direct, grove_version),
            legacy,
            "GROVE_V3 direct insert must keep hashing the looked-through terminal"
        );
        assert_eq!(
            stored_reference_value_hash(&batched, grove_version),
            expected_reference_value_hash(grove_version),
            "the batch path has always committed to the stored bytes"
        );
        assert_ne!(root(&direct, grove_version), root(&batched, grove_version));

        for reader_version in [&GROVE_V3, GroveVersion::latest()] {
            for (db, expected) in [
                (&direct, wrapped_item().into_underlying()),
                (&batched, wrapped_item()),
            ] {
                let before = root(db, reader_version);
                let surfaced = prove_and_verify_ref(db, reader_version);
                assert_eq!(
                    Element::deserialize(&surfaced, reader_version).unwrap(),
                    expected
                );
                assert!(db
                    .verify_grovedb(None, true, true, reader_version)
                    .expect("verify_grovedb runs")
                    .is_empty());
                assert_eq!(
                    root(db, reader_version),
                    before,
                    "reads must not rewrite commitments"
                );
            }
        }
    }

    /// A single tree can contain both commitment formats. The reader's
    /// current version cannot identify how any individual row was written.
    #[test]
    fn mixed_commitments_round_trip_in_regular_and_paginated_reference_proofs() {
        let latest = GroveVersion::latest();
        for host in [
            Element::empty_provable_count_tree(),
            Element::empty_provable_count_sum_tree(),
            Element::empty_provable_count_provable_sum_tree(),
        ] {
            for with_sum in [false, true] {
                let db = db_with_wrapped_target(&GROVE_V3);
                // Include a wrapped intermediate reference, itself written
                // under V3, so the integrity walk also checks a legacy hop.
                db.insert(
                    [TEST_LEAF, COUNT_TREE].as_ref(),
                    b"hop",
                    Element::new_non_counted(reference_to_target()).unwrap(),
                    None,
                    None,
                    &GROVE_V3,
                )
                .unwrap()
                .unwrap();
                db.insert(
                    [TEST_LEAF].as_ref(),
                    b"refs",
                    host.clone(),
                    None,
                    None,
                    latest,
                )
                .unwrap()
                .unwrap();
                let path = vec![TEST_LEAF.to_vec(), b"refs".to_vec()];
                let reference_path = ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    COUNT_TREE.to_vec(),
                    b"hop".to_vec(),
                ]);
                let reference = if with_sum {
                    Element::new_reference_with_sum_item(reference_path, 7)
                } else {
                    Element::new_reference(reference_path)
                };
                for (key, version, batch) in [
                    (b"a", &GROVE_V3, true),
                    (b"b", &GROVE_V3, false),
                    (b"c", latest, false),
                    (b"d", latest, true),
                ] {
                    if batch {
                        db.apply_batch(
                            vec![QualifiedGroveDbOp::insert_or_replace_op(
                                path.clone(),
                                key.to_vec(),
                                reference.clone(),
                            )],
                            None,
                            None,
                            version,
                        )
                        .unwrap()
                        .unwrap();
                    } else {
                        db.insert(path.as_slice(), key, reference.clone(), None, None, version)
                            .unwrap()
                            .unwrap();
                    }
                }
                let before = root(&db, latest);
                for reader_version in [&GROVE_V3, latest] {
                    for ascending in [false, true] {
                        for offset in [None, Some(1)] {
                            let mut query = Query::new();
                            query.left_to_right = ascending;
                            query.insert_range_inclusive(b"a".to_vec()..=b"d".to_vec());
                            let query = PathQuery::new(
                                path.clone(),
                                SizedQuery::new(query, offset.map(|_| 2), offset),
                            );
                            let proof = db
                                .prove_query(&query, None, reader_version)
                                .unwrap()
                                .unwrap();
                            let (proved_root, rows) =
                                GroveDb::verify_query_raw(&proof, &query, reader_version)
                                    .expect("mixed reference commitments must verify");
                            assert_eq!(proved_root, before);
                            let mut expected_keys = if offset.is_some() {
                                vec![b"b".to_vec(), b"c".to_vec()]
                            } else {
                                vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), b"d".to_vec()]
                            };
                            if !ascending {
                                expected_keys.reverse();
                            }
                            assert_eq!(
                                rows.iter().map(|r| r.key.clone()).collect::<Vec<_>>(),
                                expected_keys
                            );
                            for row in rows {
                                let expected = if row.key == b"b" {
                                    wrapped_item().into_underlying()
                                } else {
                                    wrapped_item()
                                };
                                assert_eq!(
                                    Element::deserialize(&row.value, reader_version).unwrap(),
                                    expected
                                );
                            }
                        }
                    }
                    assert!(db
                        .verify_grovedb(None, true, true, reader_version)
                        .unwrap()
                        .is_empty());
                    assert_eq!(root(&db, reader_version), before);
                }
            }
        }
    }

    /// Compatibility may select an older representation, but never an
    /// unrelated terminal whose bytes match neither committed format.
    #[test]
    fn stale_wrapped_reference_commitments_are_still_detected() {
        for batch in [false, true] {
            let db = db_with_wrapped_target(&GROVE_V3);
            if batch {
                insert_reference_in_batch(&db, &GROVE_V3);
            } else {
                insert_reference_directly(&db, &GROVE_V3);
            }
            db.insert(
                [TEST_LEAF, COUNT_TREE].as_ref(),
                TARGET,
                Element::new_non_counted(Element::new_item(b"changed".to_vec())).unwrap(),
                None,
                None,
                GroveVersion::latest(),
            )
            .unwrap()
            .unwrap();
            for reader_version in [&GROVE_V3, GroveVersion::latest()] {
                let issues = db.verify_grovedb(None, true, true, reader_version).unwrap();
                assert_eq!(issues.len(), 1);
                assert!(issues.contains_key(&vec![TEST_LEAF.to_vec(), REF.to_vec()]));
                let query = query_for_ref();
                let proof = db
                    .prove_query(&query, None, reader_version)
                    .unwrap()
                    .unwrap();
                let result = GroveDb::verify_query_raw(&proof, &query, reader_version);
                assert!(result.is_err() || result.unwrap().0 != root(&db, reader_version));
            }
        }
    }
}
