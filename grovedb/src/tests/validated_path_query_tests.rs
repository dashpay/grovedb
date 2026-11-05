//! Cross-consumer contracts and compatibility fixtures for query dispatch.

use grovedb_costs::{CostResult, OperationCost};
use grovedb_merk::proofs::{
    query::{AxisQuery, IndexAxis, QueryItem},
    Query,
};
use grovedb_version::{
    error::GroveVersionError,
    version::{GroveVersion, GROVE_VERSIONS},
};

use crate::{
    operations::{get::PathQueryRun, proof::VerifiedPathQuery},
    query::{AggregateKind, PathQueryShape},
    query_result_type::{PathKeyOptionalElementTrio, QueryResultType},
    tests::{make_test_grovedb, TEST_LEAF},
    Element, Error, GroveDb, PathQuery, SizedQuery,
};

fn run(db: &GroveDb, query: &PathQuery, version: &GroveVersion) -> CostResult<PathQueryRun, Error> {
    db.run_path_query(
        query,
        true,
        true,
        true,
        QueryResultType::QueryPathKeyElementTrioResultType,
        None,
        version,
    )
}

fn elements(run: PathQueryRun) -> Vec<PathKeyOptionalElementTrio> {
    let PathQueryRun::Elements { elements, .. } = run else {
        panic!("expected elements, got {run:?}");
    };
    elements
        .to_path_key_elements()
        .into_iter()
        .map(|(path, key, element)| (path, key, Some(element)))
        .collect()
}

fn assert_agree(read: PathQueryRun, verified: VerifiedPathQuery) {
    match (read, verified) {
        (
            read @ PathQueryRun::Elements { .. },
            VerifiedPathQuery::Elements {
                elements: proved, ..
            },
        ) => assert_eq!(elements(read), proved),
        (PathQueryRun::AggregateCount(read), VerifiedPathQuery::AggregateCount { count, .. }) => {
            assert_eq!(read, count)
        }
        (PathQueryRun::AggregateSum(read), VerifiedPathQuery::AggregateSum { sum, .. }) => {
            assert_eq!(read, sum)
        }
        (
            PathQueryRun::AggregateCountAndSum { count: rc, sum: rs },
            VerifiedPathQuery::AggregateCountAndSum { count, sum, .. },
        ) => assert_eq!((rc, rs), (count, sum)),
        (
            PathQueryRun::AggregateCountPerKey(read),
            VerifiedPathQuery::AggregatePerKey { per_key, .. },
        ) => assert_eq!(
            read.into_iter()
                .map(|(k, c)| (k, Some(c), None))
                .collect::<Vec<_>>(),
            per_key
        ),
        (
            PathQueryRun::AggregateSumPerKey(read),
            VerifiedPathQuery::AggregatePerKey { per_key, .. },
        ) => assert_eq!(
            read.into_iter()
                .map(|(k, s)| (k, None, Some(s)))
                .collect::<Vec<_>>(),
            per_key
        ),
        (
            PathQueryRun::AggregateCountAndSumPerKey(read),
            VerifiedPathQuery::AggregatePerKey { per_key, .. },
        ) => assert_eq!(
            read.into_iter()
                .map(|(k, c, s)| (k, Some(c), Some(s)))
                .collect::<Vec<_>>(),
            per_key
        ),
        (read, verified) => panic!("result contracts differ: {read:?}, {verified:?}"),
    }
}

fn aggregate(kind: AggregateKind) -> Query {
    let range = QueryItem::Range(b"a".to_vec()..b"z".to_vec());
    match kind {
        AggregateKind::Count => Query::new_aggregate_count_on_range(range),
        AggregateKind::Sum => Query::new_aggregate_sum_on_range(range),
        AggregateKind::CountAndSum => Query::new_aggregate_count_and_sum_on_range(range),
    }
}

fn insert(db: &GroveDb, path: &[&[u8]], key: &[u8], element: Element, version: &GroveVersion) {
    db.insert(path, key, element, None, None, version)
        .unwrap()
        .unwrap();
}

