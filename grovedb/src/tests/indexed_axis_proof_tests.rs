//! End-to-end proof tests for the Phase-4 unified per-axis proof
//! envelopes (`prove_indexed_axis_*` / `verify_indexed_axis_*` and
//! their `prove/verify_indexed_<axis>_*` per-axis wrappers).
//!
//! Coverage targets:
//! - PCIT against the **count** axis (compatibility with the
//!   pre-existing PCIT envelope behavior — same data, same shape).
//! - PSIT against the **sum** axis.
//! - PCPSIT against each of {count, sum, avg}, including subsets where
//!   only one axis is in the TLV.
//! - Negative paths: wrong axis on wrong variant, tampered bytes,
//!   mismatched k / offset / direction.

#[cfg(test)]
mod tests {
    use grovedb_element::indexed::IndexAxis;
    use grovedb_merk::proofs::Query as MerkQuery;
    use grovedb_version::version::GroveVersion;

    use crate::{
        operations::proof::indexed_axis::AxisEntries,
        tests::{make_test_grovedb, TEST_LEAF},
        Element, Error, GroveDb,
    };

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    /// Build a PCIT at `[TEST_LEAF, b"pcit"]` and insert the supplied
    /// `(key, count)` entries.
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
        for (k, c) in entries {
            let ct = Element::new_provable_count_tree_with_flags_and_count_value(None, *c, None);
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"pcit"].as_ref(),
                k,
                ct,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCIT entry");
        }
    }

    /// Build a PSIT at `[TEST_LEAF, b"psit"]` and insert the supplied
    /// `(key, sum)` entries via `Element::new_sum_item`.
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

    /// Build a PCPSIT at `[TEST_LEAF, b"pcpsit"]` with the supplied
    /// axis subset, then insert `(key, sum_value)` entries. Each insert
    /// goes through `insert_into_provable_count_provable_sum_indexed_tree`
    /// with an `ItemWithSumItem`.
    fn build_pcpsit(
        db: &GroveDb,
        grove_version: &GroveVersion,
        axes_tags: &[u8],
        entries: &[(&[u8], i64)],
    ) {
        let axes: Vec<(u8, Option<Vec<u8>>)> = axes_tags.iter().map(|t| (*t, None)).collect();
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

    fn root_hash(db: &GroveDb, grove_version: &GroveVersion) -> [u8; 32] {
        db.root_hash(None, grove_version).unwrap().expect("root")
    }

    fn entries_as_count(entries: &AxisEntries) -> &[(u64, Vec<u8>)] {
        match entries {
            AxisEntries::Count(v) => v.as_slice(),
            other => panic!("expected count entries, got {:?}", other),
        }
    }

    fn entries_as_sum(entries: &AxisEntries) -> &[(i64, Vec<u8>)] {
        match entries {
            AxisEntries::Sum(v) => v.as_slice(),
            other => panic!("expected sum entries, got {:?}", other),
        }
    }

    fn entries_as_avg(entries: &AxisEntries) -> &[(i128, Vec<u8>)] {
        match entries {
            AxisEntries::Avg(v) => v.as_slice(),
            other => panic!("expected avg entries, got {:?}", other),
        }
    }

    // =================================================================
    // PCIT × count axis (compat with PCIT proof family)
    // =================================================================

    #[test]
    fn pcit_indexed_axis_top_k_descending_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(
            &db,
            grove_version,
            &[(b"alice", 5), (b"bob", 12), (b"carol", 1), (b"dave", 7)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_top_k(path, 3, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_count_top_k(&proof, path, 3, true).expect("verify");
        let entries = entries_as_count(&result.entries);
        assert_eq!(
            entries,
            &[
                (12u64, b"bob".to_vec()),
                (7u64, b"dave".to_vec()),
                (5u64, b"alice".to_vec()),
            ]
        );
        assert_eq!(result.root_hash, root_hash(&db, grove_version));
    }

    #[test]
    fn pcit_indexed_axis_top_k_ascending_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(
            &db,
            grove_version,
            &[(b"alice", 5), (b"bob", 12), (b"carol", 1), (b"dave", 7)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_top_k(path, 3, false, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_count_top_k(&proof, path, 3, false).expect("verify");
        let entries = entries_as_count(&result.entries);
        assert_eq!(
            entries,
            &[
                (1u64, b"carol".to_vec()),
                (5u64, b"alice".to_vec()),
                (7u64, b"dave".to_vec()),
            ]
        );
    }

    #[test]
    fn pcit_indexed_axis_paginated_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(
            &db,
            grove_version,
            &[
                (b"a", 1),
                (b"b", 2),
                (b"c", 3),
                (b"d", 4),
                (b"e", 5),
                (b"f", 6),
            ],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_top_k_paginated(path, 2, 2, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_count_top_k_paginated(&proof, path, 2, 2, true)
            .expect("verify");
        // Descending paged after skipping 2: c(3 was top-3? no — desc top-2 = f(6), e(5);
        // after skip-2 of f,e → d(4), c(3).
        let entries = entries_as_count(&result.entries);
        assert_eq!(entries, &[(4u64, b"d".to_vec()), (3u64, b"c".to_vec())]);
        assert_eq!(result.skipped, 2);
        assert_eq!(result.root_hash, root_hash(&db, grove_version));
    }

    #[test]
    fn pcit_indexed_axis_aggregate_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(
            &db,
            grove_version,
            &[(b"a", 1), (b"b", 5), (b"c", 10), (b"d", 20)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_range_aggregate(path, 5, 15, None, grove_version)
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_count_range_aggregate(&proof, path, 5, 15).expect("verify");
        // b(5) + c(10) — both in [5,15]; a(1) outside, d(20) outside.
        assert_eq!(result.aggregate, 2);
        assert_eq!(result.axis, IndexAxis::Count);
        assert_eq!(result.root_hash, root_hash(&db, grove_version));
    }

    #[test]
    fn pcit_indexed_axis_query_arbitrary_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1), (b"b", 5), (b"c", 10)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let mut q = MerkQuery::new();
        q.insert_all();
        q.left_to_right = true;
        let proof = db
            .prove_indexed_count_query(path, q.clone(), Some(10), None, grove_version)
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_count_query(&proof, path, q, Some(10)).expect("verify");
        let entries = entries_as_count(&result.entries);
        // Ascending all: 1,5,10.
        assert_eq!(
            entries,
            &[
                (1u64, b"a".to_vec()),
                (5u64, b"b".to_vec()),
                (10u64, b"c".to_vec()),
            ]
        );
    }

    // =================================================================
    // PSIT × sum axis
    // =================================================================

    #[test]
    fn psit_indexed_axis_top_k_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(
            &db,
            grove_version,
            &[(b"a", 5), (b"b", -3), (b"c", 10), (b"d", 7)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let proof = db
            .prove_indexed_sum_top_k(path, 3, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_sum_top_k(&proof, path, 3, true).expect("verify");
        let entries = entries_as_sum(&result.entries);
        assert_eq!(
            entries,
            &[
                (10i64, b"c".to_vec()),
                (7, b"d".to_vec()),
                (5, b"a".to_vec())
            ]
        );
        assert_eq!(result.root_hash, root_hash(&db, grove_version));
    }

    #[test]
    fn psit_indexed_axis_top_k_handles_negative_sums_descending() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(
            &db,
            grove_version,
            &[(b"a", -100), (b"b", -50), (b"c", 0), (b"d", 50)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let proof = db
            .prove_indexed_sum_top_k(path, 4, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_sum_top_k(&proof, path, 4, true).expect("verify");
        let entries = entries_as_sum(&result.entries);
        assert_eq!(
            entries,
            &[
                (50i64, b"d".to_vec()),
                (0, b"c".to_vec()),
                (-50, b"b".to_vec()),
                (-100, b"a".to_vec()),
            ]
        );
    }

    #[test]
    fn psit_indexed_axis_paginated_round_trip_uses_fallback() {
        // Sum axis has no count-offset primitive — verify the fallback
        // (regular range proof + post-skip) round-trips correctly.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(
            &db,
            grove_version,
            &[
                (b"a", 1),
                (b"b", 2),
                (b"c", 3),
                (b"d", 4),
                (b"e", 5),
                (b"f", 6),
            ],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 2, 2, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 2, 2, true).expect("verify");
        // Descending after skip-2: 6,5 skipped → d(4), c(3) returned.
        let entries = entries_as_sum(&result.entries);
        assert_eq!(entries, &[(4i64, b"d".to_vec()), (3, b"c".to_vec())]);
        assert_eq!(result.skipped, 2);
    }

    #[test]
    fn psit_indexed_axis_aggregate_round_trip_sum() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(
            &db,
            grove_version,
            &[(b"a", -10), (b"b", 5), (b"c", 20), (b"d", 30)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let proof = db
            .prove_indexed_sum_range_aggregate(path, 0, 25, None, grove_version)
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_sum_range_aggregate(&proof, path, 0, 25).expect("verify");
        // In [0,25]: b(5) + c(20) = 25.
        assert_eq!(result.aggregate, 25);
        assert_eq!(result.axis, IndexAxis::Sum);
        assert_eq!(result.root_hash, root_hash(&db, grove_version));
    }

    #[test]
    fn psit_axis_rejects_count_query_against_psit() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, &[(b"a", 1)]);
        let result = db
            .prove_indexed_count_top_k([TEST_LEAF, b"psit"].as_ref(), 1, false, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    #[test]
    fn psit_axis_rejects_avg_query_against_psit() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, &[(b"a", 1)]);
        let result = db
            .prove_indexed_avg_top_k([TEST_LEAF, b"psit"].as_ref(), 1, false, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    // =================================================================
    // PCPSIT × each axis
    // =================================================================

    #[test]
    fn pcpsit_indexed_count_top_k_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(
            &db,
            grove_version,
            &[
                IndexAxis::Count.tag(),
                IndexAxis::Sum.tag(),
                IndexAxis::Avg.tag(),
            ],
            &[(b"a", 5), (b"b", 10), (b"c", 20)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];
        let proof = db
            .prove_indexed_count_top_k(path, 3, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_count_top_k(&proof, path, 3, true).expect("verify");
        // Each ItemWithSumItem insert contributes count = 1, so all
        // count_values are 1 — secondary keys differ only by original
        // key suffix, and the result list is in descending lex order
        // of original_key for ties (b/c/a → c, b, a).
        let entries = entries_as_count(&result.entries);
        assert_eq!(entries.len(), 3);
        for (c, _) in entries {
            assert_eq!(*c, 1);
        }
        assert_eq!(result.root_hash, root_hash(&db, grove_version));
    }

    #[test]
    fn pcpsit_indexed_sum_top_k_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(
            &db,
            grove_version,
            &[
                IndexAxis::Count.tag(),
                IndexAxis::Sum.tag(),
                IndexAxis::Avg.tag(),
            ],
            &[(b"a", 5), (b"b", 10), (b"c", 20)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];
        let proof = db
            .prove_indexed_sum_top_k(path, 3, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_sum_top_k(&proof, path, 3, true).expect("verify");
        let entries = entries_as_sum(&result.entries);
        assert_eq!(
            entries,
            &[
                (20i64, b"c".to_vec()),
                (10, b"b".to_vec()),
                (5, b"a".to_vec())
            ]
        );
    }

    #[test]
    fn pcpsit_indexed_avg_top_k_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Each item has count=1 + its sum, so avg = sum/1 = sum (in
        // fixed-point at SCALE=10^15 → multiply by 10^15).
        const SCALE: i128 = grovedb_element::indexed::AVG_FIXED_POINT_SCALE;
        build_pcpsit(
            &db,
            grove_version,
            &[
                IndexAxis::Count.tag(),
                IndexAxis::Sum.tag(),
                IndexAxis::Avg.tag(),
            ],
            &[(b"a", 5), (b"b", 10), (b"c", 20)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];
        let proof = db
            .prove_indexed_avg_top_k(path, 3, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_avg_top_k(&proof, path, 3, true).expect("verify");
        let entries = entries_as_avg(&result.entries);
        assert_eq!(
            entries,
            &[
                (20i128 * SCALE, b"c".to_vec()),
                (10 * SCALE, b"b".to_vec()),
                (5 * SCALE, b"a".to_vec()),
            ]
        );
    }

    #[test]
    fn pcpsit_indexed_count_aggregate_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(
            &db,
            grove_version,
            &[IndexAxis::Count.tag()],
            &[(b"a", 5), (b"b", 10), (b"c", 20)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];
        let proof = db
            .prove_indexed_count_range_aggregate(path, 1, 1, None, grove_version)
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_count_range_aggregate(&proof, path, 1, 1).expect("verify");
        // All 3 entries have count_value=1, so [1,1] captures all 3.
        assert_eq!(result.aggregate, 3);
    }

    #[test]
    fn pcpsit_indexed_sum_aggregate_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(
            &db,
            grove_version,
            &[IndexAxis::Sum.tag()],
            &[(b"a", -5), (b"b", 10), (b"c", 20), (b"d", 30)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];
        let proof = db
            .prove_indexed_sum_range_aggregate(path, 0, 25, None, grove_version)
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_sum_range_aggregate(&proof, path, 0, 25).expect("verify");
        // In [0,25]: b(10) + c(20) = 30.
        assert_eq!(result.aggregate, 30);
    }

    #[test]
    fn pcpsit_paginated_count_axis_uses_count_offset() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(
            &db,
            grove_version,
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            &[(b"a", 1), (b"b", 2), (b"c", 3), (b"d", 4)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];
        let proof = db
            .prove_indexed_count_top_k_paginated(path, 2, 1, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_count_top_k_paginated(&proof, path, 2, 1, true)
            .expect("verify");
        // count_value=1 for all 4 entries (ItemWithSumItem). Descending
        // order by (count=1 ‖ original_key) → desc lex: d, c, b, a.
        // Skip-1 then take-2 → c, b.
        let entries = entries_as_count(&result.entries);
        assert_eq!(entries.len(), 2);
        for (c, _) in entries {
            assert_eq!(*c, 1);
        }
        assert_eq!(result.skipped, 1);
    }

    #[test]
    fn pcpsit_paginated_avg_axis_uses_count_offset() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(
            &db,
            grove_version,
            &[IndexAxis::Avg.tag()],
            &[(b"a", 5), (b"b", 10), (b"c", 20), (b"d", 30)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];
        let proof = db
            .prove_indexed_avg_top_k_paginated(path, 2, 1, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_avg_top_k_paginated(&proof, path, 2, 1, true).expect("verify");
        assert_eq!(result.skipped, 1);
        let entries = entries_as_avg(&result.entries);
        assert_eq!(entries.len(), 2);
        const SCALE: i128 = grovedb_element::indexed::AVG_FIXED_POINT_SCALE;
        // Descending top-3 by avg: d(30), c(20), b(10). After skip-1 → c, b.
        assert_eq!(
            entries,
            &[(20i128 * SCALE, b"c".to_vec()), (10 * SCALE, b"b".to_vec())]
        );
    }

    // =================================================================
    // Axis-subset rejection (PCPSIT TLV-based)
    // =================================================================

    #[test]
    fn pcpsit_count_only_rejects_sum_axis_query() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(&db, grove_version, &[IndexAxis::Count.tag()], &[(b"a", 1)]);
        let result = db
            .prove_indexed_sum_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                1,
                false,
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    #[test]
    fn pcpsit_sum_only_rejects_count_axis_query() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(&db, grove_version, &[IndexAxis::Sum.tag()], &[(b"a", 1)]);
        let result = db
            .prove_indexed_count_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                1,
                false,
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    #[test]
    fn pcpsit_count_sum_only_rejects_avg_axis_query() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(
            &db,
            grove_version,
            &[IndexAxis::Count.tag(), IndexAxis::Sum.tag()],
            &[(b"a", 1)],
        );
        let result = db
            .prove_indexed_avg_top_k(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                1,
                false,
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    #[test]
    fn pcit_rejects_sum_axis_query() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1)]);
        let result = db
            .prove_indexed_sum_top_k([TEST_LEAF, b"pcit"].as_ref(), 1, false, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    // =================================================================
    // Avg-axis aggregate is not supported
    // =================================================================

    #[test]
    fn avg_axis_aggregate_is_not_supported() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(&db, grove_version, &[IndexAxis::Avg.tag()], &[(b"a", 1)]);
        let result = db
            .prove_indexed_axis_range_aggregate(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                IndexAxis::Avg,
                0,
                100,
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::NotSupported(_))));
    }

    // =================================================================
    // Tamper-detection
    // =================================================================

    #[test]
    fn verify_indexed_count_top_k_rejects_tampered_bytes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1), (b"b", 2)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let mut proof = db
            .prove_indexed_count_top_k(path, 2, true, None, grove_version)
            .unwrap()
            .expect("prove");
        // Tamper near the end of the secondary proof region.
        let i = proof.len() - 10;
        proof[i] ^= 0xFF;
        let result = GroveDb::verify_indexed_count_top_k(&proof, path, 2, true);
        assert!(result.is_err(), "tampered proof should not verify");
    }

    #[test]
    fn verify_indexed_sum_aggregate_rejects_tampered_bytes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, &[(b"a", 1), (b"b", 5)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let mut proof = db
            .prove_indexed_sum_range_aggregate(path, 0, 10, None, grove_version)
            .unwrap()
            .expect("prove");
        let i = proof.len() / 2;
        proof[i] ^= 0xFF;
        let result = GroveDb::verify_indexed_sum_range_aggregate(&proof, path, 0, 10);
        assert!(result.is_err(), "tampered proof should not verify");
    }

    #[test]
    fn verify_indexed_count_top_k_rejects_axis_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_top_k(path, 1, true, None, grove_version)
            .unwrap()
            .expect("prove");
        // Verifying with Sum axis must fail (the envelope tag is Count).
        let result = GroveDb::verify_indexed_axis_top_k(&proof, path, IndexAxis::Sum, 1, true);
        assert!(result.is_err());
    }

    #[test]
    fn verify_indexed_count_top_k_rejects_k_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_top_k(path, 3, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_count_top_k(&proof, path, 1, true);
        assert!(result.is_err());
    }

    #[test]
    fn verify_indexed_count_top_k_rejects_direction_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_top_k(path, 1, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_count_top_k(&proof, path, 1, false);
        assert!(result.is_err());
    }

    #[test]
    fn verify_indexed_sum_paginated_rejects_offset_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, &[(b"a", 1), (b"b", 2)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 1, 0, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 1, 1, true);
        assert!(result.is_err());
    }

    // =================================================================
    // Nested PCIT under PCIT — exercises the
    // AncestorAttestation::SingleSecondary path. (Nested PSIT/PCPSIT
    // *under* a PCIT is not yet supported by the Phase-2 insert path,
    // so we use a PCIT-nested-in-PCIT topology that IS supported.)
    // =================================================================

    #[test]
    fn nested_pcit_under_pcit_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_provable_count_indexed_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer PCIT");
        db.insert_into_count_indexed_tree(
            [TEST_LEAF, b"outer"].as_ref(),
            b"inner",
            Element::empty_provable_count_indexed_tree(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert PCIT inside PCIT primary");
        for (k, c) in &[(b"a" as &[u8], 4u64), (b"b" as &[u8], 9u64)] {
            db.insert_into_count_indexed_tree(
                [TEST_LEAF, b"outer", b"inner"].as_ref(),
                k,
                Element::new_provable_count_tree_with_flags_and_count_value(None, *c, None),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert count entry under inner PCIT");
        }

        let path: &[&[u8]] = &[TEST_LEAF, b"outer", b"inner"];
        let proof = db
            .prove_indexed_count_top_k(path, 2, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_count_top_k(&proof, path, 2, true).expect("verify");
        let entries = entries_as_count(&result.entries);
        assert_eq!(entries, &[(9u64, b"b".to_vec()), (4, b"a".to_vec())]);
        assert_eq!(result.root_hash, root_hash(&db, grove_version));
    }

    // =================================================================
    // Empty range / degenerate inputs
    // =================================================================

    #[test]
    fn count_aggregate_lo_greater_than_hi_returns_zero() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 5)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_range_aggregate(path, 10, 5, None, grove_version)
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_count_range_aggregate(&proof, path, 10, 5).expect("verify");
        assert_eq!(result.aggregate, 0);
    }

    #[test]
    fn sum_aggregate_lo_greater_than_hi_returns_zero() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, &[(b"a", 5)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let proof = db
            .prove_indexed_sum_range_aggregate(path, 10, 5, None, grove_version)
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_sum_range_aggregate(&proof, path, 10, 5).expect("verify");
        assert_eq!(result.aggregate, 0);
    }

    #[test]
    fn top_k_at_root_path_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty: &[&[u8]] = &[];
        let result = db
            .prove_indexed_count_top_k(empty, 3, true, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    #[test]
    fn top_k_on_non_indexed_target_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Insert a regular tree (not indexed); target it.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"plain",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create plain tree");
        let result = db
            .prove_indexed_count_top_k([TEST_LEAF, b"plain"].as_ref(), 3, true, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }
}
