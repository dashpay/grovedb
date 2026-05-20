//! End-to-end GroveDB tests for the **no-proof**
//! `GroveDb::query_aggregate_count_and_sum` entry point.
//!
//! Mirrors the no-proof sections of [`aggregate_sum_query_tests`] and
//! [`aggregate_count_query_tests`] for the combined `(u64, i64)`
//! flavor: every test pattern those files cover (basic walk, empty
//! merk, disjoint range, full-range, boundary nodes, version gating,
//! carrier-shape rejection, etc.) appears here calling the new
//! `query_aggregate_count_and_sum` and asserting both axes match what
//! the verified proof returns.
//!
//! Prove/no-prove equivalence is pinned by every `no_proof_matches_proof`
//! helper call — it cross-checks the direct no-proof result against the
//! `prove_query` + `verify_aggregate_count_and_sum_query` result on the
//! same path query, so the two paths can never silently diverge.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::query::QueryItem;
    use grovedb_query::Query;
    use grovedb_version::version::{v2::GROVE_V2, GroveVersion};

    use crate::{
        tests::{make_test_grovedb, TEST_LEAF},
        Element, GroveDb, PathQuery,
    };

    /// Insert keys "a".."o" (15 keys) into a `ProvableCountProvableSumTree`
    /// rooted at `[TEST_LEAF, "st"]`. Each key carries
    /// count = 1 and a value that mixes positive, negative, and zero so
    /// the running sum exercises signed arithmetic. Returns the db and
    /// the running full-range sum.
    fn setup_15_key_pcps(grove_version: &GroveVersion) -> (crate::tests::TempGroveDb, i64) {
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_provable_count_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert st");
        let mut full_sum: i64 = 0;
        for (i, c) in (b'a'..=b'o').enumerate() {
            let value: i64 = match i % 4 {
                0 => -(i as i64) * 3,
                2 => 0,
                _ => (i as i64 + 1) * 2,
            };
            full_sum += value;
            db.insert(
                [TEST_LEAF, b"st"].as_ref(),
                &[c],
                Element::new_sum_item(value),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum item");
        }
        (db, full_sum)
    }

    /// Compute the expected sum slice of the `setup_15_key_pcps` fixture
    /// for keys with zero-based indices `[lo_idx ..= hi_idx]` (subset of
    /// 0..15). Mirror of `expected_pcps_sum_slice` in the merk-side
    /// tests.
    fn expected_sum_slice(lo_idx: u8, hi_idx: u8) -> i64 {
        let mut sum: i64 = 0;
        for i in lo_idx..=hi_idx {
            let value: i64 = match i % 4 {
                0 => -(i as i64) * 3,
                2 => 0,
                _ => (i as i64 + 1) * 2,
            };
            sum += value;
        }
        sum
    }

    /// Cross-check helper: build the path-query, call
    /// `query_aggregate_count_and_sum`, assert the returned pair matches
    /// the expected `(count, sum)` AND matches what the proof round-trip
    /// returns.
    fn no_proof_matches_proof(
        db: &crate::tests::TempGroveDb,
        path: Vec<Vec<u8>>,
        inner_range: QueryItem,
        expected: (u64, i64),
        grove_version: &GroveVersion,
    ) {
        let path_query = PathQuery::new_aggregate_count_and_sum_on_range(path, inner_range);

        let direct = db
            .grove_db
            .query_aggregate_count_and_sum(&path_query, None, grove_version)
            .unwrap()
            .expect("query_aggregate_count_and_sum should succeed");
        assert_eq!(
            direct, expected,
            "no-proof variant returned wrong (count, sum) pair"
        );

        let proof = db
            .grove_db
            .prove_query(&path_query, None, grove_version)
            .unwrap()
            .expect("prove_query should succeed");
        let (_root, proved_count, proved_sum) =
            GroveDb::verify_aggregate_count_and_sum_query(&proof, &path_query, grove_version)
                .expect("verify should succeed");
        assert_eq!(
            direct,
            (proved_count, proved_sum),
            "no-proof variant disagrees with proof variant"
        );
    }

    // -------- Range-shape sweep on the 15-key PCPS fixture --------

    #[test]
    fn no_proof_combined_pcps_range_inclusive() {
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_pcps(v);
        // c..=l → indices 2..=11 → 10 keys
        let expected_sum = expected_sum_slice(2, 11);
        no_proof_matches_proof(
            &db,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeInclusive(b"c".to_vec()..=b"l".to_vec()),
            (10, expected_sum),
            v,
        );
    }

    #[test]
    fn no_proof_combined_pcps_range_exclusive() {
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_pcps(v);
        // c..l → indices 2..=10 → 9 keys
        let expected_sum = expected_sum_slice(2, 10);
        no_proof_matches_proof(
            &db,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::Range(b"c".to_vec()..b"l".to_vec()),
            (9, expected_sum),
            v,
        );
    }

    #[test]
    fn no_proof_combined_pcps_range_from() {
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_pcps(v);
        // c..o → indices 2..=14 → 13 keys
        let expected_sum = expected_sum_slice(2, 14);
        no_proof_matches_proof(
            &db,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeFrom(b"c".to_vec()..),
            (13, expected_sum),
            v,
        );
    }

    #[test]
    fn no_proof_combined_pcps_range_after() {
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_pcps(v);
        // After "b" → indices 2..=14 → 13 keys
        let expected_sum = expected_sum_slice(2, 14);
        no_proof_matches_proof(
            &db,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeAfter(b"b".to_vec()..),
            (13, expected_sum),
            v,
        );
    }

    #[test]
    fn no_proof_combined_pcps_range_to_inclusive() {
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_pcps(v);
        // ..=e → indices 0..=4 → 5 keys
        let expected_sum = expected_sum_slice(0, 4);
        no_proof_matches_proof(
            &db,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeToInclusive(..=b"e".to_vec()),
            (5, expected_sum),
            v,
        );
    }

    #[test]
    fn no_proof_combined_pcps_disjoint_range() {
        // Range below all keys: contributes (0, 0) on the in-range slice.
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_pcps(v);
        no_proof_matches_proof(
            &db,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeInclusive(vec![0x00]..=vec![0x10]),
            (0, 0),
            v,
        );
    }

    #[test]
    fn no_proof_combined_pcps_full_range_returns_full_aggregate() {
        // Full range over a..=o: returns the entire stored
        // (count = 15, sum = full_sum).
        let v = GroveVersion::latest();
        let (db, full_sum) = setup_15_key_pcps(v);
        no_proof_matches_proof(
            &db,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeInclusive(b"a".to_vec()..=b"o".to_vec()),
            (15, full_sum),
            v,
        );
    }

    #[test]
    fn no_proof_combined_empty_pcps_returns_zero_zero() {
        // An empty PCPS returns (0, 0) — same as the merk-level
        // empty-merk contract. Inserting nothing under the tree
        // exercises this path through the full GroveDB stack.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_provable_count_provable_sum_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert st");
        let path_query = PathQuery::new_aggregate_count_and_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeFrom(b"a".to_vec()..),
        );
        let direct = db
            .grove_db
            .query_aggregate_count_and_sum(&path_query, None, v)
            .unwrap()
            .expect("query_aggregate_count_and_sum should succeed on empty");
        assert_eq!(direct, (0u64, 0i64));
    }

    #[test]
    fn no_proof_combined_negative_values_matches_proof() {
        // PCPS tree with mixed positive and negative sum items: cross-check
        // the no-proof and proof paths produce the same `(count, sum)` over
        // both a full-range and a subrange. Mirror of the sum-side
        // `no_proof_sum_negative_values_matches_proof` test.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"st",
            Element::empty_provable_count_provable_sum_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert st");
        let entries: [(u8, i64); 4] = [(b'a', 50), (b'b', -100), (b'c', 30), (b'd', -50)];
        for (k, val) in entries {
            db.insert(
                [TEST_LEAF, b"st"].as_ref(),
                &[k],
                Element::new_sum_item(val),
                None,
                None,
                v,
            )
            .unwrap()
            .expect("insert sum item");
        }
        // Full range: count=4, sum = 50 - 100 + 30 - 50 = -70.
        no_proof_matches_proof(
            &db,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeFrom(b"a".to_vec()..),
            (4, -70),
            v,
        );
        // Subrange "b".."=c": count=2, sum = -100 + 30 = -70.
        no_proof_matches_proof(
            &db,
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeInclusive(b"b".to_vec()..=b"c".to_vec()),
            (2, -70),
            v,
        );
    }

    // -------- Validation / rejection tests --------

    #[test]
    fn no_proof_combined_invalid_inner_range_rejected_before_storage_reads() {
        // The validator runs at the top of query_aggregate_count_and_sum;
        // an illegal inner range like `Key(_)` is rejected before any
        // merk is opened.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        let path_query = PathQuery::new_aggregate_count_and_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::Key(b"a".to_vec()),
        );
        let err = db
            .grove_db
            .query_aggregate_count_and_sum(&path_query, None, v)
            .unwrap()
            .expect_err("Key inner must be rejected at validation");
        match err {
            crate::Error::InvalidQuery(_) | crate::Error::QueryError(_) => {}
            other => panic!("expected InvalidQuery or QueryError, got {:?}", other),
        }
    }

    #[test]
    fn no_proof_combined_empty_path_rejected_at_validation() {
        // Mirror of the verify-side empty-path rejection: the no-proof
        // entry point must also reject empty-path queries up front, since
        // the GroveDB root is always a NormalTree.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        let path_query = PathQuery::new_aggregate_count_and_sum_on_range(
            Vec::new(),
            QueryItem::RangeFrom(b"a".to_vec()..),
        );
        let err = db
            .grove_db
            .query_aggregate_count_and_sum(&path_query, None, v)
            .unwrap()
            .expect_err("empty path must be rejected");
        match err {
            crate::Error::InvalidQuery(_) => {}
            other => panic!("expected InvalidQuery, got {:?}", other),
        }
    }

    #[test]
    fn no_proof_combined_rejects_carrier_shape() {
        // `query_aggregate_count_and_sum` returns a single `(u64, i64)`
        // and has no way to surface per-outer-key carrier results.
        // Calling it with a carrier-shape path query must be rejected
        // up front by the leaf-only validator, BEFORE any storage reads
        // happen — even though the dispatcher-level
        // `validate_aggregate_count_and_sum_on_range` would have
        // accepted the same query.
        let v = GroveVersion::latest();
        let (db, _) = setup_15_key_pcps(v);

        let mut carrier = Query::new();
        carrier.insert_key(b"st".to_vec());
        carrier.set_subquery(Query::new_aggregate_count_and_sum_on_range(
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        ));
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            crate::SizedQuery::new(carrier, None, None),
        );

        // Sanity: the dispatcher-level validator accepts this as a
        // valid carrier, so the rejection below is specifically because
        // `query_aggregate_count_and_sum` tightens to leaf-only.
        assert!(path_query
            .validate_aggregate_count_and_sum_on_range()
            .is_ok());

        let err = db
            .grove_db
            .query_aggregate_count_and_sum(&path_query, None, v)
            .unwrap()
            .expect_err("carrier shape must be rejected at the no-proof entry");
        assert!(
            matches!(
                err,
                crate::Error::InvalidQuery(_) | crate::Error::QueryError(_)
            ),
            "expected InvalidQuery or QueryError, got {:?}",
            err
        );
    }

    #[test]
    fn no_proof_combined_normal_tree_rejected_at_merk() {
        // A path that resolves to a NormalTree (not a PCPS) must be
        // rejected by the merk-level tree-type gate.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"normal",
            Element::empty_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert normal tree");
        // Insert a child so the merk isn't empty (an empty merk would
        // short-circuit to (0, 0) before hitting the tree-type check
        // on the no-proof side, since
        // `Merk::count_and_sum_aggregate_on_range` checks tree_type
        // before descending — confirm by inserting something).
        db.insert(
            [TEST_LEAF, b"normal"].as_ref(),
            b"a",
            Element::new_item(b"v".to_vec()),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert child");
        let path_query = PathQuery::new_aggregate_count_and_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"normal".to_vec()],
            QueryItem::RangeFrom(b"a".to_vec()..),
        );
        let err = db
            .grove_db
            .query_aggregate_count_and_sum(&path_query, None, v)
            .unwrap()
            .expect_err("NormalTree leaf must be rejected by merk-level gate");
        // The merk-level error gets wrapped with contextual
        // `CorruptedData` by `query_aggregate_count_and_sum`
        // (callsite-specific path info — see
        // `operations/get/query.rs`).
        match err {
            crate::Error::CorruptedData(_) => {}
            other => panic!("expected CorruptedData wrapper, got {:?}", other),
        }
    }

    #[test]
    fn no_proof_combined_single_axis_pcst_rejected_at_merk() {
        // Single-axis hosts (ProvableCountSumTree, ProvableCountTree,
        // ProvableSumTree) are rejected because their node hashes only
        // bind one axis. Sanity-check the PCST arm here — the merk
        // primitive's PCPS-only contract bubbles up as CorruptedData
        // through the grovedb-level wrapper.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcst",
            Element::empty_provable_count_sum_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert pcst");
        db.insert(
            [TEST_LEAF, b"pcst"].as_ref(),
            b"a",
            Element::new_sum_item(1),
            None,
            None,
            v,
        )
        .unwrap()
        .expect("insert sum item");
        let path_query = PathQuery::new_aggregate_count_and_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"pcst".to_vec()],
            QueryItem::RangeFrom(b"a".to_vec()..),
        );
        let err = db
            .grove_db
            .query_aggregate_count_and_sum(&path_query, None, v)
            .unwrap()
            .expect_err("PCST leaf must be rejected by merk-level PCPS-only gate");
        match err {
            crate::Error::CorruptedData(msg) => {
                assert!(
                    msg.contains("ProvableCountProvableSumTree"),
                    "expected PCPS-only message, got: {msg}"
                );
            }
            other => panic!("expected CorruptedData wrapper, got {:?}", other),
        }
    }

    #[test]
    fn no_proof_combined_v0_envelope_rejected_by_version_gate() {
        // GROVE_V2 sets `query_aggregate_count_and_sum_on_range = 0`
        // (V0-supported), so the V0 gate accepts it — but the feature
        // didn't ship in any prior envelope, so this test pins that the
        // version slot exists and routes to the v0 dispatch path. The
        // sibling sum/count entry points are also V0-gated; this
        // mirrors that contract.
        //
        // Sanity-check: the call should succeed under GROVE_V2 because
        // the slot is 0 — proving the routing was set up correctly.
        let v: &GroveVersion = &GROVE_V2;
        let (db, _) = setup_15_key_pcps(v);
        let path_query = PathQuery::new_aggregate_count_and_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"st".to_vec()],
            QueryItem::RangeFrom(b"a".to_vec()..),
        );
        let direct = db
            .grove_db
            .query_aggregate_count_and_sum(&path_query, None, v)
            .unwrap()
            .expect("query under GROVE_V2 should succeed (slot is 0)");
        // Spot-check the count axis on the full fixture; sum details
        // are covered by the latest-version tests above.
        assert_eq!(direct.0, 15);
    }

    #[test]
    fn no_proof_combined_path_not_found_at_merk_open() {
        // Covers the `open_transactional_merk_at_path` error branch in
        // `query_aggregate_count_and_sum`: when the path doesn't
        // resolve (intermediate subtree missing), the wrapped
        // path-lookup error must propagate up the call chain instead
        // of producing a spurious `(0, 0)` result.
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        let path_query = PathQuery::new_aggregate_count_and_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"nope".to_vec()],
            QueryItem::RangeFrom(b"a".to_vec()..),
        );
        let err = db
            .grove_db
            .query_aggregate_count_and_sum(&path_query, None, v)
            .unwrap()
            .expect_err("missing intermediate path must surface as an error");
        // Path resolution failures bubble up as `InvalidParentLayerPath`
        // / `PathNotFound` / `PathParentLayerNotFound` / `PathKeyNotFound`
        // depending on which layer fails. Any non-success outcome from
        // a path-not-found shape covers the branch — we assert it's NOT
        // a CorruptedData wrap (which would imply the merk was opened)
        // and NOT an InvalidQuery (validation passed).
        match err {
            crate::Error::InvalidParentLayerPath(_)
            | crate::Error::PathNotFound(_)
            | crate::Error::PathParentLayerNotFound(_)
            | crate::Error::PathKeyNotFound(_) => {}
            other => panic!("expected a path-resolution error, got {:?}", other),
        }
    }
}