#[test]
fn aggregates_agree_for_leaves_and_descending_limited_carriers() {
    for version in &GROVE_VERSIONS[2..] {
        for kind in [
            AggregateKind::Count,
            AggregateKind::Sum,
            AggregateKind::CountAndSum,
        ] {
            let db = make_test_grovedb(version);
            for branch in [b"b", b"d"] {
                insert(&db, &[TEST_LEAF], branch, Element::empty_tree(), version);
                let tree = match kind {
                    AggregateKind::Count => Element::empty_provable_count_tree(),
                    AggregateKind::Sum => Element::empty_provable_sum_tree(),
                    AggregateKind::CountAndSum => Element::empty_provable_count_provable_sum_tree(),
                };
                insert(&db, &[TEST_LEAF, branch], b"values", tree, version);
                if branch == b"b" {
                    for (key, value) in [(b"a", 8), (b"m", -3), (b"z", 99)] {
                        let element = if kind == AggregateKind::Count {
                            Element::new_item(vec![1])
                        } else {
                            Element::new_sum_item(value)
                        };
                        insert(&db, &[TEST_LEAF, branch, b"values"], key, element, version);
                    }
                }
            }
            for branch in [b"b", b"d"] {
                let leaf = PathQuery::new_unsized(
                    vec![TEST_LEAF.to_vec(), branch.to_vec(), b"values".to_vec()],
                    aggregate(kind),
                );
                let read = run(&db, &leaf, version).unwrap().unwrap();
                let proof = db.prove_query(&leaf, None, version).unwrap().unwrap();
                let verified = GroveDb::verify_path_query(&proof, &leaf, version).unwrap();
                assert_eq!(
                    verified.root_hash(),
                    &db.root_hash(None, version).unwrap().unwrap()
                );
                assert_agree(read, verified);
            }
            for limit in [None, Some(1), Some(2)] {
                let mut outer = Query::new_with_direction(false);
                for key in [b"a", b"b", b"c", b"d", b"e"] {
                    outer.insert_key(key.to_vec());
                }
                outer.set_subquery_path(vec![b"values".to_vec()]);
                outer.set_subquery(aggregate(kind));
                let carrier = PathQuery::new(
                    vec![TEST_LEAF.to_vec()],
                    SizedQuery::new(outer, limit, None),
                );
                let read = run(&db, &carrier, version).unwrap().unwrap();
                let proof = db.prove_query(&carrier, None, version).unwrap().unwrap();
                let verified = GroveDb::verify_path_query(&proof, &carrier, version).unwrap();
                let VerifiedPathQuery::AggregatePerKey { per_key, .. } = &verified else {
                    panic!("carrier")
                };
                assert_eq!(
                    per_key
                        .iter()
                        .map(|(k, ..)| k.as_slice())
                        .collect::<Vec<_>>(),
                    if limit == Some(1) {
                        vec![b"d".as_slice()]
                    } else {
                        vec![b"d".as_slice(), b"b".as_slice()]
                    }
                );
                assert_agree(read, verified);
            }
        }
    }
}

