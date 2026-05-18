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

    // =================================================================
    // Additional coverage: tampering, edge cases, axis-rejection grid
    // =================================================================

    /// Helper: build a PSIT and prove a sum top-k. Returns (proof, path).
    fn psit_sum_top_k_proof<'a>(
        db: &GroveDb,
        grove_version: &GroveVersion,
        k: u16,
        descending: bool,
    ) -> Vec<u8> {
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        db.prove_indexed_sum_top_k(path, k, descending, None, grove_version)
            .unwrap()
            .expect("prove")
    }

    // --- Range tamper grid: 3 axes × varied tamper sites ---

    #[test]
    fn verify_indexed_sum_top_k_rejects_tampered_layer_proof() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, &[(b"a", 1), (b"b", 2)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let mut proof = psit_sum_top_k_proof(&db, grove_version, 2, true);
        // Flip a byte near the front of the proof (within the layer
        // proof region after the brief header).
        let i = 12.min(proof.len() - 1);
        proof[i] ^= 0xFF;
        let result = GroveDb::verify_indexed_sum_top_k(&proof, path, 2, true);
        assert!(result.is_err(), "tampered layer proof should not verify");
    }

    #[test]
    fn verify_indexed_count_paginated_rejects_tampered_bytes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1), (b"b", 2), (b"c", 3)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let mut proof = db
            .prove_indexed_count_top_k_paginated(path, 1, 1, true, None, grove_version)
            .unwrap()
            .expect("prove");
        // Tamper near the middle of the proof.
        let i = proof.len() / 2;
        proof[i] ^= 0xFF;
        let result = GroveDb::verify_indexed_count_top_k_paginated(&proof, path, 1, 1, true);
        assert!(
            result.is_err(),
            "tampered paginated proof should not verify"
        );
    }

    #[test]
    fn verify_indexed_sum_paginated_rejects_tampered_bytes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, &[(b"a", 1), (b"b", 2), (b"c", 3)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let mut proof = db
            .prove_indexed_sum_top_k_paginated(path, 1, 1, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let i = proof.len() - 5;
        proof[i] ^= 0xFF;
        let result = GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 1, 1, true);
        assert!(
            result.is_err(),
            "tampered sum paginated proof should not verify"
        );
    }

    #[test]
    fn verify_indexed_avg_top_k_rejects_tampered_bytes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(
            &db,
            grove_version,
            &[IndexAxis::Avg.tag()],
            &[(b"a", 1), (b"b", 2), (b"c", 3)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];
        let mut proof = db
            .prove_indexed_avg_top_k(path, 2, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let i = proof.len() - 8;
        proof[i] ^= 0xFF;
        let result = GroveDb::verify_indexed_avg_top_k(&proof, path, 2, true);
        assert!(
            result.is_err(),
            "tampered avg top-k proof should not verify"
        );
    }

    #[test]
    fn verify_indexed_avg_paginated_rejects_tampered_bytes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(
            &db,
            grove_version,
            &[IndexAxis::Avg.tag()],
            &[(b"a", 1), (b"b", 2), (b"c", 3)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];
        let mut proof = db
            .prove_indexed_avg_top_k_paginated(path, 1, 1, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let i = proof.len() / 3;
        proof[i] ^= 0xFF;
        let result = GroveDb::verify_indexed_avg_top_k_paginated(&proof, path, 1, 1, true);
        assert!(
            result.is_err(),
            "tampered avg paginated proof should not verify"
        );
    }

    #[test]
    fn verify_indexed_count_aggregate_rejects_tampered_bytes() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1), (b"b", 5)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let mut proof = db
            .prove_indexed_count_range_aggregate(path, 0, 10, None, grove_version)
            .unwrap()
            .expect("prove");
        // Tamper at multiple sites: front and back.
        let i = proof.len() - 4;
        proof[i] ^= 0xFF;
        let result = GroveDb::verify_indexed_count_range_aggregate(&proof, path, 0, 10);
        assert!(
            result.is_err(),
            "tampered count aggregate proof should not verify"
        );
    }

    #[test]
    fn verify_indexed_count_query_rejects_lo_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1), (b"b", 5)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_range_aggregate(path, 0, 10, None, grove_version)
            .unwrap()
            .expect("prove");
        // Wrong expected lo on verify.
        let result = GroveDb::verify_indexed_count_range_aggregate(&proof, path, 1, 10);
        assert!(result.is_err(), "lo mismatch should be rejected");
    }

    #[test]
    fn verify_indexed_count_aggregate_rejects_hi_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1), (b"b", 5)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_range_aggregate(path, 0, 10, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_count_range_aggregate(&proof, path, 0, 11);
        assert!(result.is_err(), "hi mismatch should be rejected");
    }

    #[test]
    fn verify_indexed_axis_aggregate_rejects_avg_axis_at_verify() {
        // Sum-axis aggregate proof is built; then we try to verify under
        // Avg axis. The pre-check at the verifier returns NotSupported.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let proof = db
            .prove_indexed_sum_range_aggregate(path, 0, 10, None, grove_version)
            .unwrap()
            .expect("prove");
        // Tamper the envelope to claim axis=Avg by reading and forging
        // bytes is brittle; instead just call verify_indexed_axis_range_aggregate
        // expecting axis=Avg — the axis-tag mismatch fires first.
        let result =
            GroveDb::verify_indexed_axis_range_aggregate(&proof, path, IndexAxis::Avg, 0, 10);
        assert!(matches!(result, Err(Error::CorruptedData(_))));
    }

    #[test]
    fn verify_indexed_axis_paginated_axis_mismatch_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_top_k_paginated(path, 1, 0, true, None, grove_version)
            .unwrap()
            .expect("prove");
        // Envelope tag is Count, verify under Sum.
        let result =
            GroveDb::verify_indexed_axis_top_k_paginated(&proof, path, IndexAxis::Sum, 1, 0, true);
        assert!(matches!(result, Err(Error::CorruptedData(_))));
    }

    #[test]
    fn verify_indexed_axis_paginated_k_mismatch_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_top_k_paginated(path, 1, 0, true, None, grove_version)
            .unwrap()
            .expect("prove");
        // Wrong k on verify.
        let result = GroveDb::verify_indexed_count_top_k_paginated(&proof, path, 3, 0, true);
        assert!(matches!(result, Err(Error::CorruptedData(_))));
    }

    #[test]
    fn verify_indexed_axis_paginated_direction_mismatch_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_top_k_paginated(path, 1, 0, true, None, grove_version)
            .unwrap()
            .expect("prove");
        // Verify with wrong direction.
        let result = GroveDb::verify_indexed_count_top_k_paginated(&proof, path, 1, 0, false);
        assert!(matches!(result, Err(Error::CorruptedData(_))));
    }

    #[test]
    fn verify_indexed_query_limit_mismatch_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let mut q = MerkQuery::new();
        q.insert_all();
        let proof = db
            .prove_indexed_count_query(path, q.clone(), Some(2), None, grove_version)
            .unwrap()
            .expect("prove");
        // Verify with wrong expected_limit.
        let result = GroveDb::verify_indexed_count_query(&proof, path, q, Some(5));
        assert!(matches!(result, Err(Error::CorruptedData(_))));
    }

    #[test]
    fn verify_indexed_query_direction_mismatch_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let mut q_asc = MerkQuery::new();
        q_asc.insert_all();
        q_asc.left_to_right = true;
        let proof = db
            .prove_indexed_count_query(path, q_asc.clone(), Some(2), None, grove_version)
            .unwrap()
            .expect("prove");
        let mut q_desc = MerkQuery::new();
        q_desc.insert_all();
        q_desc.left_to_right = false;
        let result = GroveDb::verify_indexed_count_query(&proof, path, q_desc, Some(2));
        assert!(matches!(result, Err(Error::CorruptedData(_))));
    }

    #[test]
    fn decode_garbage_bytes_rejected() {
        // Pure garbage bytes — bincode decode fails.
        let garbage = vec![0xFFu8; 4];
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let r1 = GroveDb::verify_indexed_count_top_k(&garbage, path, 1, true);
        assert!(matches!(r1, Err(Error::CorruptedData(_))));
        let r2 = GroveDb::verify_indexed_count_top_k_paginated(&garbage, path, 1, 0, true);
        assert!(matches!(r2, Err(Error::CorruptedData(_))));
        let r3 = GroveDb::verify_indexed_count_range_aggregate(&garbage, path, 0, 10);
        assert!(matches!(r3, Err(Error::CorruptedData(_))));
    }

    // --- Edge case combinations ---

    #[test]
    fn pcit_top_k_zero_returns_empty() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1), (b"b", 2)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_top_k(path, 0, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_count_top_k(&proof, path, 0, true).expect("verify");
        assert!(result.entries.is_empty());
        assert_eq!(result.entries.len(), 0);
    }

    #[test]
    fn pcit_top_k_larger_than_total_returns_all() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1), (b"b", 2)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_top_k(path, 100, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_count_top_k(&proof, path, 100, true).expect("verify");
        assert_eq!(result.entries.len(), 2);
    }

    #[test]
    fn pcit_top_k_paginated_offset_zero_returns_all() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1), (b"b", 2), (b"c", 3)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_top_k_paginated(path, 3, 0, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_count_top_k_paginated(&proof, path, 3, 0, true)
            .expect("verify");
        assert_eq!(result.skipped, 0);
        assert_eq!(result.entries.len(), 3);
    }

    #[test]
    fn pcit_top_k_paginated_offset_larger_than_total_returns_empty() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1), (b"b", 2)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_top_k_paginated(path, 2, 100, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_count_top_k_paginated(&proof, path, 2, 100, true)
            .expect("verify");
        // Only 2 entries total; offset 100 → empty result.
        assert_eq!(result.entries.len(), 0);
    }

    #[test]
    fn psit_top_k_paginated_offset_larger_than_total_returns_empty() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, &[(b"a", 1), (b"b", 2)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 2, 100, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 2, 100, true)
            .expect("verify");
        // 2 entries in proof, skip should be min(100, 2) = 2 → empty.
        assert_eq!(result.skipped, 2);
        assert_eq!(result.entries.len(), 0);
    }

    #[test]
    fn count_aggregate_range_at_u64_max_round_trip() {
        // Test the hi=u64::MAX (RangeFrom) path in count_aggregate_inner_range.
        // We use moderate count values but ask for [100, u64::MAX].
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1), (b"b", 1000), (b"c", 100)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_range_aggregate(path, 100, u64::MAX, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_count_range_aggregate(&proof, path, 100, u64::MAX)
            .expect("verify");
        // c(100) and b(1000) in range; a(1) excluded.
        assert_eq!(result.aggregate, 2);
    }

    #[test]
    fn count_aggregate_full_range_equals_total() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1), (b"b", 2), (b"c", 5)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_range_aggregate(path, 0, u64::MAX, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_count_range_aggregate(&proof, path, 0, u64::MAX)
            .expect("verify");
        assert_eq!(result.aggregate, 3);
    }

    #[test]
    fn count_aggregate_empty_primary_returns_zero() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_range_aggregate(path, 0, 100, None, grove_version)
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_count_range_aggregate(&proof, path, 0, 100).expect("verify");
        assert_eq!(result.aggregate, 0);
    }

    #[test]
    fn sum_aggregate_range_at_i64_max_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, &[(b"a", i64::MAX), (b"b", 0)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let proof = db
            .prove_indexed_sum_range_aggregate(path, 1, i64::MAX, None, grove_version)
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_sum_range_aggregate(&proof, path, 1, i64::MAX).expect("verify");
        // Only a(i64::MAX) is in [1, i64::MAX].
        assert_eq!(result.aggregate, i64::MAX as i128);
    }

    #[test]
    fn sum_aggregate_negative_only_range() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(
            &db,
            grove_version,
            &[(b"a", -10), (b"b", -5), (b"c", 5), (b"d", 10)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let proof = db
            .prove_indexed_sum_range_aggregate(path, -100, -1, None, grove_version)
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_sum_range_aggregate(&proof, path, -100, -1).expect("verify");
        // -10 + -5 = -15
        assert_eq!(result.aggregate, -15);
    }

    #[test]
    fn count_aggregate_negative_hi_returns_zero() {
        // hi < 0 → empty-range builder path.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1), (b"b", 5)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_axis_range_aggregate(
                path,
                IndexAxis::Count,
                -50,
                -10,
                None,
                grove_version,
            )
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_axis_range_aggregate(&proof, path, IndexAxis::Count, -50, -10)
                .expect("verify");
        assert_eq!(result.aggregate, 0);
    }

    #[test]
    fn count_top_k_on_empty_primary_returns_error_from_merk() {
        // Empty merk secondary can't produce a range proof; we verify
        // the prover surfaces a CorruptedData error (the wrapped merk
        // "Cannot create proof for empty tree" path).
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let result = db
            .prove_indexed_count_top_k(path, 5, true, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::CorruptedData(_))));
    }

    #[test]
    fn sum_top_k_on_empty_primary_returns_error_from_merk() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, &[]);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let result = db
            .prove_indexed_sum_top_k(path, 5, true, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::CorruptedData(_))));
    }

    #[test]
    fn avg_top_k_on_empty_pcpsit_returns_error_from_merk() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(&db, grove_version, &[IndexAxis::Avg.tag()], &[]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];
        let result = db
            .prove_indexed_avg_top_k(path, 5, true, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::CorruptedData(_))));
    }

    // --- Cross-axis on PCPSIT ---

    #[test]
    fn pcpsit_multi_axis_proves_independently() {
        // Same PCPSIT, query count, sum, avg, each independently. Each
        // proof should verify and reconstruct the same root hash.
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
        let root = root_hash(&db, grove_version);

        let proof_c = db
            .prove_indexed_count_top_k(path, 3, true, None, grove_version)
            .unwrap()
            .expect("prove count");
        let r_c =
            GroveDb::verify_indexed_count_top_k(&proof_c, path, 3, true).expect("verify count");
        assert_eq!(r_c.root_hash, root);

        let proof_s = db
            .prove_indexed_sum_top_k(path, 3, true, None, grove_version)
            .unwrap()
            .expect("prove sum");
        let r_s = GroveDb::verify_indexed_sum_top_k(&proof_s, path, 3, true).expect("verify sum");
        assert_eq!(r_s.root_hash, root);

        let proof_a = db
            .prove_indexed_avg_top_k(path, 3, true, None, grove_version)
            .unwrap()
            .expect("prove avg");
        let r_a = GroveDb::verify_indexed_avg_top_k(&proof_a, path, 3, true).expect("verify avg");
        assert_eq!(r_a.root_hash, root);
    }

    // --- Axis-rejection grid (variant × axis) ---

    #[test]
    fn pcit_rejects_avg_axis_query() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1)]);
        let result = db
            .prove_indexed_avg_top_k([TEST_LEAF, b"pcit"].as_ref(), 1, false, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    #[test]
    fn pcpsit_count_only_rejects_avg_axis_query() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(&db, grove_version, &[IndexAxis::Count.tag()], &[(b"a", 1)]);
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
    fn pcpsit_sum_only_rejects_avg_axis_query() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(&db, grove_version, &[IndexAxis::Sum.tag()], &[(b"a", 1)]);
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
    fn pcpsit_count_avg_only_rejects_sum_axis_query() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(
            &db,
            grove_version,
            &[IndexAxis::Count.tag(), IndexAxis::Avg.tag()],
            &[(b"a", 1)],
        );
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
    fn pcpsit_sum_avg_only_rejects_count_axis_query() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(
            &db,
            grove_version,
            &[IndexAxis::Sum.tag(), IndexAxis::Avg.tag()],
            &[(b"a", 1)],
        );
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

    // --- Paginated + aggregate root-path rejection ---

    #[test]
    fn paginated_at_root_path_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty: &[&[u8]] = &[];
        let result = db
            .prove_indexed_count_top_k_paginated(empty, 1, 0, true, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    #[test]
    fn aggregate_at_root_path_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        let empty: &[&[u8]] = &[];
        let result = db
            .prove_indexed_count_range_aggregate(empty, 0, 10, None, grove_version)
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    #[test]
    fn paginated_on_non_indexed_target_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
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
            .prove_indexed_count_top_k_paginated(
                [TEST_LEAF, b"plain"].as_ref(),
                1,
                0,
                true,
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    #[test]
    fn aggregate_on_non_indexed_target_is_rejected() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
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
            .prove_indexed_count_range_aggregate(
                [TEST_LEAF, b"plain"].as_ref(),
                0,
                10,
                None,
                grove_version,
            )
            .unwrap();
        assert!(matches!(result, Err(Error::InvalidPath(_))));
    }

    // --- Descending direction round-trips per axis ---

    #[test]
    fn pcit_descending_query_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1), (b"b", 5), (b"c", 10)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let mut q = MerkQuery::new();
        q.insert_all();
        q.left_to_right = false;
        let proof = db
            .prove_indexed_count_query(path, q.clone(), Some(3), None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_count_query(&proof, path, q, Some(3)).expect("verify");
        let entries = entries_as_count(&result.entries);
        assert_eq!(
            entries,
            &[
                (10u64, b"c".to_vec()),
                (5u64, b"b".to_vec()),
                (1u64, b"a".to_vec())
            ]
        );
    }

    #[test]
    fn psit_ascending_query_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, &[(b"a", -3), (b"b", 0), (b"c", 7)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let mut q = MerkQuery::new();
        q.insert_all();
        q.left_to_right = true;
        let proof = db
            .prove_indexed_sum_query(path, q.clone(), Some(3), None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_sum_query(&proof, path, q, Some(3)).expect("verify");
        let entries = entries_as_sum(&result.entries);
        assert_eq!(
            entries,
            &[
                (-3i64, b"a".to_vec()),
                (0, b"b".to_vec()),
                (7, b"c".to_vec())
            ]
        );
    }

    #[test]
    fn avg_axis_descending_paginated_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(
            &db,
            grove_version,
            &[IndexAxis::Avg.tag()],
            &[(b"a", 5), (b"b", 10), (b"c", 20)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];
        let proof = db
            .prove_indexed_avg_top_k_paginated(path, 1, 0, false, None, grove_version)
            .unwrap()
            .expect("prove");
        let result =
            GroveDb::verify_indexed_avg_top_k_paginated(&proof, path, 1, 0, false).expect("verify");
        assert_eq!(result.skipped, 0);
        assert_eq!(result.entries.len(), 1);
    }

    // --- Direct verify_indexed_axis_query for arbitrary query ---

    #[test]
    fn verify_indexed_axis_query_pcpsit_avg_returns_root() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcpsit(
            &db,
            grove_version,
            &[IndexAxis::Avg.tag()],
            &[(b"a", 5), (b"b", 10)],
        );
        let path: &[&[u8]] = &[TEST_LEAF, b"pcpsit"];
        let mut q = MerkQuery::new();
        q.insert_all();
        q.left_to_right = true;
        let proof = db
            .prove_indexed_avg_query(path, q.clone(), None, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_avg_query(&proof, path, q, None).expect("verify");
        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.root_hash, root_hash(&db, grove_version));
    }

    // --- Nested PCPSIT under tree depth 2 ---

    #[test]
    fn pcpsit_at_depth_2_round_trip() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"outer",
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create outer tree");
        let axes: Vec<(u8, Option<Vec<u8>>)> =
            vec![(IndexAxis::Count.tag(), None), (IndexAxis::Sum.tag(), None)];
        let elem =
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("axes canonical");
        db.insert(
            [TEST_LEAF, b"outer"].as_ref(),
            b"pcpsit",
            elem,
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPSIT under outer");
        for (k, sum) in &[
            (b"a" as &[u8], 5i64),
            (b"b" as &[u8], 10),
            (b"c" as &[u8], 15),
        ] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"outer", b"pcpsit"].as_ref(),
                k,
                Element::new_item_with_sum_item(b"v".to_vec(), *sum),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCPSIT entry");
        }
        let path: &[&[u8]] = &[TEST_LEAF, b"outer", b"pcpsit"];
        let proof = db
            .prove_indexed_sum_top_k(path, 3, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_sum_top_k(&proof, path, 3, true).expect("verify");
        let entries = entries_as_sum(&result.entries);
        assert_eq!(
            entries,
            &[
                (15i64, b"c".to_vec()),
                (10, b"b".to_vec()),
                (5, b"a".to_vec())
            ]
        );
        assert_eq!(result.root_hash, root_hash(&db, grove_version));
    }

    // --- Aggregate query AVG axis at the public API level ---

    #[test]
    fn prove_indexed_axis_range_aggregate_avg_returns_not_supported() {
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

    // --- AxisEntries methods (Display/coverage glue) ---

    #[test]
    fn axis_entries_helpers() {
        let c = AxisEntries::Count(vec![(1u64, b"a".to_vec())]);
        assert_eq!(c.len(), 1);
        assert!(!c.is_empty());
        let empty_c = AxisEntries::Count(vec![]);
        assert_eq!(empty_c.len(), 0);
        assert!(empty_c.is_empty());
        let s = AxisEntries::Sum(vec![(1i64, b"a".to_vec()), (2i64, b"b".to_vec())]);
        assert_eq!(s.len(), 2);
        let a = AxisEntries::Avg(vec![(1i128, b"a".to_vec())]);
        assert_eq!(a.len(), 1);
    }

    // -----------------------------------------------------------------
    // Round 8: layer-count vs path-length mismatch — exercises the
    // "indexed-axis <shape> proof has N layers but path has M segments"
    // error in verify_indexed_axis_{range,paginated,aggregate}_inner.
    // The envelope is valid; we just lie about the path at verify time.
    // -----------------------------------------------------------------

    #[test]
    fn verify_indexed_count_top_k_rejects_path_length_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_top_k(path, 1, true, None, grove_version)
            .unwrap()
            .expect("prove");
        // Lie about path: extend by one extra segment.
        let bad: &[&[u8]] = &[TEST_LEAF, b"pcit", b"extra"];
        let r = GroveDb::verify_indexed_count_top_k(&proof, bad, 1, true);
        assert!(matches!(r, Err(Error::CorruptedData(s)) if s.contains("layers")));
    }

    #[test]
    fn verify_indexed_sum_top_k_rejects_path_length_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let proof = db
            .prove_indexed_sum_top_k(path, 1, true, None, grove_version)
            .unwrap()
            .expect("prove");
        // Drop a path segment.
        let bad: &[&[u8]] = &[TEST_LEAF];
        let r = GroveDb::verify_indexed_sum_top_k(&proof, bad, 1, true);
        assert!(matches!(r, Err(Error::CorruptedData(s)) if s.contains("layers")));
    }

    #[test]
    fn verify_indexed_count_paginated_rejects_path_length_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_top_k_paginated(path, 1, 0, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let bad: &[&[u8]] = &[TEST_LEAF];
        let r = GroveDb::verify_indexed_count_top_k_paginated(&proof, bad, 1, 0, true);
        assert!(matches!(r, Err(Error::CorruptedData(s)) if s.contains("layers")));
    }

    #[test]
    fn verify_indexed_sum_paginated_rejects_path_length_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 1, 0, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let bad: &[&[u8]] = &[TEST_LEAF];
        let r = GroveDb::verify_indexed_sum_top_k_paginated(&proof, bad, 1, 0, true);
        assert!(matches!(r, Err(Error::CorruptedData(s)) if s.contains("layers")));
    }

    #[test]
    fn verify_indexed_count_aggregate_rejects_path_length_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_range_aggregate(path, 0, 10, None, grove_version)
            .unwrap()
            .expect("prove");
        let bad: &[&[u8]] = &[TEST_LEAF];
        let r = GroveDb::verify_indexed_count_range_aggregate(&proof, bad, 0, 10);
        assert!(matches!(r, Err(Error::CorruptedData(s)) if s.contains("layers")));
    }

    #[test]
    fn verify_indexed_sum_aggregate_rejects_path_length_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let proof = db
            .prove_indexed_sum_range_aggregate(path, -10, 10, None, grove_version)
            .unwrap()
            .expect("prove");
        let bad: &[&[u8]] = &[TEST_LEAF, b"psit", b"extra"];
        let r = GroveDb::verify_indexed_sum_range_aggregate(&proof, bad, -10, 10);
        assert!(matches!(r, Err(Error::CorruptedData(s)) if s.contains("layers")));
    }

    /// verify_indexed_axis_query (non-top_k arbitrary query) also has
    /// limit_for_verify routing. Exercise limit=None acceptance and
    /// limit=Some-not-matching rejection (axis_mismatch path).
    #[test]
    fn verify_indexed_count_query_rejects_axis_mismatch() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1), (b"b", 2)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let mut q = MerkQuery::new();
        q.insert_all();
        let proof = db
            .prove_indexed_count_query(path, q.clone(), None, None, grove_version)
            .unwrap()
            .expect("prove");
        // Verify under sum axis — axis tag mismatch should be reported.
        let result = GroveDb::verify_indexed_axis_query(&proof, path, IndexAxis::Sum, q, None);
        assert!(matches!(result, Err(Error::CorruptedData(_))));
    }

    /// verify_indexed_axis_top_k for the unified entry under axis=Avg
    /// against a Count-axis proof exercises the axis_tag mismatch arm
    /// inside `verify_indexed_axis_top_k`.
    #[test]
    fn verify_indexed_axis_top_k_rejects_axis_tag_mismatch_avg_vs_count() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, &[(b"a", 1)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"pcit"];
        let proof = db
            .prove_indexed_count_top_k(path, 1, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let r = GroveDb::verify_indexed_axis_top_k(&proof, path, IndexAxis::Avg, 1, true);
        assert!(matches!(r, Err(Error::CorruptedData(s)) if s.contains("axis mismatch")));
    }

    /// PSIT paginated with empty primary (only top of subtree) and
    /// post-skip exceeding total: exercises the `min(total_returned)`
    /// branch in `verify_indexed_axis_paginated_inner` for the Sum
    /// axis.
    #[test]
    fn psit_paginated_sum_skip_exceeds_total_returns_empty() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, &[(b"a", 1), (b"b", 2)]);
        let path: &[&[u8]] = &[TEST_LEAF, b"psit"];
        let proof = db
            .prove_indexed_sum_top_k_paginated(path, 1, 1000, true, None, grove_version)
            .unwrap()
            .expect("prove");
        let result = GroveDb::verify_indexed_sum_top_k_paginated(&proof, path, 1, 1000, true)
            .expect("verify");
        assert!(result.entries.is_empty());
        // skipped is clamped to total_returned which is 2 (whole secondary)
        // when combined_limit = offset+k = 1001 covers everything.
        assert!(result.skipped <= 2);
    }
}
