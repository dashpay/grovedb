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
    let grove_version = GroveVersion::latest();

    let mut limited_sub = Query::new_range_full();
    limited_sub.limit = Some(1);
    let mut limited = Query::new_range_full();
    limited.set_subquery(limited_sub);
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(limited, None, None));

    // An empty (undecodable) proof plus an unsupported limited query:
    // the query-shape gate must win over the proof-decode error, so no
    // parsing budget is spent on a request that can never be served.
    let result = crate::GroveDb::verify_query_with_options(
        &[],
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: true,
            include_empty_trees_in_result: false,
        },
        grove_version,
    );
    assert!(
        matches!(result, Err(Error::NotSupported(_))),
        "the limited-query gate must fire before proof decoding, got {result:?}"
    );
    let result = crate::GroveDb::verify_query_raw(&[], &path_query, grove_version);
    assert!(
        matches!(result, Err(Error::NotSupported(_))),
        "verify_query_raw must gate before decoding too, got {result:?}"
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
