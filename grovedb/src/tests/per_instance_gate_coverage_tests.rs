//! Gate and dispatch coverage for per-instance limits (`Query::limit`)
//! beyond the engine-semantics tests: the shape/validator rejects fire
//! and the public `query_item` wrapper keeps its limit/offset contract.

use grovedb_version::version::GroveVersion;

use crate::{
    query_result_type::QueryResultType,
    tests::{make_test_grovedb, TempGroveDb},
    Element, Error, PathQuery, Query, SizedQuery,
};

const DOCS: &[u8] = b"docs";

fn populate(db: &TempGroveDb, grove_version: &GroveVersion) {
    use crate::tests::common::EMPTY_PATH;
    db.insert(
        EMPTY_PATH,
        DOCS,
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert docs tree");
    for parent in [b"p1", b"p2"] {
        db.insert(
            [DOCS].as_ref(),
            parent,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert parent tree");
        for i in 0..3u8 {
            db.insert(
                [DOCS, parent.as_slice()].as_ref(),
                &[b'k', b'0' + i],
                Element::new_item(vec![i]),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert item");
        }
    }
}

#[test]
fn capability_slot_is_validated_exactly_and_for_engine_coherence() {
    let real = GroveVersion::latest();
    let db = make_test_grovedb(real);
    populate(&db, real);

    let mut limited_sub = Query::new_range_full();
    limited_sub.limit = Some(1);
    let mut limited = Query::new_range_full();
    limited.set_subquery(limited_sub);
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(limited, None, None));

    // An unknown future capability value must be a typed version error,
    // not a silent run of today's semantics.
    let mut unknown_capability = real.clone();
    unknown_capability
        .grovedb_versions
        .path_query_methods
        .per_instance_query_limits = 2;
    let result = db.query_raw(
        &path_query,
        true,
        true,
        true,
        QueryResultType::QueryPathKeyElementTrioResultType,
        None,
        &unknown_capability,
    );
    assert!(
        matches!(result.unwrap(), Err(Error::VersionError(_))),
        "unknown per_instance_query_limits values must be rejected"
    );

    // A doctored table that serves the capability but selects the v0
    // read engine would silently drop ancestor budgets — refused as
    // incoherent instead.
    let mut incoherent = real.clone();
    incoherent.grovedb_versions.element.path_query_push = 0;
    let result = db.query_raw(
        &path_query,
        true,
        true,
        true,
        QueryResultType::QueryPathKeyElementTrioResultType,
        None,
        &incoherent,
    );
    assert!(
        matches!(result.unwrap(), Err(Error::CorruptedCodeExecution(_))),
        "a capability/engine mismatch must be refused"
    );
}

#[test]
fn limited_query_verification_rejects_before_decoding_the_proof() {
    let mut limited_sub = Query::new_range_full();
    limited_sub.limit = Some(1);
    let mut limited = Query::new_range_full();
    limited.set_subquery(limited_sub);
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(limited, None, None));

    // An empty (undecodable) proof plus a limited query a given mode
    // cannot serve: the query-shape gate must win over the proof-decode
    // error, so no parsing budget is spent on a request that can never
    // be served. Pre-V4, limited queries are unserved entirely…
    let legacy_version = &grovedb_version::version::GROVE_VERSIONS[2];
    assert_eq!(legacy_version.protocol_version, 3);
    let result = crate::GroveDb::verify_query_with_options(
        &[],
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: true,
            include_empty_trees_in_result: false,
        },
        legacy_version,
    );
    assert!(
        matches!(result, Err(Error::NotSupported(_))),
        "the limited-query gate must fire before proof decoding, got {result:?}"
    );
    let result = crate::GroveDb::verify_query_raw(&[], &path_query, legacy_version);
    assert!(
        matches!(result, Err(Error::NotSupported(_))),
        "verify_query_raw must gate before decoding too, got {result:?}"
    );

    // …and at latest, absence-proof assembly still cannot serve them.
    let grove_version = GroveVersion::latest();
    let mut absence_query = path_query.clone();
    absence_query.query.limit = Some(10); // absence mode requires a global limit
    let result = crate::GroveDb::verify_query_with_options(
        &[],
        &absence_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: true,
            verify_proof_succinctness: true,
            include_empty_trees_in_result: false,
        },
        grove_version,
    );
    assert!(
        matches!(result, Err(Error::NotSupported(_))),
        "the absence-mode gate must fire before proof decoding, got {result:?}"
    );
}

