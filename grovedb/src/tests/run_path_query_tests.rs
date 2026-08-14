//! Differential tests for [`GroveDb::run_path_query`], the unified read
//! dispatch: for every shape, the unified answer must equal the answer
//! of the dedicated entry point it routes to, over the same state.

#[cfg(test)]
mod tests {
    use grovedb_merk::proofs::{
        query::{query_item::QueryItem, AggregateFold, AggregateSumQuery, AxisQuery, IndexAxis},
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
        AggregateSumPathQuery, Element, Error, GroveDb, PathQuery, SizedQuery,
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
    fn axis_aggregate_over_value_range_matches_direct_primitive() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);

        let direct = db
            .indexed_sum_aggregate_over_value_range(
                [TEST_LEAF, b"psit"].as_ref(),
                0,
                40,
                None,
                grove_version,
            )
            .unwrap()
            .expect("direct range aggregate");
        let run = db
            .run_path_query(
                &PathQuery::new_axis_aggregate_over_value_range(
                    psit_path(),
                    IndexAxis::Sum,
                    0,
                    40,
                    AggregateFold::Total,
                ),
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
            PathQueryRun::AxisAggregate(AxisAggregateValue::Total(sum)) => assert_eq!(sum, direct),
            other => panic!("expected AxisAggregate(Sum), got {other:?}"),
        }
    }

    #[test]
    fn axis_population_over_value_range_matches_direct_primitive() {
        // The Population fold over the same sum band routes to the
        // count aggregate of the (PCPS) sum secondary — a different
        // dispatch arm and a different trusted reader than Total.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_psit(&db, grove_version, PSIT_ENTRIES);

        let direct = db
            .indexed_sum_population_over_value_range(
                [TEST_LEAF, b"psit"].as_ref(),
                0,
                40,
                None,
                grove_version,
            )
            .unwrap()
            .expect("direct population over range");
        let run = db
            .run_path_query(
                &PathQuery::new_axis_aggregate_over_value_range(
                    psit_path(),
                    IndexAxis::Sum,
                    0,
                    40,
                    AggregateFold::Population,
                ),
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("unified population over range");
        match run {
            PathQueryRun::AxisAggregate(AxisAggregateValue::Population(population)) => {
                // [0, 40] over sums [40, -10, 25, 40, 5] selects four.
                assert_eq!(population, direct);
                assert_eq!(population, 4);
            }
            other => panic!("expected AxisAggregate(Population), got {other:?}"),
        }

        // The reader's own edge shapes, direct: inverted bounds answer
        // an empty population, and hi = i64::MAX takes the unbounded
        // upper branch.
        let inverted = db
            .indexed_sum_population_over_value_range(
                [TEST_LEAF, b"psit"].as_ref(),
                40,
                0,
                None,
                grove_version,
            )
            .unwrap()
            .expect("inverted bounds are a valid empty answer");
        assert_eq!(inverted, 0);
        let unbounded = db
            .indexed_sum_population_over_value_range(
                [TEST_LEAF, b"psit"].as_ref(),
                i64::MIN,
                i64::MAX,
                None,
                grove_version,
            )
            .unwrap()
            .expect("full-domain population");
        assert_eq!(unbounded, 5, "every PSIT entry is in the full domain");
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
        // The slots are documented as "per branch key, in query order",
        // so pin the order itself — matching each key by value would let
        // a reordering regression through.
        assert_eq!(
            branches
                .iter()
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>(),
            vec![b"alice".to_vec(), b"bob".to_vec(), b"carol".to_vec()],
            "branch slots must follow query order"
        );

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
    // The other two axes
    //
    // The dispatch fans out per axis inside `axis_top_k_paginated_entries`
    // / `axis_bounded_entries` / the aggregate-over-value-range arm, so exercising
    // only the sum axis leaves two thirds of each fan-out — and the whole
    // count-bounds clamp — unexecuted.
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

    const PCIT_ENTRIES: &[(&[u8], u64)] = &[(b"alpha", 3), (b"beta", 1), (b"gamma", 5)];

    #[test]
    fn count_axis_reads_match_the_direct_primitives() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        build_pcit(&db, grove_version, PCIT_ENTRIES);
        let path = [TEST_LEAF, b"pcit"];

        // Paginated page on the count axis.
        let direct = db
            .indexed_count_top_k_paginated(path.as_ref(), 2, 1, true, None, grove_version)
            .unwrap()
            .expect("direct count top-k");
        let run = db
            .run_path_query(
                &PathQuery::new_axis_top_k(pcit_path(), IndexAxis::Count, 2, 1, true),
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("unified count top-k");
        assert_eq!(run_entries(run), AxisEntries::Count(direct));

        // Bounded on the count axis, with bounds deliberately below and
        // above the u64 domain so the count clamp is exercised (the sum
        // clamp is covered by the sum-axis test).
        let direct = db
            .indexed_count_range(path.as_ref(), 0, u64::MAX, false, 10, None, grove_version)
            .unwrap()
            .expect("direct count range");
        let run = db
            .run_path_query(
                &PathQuery::new_axis_bounded(
                    pcit_path(),
                    IndexAxis::Count,
                    i128::MIN,
                    i128::MAX,
                    10,
                    false,
                ),
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("unified count bounded");
        assert_eq!(run_entries(run), AxisEntries::Count(direct));

        // Aggregate over the value range on the count axis.
        let direct = db
            .indexed_count_aggregate_over_value_range(path.as_ref(), 0, 10, None, grove_version)
            .unwrap()
            .expect("direct count range aggregate");
        let run = db
            .run_path_query(
                &PathQuery::new_axis_aggregate_over_value_range(
                    pcit_path(),
                    IndexAxis::Count,
                    0,
                    10,
                    AggregateFold::Population,
                ),
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("unified count range aggregate");
        match run {
            PathQueryRun::AxisAggregate(AxisAggregateValue::Population(value)) => {
                assert_eq!(value, direct)
            }
            other => panic!("expected AxisAggregate(Count), got {other:?}"),
        }
    }

    #[test]
    fn avg_axis_reads_match_the_direct_primitives() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // A PCPSIT carrying all three axes, so the avg secondary exists.
        let axes: Vec<(u8, Option<Vec<u8>>)> = vec![(0, None), (1, None), (2, None)];
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcpsit",
            Element::empty_provable_count_provable_sum_indexed_tree(axes).expect("axes canonical"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPSIT");
        for (key, sum) in [(b"a", 10i64), (b"b", 40), (b"c", -5)] {
            db.insert_into_provable_count_provable_sum_indexed_tree(
                [TEST_LEAF, b"pcpsit"].as_ref(),
                key,
                Element::new_item_with_sum_item(b"v".to_vec(), sum),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCPSIT entry");
        }
        let path = [TEST_LEAF, b"pcpsit"];
        let pcpsit_path = vec![TEST_LEAF.to_vec(), b"pcpsit".to_vec()];

        let direct = db
            .indexed_avg_top_k_paginated(path.as_ref(), 2, 0, true, None, grove_version)
            .unwrap()
            .expect("direct avg top-k");
        let run = db
            .run_path_query(
                &PathQuery::new_axis_top_k(pcpsit_path.clone(), IndexAxis::Avg, 2, 0, true),
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("unified avg top-k");
        assert_eq!(run_entries(run), AxisEntries::Avg(direct));

        // Bounded on the avg axis takes the i128 bounds unclamped — the
        // avg domain is the whole i128 range.
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
        let run = db
            .run_path_query(
                &PathQuery::new_axis_bounded(
                    pcpsit_path,
                    IndexAxis::Avg,
                    i128::MIN,
                    i128::MAX,
                    10,
                    false,
                ),
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("unified avg bounded");
        assert_eq!(run_entries(run), AxisEntries::Avg(direct));
    }

    // -----------------------------------------------------------------
    // The remaining aggregate arms
    // -----------------------------------------------------------------

    #[test]
    fn aggregate_leaf_sum_and_count_and_sum_match_their_readers() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        // Sum leaf against a ProvableSumTree.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pst",
            Element::empty_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create provable sum tree");
        for (key, sum) in [(b"a", 5i64), (b"b", -2), (b"c", 11)] {
            db.insert(
                [TEST_LEAF, b"pst"].as_ref(),
                key,
                Element::new_sum_item(sum),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert sum item");
        }
        let sum_pq = PathQuery::new_aggregate_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"pst".to_vec()],
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        );
        let direct = db
            .query_aggregate_sum(&sum_pq, None, grove_version)
            .unwrap()
            .expect("direct aggregate sum");
        match db
            .run_path_query(
                &sum_pq,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("unified aggregate sum")
        {
            PathQueryRun::AggregateSum(sum) => assert_eq!(sum, direct),
            other => panic!("expected AggregateSum, got {other:?}"),
        }

        // Combined leaf against a ProvableCountProvableSumTree.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pcps",
            Element::empty_provable_count_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create PCPS tree");
        for (key, sum) in [(b"a", 7i64), (b"b", 3)] {
            db.insert(
                [TEST_LEAF, b"pcps"].as_ref(),
                key,
                Element::new_sum_item(sum),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert PCPS sum item");
        }
        let combined_pq = PathQuery::new_aggregate_count_and_sum_on_range(
            vec![TEST_LEAF.to_vec(), b"pcps".to_vec()],
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        );
        let (direct_count, direct_sum) = db
            .query_aggregate_count_and_sum(&combined_pq, None, grove_version)
            .unwrap()
            .expect("direct combined aggregate");
        match db
            .run_path_query(
                &combined_pq,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("unified combined aggregate")
        {
            PathQueryRun::AggregateCountAndSum { count, sum } => {
                assert_eq!((count, sum), (direct_count, direct_sum))
            }
            other => panic!("expected AggregateCountAndSum, got {other:?}"),
        }
    }

    #[test]
    fn aggregate_carrier_all_kinds_match_their_per_key_readers() {
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        // Two outer keys, each holding a provable count tree.
        for outer in [b"one", b"two"] {
            db.insert(
                [TEST_LEAF].as_ref(),
                outer,
                Element::empty_provable_count_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create carrier outer");
            for key in [b"a", b"b"] {
                db.insert(
                    [TEST_LEAF, outer].as_ref(),
                    key,
                    Element::new_item(b"v".to_vec()),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert counted item");
            }
        }

        // Carrier: outer keys at TEST_LEAF, leaf aggregate underneath.
        let mut carrier = Query::new();
        carrier.insert_key(b"one".to_vec());
        carrier.insert_key(b"two".to_vec());
        carrier.set_subquery(Query::new_aggregate_count_on_range(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        let carrier_pq = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], carrier);

        let direct = db
            .query_aggregate_count_per_key(&carrier_pq, None, grove_version)
            .unwrap()
            .expect("direct per-key counts");
        match db
            .run_path_query(
                &carrier_pq,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("unified per-key counts")
        {
            PathQueryRun::AggregateCountPerKey(per_key) => assert_eq!(per_key, direct),
            other => panic!("expected AggregateCountPerKey, got {other:?}"),
        }

        // Sum carrier: outer keys holding ProvableSumTrees. Before the
        // per-key sum reader existed this arm returned NotSupported;
        // now it must route and agree with the dedicated reader.
        for outer in [b"sone", b"stwo"] {
            db.insert(
                [TEST_LEAF].as_ref(),
                outer,
                Element::empty_provable_sum_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create sum carrier outer");
            for (key, sum) in [(b"a", 11i64), (b"b", -4)] {
                db.insert(
                    [TEST_LEAF, outer].as_ref(),
                    key,
                    Element::new_sum_item(sum),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert carrier sum item");
            }
        }
        let mut sum_carrier = Query::new();
        sum_carrier.insert_key(b"sone".to_vec());
        sum_carrier.insert_key(b"stwo".to_vec());
        sum_carrier.set_subquery(Query::new_aggregate_sum_on_range(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        let sum_carrier_pq = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], sum_carrier);

        let direct_sums = db
            .query_aggregate_sum_per_key(&sum_carrier_pq, None, grove_version)
            .unwrap()
            .expect("direct per-key sums");
        match db
            .run_path_query(
                &sum_carrier_pq,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("unified per-key sums")
        {
            PathQueryRun::AggregateSumPerKey(per_key) => {
                assert_eq!(per_key, direct_sums);
                // Sanity-check the value itself, not just the agreement:
                // 11 + (-4) = 7 per outer key.
                assert_eq!(
                    per_key,
                    vec![(b"sone".to_vec(), 7i64), (b"stwo".to_vec(), 7)]
                );
            }
            other => panic!("expected AggregateSumPerKey, got {other:?}"),
        }

        // Combined carrier: outer keys holding PCPS trees — the only
        // tree type that can terminate a dual-axis walk.
        for outer in [b"cone", b"ctwo"] {
            db.insert(
                [TEST_LEAF].as_ref(),
                outer,
                Element::empty_provable_count_provable_sum_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("create combined carrier outer");
            for (key, sum) in [(b"a", 11i64), (b"b", -4)] {
                db.insert(
                    [TEST_LEAF, outer].as_ref(),
                    key,
                    Element::new_sum_item(sum),
                    None,
                    None,
                    grove_version,
                )
                .unwrap()
                .expect("insert carrier PCPS sum item");
            }
        }
        let mut combined_carrier = Query::new();
        combined_carrier.insert_key(b"cone".to_vec());
        combined_carrier.insert_key(b"ctwo".to_vec());
        combined_carrier.set_subquery(Query::new_aggregate_count_and_sum_on_range(
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        ));
        let combined_carrier_pq =
            PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], combined_carrier);

        let direct_combined = db
            .query_aggregate_count_and_sum_per_key(&combined_carrier_pq, None, grove_version)
            .unwrap()
            .expect("direct per-key combined");
        match db
            .run_path_query(
                &combined_carrier_pq,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap()
            .expect("unified per-key combined")
        {
            PathQueryRun::AggregateCountAndSumPerKey(per_key) => {
                assert_eq!(per_key, direct_combined);
                assert_eq!(
                    per_key,
                    vec![
                        (b"cone".to_vec(), 2u64, 7i64),
                        (b"ctwo".to_vec(), 2u64, 7i64)
                    ]
                );
            }
            other => panic!("expected AggregateCountAndSumPerKey, got {other:?}"),
        }
    }

    #[test]
    fn aggregate_carrier_per_key_dispatch_surfaces_reader_errors() {
        // The dispatch must not paper over the readers' rejections: a
        // carrier whose outer match is a non-tree element, and a
        // combined carrier terminating in a single-axis host, must fail
        // through `run_path_query` exactly as they fail through the
        // dedicated readers.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);
        db.insert(
            [TEST_LEAF].as_ref(),
            b"item",
            Element::new_item(b"not a tree".to_vec()),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert non-tree outer");
        // Single-axis host under a combined carrier.
        db.insert(
            [TEST_LEAF].as_ref(),
            b"pst",
            Element::empty_provable_sum_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("create single-axis host");
        db.insert(
            [TEST_LEAF, b"pst"].as_ref(),
            b"a",
            Element::new_sum_item(5),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert sum item");

        let mut non_tree_carrier = Query::new();
        non_tree_carrier.insert_key(b"item".to_vec());
        non_tree_carrier.set_subquery(Query::new_aggregate_sum_on_range(QueryItem::Range(
            b"a".to_vec()..b"z".to_vec(),
        )));
        let non_tree_pq = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], non_tree_carrier);

        let mut single_axis_carrier = Query::new();
        single_axis_carrier.insert_key(b"pst".to_vec());
        single_axis_carrier.set_subquery(Query::new_aggregate_count_and_sum_on_range(
            QueryItem::Range(b"a".to_vec()..b"z".to_vec()),
        ));
        let single_axis_pq = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], single_axis_carrier);

        // Pair each query with the reader the dispatch should route it
        // to, so the comparison is explicit rather than inferred from
        // the query's shape.
        let cases: [(&PathQuery, Box<dyn Fn() -> Error>, &str); 2] = [
            (
                &non_tree_pq,
                Box::new(|| {
                    db.query_aggregate_sum_per_key(&non_tree_pq, None, grove_version)
                        .unwrap()
                        .map(|_| ())
                        .expect_err("non-tree outer must be rejected")
                }),
                "non-tree outer match",
            ),
            (
                &single_axis_pq,
                Box::new(|| {
                    db.query_aggregate_count_and_sum_per_key(&single_axis_pq, None, grove_version)
                        .unwrap()
                        .map(|_| ())
                        .expect_err("single-axis host must be rejected")
                }),
                "single-axis host under combined carrier",
            ),
        ];

        for (pq, direct_reader, label) in cases {
            let direct = direct_reader();
            let unified = db
                .run_path_query(
                    pq,
                    true,
                    true,
                    true,
                    QueryResultType::QueryKeyElementPairResultType,
                    None,
                    grove_version,
                )
                .unwrap()
                .map(|_| ())
                .expect_err(label);
            assert_eq!(
                unified.to_string(),
                direct.to_string(),
                "dispatch must surface the reader's error verbatim for {label}"
            );
        }
    }

    // -----------------------------------------------------------------
    // Count-offset pagination and the version gate
    // -----------------------------------------------------------------

    #[test]
    fn count_offset_paginated_matches_query_raw() {
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
        for key in [b"a", b"b", b"c", b"d"] {
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
        // A non-zero offset over a single range item classifies as the
        // count-offset paginated shape, a distinct dispatch arm from
        // plain key selection.
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec(), b"pct".to_vec()],
            SizedQuery::new(
                Query::new_single_query_item(QueryItem::RangeFull(..)),
                Some(2),
                Some(1),
            ),
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
        match db
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
            .expect("unified paginated read")
        {
            PathQueryRun::Elements { elements, skipped } => {
                assert_eq!(elements.to_key_elements(), direct.to_key_elements());
                assert_eq!(skipped, direct_skipped);
            }
            other => panic!("expected Elements, got {other:?}"),
        }
    }

    #[test]
    fn unknown_unified_read_mode_version_is_rejected() {
        // The slot is versioned, so an unrecognized value must surface
        // as a VersionError rather than being treated as "off" (0) or
        // "on" (1).
        let mut doctored = GroveVersion::latest().clone();
        doctored
            .grovedb_versions
            .path_query_methods
            .unified_read_mode = 9;
        let db = make_test_grovedb(&doctored);
        let path_query = PathQuery::new_axis_top_k(psit_path(), IndexAxis::Sum, 1, 0, true);
        match db
            .run_path_query(
                &path_query,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                &doctored,
            )
            .unwrap()
        {
            Err(Error::VersionError(_)) => {}
            other => panic!("unknown slot value must be rejected, got {other:?}"),
        }
    }

    #[test]
    fn unknown_run_path_query_version_is_rejected() {
        // The method's own slot, distinct from the read-mode gate above.
        let mut doctored = GroveVersion::latest().clone();
        doctored.grovedb_versions.operations.query.run_path_query = 9;
        let db = make_test_grovedb(&doctored);
        match db
            .run_path_query(
                &PathQuery::new_single_key(vec![TEST_LEAF.to_vec()], b"k".to_vec()),
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                &doctored,
            )
            .unwrap()
        {
            Err(Error::VersionError(_)) => {}
            other => panic!("unknown run_path_query version must be rejected, got {other:?}"),
        }
    }

    #[test]
    fn classification_errors_surface_from_the_dispatch() {
        // A malformed shape must fail at classification and propagate
        // out of `run_path_query` unchanged, rather than being routed
        // anywhere. An axis read carrying query items is the simplest
        // violation of the read-mode grammar.
        let grove_version = GroveVersion::latest();
        let db = make_test_grovedb(grove_version);

        let mut malformed = Query::new();
        malformed.read_mode = Some(Box::new(grovedb_merk::proofs::query::ReadMode::Axis(
            AxisQuery::top_k(IndexAxis::Sum, 1, 0, true),
        )));
        malformed.insert_key(b"unexpected".to_vec());
        let path_query = PathQuery::new_unsized(psit_path(), malformed);

        let from_classify = path_query
            .classify()
            .expect_err("an axis read with items is malformed");
        match db
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
        {
            Err(e) => assert_eq!(
                format!("{e}"),
                format!("{from_classify}"),
                "the dispatch must surface classify's error verbatim"
            ),
            Ok(run) => panic!("malformed query must be rejected, got {run:?}"),
        }
    }

    #[test]
    fn branched_non_entry_listing_traversal_is_rejected_at_classification() {
        // A branched read whose terminal is rank-of-key or aggregate-over-value-range
        // has no per-branch entry list to return. It must be refused as a
        // malformed query, not reach the dispatch and surface as an
        // internal CorruptedCodeExecution.
        for axis_query in [
            AxisQuery::rank_of_key(IndexAxis::Sum, b"alice".to_vec(), true),
            AxisQuery::aggregate_over_value_range(IndexAxis::Sum, 0, 10, AggregateFold::Total),
        ] {
            let pq = PathQuery::new_branched_axis(
                vec![TEST_LEAF.to_vec()],
                vec![b"alice".to_vec()],
                vec![b"scores".to_vec()],
                axis_query,
            );
            match pq.classify() {
                Err(Error::InvalidQuery(m)) => {
                    assert!(m.contains("entry-listing"), "got: {m}")
                }
                other => panic!("must be rejected at classification, got {other:?}"),
            }
        }
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