#[test]
fn conflicting_shapes_fail_identically_before_storage_or_proof_decode() {
    let version = GroveVersion::latest();
    let db = make_test_grovedb(version);
    let base = vec![b"missing".to_vec()];
    let mut conflicts = Vec::new();
    for kind in [
        AggregateKind::Count,
        AggregateKind::Sum,
        AggregateKind::CountAndSum,
    ] {
        let query = aggregate(kind);
        conflicts.push(PathQuery::new(
            base.clone(),
            SizedQuery::new(query.clone(), Some(1), None),
        ));
        conflicts.push(PathQuery::new(
            base.clone(),
            SizedQuery::new(query.clone(), None, Some(0)),
        ));
        let mut mixed = query.clone();
        mixed.items.push(QueryItem::Key(b"other".to_vec()));
        conflicts.push(PathQuery::new_unsized(base.clone(), mixed));
        let mut nested = Query::new_single_key(b"outer".to_vec());
        nested.set_subquery(query);
        nested.set_subquery_key(b"suffix".to_vec());
        conflicts.push(PathQuery::new(
            base.clone(),
            SizedQuery::new(nested, None, Some(1)),
        ));
    }
    let mut mixed = aggregate(AggregateKind::Count);
    mixed.items.extend(aggregate(AggregateKind::Sum).items);
    conflicts.push(PathQuery::new_unsized(base.clone(), mixed));
    let mut read_mode =
        PathQuery::new_sum_budget(base.clone(), vec![QueryItem::RangeFull(..)], true, 10, None);
    read_mode
        .query
        .query
        .set_subquery(aggregate(AggregateKind::Sum));
    conflicts.push(read_mode);
    conflicts.push(PathQuery::new(
        base,
        SizedQuery::new(Query::new_single_key(b"k".to_vec()), None, Some(1)),
    ));

    for query in conflicts {
        let expected = format!("{:?}", query.classify().unwrap_err());
        let read = run(&db, &query, version);
        assert_eq!(read.cost, OperationCost::default());
        assert_eq!(format!("{:?}", read.unwrap().unwrap_err()), expected);
        let proof = db.prove_query(&query, None, version);
        assert_eq!(proof.cost, OperationCost::default());
        assert_eq!(format!("{:?}", proof.unwrap().unwrap_err()), expected);
        assert_eq!(
            format!(
                "{:?}",
                GroveDb::verify_path_query(&[], &query, version).unwrap_err()
            ),
            expected
        );
    }
}

#[test]
fn unknown_proof_versions_preserve_validation_precedence() {
    let latest = GroveVersion::latest();
    let db = make_test_grovedb(latest);
    insert(&db, &[TEST_LEAF], b"a", Element::new_item(vec![1]), latest);
    let selection = PathQuery::new_single_key(vec![TEST_LEAF.to_vec()], b"a".to_vec());
    let proof = db.prove_query(&selection, None, latest).unwrap().unwrap();

    for received in [2, u16::MAX] {
        let mut version = latest.clone();
        version
            .grovedb_versions
            .operations
            .proof
            .prove_query_non_serialized = received;
        // This slot governs generation only: reads and verification of an
        // existing proof must keep working with their supported versions.
        assert_agree(
            run(&db, &selection, &version).unwrap().unwrap(),
            GroveDb::verify_path_query(&proof, &selection, &version).unwrap(),
        );

        for offset in [None, Some(0), Some(1)] {
            for item in [QueryItem::Key(b"a".to_vec()), QueryItem::RangeFull(..)] {
                let query = PathQuery::new(
                    vec![b"missing".to_vec()],
                    SizedQuery::new(Query::new_single_query_item(item), Some(1), offset),
                );
                // Even the invalid key+offset shape reaches the historical
                // envelope-version gate before pagination syntax validation.
                let result = db.prove_query(&query, None, &version);
                assert_eq!(result.cost, OperationCost::default());
                match result.unwrap().unwrap_err() {
                    Error::VersionError(GroveVersionError::UnknownVersionMismatch {
                        method,
                        known_versions,
                        received: actual,
                    }) => {
                        assert_eq!(method, "prove_query_non_serialized");
                        assert_eq!(known_versions, vec![0, 1]);
                        assert_eq!(actual, received);
                    }
                    error => panic!("expected unknown proof version, got {error:?}"),
                }
            }
        }

        // Aggregate syntax precedes that gate, including when an offset is
        // present. Refactoring dispatch must not replace its error.
        for kind in [
            AggregateKind::Count,
            AggregateKind::Sum,
            AggregateKind::CountAndSum,
        ] {
            let query = PathQuery::new(
                vec![b"missing".to_vec()],
                SizedQuery::new(aggregate(kind), None, Some(1)),
            );
            let expected = format!("{:?}", query.classify().unwrap_err());
            let result = db.prove_query(&query, None, &version);
            assert_eq!(result.cost, OperationCost::default());
            assert_eq!(format!("{:?}", result.unwrap().unwrap_err()), expected);
        }
    }
}