#[test]
fn absent_terminal_instance_charge_honors_the_governing_flag() {
    use crate::element::{query::ElementQueryExtensions, query_options::QueryOptions};
    use crate::tests::common::EMPTY_PATH;

    // docs/g/{m1, m2}: m1 has no `t` child (absent terminal), m2 does.
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    db.insert(
        EMPTY_PATH,
        DOCS,
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert docs tree");
    db.insert(
        [DOCS].as_ref(),
        b"g",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert g");
    for mid in [b"m1", b"m2"] {
        db.insert(
            [DOCS, b"g".as_slice()].as_ref(),
            mid,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert mid");
    }
    db.insert(
        [DOCS, b"g".as_slice(), b"m2".as_slice()].as_ref(),
        b"t",
        Element::new_item(vec![1]),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert terminal");

    // g-level instances capped at 1, terminals selected via a
    // subquery_path-only branch. With the governing empty-range flag
    // OFF, the subordinate instance flag must have no effect: m1's
    // absent terminal may not consume the cap that m2's real terminal
    // needs.
    let mut g_query = Query::new_range_full();
    g_query.set_subquery_path(vec![b"t".to_vec()]);
    g_query.limit = Some(1);
    let mut root = Query::new_single_key(b"g".to_vec());
    root.set_subquery(g_query);
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(root, None, None));

    let (elements, _) = Element::get_path_query(
        &db.db,
        &path_query,
        QueryOptions {
            allow_get_raw: true,
            include_parent_tree_on_subquery: true,
            allow_cache: true,
            decrease_limit_on_range_with_no_sub_elements: false,
            error_if_intermediate_path_tree_not_present: true,
            decrease_instance_limits_on_range_with_no_sub_elements: true,
        },
        QueryResultType::QueryPathKeyElementTrioResultType,
        None,
        grove_version,
    )
    .unwrap()
    .expect("query should succeed");
    assert_eq!(
        elements.len(),
        1,
        "m2's real terminal must still fit the instance cap"
    );
}

#[test]
fn unmatched_conditional_limits_are_caught_by_the_wrapper_preflight() {
    use crate::element::{query::ElementQueryExtensions, query_options::QueryOptions};
    use grovedb_merk::proofs::query::QueryItem;

    let real = GroveVersion::latest();
    let db = make_test_grovedb(real);
    populate(&db, real);

    // A per-instance limit hiding in a conditional branch no key
    // matches: the per-frame checks never reach it, so the public
    // wrapper's recursive preflight is what must catch it.
    let make_query = |limit| {
        let mut hidden = Query::new_range_full();
        hidden.limit = Some(limit);
        let mut query = Query::new_range_full();
        query.set_subquery(Query::new_range_full());
        query.add_conditional_subquery(QueryItem::Key(b"zz".to_vec()), None, Some(hidden));
        PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(query, None, None))
    };

    // Pre-V4: fail closed even though the branch never matches.
    let legacy_version = &grovedb_version::version::GROVE_VERSIONS[2];
    assert_eq!(legacy_version.protocol_version, 3);
    let result = Element::get_path_query(
        &db.db,
        &make_query(1),
        QueryOptions::default(),
        QueryResultType::QueryPathKeyElementTrioResultType,
        None,
        legacy_version,
    );
    assert!(
        matches!(result.unwrap(), Err(Error::NotSupported(_))),
        "an unmatched limited conditional must still fail closed pre-V4"
    );

    // Latest: an unmatched zero cap is still malformed.
    let result = Element::get_path_query(
        &db.db,
        &make_query(0),
        QueryOptions::default(),
        QueryResultType::QueryPathKeyElementTrioResultType,
        None,
        real,
    );
    assert!(
        matches!(result.unwrap(), Err(Error::InvalidQuery(_))),
        "an unmatched zero cap must still be rejected as malformed"
    );
}

#[test]
fn read_mode_shapes_reject_instance_limits_at_classify() {
    use grovedb_merk::proofs::query::IndexAxis;

    let mut path_query =
        PathQuery::new_axis_top_k(vec![DOCS.to_vec()], IndexAxis::Count, 3, 0, true);
    path_query.query.query.limit = Some(1);
    match path_query.classify() {
        Err(Error::InvalidQuery(message)) => {
            assert!(message.contains("per-instance"), "got: {message}")
        }
        other => panic!("read-mode shapes must reject instance limits, got {other:?}"),
    }
}

#[test]
fn aggregate_and_count_offset_validators_reject_instance_limits() {
    use grovedb_merk::proofs::query::QueryItem;

    // Aggregate carrier with a nested instance limit.
    let mut carrier_sub = Query::new_aggregate_count_on_range(QueryItem::RangeFull(..));
    carrier_sub.limit = Some(1);
    let mut carrier = Query::new_range_full();
    carrier.set_subquery(carrier_sub);
    let path_query = PathQuery::new_unsized(vec![DOCS.to_vec()], carrier);
    match path_query.validate_aggregate_count_on_range() {
        Err(Error::InvalidQuery(message)) => {
            assert!(message.contains("per-instance"), "got: {message}")
        }
        other => panic!("aggregate validators must reject instance limits, got {other:?}"),
    }

    // Count-offset pagination with an instance limit.
    let mut paginated = Query::new_range_full();
    paginated.limit = Some(2);
    let sized = SizedQuery::new(paginated, Some(2), Some(1));
    match sized.validate_count_offset_paginated() {
        Err(Error::InvalidQuery(message)) => {
            assert!(message.contains("per-instance"), "got: {message}")
        }
        other => panic!("count-offset validation must reject instance limits, got {other:?}"),
    }
}

#[test]
fn query_keys_optional_rejects_instance_limits() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate(&db, grove_version);

    let mut sub = Query::new_range_full();
    sub.limit = Some(1);
    let mut query = Query::new_range_full();
    query.set_subquery(sub);
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(query, Some(10), None));

    for result in [
        db.query_keys_optional(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .map(|_| ()),
        db.query_raw_keys_optional(&path_query, true, true, true, None, grove_version)
            .unwrap()
            .map(|_| ()),
    ] {
        assert!(
            matches!(result, Err(Error::NotSupported(_))),
            "terminal-keys projections must reject instance limits, got {result:?}"
        );
    }
}

#[test]
fn query_options_display_covers_the_instance_charge_flag() {
    use crate::element::query_options::QueryOptions;

    let options = QueryOptions::default();
    let rendered = format!("{options}");
    assert!(
        rendered.contains("decrease_instance_limits_on_range_with_no_sub_elements: false"),
        "got: {rendered}"
    );
}

#[test]
fn public_query_item_wrapper_keeps_the_limit_offset_contract() {
    use grovedb_merk::proofs::query::QueryItem;

    use crate::{element::query::ElementQueryExtensions, query_result_type::QueryResultElement};

    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate(&db, grove_version);

    // Drive the public trait method directly: one range item over the
    // p1 subtree, with a limit and an offset threaded through the
    // legacy `&mut Option<u16>` signature.
    let sized_query = SizedQuery::new(Query::new_range_full(), Some(2), Some(1));
    let mut results: Vec<QueryResultElement> = Vec::new();
    let mut limit = Some(2u16);
    let mut offset = Some(1u16);
    let path: [&[u8]; 2] = [DOCS, b"p1"];
    Element::query_item(
        &db.db,
        &QueryItem::RangeFull(..),
        &mut results,
        &path,
        &sized_query,
        None,
        &mut limit,
        &mut offset,
        Default::default(),
        QueryResultType::QueryKeyElementPairResultType,
        |args, grove_version| {
            use grovedb_costs::CostsExt;
            Element::basic_push(args, grove_version)
                .wrap_with_cost(grovedb_costs::OperationCost::default())
        },
        grove_version,
    )
    .unwrap()
    .expect("query_item should succeed");

    assert_eq!(results.len(), 2, "limit rows after the offset skip");
    assert_eq!(limit, Some(0), "consumed budget is written back");
    assert_eq!(offset, Some(0), "consumed offset is written back");

    // A queried node carrying its own per-instance cap fails closed:
    // this legacy signature cannot thread an instance budget (it is
    // called per item, and the budget is per node instance).
    let mut capped = Query::new_range_full();
    capped.limit = Some(1);
    let capped_sized = SizedQuery::new(capped, None, None);
    let mut limit = None;
    let mut offset = None;
    let result = Element::query_item(
        &db.db,
        &QueryItem::RangeFull(..),
        &mut results,
        &path,
        &capped_sized,
        None,
        &mut limit,
        &mut offset,
        Default::default(),
        QueryResultType::QueryKeyElementPairResultType,
        |args, grove_version| {
            use grovedb_costs::CostsExt;
            Element::basic_push(args, grove_version)
                .wrap_with_cost(grovedb_costs::OperationCost::default())
        },
        grove_version,
    );
    assert!(
        matches!(result.unwrap(), Err(Error::NotSupported(_))),
        "query_item must reject a per-instance cap on the queried node"
    );
}

#[test]
fn query_item_preflights_the_entire_query() {
    use grovedb_merk::proofs::query::QueryItem;

    use crate::{element::query::ElementQueryExtensions, query_result_type::QueryResultElement};

    // A per-instance limit hiding in an unmatched conditional branch:
    // without the recursive preflight, acceptance depended on database
    // contents (rejected only once stored data made the branch match).
    let legacy_version = &grovedb_version::version::GROVE_VERSIONS[2];
    assert_eq!(legacy_version.protocol_version, 3);
    let db = make_test_grovedb(legacy_version);
    populate(&db, legacy_version);

    let mut hidden = Query::new_range_full();
    hidden.limit = Some(1);
    let mut query = Query::new_range_full();
    query.add_conditional_subquery(QueryItem::Key(b"zz".to_vec()), None, Some(hidden));
    let sized_query = SizedQuery::new(query, None, None);

    let mut results: Vec<QueryResultElement> = Vec::new();
    let mut limit = None;
    let mut offset = None;
    let path: [&[u8]; 2] = [DOCS, b"p1"];
    let result = Element::query_item(
        &db.db,
        &QueryItem::RangeFull(..),
        &mut results,
        &path,
        &sized_query,
        None,
        &mut limit,
        &mut offset,
        Default::default(),
        QueryResultType::QueryKeyElementPairResultType,
        |args, grove_version| {
            use grovedb_costs::CostsExt;
            Element::basic_push(args, grove_version)
                .wrap_with_cost(grovedb_costs::OperationCost::default())
        },
        legacy_version,
    );
    assert!(
        matches!(result.unwrap(), Err(Error::NotSupported(_))),
        "query_item must preflight limits in unmatched branches too"
    );
}

#[test]
fn unknown_push_engine_is_rejected_independently_of_data() {
    // capability = 1 with an unknown engine selector must be a typed
    // version error even when the query matches nothing — previously a
    // limited missing-key query returned Ok(empty) because the
    // dispatcher was never reached, making validation data-dependent.
    let real = GroveVersion::latest();
    let db = make_test_grovedb(real);
    populate(&db, real);

    let mut missing_sub = Query::new_range_full();
    missing_sub.limit = Some(1);
    let mut missing = Query::new_single_key(b"no-such-key".to_vec());
    missing.set_subquery(missing_sub);
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(missing, None, None));

    let mut unknown_engine = real.clone();
    unknown_engine.grovedb_versions.element.path_query_push = 3;
    let result = db.query_raw(
        &path_query,
        true,
        true,
        true,
        QueryResultType::QueryPathKeyElementTrioResultType,
        None,
        &unknown_engine,
    );
    assert!(
        matches!(result.unwrap(), Err(Error::VersionError(_))),
        "an unknown engine selector must be rejected up front"
    );
}

