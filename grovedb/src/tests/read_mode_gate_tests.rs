//! Fail-closed gates for read-mode (axis / sum-budget) path queries.
//!
//! The read-mode vocabulary can be constructed and round-tripped, but
//! no entry point serves it yet. Until the unified dispatch lands,
//! every existing read / prove / verify / merge entry point must
//! reject a read-mode query with a typed `NotSupported` — never run it
//! as plain key selection (an axis read has empty items, so key
//! selection would return an empty result indistinguishable from real
//! absence, and a proof would attest to the wrong read entirely).

use grovedb_merk::proofs::query::{AxisQuery, IndexAxis};
use grovedb_version::version::GroveVersion;

use crate::{
    query_result_type::QueryResultType, tests::make_empty_grovedb, Error, GroveDb, PathQuery,
};

fn axis_path_query() -> PathQuery {
    PathQuery::new_axis_top_k(vec![b"tree".to_vec()], IndexAxis::Count, 3, 0, true)
}

fn sum_budget_path_query() -> PathQuery {
    PathQuery::new_sum_budget(
        vec![b"tree".to_vec()],
        vec![grovedb_merk::proofs::query::query_item::QueryItem::RangeFull(..)],
        true,
        100,
        None,
    )
}

fn branched_axis_path_query() -> PathQuery {
    PathQuery::new_branched_axis(
        vec![b"contracts".to_vec()],
        vec![b"alice".to_vec(), b"bob".to_vec()],
        vec![b"scores".to_vec()],
        AxisQuery::top_k(IndexAxis::Sum, 2, 0, true),
    )
}

fn all_read_mode_queries() -> Vec<PathQuery> {
    vec![
        axis_path_query(),
        sum_budget_path_query(),
        branched_axis_path_query(),
    ]
}

fn assert_not_supported<T: std::fmt::Debug>(result: Result<T, Error>, entry_point: &str) {
    match result {
        Err(Error::NotSupported(_)) => {}
        Err(other) => panic!("{entry_point}: expected NotSupported, got {other:?}"),
        Ok(value) => panic!("{entry_point}: read-mode query must be rejected, got {value:?}"),
    }
}

#[test]
fn query_raw_rejects_read_mode_queries() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    for path_query in all_read_mode_queries() {
        assert_not_supported(
            db.query_raw(
                &path_query,
                true,
                true,
                true,
                QueryResultType::QueryKeyElementPairResultType,
                None,
                grove_version,
            )
            .unwrap(),
            "query_raw",
        );
    }
}

#[test]
fn prove_query_gates_read_mode_queries() {
    let db = make_empty_grovedb();

    // Axis shapes are served from GROVE_V4 (round-trip coverage lives
    // in axis_descent_proof_tests); below V4 the prover refuses them.
    let v3 = &grovedb_version::version::GROVE_VERSIONS[2];
    assert_eq!(v3.protocol_version, 3);
    for path_query in [axis_path_query(), branched_axis_path_query()] {
        assert_not_supported(
            db.prove_query(&path_query, None, v3).unwrap(),
            "prove_query (axis, V3)",
        );
    }

    // Sum-budget shapes have no proof form at any version yet.
    assert_not_supported(
        db.prove_query(&sum_budget_path_query(), None, GroveVersion::latest())
            .unwrap(),
        "prove_query (sum budget)",
    );
}

#[test]
fn verify_query_rejects_read_mode_queries_before_touching_the_proof() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    db.insert(
        grovedb_path::SubtreePath::empty(),
        b"tree",
        crate::Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert tree");

    // A real proof over a plain query — then verified against a
    // read-mode query. The read-mode gate must fire before any proof
    // decoding or query walking.
    let plain = PathQuery::new_single_key(vec![b"tree".to_vec()], b"key".to_vec());
    let proof = db
        .prove_query(&plain, None, grove_version)
        .unwrap()
        .expect("plain proof must generate");

    for path_query in all_read_mode_queries() {
        assert_not_supported(
            GroveDb::verify_query(&proof, &path_query, grove_version),
            "verify_query",
        );
        assert_not_supported(
            GroveDb::verify_query_raw(&proof, &path_query, grove_version),
            "verify_query_raw",
        );
        assert_not_supported(
            GroveDb::verify_subset_query(&proof, &path_query, grove_version),
            "verify_subset_query",
        );
    }
}

#[test]
fn merge_rejects_read_mode_queries() {
    let grove_version = GroveVersion::latest();
    let plain = PathQuery::new_single_key(vec![b"tree".to_vec()], b"key".to_vec());
    for path_query in all_read_mode_queries() {
        assert_not_supported(
            PathQuery::merge(vec![&plain, &path_query], grove_version),
            "merge",
        );
    }
    // The single-query short-circuit must also refuse: merging one
    // read-mode query "successfully" would hand callers a clone they
    // then feed to entry points expecting merged key selection.
    let axis = axis_path_query();
    assert_not_supported(
        PathQuery::merge(vec![&axis], grove_version),
        "merge(single)",
    );
}

#[test]
fn query_many_raw_rejects_read_mode_queries() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    let plain = PathQuery::new_single_key(vec![b"tree".to_vec()], b"key".to_vec());
    let axis = axis_path_query();
    assert_not_supported(
        db.query_many_raw(
            &[&plain, &axis],
            true,
            true,
            true,
            QueryResultType::QueryKeyElementPairResultType,
            None,
            grove_version,
        )
        .unwrap(),
        "query_many_raw",
    );
}
