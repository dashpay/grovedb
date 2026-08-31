//! Gate and dispatch coverage for per-instance limits (`Query::limit`)
//! beyond the engine-semantics tests: the historical `path_query_push`
//! v1 arm stays exercised (no shipping grove version selects it now
//! that `GROVE_V4` maps to v2), the shape/validator rejects fire, and
//! the public `query_item` wrapper keeps its limit/offset contract.

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

fn subquery_path_query() -> PathQuery {
    let mut query = Query::new_range_full();
    query.set_subquery(Query::new_range_full());
    PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(query, Some(4), None))
}

#[test]
fn path_query_push_v1_dispatch_arm_still_serves_plain_queries() {
    // GROVE_V4 maps `element.path_query_push` to v2, so the historical
    // v1 intermediate is unreachable from any shipping version table —
    // pin it through a doctored version so its accounting (the
    // issue-#690 guard without consumed-based reconciliation) stays
    // exercised and comparable.
    let real = GroveVersion::latest();
    let db = make_test_grovedb(real);
    populate(&db, real);

    let mut v1_doctored = real.clone();
    v1_doctored.grovedb_versions.element.path_query_push = 1;

    let (v2_result, v2_skipped) = db
        .query_raw(
            &subquery_path_query(),
            true,
            true,
            true,
            QueryResultType::QueryPathKeyElementTrioResultType,
            None,
            real,
        )
        .unwrap()
        .expect("v2 read");
    let (v1_result, v1_skipped) = db
        .query_raw(
            &subquery_path_query(),
            true,
            true,
            true,
            QueryResultType::QueryPathKeyElementTrioResultType,
            None,
            &v1_doctored,
        )
        .unwrap()
        .expect("v1 read");
    assert_eq!(v1_skipped, v2_skipped);
    assert_eq!(
        v1_result.to_path_key_elements(),
        v2_result.to_path_key_elements(),
        "for plain single-level subqueries v1 and v2 agree"
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