#[test]
fn subordinate_method_version_slots_still_gate_the_internal_walk() {
    // The internal helpers are entered directly by the engines, so
    // their subordinate slots must be validated inside them — a
    // future/replay table bumping a slot must not silently run today's
    // semantics.
    let real = GroveVersion::latest();
    let db = make_test_grovedb(real);
    populate(&db, real);

    let mut query = Query::new_range_full();
    query.set_subquery(Query::new_range_full());
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(query, Some(2), None));

    for doctor in [
        (|v: &mut GroveVersion| v.grovedb_versions.element.query_item = 1) as fn(&mut GroveVersion),
        |v| v.grovedb_versions.element.get_query_apply_function = 1,
        |v| v.grovedb_versions.element.get_path_query = 1,
    ] {
        let mut doctored = real.clone();
        doctor(&mut doctored);
        let result = db.query_raw(
            &path_query,
            true,
            true,
            true,
            QueryResultType::QueryPathKeyElementTrioResultType,
            None,
            &doctored,
        );
        assert!(
            matches!(result.unwrap(), Err(Error::VersionError(_))),
            "a bumped subordinate slot must reject, not run today's semantics"
        );
    }
}

#[test]
fn query_item_rejects_global_offset_with_local_descendants_before_reading() {
    use crate::{
        element::query::ElementQueryExtensions, query_result_type::QueryResultElement, QueryItem,
    };

    let v = GroveVersion::latest();
    let db = make_test_grovedb(v);
    populate(&db, v);
    db.insert(
        [DOCS].as_ref(),
        b"counts",
        Element::empty_provable_count_tree(),
        None,
        None,
        v,
    )
    .unwrap()
    .unwrap();
    for selected_key in [b"missing".as_slice(), b"counts", b"p1"] {
        for global_offset in [0, 1] {
            let mut child = Query::new_range_full();
            child.offset = Some(0);
            let mut query = Query::new_single_key(selected_key.to_vec());
            if selected_key == b"p1" {
                // The offset-bearing branch does not match the selected key.
                query.add_conditional_subquery(
                    QueryItem::Key(b"unmatched".to_vec()),
                    None,
                    Some(child),
                );
            } else {
                query.set_subquery(child);
            }
            let sized = SizedQuery::new(query, None, None);
            let sentinel = QueryResultElement::ElementResultItem(Element::new_item(vec![42]));
            let mut rows = vec![sentinel.clone()];
            let mut limit = Some(3);
            let mut offset = Some(global_offset);
            let result = Element::query_item(
                &db.db,
                &QueryItem::Key(selected_key.to_vec()),
                &mut rows,
                &[DOCS],
                &sized,
                None,
                &mut limit,
                &mut offset,
                Default::default(),
                QueryResultType::QueryElementResultType,
                Element::path_query_push,
                v,
            );
            assert!(
                matches!(
                    result.value(),
                    Err(Error::InvalidQuery(
                        "global and local offsets cannot be combined"
                    ))
                ),
                "selected key {selected_key:?}, global offset {global_offset}: {result:?}"
            );
            assert!(
                result.cost.is_nothing(),
                "refusal must precede storage access"
            );
            assert_eq!(rows, vec![sentinel], "refusal must not append partial rows");
            assert_eq!(limit, Some(3));
            assert_eq!(offset, Some(global_offset));
        }
    }
}