#[test]
fn legacy_envelopes_reject_read_modes_before_storage_or_proof_walk() {
    let path = vec![b"missing".to_vec()];
    let cases = [
        (
            PathQuery::new_axis_top_k(path.clone(), IndexAxis::Count, 1, 0, true),
            "axis-ordered",
        ),
        (
            PathQuery::new_branched_axis(
                path.clone(),
                vec![b"branch".to_vec()],
                vec![b"suffix".to_vec()],
                AxisQuery::top_k(IndexAxis::Sum, 1, 0, true),
            ),
            "axis-ordered",
        ),
        (
            PathQuery::new_sum_budget(path, vec![QueryItem::RangeFull(..)], true, 10, None),
            "sum-budget",
        ),
    ];
    for version in GROVE_VERSIONS.iter().filter(|version| {
        version
            .grovedb_versions
            .operations
            .proof
            .prove_query_non_serialized
            == 0
    }) {
        let db = make_test_grovedb(version);
        let selection = PathQuery::new_single_key(vec![TEST_LEAF.to_vec()], b"a".to_vec());
        let proof = db.prove_query(&selection, None, version).unwrap().unwrap();
        for (query, family) in &cases {
            let result = db.prove_query(query, None, version);
            assert_eq!(result.cost, OperationCost::default());
            assert!(matches!(
                result.unwrap(),
                Err(Error::NotSupported(message)) if message == format!(
                    "{family} path queries require V1 proof envelopes; upgrade the grove version producing the proof"
                )
            ));
            // The latest verifier supports these shapes, but must reject a
            // canonical V0 envelope before inspecting its layers or paths.
            assert!(matches!(
                GroveDb::verify_path_query(&proof, query, GroveVersion::latest()),
                Err(Error::NotSupported(message)) if message == format!(
                    "{family} path queries require V1 proof envelopes"
                )
            ));
        }
    }
}

#[test]
fn descending_merged_queries_and_subsets_agree_across_versions() {
    for version in GROVE_VERSIONS {
        let db = make_test_grovedb(version);
        for branch in [b"left", b"right".as_slice()] {
            insert(&db, &[TEST_LEAF], branch, Element::empty_tree(), version);
            for key in [b"a", b"b", b"c"] {
                insert(
                    &db,
                    &[TEST_LEAF, branch],
                    key,
                    Element::new_item(key.to_vec()),
                    version,
                );
            }
        }
        let queries: Vec<_> = [b"left", b"right".as_slice()]
            .into_iter()
            .map(|branch| {
                let mut query = Query::new_with_direction(false);
                query.insert_all();
                PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), branch.to_vec()], query)
            })
            .collect();
        let merged = PathQuery::merge(queries.iter().collect(), version).unwrap();
        let proof = db
            .prove_query_many(queries.iter().collect(), None, version)
            .unwrap()
            .unwrap();
        assert_agree(
            run(&db, &merged, version).unwrap().unwrap(),
            GroveDb::verify_path_query(&proof, &merged, version).unwrap(),
        );
        for mut subset in queries {
            subset.query.query.items = vec![QueryItem::RangeAfter(b"a".to_vec()..)];
            let (_, verified) = GroveDb::verify_subset_query(&proof, &subset, version).unwrap();
            let read = elements(run(&db, &subset, version).unwrap().unwrap());
            assert_eq!(
                read.iter()
                    .map(|(_, k, _)| k.as_slice())
                    .collect::<Vec<_>>(),
                vec![b"c", b"b"]
            );
            assert_eq!(read, verified);
        }
    }
}

