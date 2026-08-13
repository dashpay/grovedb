//! Tests for the [`AxisPathQuery`] vocabulary: read / prove / verify
//! round trips on every axis and traversal, the validation surface, and
//! the property that makes this a safe refactor — the proofs it emits
//! are byte-identical to the hand-rolled primitive calls it dispatches
//! to.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::proofs::{query::QueryItem as MerkQueryItem, Query as MerkQuery};
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::proof::indexed_axis::AxisEntries,
        query::{AxisPathQuery, AxisQuery, AxisTraversal},
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb,
    };

    const PCIT: &[u8] = b"pcit";
    const PCPSIT: &[u8] = b"pcpsit";

    /// A count-indexed tree at `[TEST_LEAF, "pcit"]` whose entries carry
    /// the given counts (counts are derived from child population).
    fn build_count_fixture(db: &GroveDb, gv: &GroveVersion, entries: &[(&[u8], u64)]) {
        db.insert(
            [TEST_LEAF].as_ref(),
            PCIT,
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create pcit");
        for (key, count) in entries {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, PCIT].as_ref(),
                key,
                Element::empty_provable_count_tree(),
                None,
                gv,
            )
            .unwrap()
            .expect("insert group");
            for i in 0..*count {
                db.insert(
                    [TEST_LEAF, PCIT, key].as_ref(),
                    &i.to_be_bytes(),
                    Element::new_item(b"v".to_vec()),
                    None,
                    None,
                    gv,
                )
                .unwrap()
                .expect("insert doc");
            }
        }
    }

    /// A dual-axis tree at `[TEST_LEAF, "pcpsit"]` carrying count, sum
    /// and avg axes over `(key, sum_value)` entries.
    fn build_multi_axis_fixture(db: &GroveDb, gv: &GroveVersion, entries: &[(&[u8], i64)]) {
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![(0, None), (1, None), (2, None)];
        db.insert(
            [TEST_LEAF].as_ref(),
            PCPSIT,
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("canonical axes"),
            None,
            None,
            gv,
        )
        .unwrap()
        .expect("create pcpsit");
        for (key, sum) in entries {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, PCPSIT].as_ref(),
                key,
                Element::new_item_with_sum_item(b"v".to_vec(), *sum),
                None,
                gv,
            )
            .unwrap()
            .expect("insert entry");
        }
    }

    fn count_path() -> Vec<Vec<u8>> {
        vec![TEST_LEAF.to_vec(), PCIT.to_vec()]
    }

    fn multi_axis_path() -> Vec<Vec<u8>> {
        vec![TEST_LEAF.to_vec(), PCPSIT.to_vec()]
    }

    fn counts(entries: &AxisEntries) -> Vec<(u64, Vec<u8>)> {
        match entries {
            AxisEntries::Count(v) => v.clone(),
            other => panic!("expected count entries, got {other:?}"),
        }
    }

    /// Read, prove and verify agree, and the verified root hash is the
    /// live one — the basic contract, exercised through the vocabulary
    /// rather than the bespoke methods.
    #[test]
    fn top_k_round_trips_through_the_vocabulary() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_count_fixture(&db, gv, &[(b"a", 3), (b"b", 7), (b"c", 5)]);

        let query = AxisPathQuery::top_k(count_path(), IndexAxis::Count, 2, 0, true);

        let read = db
            .query_axis_path_query(&query, None, gv)
            .unwrap()
            .expect("read succeeds");
        assert_eq!(
            counts(&read),
            vec![(7, b"b".to_vec()), (5, b"c".to_vec())],
            "top 2 by count, descending"
        );

        let proof = db
            .prove_axis_path_query(&query, None, gv)
            .unwrap()
            .expect("prove succeeds");
        let verified = GroveDb::verify_axis_path_query(&proof, &query).expect("verify succeeds");
        assert_eq!(counts(&verified.entries), counts(&read));
        assert_eq!(verified.skipped, 0);
        assert_eq!(
            verified.root_hash,
            db.root_hash(None, gv).unwrap().expect("root hash")
        );
    }

    /// The bounded traversal's lowering — inclusive on both ends,
    /// bracketing every key-suffix at the boundary sort key.
    #[test]
    fn bounded_traversal_is_inclusive_on_both_ends() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_count_fixture(&db, gv, &[(b"a", 3), (b"b", 7), (b"c", 5), (b"d", 9)]);

        let query = AxisPathQuery::bounded(count_path(), IndexAxis::Count, 5, 7, 10, false);
        let read = db
            .query_axis_path_query(&query, None, gv)
            .unwrap()
            .expect("read succeeds");
        assert_eq!(
            counts(&read),
            vec![(5, b"c".to_vec()), (7, b"b".to_vec())],
            "both bounds inclusive: 5 and 7 in, 3 and 9 out"
        );

        let proof = db
            .prove_axis_path_query(&query, None, gv)
            .unwrap()
            .expect("prove succeeds");
        let verified = GroveDb::verify_axis_path_query(&proof, &query).expect("verify succeeds");
        assert_eq!(counts(&verified.entries), counts(&read));
        assert_eq!(
            verified.root_hash,
            db.root_hash(None, gv).unwrap().expect("root hash")
        );
    }

    /// Every axis is reachable through the same vocabulary, and each
    /// answers on its own ordering.
    #[test]
    fn all_three_axes_round_trip() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_multi_axis_fixture(&db, gv, &[(b"x", 10), (b"y", 30), (b"z", 20)]);

        for axis in [IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg] {
            let query = AxisPathQuery::top_k(multi_axis_path(), axis, 3, 0, true);
            let read = db
                .query_axis_path_query(&query, None, gv)
                .unwrap()
                .unwrap_or_else(|e| panic!("{axis:?} read: {e}"));
            let proof = db
                .prove_axis_path_query(&query, None, gv)
                .unwrap()
                .unwrap_or_else(|e| panic!("{axis:?} prove: {e}"));
            let verified = GroveDb::verify_axis_path_query(&proof, &query)
                .unwrap_or_else(|e| panic!("{axis:?} verify: {e}"));
            assert_eq!(
                verified.entries.len(),
                read.len(),
                "{axis:?}: read and verified entry counts must agree"
            );
            assert_eq!(
                verified.root_hash,
                db.root_hash(None, gv).unwrap().expect("root hash"),
                "{axis:?}: root hash"
            );
        }
    }

    /// **The refactor-safety property.** A proof produced through the
    /// vocabulary is byte-identical to one produced by calling the
    /// underlying primitive directly, for both traversals — so this is
    /// dispatch, not a new proof shape, and existing verifiers are
    /// unaffected.
    #[test]
    fn emitted_proofs_are_byte_identical_to_the_primitives() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_count_fixture(&db, gv, &[(b"a", 3), (b"b", 7), (b"c", 5)]);
        let path: Vec<&[u8]> = vec![TEST_LEAF, PCIT];

        // TopK ↔ prove_indexed_axis_top_k_paginated
        let via_vocabulary = db
            .prove_axis_path_query(
                &AxisPathQuery::top_k(count_path(), IndexAxis::Count, 2, 1, true),
                None,
                gv,
            )
            .unwrap()
            .expect("vocabulary prove");
        let via_primitive = db
            .prove_indexed_axis_top_k_paginated(
                path.as_slice(),
                IndexAxis::Count,
                2,
                1,
                true,
                None,
                gv,
            )
            .unwrap()
            .expect("primitive prove");
        assert_eq!(
            via_vocabulary, via_primitive,
            "the top-k vocabulary must emit the primitive's exact bytes"
        );

        // Bounded ↔ prove_indexed_axis_query, with the same lowering
        // the vocabulary performs.
        let bounded = AxisQuery::bounded(IndexAxis::Count, 5, 7, 10, false);
        let via_vocabulary = db
            .prove_axis_path_query(&AxisPathQuery::new(count_path(), bounded), None, gv)
            .unwrap()
            .expect("vocabulary prove");
        let via_primitive = db
            .prove_indexed_axis_query(
                path.as_slice(),
                IndexAxis::Count,
                bounded.merk_query().expect("lowering"),
                Some(10),
                None,
                gv,
            )
            .unwrap()
            .expect("primitive prove");
        assert_eq!(
            via_vocabulary, via_primitive,
            "the bounded vocabulary must emit the primitive's exact bytes"
        );
    }

    /// The lowering the vocabulary owns produces the same secondary
    /// query a caller would hand-build — the property that lets both
    /// sides of the prover/verifier boundary share it.
    #[test]
    fn bounds_lowering_matches_a_hand_built_secondary_query() {
        let query = AxisQuery::bounded(IndexAxis::Count, 5, 7, 10, false);
        let lowered = query.merk_query().expect("lowering");

        let mut expected = MerkQuery::new();
        expected.insert_item(MerkQueryItem::Range(
            5u64.to_be_bytes().to_vec()..8u64.to_be_bytes().to_vec(),
        ));
        expected.left_to_right = true;
        assert_eq!(lowered.items, expected.items);
        assert_eq!(lowered.left_to_right, expected.left_to_right);

        // At the axis maximum there is no successor, so the range is
        // open-ended rather than wrapping.
        let open = AxisQuery::bounded(IndexAxis::Count, 0, u64::MAX as i128, 10, false)
            .merk_query()
            .expect("lowering");
        assert!(
            matches!(open.items.first(), Some(MerkQueryItem::RangeFrom(_))),
            "an upper bound at the axis maximum lowers to an open range, got {:?}",
            open.items.first()
        );
    }

    /// A query that cannot describe any answer is rejected rather than
    /// returning an empty page that looks like real absence.
    #[test]
    fn degenerate_queries_are_rejected() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_count_fixture(&db, gv, &[(b"a", 3)]);

        let cases = vec![
            (
                "zero k",
                AxisPathQuery::top_k(count_path(), IndexAxis::Count, 0, 0, true),
            ),
            (
                "zero limit",
                AxisPathQuery::bounded(count_path(), IndexAxis::Count, 0, 10, 0, true),
            ),
            (
                "inverted bounds",
                AxisPathQuery::bounded(count_path(), IndexAxis::Count, 10, 5, 10, true),
            ),
            (
                "bounds below the count domain",
                AxisPathQuery::bounded(count_path(), IndexAxis::Count, -100, -1, 10, true),
            ),
            (
                "empty path",
                AxisPathQuery::top_k(vec![], IndexAxis::Count, 2, 0, true),
            ),
        ];
        for (label, query) in cases {
            assert!(
                db.query_axis_path_query(&query, None, gv).unwrap().is_err(),
                "{label}: read must reject"
            );
            assert!(
                db.prove_axis_path_query(&query, None, gv).unwrap().is_err(),
                "{label}: prove must reject"
            );
        }
    }

    /// The verifier's own query is what the envelope is checked
    /// against, so a proof does not verify under a different one.
    #[test]
    fn a_proof_does_not_verify_under_a_different_query() {
        let gv = GroveVersion::latest();
        let db = make_test_grovedb(gv);
        build_count_fixture(&db, gv, &[(b"a", 3), (b"b", 7), (b"c", 5)]);

        let query = AxisPathQuery::top_k(count_path(), IndexAxis::Count, 2, 0, true);
        let proof = db
            .prove_axis_path_query(&query, None, gv)
            .unwrap()
            .expect("prove");

        for (label, other) in [
            (
                "different k",
                AxisPathQuery::top_k(count_path(), IndexAxis::Count, 3, 0, true),
            ),
            (
                "different offset",
                AxisPathQuery::top_k(count_path(), IndexAxis::Count, 2, 1, true),
            ),
            (
                "different direction",
                AxisPathQuery::top_k(count_path(), IndexAxis::Count, 2, 0, false),
            ),
            (
                "different traversal",
                AxisPathQuery::bounded(count_path(), IndexAxis::Count, 0, 10, 2, true),
            ),
        ] {
            assert!(
                GroveDb::verify_axis_path_query(&proof, &other).is_err(),
                "{label}: must not verify"
            );
        }
    }

    /// The vocabulary survives a bincode round trip, including the
    /// hand-written axis-tag codec.
    #[test]
    fn the_vocabulary_round_trips_through_bincode() {
        let config = bincode::config::standard();
        for query in [
            AxisPathQuery::top_k(count_path(), IndexAxis::Avg, 5, 12, true),
            AxisPathQuery::bounded(count_path(), IndexAxis::Sum, -50, 50, 7, false),
        ] {
            let bytes = bincode::encode_to_vec(&query, config).expect("encode");
            let (decoded, _): (AxisPathQuery, _) =
                bincode::decode_from_slice(&bytes, config).expect("decode");
            assert_eq!(decoded, query);
        }

        // An unknown axis tag is rejected rather than silently
        // becoming a valid axis.
        let mut bytes =
            bincode::encode_to_vec(AxisQuery::top_k(IndexAxis::Count, 1, 0, true), config)
                .expect("encode");
        bytes[0] = 99;
        assert!(
            bincode::decode_from_slice::<AxisQuery, _>(&bytes, config).is_err(),
            "an unknown axis tag must not decode"
        );
    }
}