mod local_offset_public_reads {
    use grovedb_costs::CostResult;
    use grovedb_query::{AxisQuery, IndexAxis, ReadMode, SumBudgetRead};

    use super::*;
    use crate::{element::query::ElementQueryExtensions, QueryItem};

    fn count_db() -> TempGroveDb {
        let v = GroveVersion::latest();
        let db = make_test_grovedb(v);
        db.insert(
            &[] as &[&[u8]],
            DOCS,
            Element::empty_provable_count_tree(),
            None,
            None,
            v,
        )
        .unwrap()
        .unwrap();
        for i in 0..3 {
            db.insert(&[DOCS], &[i], Element::new_item(vec![i]), None, None, v)
                .unwrap()
                .unwrap();
        }
        db
    }

    fn read_each(
        db: &TempGroveDb,
        query: &Query,
        v: &GroveVersion,
    ) -> [(&'static str, CostResult<Vec<Element>, Error>); 6] {
        let sized = SizedQuery::new(query.clone(), None, None);
        let path_query = PathQuery::new(vec![DOCS.to_vec()], sized.clone());
        let result_type = QueryResultType::QueryElementResultType;
        let mut rows = Vec::new();
        let query_item = Element::query_item(
            &db.db,
            query.items.first().unwrap_or(&QueryItem::RangeFull(..)),
            &mut rows,
            &[DOCS],
            &sized,
            None,
            &mut None,
            &mut None,
            Default::default(),
            result_type,
            Element::path_query_push,
            v,
        )
        .map_ok(|()| {
            crate::query_result_type::QueryResultElements::from_elements(rows).to_elements()
        });
        [
            (
                "get_query",
                Element::get_query(
                    &db.db,
                    &[DOCS],
                    query,
                    Default::default(),
                    result_type,
                    None,
                    v,
                )
                .map_ok(|rows| rows.to_elements()),
            ),
            (
                "get_query_values",
                Element::get_query_values(&db.db, &[DOCS], query, Default::default(), None, v),
            ),
            (
                "get_sized_query",
                Element::get_sized_query(
                    &db.db,
                    &[DOCS],
                    &sized,
                    Default::default(),
                    result_type,
                    None,
                    v,
                )
                .map_ok(|(rows, _)| rows.to_elements()),
            ),
            (
                "get_query_apply_function",
                Element::get_query_apply_function(
                    &db.db,
                    &[DOCS],
                    &sized,
                    Default::default(),
                    result_type,
                    None,
                    Element::path_query_push,
                    v,
                )
                .map_ok(|(rows, _)| rows.to_elements()),
            ),
            (
                "get_path_query",
                Element::get_path_query(
                    &db.db,
                    &path_query,
                    Default::default(),
                    result_type,
                    None,
                    v,
                )
                .map_ok(|(rows, _)| rows.to_elements()),
            ),
            ("query_item", query_item),
        ]
    }

    fn assert_invalid_before_storage(db: &TempGroveDb, query: &Query, label: &str) {
        for (api, result) in read_each(db, query, GroveVersion::latest()) {
            assert!(
                matches!(result.value(), Err(Error::InvalidQuery(_))),
                "{api} must reject {label}: {result:?}"
            );
            assert!(
                result.cost.is_nothing(),
                "{api}: {label} must fail before storage access"
            );
        }
    }

    #[test]
    fn empty_local_offset_queries_return_errors_through_every_element_read() {
        let db = count_db();
        let mut query = Query::new();
        query.offset = Some(0);
        assert_invalid_before_storage(&db, &query, "an empty local-offset query");
    }

    #[test]
    fn unsupported_local_offset_shapes_are_rejected_through_every_element_read() {
        let db = count_db();
        let mut multiple = Query::new();
        multiple.insert_range(vec![0]..vec![1]);
        multiple.insert_range(vec![2]..vec![3]);
        let mut subquery = Query::new_range_full();
        subquery.set_subquery(Query::new_range_full());
        let mut conditional = Query::new_range_full();
        conditional.add_conditional_subquery(
            QueryItem::Key(vec![99]),
            None,
            Some(Query::new_range_full()),
        );
        let mut path = Query::new_range_full();
        path.set_subquery_path(vec![b"child".to_vec()]);
        let mut echo = Query::new_range_full();
        echo.add_parent_tree_on_subquery = true;
        let mut sum_budget = Query::new_range_full();
        sum_budget.read_mode = Some(Box::new(ReadMode::SumBudget(SumBudgetRead {
            sum_limit: 1,
            match_limit: None,
        })));
        let mut axis = Query::new_range_full();
        axis.read_mode = Some(Box::new(ReadMode::Axis(AxisQuery::top_k(
            IndexAxis::Count,
            1,
            0,
            true,
        ))));
        for (label, mut query) in [
            ("multiple ranges", multiple),
            ("an exact key", Query::new_single_key(vec![0])),
            ("a default subquery", subquery),
            ("a conditional subquery", conditional),
            ("a subquery path", path),
            ("parent echo", echo),
            (
                "a count aggregate",
                Query::new_aggregate_count_on_range(QueryItem::RangeFull(..)),
            ),
            (
                "a sum aggregate",
                Query::new_aggregate_sum_on_range(QueryItem::RangeFull(..)),
            ),
            (
                "a count-and-sum aggregate",
                Query::new_aggregate_count_and_sum_on_range(QueryItem::RangeFull(..)),
            ),
            ("a sum-budget read", sum_budget),
            ("an axis read", axis),
        ] {
            query.offset = Some(1);
            assert_invalid_before_storage(&db, &query, label);
        }
    }

    #[test]
    fn unmatched_malformed_local_offsets_are_preflighted_by_every_element_read() {
        let db = count_db();
        let mut invalid = Query::new();
        invalid.offset = Some(0);
        let mut query = Query::new_single_key(vec![0]);
        query.add_conditional_subquery(QueryItem::Key(vec![99]), None, Some(invalid));
        assert_invalid_before_storage(&db, &query, "a malformed unmatched local-offset descendant");
    }

    #[test]
    fn older_versions_refuse_local_offsets_through_every_element_read() {
        let db = count_db();
        let mut local = Query::new_range_full();
        local.offset = Some(0);
        let mut nested = Query::new_single_key(vec![0]);
        nested.add_conditional_subquery(QueryItem::Key(vec![99]), None, Some(local.clone()));
        for v in grovedb_version::version::GROVE_VERSIONS
            .iter()
            .filter(|v| v.protocol_version < 5)
        {
            for query in [&local, &nested] {
                for (api, result) in read_each(&db, query, v) {
                    assert!(
                        matches!(result.value(), Err(Error::NotSupported(_))),
                        "{api}, V{}: {result:?}",
                        v.protocol_version
                    );
                    assert!(
                        result.cost.is_nothing(),
                        "{api} must validate the version before storage access"
                    );
                }
            }
        }
    }

    #[test]
    fn query_item_serves_local_descendants_without_a_global_offset() {
        let db = count_db();
        let v = GroveVersion::latest();
        let mut local = Query::new_range_full();
        local.offset = Some(1);
        local.limit = Some(1);
        let mut query = Query::new_single_key(DOCS.to_vec());
        query.set_subquery(local);
        let sized = SizedQuery::new(query, None, None);
        let mut rows = Vec::new();
        let mut limit = Some(3);
        let mut offset = None;
        Element::query_item(
            &db.db,
            &QueryItem::Key(DOCS.to_vec()),
            &mut rows,
            &[],
            &sized,
            None,
            &mut limit,
            &mut offset,
            Default::default(),
            QueryResultType::QueryElementResultType,
            Element::path_query_push,
            v,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            crate::query_result_type::QueryResultElements::from_elements(rows).to_elements(),
            vec![Element::new_item(vec![1])],
            "the local skip must apply within the selected count-bearing descendant"
        );
        assert_eq!(
            limit,
            Some(2),
            "only the returned row consumes the global limit"
        );
        assert_eq!(offset, None, "local skips must not become a global offset");
    }

    #[test]
    fn valid_local_pages_reach_every_serving_element_read() {
        let db = count_db();
        let mut query = Query::new_range_full();
        query.offset = Some(1);
        query.limit = Some(1);
        for (api, result) in read_each(&db, &query, GroveVersion::latest()) {
            if api == "query_item" {
                assert!(matches!(result.value(), Err(Error::NotSupported(_))));
                assert!(result.cost.is_nothing());
            } else {
                assert!(
                    result.cost.seek_count > 0,
                    "{api} must read the populated host"
                );
                assert_eq!(
                    result.unwrap().unwrap(),
                    vec![Element::new_item(vec![1])],
                    "{api} must return the selected local page"
                );
            }
        }
    }
}