#[test]
fn descending_absence_results_agree_across_versions() {
    for version in GROVE_VERSIONS {
        let db = make_test_grovedb(version);
        insert(&db, &[TEST_LEAF], b"b", Element::new_item(vec![2]), version);
        let mut query = Query::new_with_direction(false);
        for key in [b"a", b"b", b"c"] {
            query.insert_key(key.to_vec());
        }
        let query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(query, Some(3), None),
        );
        let proof = db.prove_query(&query, None, version).unwrap().unwrap();
        let read = db
            .query_raw_keys_optional(&query, true, true, true, None, version)
            .unwrap()
            .unwrap();
        let (root, verified) =
            GroveDb::verify_query_with_absence_proof(&proof, &query, version).unwrap();
        assert_eq!(root, db.root_hash(None, version).unwrap().unwrap());
        assert_eq!(read, verified);
        assert_eq!(
            verified
                .iter()
                .map(|(_, k, e)| (k.as_slice(), e.is_some()))
                .collect::<Vec<_>>(),
            vec![
                (b"a".as_slice(), false),
                (b"b".as_slice(), true),
                (b"c".as_slice(), false)
            ]
        );
        assert_agree(
            run(&db, &query, version).unwrap().unwrap(),
            GroveDb::verify_path_query(&proof, &query, version).unwrap(),
        );
    }
}

#[test]
fn legacy_pagination_contracts_remain_distinct() {
    for version in GROVE_VERSIONS {
        let db = make_test_grovedb(version);
        insert(&db, &[TEST_LEAF], b"a", Element::new_item(vec![1]), version);
        insert(&db, &[TEST_LEAF], b"b", Element::new_item(vec![2]), version);
        // Ordinary reads allow offsets over explicit keys in a NormalTree.
        // The unified shape grammar cannot represent this as a count proof.
        let mut keys = Query::new();
        keys.insert_key(b"a".to_vec());
        keys.insert_key(b"b".to_vec());
        let query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(keys, Some(1), Some(1)),
        );
        let (read, skipped) = db
            .query_raw(
                &query,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                version,
            )
            .unwrap()
            .unwrap();
        assert_eq!(skipped, 1);
        assert_eq!(
            read.to_key_elements(),
            vec![(b"b".to_vec(), Element::new_item(vec![2]))]
        );
        assert!(matches!(
            run(&db, &query, version).unwrap(),
            Err(Error::InvalidQuery(_))
        ));
        let proof_error = db.prove_query(&query, None, version).unwrap().unwrap_err();
        if version
            .grovedb_versions
            .operations
            .proof
            .prove_query_non_serialized
            == 0
        {
            assert!(matches!(
                proof_error,
                Error::InvalidQuery("proved path queries can not have offsets")
            ));
        } else {
            assert_eq!(
                format!("{proof_error:?}"),
                format!("{:?}", query.classify().unwrap_err())
            );
        }
        let mut zero = query.clone();
        zero.query.offset = Some(0);
        assert_eq!(zero.classify().unwrap(), PathQueryShape::KeySelection);
        let proof = db.prove_query(&zero, None, version).unwrap().unwrap();
        assert_agree(
            run(&db, &zero, version).unwrap().unwrap(),
            GroveDb::verify_path_query(&proof, &zero, version).unwrap(),
        );
        assert!(db
            .query_raw_keys_optional(&zero, true, true, true, None, version)
            .unwrap()
            .is_err());
        // Parent-info verification defers to the shared envelope gate
        // (#707), which treats `Some(0)` as no offset: the proof
        // verifies and returns the same limited page as the read.
        let (_, _, parent_info_rows) =
            GroveDb::verify_query_get_parent_tree_info(&proof, &zero, version)
                .expect("parent-info verification serves a zero offset");
        assert_eq!(
            parent_info_rows,
            vec![(
                vec![TEST_LEAF.to_vec()],
                b"a".to_vec(),
                Some(Element::new_item(vec![1]))
            )]
        );
    }
}

