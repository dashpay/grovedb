//! Version-gated merge semantics and the indexed-axis version wiring.
//!
//! `path_query_methods.merge = 1` (GROVE_V4) makes `PathQuery::merge`
//! direction-aware: every merged query must agree on `left_to_right`
//! and the merged query carries it; a conflict is a typed error. Below
//! V4 the long-standing silent first-wins behavior is preserved —
//! merged queries feed proofs, and the verifier re-runs the same merge
//! with the same grove version, so both sides agree at every version.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::{query::query_item::QueryItem, Query};
    use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

    use crate::{Error, PathQuery};

    fn directional_query(path_key: &[u8], left_to_right: bool) -> PathQuery {
        let mut query = Query::new_with_direction(left_to_right);
        query.insert_item(QueryItem::RangeFull(..));
        PathQuery::new_unsized(vec![path_key.to_vec()], query)
    }

    #[test]
    fn v4_merge_requires_direction_agreement_and_propagates_it() {
        let v4 = GroveVersion::latest();
        assert_eq!(v4.protocol_version, 4);

        // Agreement: the shared direction survives the merge.
        for direction in [true, false] {
            let a = directional_query(b"a", direction);
            let b = directional_query(b"b", direction);
            let merged = PathQuery::merge(vec![&a, &b], v4).expect("agreeing merge succeeds");
            assert_eq!(
                merged.query.query.left_to_right, direction,
                "merged direction must be the shared one"
            );
        }

        // Conflict: typed rejection instead of silently keeping the
        // first query's direction.
        let ascending = directional_query(b"a", true);
        let descending = directional_query(b"b", false);
        match PathQuery::merge(vec![&ascending, &descending], v4) {
            Err(Error::NotSupported(message)) => {
                assert!(message.contains("direction"), "got: {message}")
            }
            other => panic!("conflicting directions must be rejected at V4, got {other:?}"),
        }
    }

    #[test]
    fn pre_v4_merge_keeps_the_silent_first_wins_behavior() {
        let v3 = &GROVE_VERSIONS[2];
        assert_eq!(v3.protocol_version, 3);

        let ascending = directional_query(b"a", true);
        let descending = directional_query(b"b", false);
        let merged = PathQuery::merge(vec![&descending, &ascending], v3)
            .expect("pre-V4 merge tolerates direction conflicts");
        // Historic quirk being preserved: for sub-level inputs the
        // merged root is a synthesized query whose direction is the
        // DEFAULT — the inputs' directions are silently dropped
        // entirely. (V4 requires agreement and propagates instead.)
        assert!(
            merged.query.query.left_to_right,
            "pre-V4 the merged root keeps the synthesized default direction"
        );
    }

    #[test]
    fn query_level_merges_reject_read_modes() {
        use grovedb_merk::proofs::query::{AxisQuery, IndexAxis, ReadMode};

        let mut axis_query = Query::new();
        axis_query.read_mode = Some(Box::new(ReadMode::Axis(AxisQuery::top_k(
            IndexAxis::Sum,
            1,
            0,
            true,
        ))));
        let plain = Query::new_single_key(b"k".to_vec());

        // Direct Query-level API (rs-drive-facing): read modes on
        // either side, at any nesting level, are rejected instead of
        // silently merged as key selection.
        match Query::merge_multiple(vec![plain.clone(), axis_query.clone()]) {
            Err(grovedb_query::error::Error::NotSupported(_)) => {}
            other => panic!("merge_multiple must reject read modes, got {other:?}"),
        }
        let mut target = plain.clone();
        match target.merge_with(axis_query.clone()) {
            Err(grovedb_query::error::Error::NotSupported(_)) => {}
            other => panic!("merge_with must reject read modes, got {other:?}"),
        }
        // Nested: a read mode hidden in a subquery branch is caught too.
        let mut nested = Query::new_single_key(b"outer".to_vec());
        nested.set_subquery(axis_query);
        match Query::merge_multiple(vec![plain, nested]) {
            Err(grovedb_query::error::Error::NotSupported(_)) => {}
            other => panic!("merge_multiple must reject nested read modes, got {other:?}"),
        }
    }

    #[test]
    fn indexed_axis_version_slots_are_wired() {
        // The slots exist so the first future divergence bumps a number
        // instead of forking silently; all-zero today. An unknown slot
        // value must be rejected by every gated entry point.
        let mut doctored = GroveVersion::latest().clone();
        doctored
            .grovedb_versions
            .operations
            .indexed_axis
            .verify_single_path = 9;
        let result = crate::GroveDb::verify_indexed_axis_top_k(
            &[0u8; 4],
            &[b"any".as_slice()],
            grovedb_merk::proofs::query::IndexAxis::Count,
            1,
            true,
            &doctored,
        );
        match result {
            Err(Error::VersionError(_)) => {}
            other => panic!("unknown verify_single_path version must be rejected, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Every gated entry point, not just one per slot
    //
    // The slots only do their job if EVERY entry point behind them
    // checks. One unchecked function is a silent fork the day a slot
    // diverges, so each is exercised against an unknown value.
    // -----------------------------------------------------------------

    /// A grove version with one indexed-axis slot set to an
    /// unrecognized value.
    fn doctored(slot: fn(&mut grovedb_version::version::GroveVersion)) -> GroveVersion {
        let mut version = GroveVersion::latest().clone();
        slot(&mut version);
        version
    }

    /// A PSIT at `[TEST_LEAF, b"psit"]` with a couple of entries, built
    /// at the real version so the fixture itself is not gated.
    fn psit_db(grove_version: &GroveVersion) -> (crate::tests::TempGroveDb, Vec<Vec<u8>>) {
        use crate::{
            tests::{make_test_grovedb, TEST_LEAF},
            Element,
        };
        let db = make_test_grovedb(grove_version);
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
        for (key, sum) in [(b"a", 5i64), (b"b", 9)] {
            db.insert_into_provable_sum_indexed_tree(
                [TEST_LEAF, b"psit"].as_ref(),
                key,
                Element::new_sum_item(sum),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PSIT entry");
        }
        (db, vec![TEST_LEAF.to_vec(), b"psit".to_vec()])
    }

    #[test]
    fn every_trusted_read_rejects_an_unknown_read_version() {
        use grovedb_merk::proofs::query::IndexAxis;

        let real = GroveVersion::latest();
        let (db, _) = psit_db(real);
        let bad = doctored(|v| v.grovedb_versions.operations.indexed_axis.read = 9);
        let path = [crate::tests::TEST_LEAF, b"psit"];

        macro_rules! assert_version_rejected {
            ($label:expr, $call:expr) => {
                match $call.unwrap() {
                    Err(Error::VersionError(_)) => {}
                    other => panic!(
                        "{} must reject an unknown read version, got {:?}",
                        $label, other
                    ),
                }
            };
        }

        assert_version_rejected!(
            "indexed_sum_top_k",
            db.indexed_sum_top_k(path.as_ref(), 1, true, None, &bad)
        );
        assert_version_rejected!(
            "indexed_sum_top_k_paginated",
            db.indexed_sum_top_k_paginated(path.as_ref(), 1, 0, true, None, &bad)
        );
        assert_version_rejected!(
            "indexed_sum_range",
            db.indexed_sum_range(path.as_ref(), 0, 100, true, 10, None, &bad)
        );
        assert_version_rejected!(
            "indexed_sum_aggregate_over_value_range",
            db.indexed_sum_aggregate_over_value_range(path.as_ref(), 0, 100, None, &bad)
        );
        assert_version_rejected!(
            "indexed_count_aggregate_over_value_range",
            db.indexed_count_aggregate_over_value_range(path.as_ref(), 0, 100, None, &bad)
        );
        // Sanity: the same calls succeed at the real version, so the
        // rejections above are the gate firing and not a broken fixture.
        db.indexed_sum_top_k(path.as_ref(), 1, true, None, real)
            .unwrap()
            .expect("ungated read works");
        let _ = IndexAxis::Sum;
    }

    #[test]
    fn every_prover_rejects_an_unknown_prove_version() {
        use grovedb_merk::proofs::{query::IndexAxis, Query as MerkQuery};

        let real = GroveVersion::latest();
        let (db, _) = psit_db(real);
        let bad = doctored(|v| v.grovedb_versions.operations.indexed_axis.prove_single_path = 9);
        let path = [crate::tests::TEST_LEAF, b"psit"];

        macro_rules! assert_version_rejected {
            ($label:expr, $call:expr) => {
                match $call.unwrap() {
                    Err(Error::VersionError(_)) => {}
                    other => panic!(
                        "{} must reject an unknown prove version, got {:?}",
                        $label, other
                    ),
                }
            };
        }

        assert_version_rejected!(
            "prove_indexed_sum_top_k",
            db.prove_indexed_sum_top_k(path.as_ref(), 1, true, None, &bad)
        );
        assert_version_rejected!(
            "prove_indexed_sum_top_k_paginated",
            db.prove_indexed_sum_top_k_paginated(path.as_ref(), 1, 0, true, None, &bad)
        );
        assert_version_rejected!(
            "prove_indexed_axis_query",
            db.prove_indexed_axis_query(
                path.as_ref(),
                IndexAxis::Sum,
                MerkQuery::new(),
                Some(1),
                None,
                &bad,
            )
        );
        assert_version_rejected!(
            "prove_indexed_axis_rank_of_key",
            db.prove_indexed_axis_rank_of_key(
                path.as_ref(),
                IndexAxis::Sum,
                b"a",
                true,
                None,
                &bad
            )
        );
        assert_version_rejected!(
            "prove_indexed_count_aggregate_over_value_range",
            db.prove_indexed_count_aggregate_over_value_range(path.as_ref(), 0, 100, None, &bad)
        );
    }

    #[test]
    fn every_verifier_rejects_an_unknown_verify_version() {
        use grovedb_merk::proofs::{query::IndexAxis, Query as MerkQuery};

        let bad = doctored(|v| {
            v.grovedb_versions
                .operations
                .indexed_axis
                .verify_single_path = 9
        });
        let path = [b"any".as_slice()];
        // The gate fires before any decoding, so garbage bytes are
        // enough — and prove it, by asserting the error is the VERSION
        // error rather than a decode failure.
        let garbage = [0u8; 4];

        macro_rules! assert_version_rejected {
            ($label:expr, $call:expr) => {
                match $call {
                    Err(Error::VersionError(_)) => {}
                    other => panic!(
                        "{} must reject an unknown verify version, got {:?}",
                        $label, other
                    ),
                }
            };
        }

        assert_version_rejected!(
            "verify_indexed_axis_top_k",
            crate::GroveDb::verify_indexed_axis_top_k(
                &garbage,
                &path,
                IndexAxis::Count,
                1,
                true,
                &bad,
            )
        );
        assert_version_rejected!(
            "verify_indexed_axis_top_k_paginated",
            crate::GroveDb::verify_indexed_axis_top_k_paginated(
                &garbage,
                &path,
                IndexAxis::Count,
                1,
                0,
                true,
                &bad,
            )
        );
        assert_version_rejected!(
            "verify_indexed_axis_query",
            crate::GroveDb::verify_indexed_axis_query(
                &garbage,
                &path,
                IndexAxis::Count,
                MerkQuery::new(),
                Some(1),
                &bad,
            )
        );
        assert_version_rejected!(
            "verify_indexed_axis_aggregate_over_value_range",
            crate::GroveDb::verify_indexed_axis_aggregate_over_value_range(
                &garbage,
                &path,
                IndexAxis::Count,
                0,
                100,
                &bad,
            )
        );
    }

    // -----------------------------------------------------------------
    // PathQuery::merge's own version handling
    // -----------------------------------------------------------------

    #[test]
    fn merge_rejects_an_unknown_merge_version() {
        let mut bad = GroveVersion::latest().clone();
        bad.grovedb_versions.path_query_methods.merge = 9;
        let a = directional_query(b"a", true);
        let b = directional_query(b"b", true);
        match PathQuery::merge(vec![&a, &b], &bad) {
            Err(Error::VersionError(_)) => {}
            other => panic!("unknown merge version must be rejected, got {other:?}"),
        }
    }

    #[test]
    fn merge_refuses_limits_and_offsets_at_every_merge_version() {
        use crate::SizedQuery;

        // Both refusals predate the version gate and must survive it —
        // a merged limit/offset would silently mean something different
        // than either input asked for.
        for version in [&GROVE_VERSIONS[2], GroveVersion::latest()] {
            let plain = directional_query(b"a", true);

            let mut query = Query::new();
            query.insert_item(QueryItem::RangeFull(..));
            let with_offset = PathQuery::new(
                vec![b"b".to_vec()],
                SizedQuery::new(query.clone(), None, Some(1)),
            );
            match PathQuery::merge(vec![&plain, &with_offset], version) {
                Err(Error::NotSupported(message)) => {
                    assert!(message.contains("offset"), "got: {message}")
                }
                other => panic!("merging an offset must be refused, got {other:?}"),
            }

            let with_limit =
                PathQuery::new(vec![b"b".to_vec()], SizedQuery::new(query, Some(2), None));
            match PathQuery::merge(vec![&plain, &with_limit], version) {
                Err(Error::NotSupported(message)) => {
                    assert!(message.contains("limit"), "got: {message}")
                }
                other => panic!("merging a limit must be refused, got {other:?}"),
            }
        }
    }

    #[test]
    fn merge_surfaces_read_mode_conflicts_at_both_merge_versions() {
        use grovedb_merk::proofs::query::{AxisQuery, IndexAxis, ReadMode};

        // `PathQuery::merge` refuses read modes up front, before the
        // version-gated Query-level merge is reached — merging would
        // drop the mode and mangle an axis or sum-budget read into key
        // selection. The refusal must not depend on the merge version:
        // the verifier re-runs the same merge at the same version, so a
        // version where this slipped through would fork the two sides.
        // (`Query::merge_multiple` refuses read modes too; that is
        // defense in depth for direct rs-drive-facing callers, exercised
        // by `query_level_merges_reject_read_modes`.)
        for version in [&GROVE_VERSIONS[2], GroveVersion::latest()] {
            let mut axis = Query::new();
            axis.read_mode = Some(Box::new(ReadMode::Axis(AxisQuery::top_k(
                IndexAxis::Sum,
                1,
                0,
                true,
            ))));
            let axis_pq = PathQuery::new_unsized(vec![b"a".to_vec()], axis);
            let plain = directional_query(b"a", true);

            match PathQuery::merge(vec![&axis_pq, &plain], version) {
                Err(Error::NotSupported(_)) => {}
                other => panic!(
                    "merging a read-mode query at merge version {} must be refused, got {:?}",
                    version.grovedb_versions.path_query_methods.merge, other
                ),
            }
        }
    }

    #[test]
    fn inverted_ranges_are_still_held_to_the_version_contract() {
        // Review finding: the count/sum/avg range wrappers answered
        // `Ok([])` for `lo > hi` BEFORE delegating to the gated generic,
        // so a degenerate range slipped the version check entirely —
        // part of each entry point sat outside the contract every other
        // input to it is held to.
        let real = GroveVersion::latest();
        let (db, _) = psit_db(real);
        let bad = doctored(|v| v.grovedb_versions.operations.indexed_axis.read = 9);
        let path = [crate::tests::TEST_LEAF, b"psit"];

        macro_rules! assert_version_rejected {
            ($label:expr, $call:expr) => {
                match $call.unwrap() {
                    Err(Error::VersionError(_)) => {}
                    other => panic!(
                        "{} must reject an unknown read version even for an inverted range, \
                         got {:?}",
                        $label, other
                    ),
                }
            };
        }

        assert_version_rejected!(
            "indexed_sum_range",
            db.indexed_sum_range(path.as_ref(), 100, 0, true, 10, None, &bad)
        );
        assert_version_rejected!(
            "indexed_count_range",
            db.indexed_count_range(path.as_ref(), 100, 0, true, 10, None, &bad)
        );
        assert_version_rejected!(
            "indexed_avg_range",
            db.indexed_avg_range(path.as_ref(), 100, 0, true, 10, None, &bad)
        );
        // The aggregate readers already gated before their fast path;
        // pin that so a future refactor cannot reintroduce the same gap.
        assert_version_rejected!(
            "indexed_sum_aggregate_over_value_range",
            db.indexed_sum_aggregate_over_value_range(path.as_ref(), 100, 0, None, &bad)
        );
        assert_version_rejected!(
            "indexed_count_aggregate_over_value_range",
            db.indexed_count_aggregate_over_value_range(path.as_ref(), 100, 0, None, &bad)
        );

        // At the real version an inverted range still answers empty
        // rather than erroring — the gate moved, the semantics did not.
        let empty = db
            .indexed_sum_range(path.as_ref(), 100, 0, true, 10, None, real)
            .unwrap()
            .expect("inverted range is still a valid, empty answer");
        assert!(empty.is_empty());
    }
}
