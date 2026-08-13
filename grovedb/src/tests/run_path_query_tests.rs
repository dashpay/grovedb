//! Differential tests for [`GroveDb::run_path_query`], the unified read
//! dispatch: for every shape, the unified answer must equal the answer
//! of the dedicated entry point it routes to, over the same state.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::{
        query::{query_item::QueryItem, AggregateSumQuery, AxisQuery, IndexAxis},
        Query,
    };
    use grovedb_version::version::{GroveVersion, GROVE_VERSIONS};

    use crate::{
        operations::{
            get::{AxisAggregateValue, PathQueryRun},
            proof::indexed_axis::AxisEntries,
        },
        query_result_type::QueryResultType,
        tests::{make_test_grovedb, make_test_sum_tree_grovedb, TEST_LEAF},
        AggregateSumPathQuery, Element, Error, GroveDb, PathQuery,
    };

    // -----------------------------------------------------------------
    // Fixtures
    // -----------------------------------------------------------------

    /// Build a PSIT at `[TEST_LEAF, b"psit"]` with `(key, sum)` entries.
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

    /// Build PSITs under two sibling branch keys:
    /// `[TEST_LEAF, branch, b"scores"]` for each `(branch, entries)`.
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

    const PSIT_ENTRIES: &[(&[u8], i64)] = &[
        (b"alice", 40),
        (b"bob", -10),
        (b"carol", 25),
        (b"dave", 40),
        (b"erin", 5),
    ];

    // -----------------------------------------------------------------
    // Key selection
    // -----------------------------------------------------------------

    #[test]
    fn key_selection_matches_query_raw() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        for (key, value) in [(b"a", b"1"), (b"b", b"2"), (b"c", b"3")] {
            db.insert(
                [TEST_LEAF].as_ref(),
                key,
                Element::new_item(value.to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert item");
        }
        let path_query = PathQuery::new_unsized(
            vec![TEST_LEAF.to_vec()],
            Query::new_single_query_item(QueryItem::RangeFull(..)),
        );

        let (direct, direct_skipped) = db
            .query_raw(
                &path_query,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("query_raw");
        let run = db
            .run_path_query(
                &path_query,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("run_path_query");
        match run {
            PathQueryRun::Elements { elements, skipped } => {
                assert_eq!(elements.len(), direct.len());
                assert_eq!(
                    elements.to_key_elements(),
                    direct.to_key_elements(),
                    "unified read must equal query_raw"
                );
                assert_eq!(skipped, direct_skipped);
            }
            other => panic!("expected Elements, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Axis reads
    // -----------------------------------------------------------------

    #[test]
    fn axis_top_k_matches_direct_primitive() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);

        for (k, offset, descending) in [
            (3u16, 0u64, true),
            (2, 1, true),
            (5, 0, false),
            (2, 3, false),
        ] {
            let direct = db
                .indexed_sum_top_k_paginated(
                    [TEST_LEAF, b"psit"].as_ref(),
                    k,
                    offset,
                    descending,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("direct top-k");
            let run = db
                .run_path_query(
                    &PathQuery::new_axis_top_k(psit_path(), IndexAxis::Sum, k, offset, descending),
                    true,
                    true,
                    true,
                    QueryResultType::QueryKeyElementPairResultType,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("unified top-k");
            assert_eq!(
                run_entries(run),
                AxisEntries::Sum(direct),
                "top-k k={k} offset={offset} descending={descending}"
            );
        }
    }

    #[test]
    fn axis_bounded_matches_direct_primitive_with_clamping() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);

        // Bounds deliberately exceed the i64 domain on both sides: the
        // unified read must clamp to the axis domain, matching a direct
        // call at the domain edges.
        let direct = db
            .indexed_sum_range(
                [TEST_LEAF, b"psit"].as_ref(),
                0,
                i64::MAX,
                true,
                10,
                None,
                grove_version,
            )
            .unwrap()
            .expect("direct bounded");
        let run = db
            .run_path_query(
                &PathQuery::new_axis_bounded(psit_path(), IndexAxis::Sum, 0, i128::MAX, 10, true),
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("unified bounded");
        assert_eq!(run_entries(run), AxisEntries::Sum(direct));
    }

    #[test]
    fn axis_rank_matches_proved_rank() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);

        for descending in [true, false] {
            let (_, proved_rank) = db
                .prove_indexed_axis_rank_of_key(
                    [TEST_LEAF, b"psit"].as_ref(),
                    IndexAxis::Sum,
                    b"carol",
                    descending,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("proved rank");
            let run = db
                .run_path_query(
                    &PathQuery::new_axis_rank_of_key(
                        psit_path(),
                        IndexAxis::Sum,
                        b"carol".to_vec(),
                        descending,
                    ),
                    true,
                    true,
                    true,
                    QueryResultType::QueryKeyElementPairResultType,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("unified rank");
            match run {
                PathQueryRun::AxisRank(rank) => {
                    assert_eq!(rank, proved_rank, "descending={descending}")
                }
                other => panic!("expected AxisRank, got {other:?}"),
            }
        }
    }

    #[test]
    fn axis_range_aggregate_matches_direct_primitive() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);

        let direct = db
            .indexed_sum_range_aggregate([TEST_LEAF, b"psit"].as_ref(), 0, 40, None, grove_version)
            .unwrap()
            .expect("direct range aggregate");
        let run = db
            .run_path_query(
                &PathQuery::new_axis_range_aggregate(psit_path(), IndexAxis::Sum, 0, 40),
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("unified range aggregate");
        match run {
            PathQueryRun::AxisAggregate(AxisAggregateValue::Sum(sum)) => assert_eq!(sum, direct),
            other => panic!("expected AxisAggregate(Sum), got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Branched axis reads
    // -----------------------------------------------------------------

    #[test]
    fn branched_axis_read_mirrors_per_branch_reads_and_absence() {
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

        let axis_query = AxisQuery::top_k(IndexAxis::Sum, 2, 0, true);
        let path_query = PathQuery::new_branched_axis(
            vec![TEST_LEAF.to_vec()],
            vec![b"alice".to_vec(), b"bob".to_vec(), b"carol".to_vec()],
            vec![b"scores".to_vec()],
            axis_query,
        );
        let run = db
            .run_path_query(
                &path_query,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("unified branched read");
        let PathQueryRun::BranchedAxisEntries(branches) = run else {
            panic!("expected BranchedAxisEntries");
        };
        assert_eq!(branches.len(), 3);

        // Present branches equal the single-path primitive.
        for (branch_key, entries) in &branches {
            match branch_key.as_slice() {
                b"bob" => assert!(entries.is_none(), "absent branch must be None"),
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
                        .expect("direct branch read");
                    assert_eq!(
                        entries.as_ref().expect("present branch"),
                        &AxisEntries::Sum(direct),
                        "branch {}",
                        String::from_utf8_lossy(present)
                    );
                }
            }
        }
    }

    // -----------------------------------------------------------------
    // Sum budget
    // -----------------------------------------------------------------

    #[test]
    fn sum_budget_matches_query_aggregate_sums() {
        let grove_version = GroveVersion::latest();
        let db = make_test_sum_tree_grovedb(grove_version);
        for (key, sum) in [(b"a", 7i64), (b"b", 5), (b"c", 3), (b"d", 11)] {
            db.insert(
                [TEST_LEAF].as_ref(),
                key,
                Element::new_sum_item(sum),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum item");
        }

        for (sum_limit, max_items) in [(10u64, None), (100, Some(2u16)), (1, None)] {
            let direct = db
                .query_aggregate_sums(
                    &AggregateSumPathQuery {
                        path: vec![TEST_LEAF.to_vec()],
                        aggregate_sum_query: AggregateSumQuery {
                            items: vec![QueryItem::RangeFull(..)],
                            left_to_right: true,
                            sum_limit,
                            limit_of_items_to_check: max_items,
                        },
                    },
                    true,
                    true,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("direct sum budget");
            let run = db
                .run_path_query(
                    &PathQuery::new_sum_budget(
                        vec![TEST_LEAF.to_vec()],
                        vec![QueryItem::RangeFull(..)],
                        true,
                        sum_limit,
                        max_items,
                    ),
                    true,
                    true,
                    true,
                    QueryResultType::QueryKeyElementPairResultType,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("unified sum budget");
            match run {
                PathQueryRun::SumBudget(result) => {
                    assert_eq!(
                        result, direct,
                        "sum_limit={sum_limit} max_items={max_items:?}"
                    );
                }
                other => panic!("expected SumBudget, got {other:?}"),
            }
        }
    }

    // -----------------------------------------------------------------
    // Aggregates route through the existing readers
    // -----------------------------------------------------------------

    #[test]
    fn aggregate_leaf_count_matches_query_aggregate_count() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pct",
            Element::empty_provable_count_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create provable count tree");
        for key in [b"a", b"b", b"c"] {
            db.insert(
                [TEST_LEAF, b"pct"].as_ref(),
                key,
                Element::new_item(b"v".to_vec()),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert counted item");
        }
        let path_query = PathQuery::new_aggregate_count_on_range(
            vec![TEST_LEAF.to_vec(), b"pct".to_vec()],
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        );

        let direct = db
            .query_aggregate_count(&path_query, None, grove_version)
            .unwrap()
            .expect("direct aggregate count");
        let run = db
            .run_path_query(
                &path_query,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("unified aggregate count");
        match run {
            PathQueryRun::AggregateCount(count) => assert_eq!(count, direct),
            other => panic!("expected AggregateCount, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Version gating
    // -----------------------------------------------------------------

    #[test]
    fn read_mode_shapes_are_gated_to_grove_v4() {
        let v3 = &GROVE_VERSIONS[2];
        assert_eq!(v3.protocol_version, 3, "GROVE_VERSIONS[2] must be V3");
        let v4 = GroveVersion::latest();
        assert_eq!(v4.protocol_version, 4, "latest must be V4");

        let db = make_test_grovedb(v4);
        build_psit(&db, v4, PSIT_ENTRIES);
        let path_query = PathQuery::new_axis_top_k(psit_path(), IndexAxis::Sum, 2, 0, true);

        // V3: the shape classifies but serving is refused.
        match db
            .run_path_query(
                &path_query,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                v3,
            )
            .unwrap()
        {
            Err(Error::NotSupported(_)) => {}
            other => panic!("V3 must reject read-mode shapes, got {other:?}"),
        }

        // V4: served.
        db.run_path_query(
            &path_query,
            true,
            true,
            true,
            QueryResultType::QueryKeyElementPairResultType,
            None,
            v4,
        )
        .unwrap()
        .expect("V4 must serve read-mode shapes");
    }

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    fn run_entries(run: PathQueryRun) -> AxisEntries {
        match run {
            PathQueryRun::AxisEntries(entries) => entries,
            other => panic!("expected AxisEntries, got {other:?}"),
        }
    }
}