/// The fixtures pin serialized queries, proof bytes, and both operations'
/// costs against the implementation before validation was unified.
#[test]
fn query_proof_bytes_and_costs_match_compatibility_fixtures() {
    fn cost(cost: &OperationCost) -> (u32, u64, u32) {
        assert_eq!(cost.storage_cost, Default::default());
        assert_eq!(cost.sinsemilla_hash_calls, 0);
        (
            cost.seek_count,
            cost.storage_loaded_bytes,
            cost.hash_node_calls,
        )
    }
    let mut fixtures = Vec::new();
    for version in GROVE_VERSIONS {
        let db = make_test_grovedb(version);
        for (key, value) in [(b"a", 7), (b"b", 2), (b"c", 5)] {
            insert(
                &db,
                &[TEST_LEAF],
                key,
                Element::new_item(vec![value]),
                version,
            );
        }
        let mut selection = Query::new_with_direction(false);
        selection.insert_all();
        let mut cases = vec![(
            "selection".to_string(),
            PathQuery::new(
                vec![TEST_LEAF.to_vec()],
                SizedQuery::new(selection, Some(2), Some(0)),
            ),
        )];
        let mut absence = Query::new_with_direction(false);
        absence.insert_key(b"missing".to_vec());
        cases.push((
            "absence".to_string(),
            PathQuery::new(
                vec![TEST_LEAF.to_vec()],
                SizedQuery::new(absence, Some(1), None),
            ),
        ));
        if version
            .grovedb_versions
            .operations
            .proof
            .prove_query_non_serialized
            == 1
        {
            for (kind, key, tree) in [
                (
                    AggregateKind::Count,
                    b"count".as_slice(),
                    Element::empty_provable_count_tree(),
                ),
                (
                    AggregateKind::Sum,
                    b"sum".as_slice(),
                    Element::empty_provable_sum_tree(),
                ),
                (
                    AggregateKind::CountAndSum,
                    b"both".as_slice(),
                    Element::empty_provable_count_provable_sum_tree(),
                ),
            ] {
                insert(&db, &[TEST_LEAF], key, tree, version);
                for (child, value) in [(b"a", 8), (b"m", -3), (b"z", 99)] {
                    let element = if kind == AggregateKind::Count {
                        Element::new_item(vec![1])
                    } else {
                        Element::new_sum_item(value)
                    };
                    insert(&db, &[TEST_LEAF, key], child, element, version);
                }
                cases.push((
                    format!("{kind:?}-leaf"),
                    PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), key.to_vec()], aggregate(kind)),
                ));
                let mut outer = Query::new_with_direction(false);
                outer.insert_key(key.to_vec());
                outer.insert_key(b"missing".to_vec());
                outer.set_subquery(aggregate(kind));
                cases.push((
                    format!("{kind:?}-carrier"),
                    PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], outer),
                ));
            }
            let mut page = Query::new_with_direction(false);
            page.insert_all();
            cases.push((
                "count-offset".to_string(),
                PathQuery::new(
                    vec![TEST_LEAF.to_vec(), b"count".to_vec()],
                    SizedQuery::new(page, Some(1), Some(1)),
                ),
            ));
        }
        for (name, query) in cases {
            let read = run(&db, &query, version);
            let proof = db.prove_query(&query, None, version);
            let read_cost = cost(&read.cost);
            let proof_cost = cost(&proof.cost);
            let read = read.unwrap().unwrap();
            let proof = proof.unwrap().unwrap();
            assert_agree(
                read,
                GroveDb::verify_path_query(&proof, &query, version).unwrap(),
            );
            let encoded = bincode::encode_to_vec(
                &query,
                bincode::config::standard()
                    .with_big_endian()
                    .with_no_limit(),
            )
            .unwrap();
            fixtures.push(format!(
                "v{} {name} {} {} {read_cost:?} {proof_cost:?}",
                version.protocol_version,
                blake3::hash(&encoded).to_hex(),
                blake3::hash(&proof).to_hex()
            ));
        }
    }
    assert_eq!(
        fixtures.join("\n") + "\n",
        include_str!("fixtures/validated_path_query.txt"),
        "query bytes, proof bytes, or operation costs changed from the pre-refactor baseline"
    );
}
