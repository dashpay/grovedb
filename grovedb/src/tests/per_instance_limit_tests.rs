//! Per-instance query limits (`Query::limit`) — trusted-read engine
//! semantics, V1 proof round-trips, merge lifting, and the fail-closed
//! gates.
//!
//! A per-instance limit gives each execution instance of a query node a
//! fresh result budget for everything originating in that instance's
//! subtree ("top k per parent"), composing with the global
//! `SizedQuery::limit` by `min`. Served under `GROVE_V4`
//! (`path_query_methods.per_instance_query_limits`) on trusted reads
//! (`element.path_query_push` v2), V1 proofs, and merges
//! (`path_query_methods.merge` v2 lifts a merged input's global limit
//! onto its exclusive branch). Everything else — older grove versions,
//! V0 proofs, absence-proof assembly, colliding merges,
//! read-mode/aggregate/count-offset shapes, terminal-keys projections —
//! fails closed.

use grovedb_version::version::GroveVersion;

use crate::{
    query_result_type::QueryResultType,
    tests::{make_empty_grovedb, make_test_grovedb, TempGroveDb},
    Element, Error, PathQuery, Query, SizedQuery,
};

const DOCS: &[u8] = b"docs";

/// Builds `docs/<parent>/<k0..kN>` — one subtree per parent, each with
/// `items_per_parent` items keyed `k0`, `k1`, … holding their index.
fn populate_parents(
    db: &TempGroveDb,
    parents: &[&[u8]],
    items_per_parent: u8,
    grove_version: &GroveVersion,
) {
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
    for parent in parents {
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
        for i in 0..items_per_parent {
            db.insert(
                [DOCS, parent].as_ref(),
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

/// A `docs`-rooted range-full query whose default subquery selects every
/// child, capped at `per_parent` rows per parent instance.
fn top_k_query(per_parent: u16, global: Option<u16>, offset: Option<u16>) -> PathQuery {
    let mut sub = Query::new_range_full();
    sub.limit = Some(per_parent);
    let mut query = Query::new_range_full();
    query.set_subquery(sub);
    PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(query, global, offset))
}

fn run(
    db: &TempGroveDb,
    path_query: &PathQuery,
    grove_version: &GroveVersion,
) -> (Vec<(Vec<Vec<u8>>, Vec<u8>)>, u16) {
    let (elements, skipped) = db
        .query_raw(
            path_query,
            true,
            true,
            true,
            QueryResultType::QueryPathKeyElementTrioResultType,
            None,
            grove_version,
        )
        .unwrap()
        .expect("query_raw should succeed");
    (
        elements
            .to_path_key_elements()
            .into_iter()
            .map(|(path, key, _)| (path, key))
            .collect(),
        skipped,
    )
}

fn path_key(parent: &[u8], key: &[u8]) -> (Vec<Vec<u8>>, Vec<u8>) {
    (vec![DOCS.to_vec(), parent.to_vec()], key.to_vec())
}

#[test]
fn top_k_per_parent_returns_k_rows_under_each_parent() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate_parents(&db, &[b"p1", b"p2", b"p3"], 5, grove_version);

    let (rows, skipped) = run(&db, &top_k_query(2, None, None), grove_version);
    assert_eq!(skipped, 0);
    assert_eq!(
        rows,
        vec![
            path_key(b"p1", b"k0"),
            path_key(b"p1", b"k1"),
            path_key(b"p2", b"k0"),
            path_key(b"p2", b"k1"),
            path_key(b"p3", b"k0"),
            path_key(b"p3", b"k1"),
        ],
        "each parent instance gets a fresh budget of 2"
    );
}

#[test]
fn global_limit_still_caps_the_total() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate_parents(&db, &[b"p1", b"p2", b"p3"], 5, grove_version);

    let (rows, _) = run(&db, &top_k_query(2, Some(5), None), grove_version);
    assert_eq!(
        rows,
        vec![
            path_key(b"p1", b"k0"),
            path_key(b"p1", b"k1"),
            path_key(b"p2", b"k0"),
            path_key(b"p2", b"k1"),
            path_key(b"p3", b"k0"),
        ],
        "the global budget runs out mid-way through the last parent"
    );
}

#[test]
fn root_query_limit_is_the_global_limit_and_min_composes() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate_parents(&db, &[b"p1"], 5, grove_version);

    // The root query node executes exactly once, so its Query::limit is
    // equivalent to SizedQuery::limit.
    let mut sub = Query::new_range_full();
    sub.limit = Some(4);
    let mut query = Query::new_range_full();
    query.set_subquery(sub.clone());
    query.limit = Some(4);
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(query, None, None));
    let (rows, _) = run(&db, &path_query, grove_version);
    assert_eq!(rows.len(), 4);

    // Setting both means the smaller wins.
    let mut query = Query::new_range_full();
    query.set_subquery(sub);
    query.limit = Some(4);
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(query, Some(3), None));
    let (rows, _) = run(&db, &path_query, grove_version);
    assert_eq!(rows.len(), 3);
}

#[test]
fn conditional_branches_carry_their_own_instance_caps() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate_parents(&db, &[b"p1", b"p2", b"p3"], 5, grove_version);

    // p1 gets a cap of 1 via a conditional branch; everything else gets
    // the default branch's cap of 2.
    let mut conditional_sub = Query::new_range_full();
    conditional_sub.limit = Some(1);
    let mut default_sub = Query::new_range_full();
    default_sub.limit = Some(2);
    let mut query = Query::new_range_full();
    query.set_subquery(default_sub);
    query.add_conditional_subquery(
        grovedb_query::QueryItem::Key(b"p1".to_vec()),
        None,
        Some(conditional_sub),
    );
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(query, None, None));

    let (rows, _) = run(&db, &path_query, grove_version);
    assert_eq!(
        rows,
        vec![
            path_key(b"p1", b"k0"),
            path_key(b"p2", b"k0"),
            path_key(b"p2", b"k1"),
            path_key(b"p3", b"k0"),
            path_key(b"p3", b"k1"),
        ]
    );
}

#[test]
fn ancestor_caps_bound_everything_below_them() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    use crate::tests::common::EMPTY_PATH;

    // docs/<group>/<child>/<items…>: groups capped at 3 rows each, every
    // child capped at 2 — each group has 3 children with 2 items each,
    // so the group cap (3) binds before the child caps (3 × 2 = 6).
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
    for group in [b"g1", b"g2"] {
        db.insert(
            [DOCS].as_ref(),
            group,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert group");
        for child in [b"c1", b"c2", b"c3"] {
            db.insert(
                [DOCS, group.as_slice()].as_ref(),
                child,
                Element::empty_tree(),
                None,
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert child");
            for i in 0..2u8 {
                db.insert(
                    [DOCS, group.as_slice(), child.as_slice()].as_ref(),
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

    let mut leaf_query = Query::new_range_full();
    leaf_query.limit = Some(2);
    let mut group_query = Query::new_range_full();
    group_query.set_subquery(leaf_query);
    group_query.limit = Some(3);
    let mut root_query = Query::new_range_full();
    root_query.set_subquery(group_query);
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(root_query, None, None));

    let (rows, _) = run(&db, &path_query, grove_version);
    let expected: Vec<_> = [
        (b"g1", b"c1", b"k0"),
        (b"g1", b"c1", b"k1"),
        (b"g1", b"c2", b"k0"),
        (b"g2", b"c1", b"k0"),
        (b"g2", b"c1", b"k1"),
        (b"g2", b"c2", b"k0"),
    ]
    .into_iter()
    .map(|(group, child, key)| {
        (
            vec![DOCS.to_vec(), group.to_vec(), child.to_vec()],
            key.to_vec(),
        )
    })
    .collect();
    assert_eq!(
        rows, expected,
        "each group instance stops at 3 rows even though its children allow 6"
    );
}

#[test]
fn right_to_left_takes_the_last_k_per_parent() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate_parents(&db, &[b"p1", b"p2"], 4, grove_version);

    let mut sub = Query::new_with_direction(false);
    sub.insert_all();
    sub.limit = Some(2);
    let mut query = Query::new_with_direction(false);
    query.insert_all();
    query.set_subquery(sub);
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(query, None, None));

    let (rows, _) = run(&db, &path_query, grove_version);
    assert_eq!(
        rows,
        vec![
            path_key(b"p2", b"k3"),
            path_key(b"p2", b"k2"),
            path_key(b"p1", b"k3"),
            path_key(b"p1", b"k2"),
        ]
    );
}

#[test]
fn offset_skips_do_not_consume_instance_budgets() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate_parents(&db, &[b"p1", b"p2"], 5, grove_version);

    // The global offset skips p1's first row; p1's instance budget of 2
    // is untouched by the skip, so p1 still contributes 2 rows (k1, k2).
    let (rows, skipped) = run(&db, &top_k_query(2, None, Some(1)), grove_version);
    assert_eq!(skipped, 1);
    assert_eq!(
        rows,
        vec![
            path_key(b"p1", b"k1"),
            path_key(b"p1", b"k2"),
            path_key(b"p2", b"k0"),
            path_key(b"p2", b"k1"),
        ]
    );
}

/// Builds `docs/<mid>/…` where each `mid` subtree contains one empty
/// child subtree (an empty-subtree charge under
/// `decrease_limit_on_range_with_no_sub_elements`) and one child with
/// two items. `empty_first` picks whether the empty child sorts before
/// or after the item-bearing one — which side of a budget the charge
/// lands on depends on walk order.
fn populate_mixed_mid_level(db: &TempGroveDb, empty_first: bool, grove_version: &GroveVersion) {
    use crate::tests::common::EMPTY_PATH;
    let (empty_key, full_key): (&[u8], &[u8]) = if empty_first {
        (b"a_empty", b"b_full")
    } else {
        (b"z_empty", b"a_full")
    };
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
    for mid in [b"m1", b"m2"] {
        db.insert(
            [DOCS].as_ref(),
            mid,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert mid");
        db.insert(
            [DOCS, mid.as_slice()].as_ref(),
            empty_key,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert empty child");
        db.insert(
            [DOCS, mid.as_slice()].as_ref(),
            full_key,
            Element::empty_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert full child");
        for i in 0..2u8 {
            db.insert(
                [DOCS, mid.as_slice(), full_key].as_ref(),
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

/// Three-level query: docs → mid (range full) → children (range full)
/// → items (range full).
fn three_level_query(global: Option<u16>) -> PathQuery {
    let leaf = Query::new_range_full();
    let mut mid = Query::new_range_full();
    mid.set_subquery(leaf);
    let mut root = Query::new_range_full();
    root.set_subquery(mid);
    PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(root, global, None))
}

#[test]
fn nested_empty_subtree_charges_reach_the_global_budget() {
    // v2 reconciles descents by consumed budget, so an empty-subtree
    // charge two levels down reaches the global counter — matching the
    // prover's shared-counter accounting. m1 consumes 3 (two rows from
    // a_full, then one charge for z_empty), so a global budget of 3 is
    // exhausted before m2 is walked.
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate_mixed_mid_level(&db, false, grove_version);

    let (rows, _) = run(&db, &three_level_query(Some(3)), grove_version);
    assert_eq!(
        rows,
        vec![
            (
                vec![DOCS.to_vec(), b"m1".to_vec(), b"a_full".to_vec()],
                b"k0".to_vec()
            ),
            (
                vec![DOCS.to_vec(), b"m1".to_vec(), b"a_full".to_vec()],
                b"k1".to_vec()
            ),
        ],
        "m1's empty-subtree charge consumes the third global slot"
    );

    // GROVE_V3 (path_query_push v0) keeps the legacy rows-only
    // reconciliation: m1's nested charge is lost at the root, so m2 is
    // still walked with one slot left and contributes a third row.
    let legacy_version = &grovedb_version::version::GROVE_VERSIONS[2];
    assert_eq!(legacy_version.protocol_version, 3);
    let db = make_test_grovedb(legacy_version);
    populate_mixed_mid_level(&db, false, legacy_version);
    let (rows, _) = run(&db, &three_level_query(Some(3)), legacy_version);
    assert_eq!(rows.len(), 3, "legacy accounting still walks into m2");
}

#[test]
fn empty_subtree_charges_spare_instance_budgets_by_default() {
    // m-level instances capped at 2: the empty-subtree charge for
    // a_empty consumes global budget but NOT the instance budget, so
    // each mid still returns both of b_full's items.
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate_mixed_mid_level(&db, true, grove_version);

    let leaf = Query::new_range_full();
    let mut mid = Query::new_range_full();
    mid.set_subquery(leaf);
    mid.limit = Some(2);
    let mut root = Query::new_range_full();
    root.set_subquery(mid);
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(root, None, None));

    let (rows, _) = run(&db, &path_query, grove_version);
    assert_eq!(
        rows.len(),
        4,
        "instance caps count result rows only: 2 rows from each mid"
    );

    // Opting in via
    // `decrease_instance_limits_on_range_with_no_sub_elements` makes the
    // charge consume the instance budget too: each mid burns one slot on
    // a_empty and has one left for b_full's first item.
    use crate::element::{query::ElementQueryExtensions, query_options::QueryOptions};
    let (elements, _) = Element::get_path_query(
        &db.db,
        &path_query,
        QueryOptions {
            allow_get_raw: true,
            allow_cache: true,
            decrease_limit_on_range_with_no_sub_elements: true,
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
        2,
        "opted-in: each mid's empty-subtree charge eats one instance slot"
    );
}

// ---------------------------------------------------------------------
// Fail-closed gates
// ---------------------------------------------------------------------

#[test]
fn pre_v4_grove_versions_fail_closed() {
    let legacy_version = &grovedb_version::version::GROVE_VERSIONS[2];
    assert_eq!(legacy_version.protocol_version, 3);
    let db = make_test_grovedb(legacy_version);
    populate_parents(&db, &[b"p1"], 3, legacy_version);

    let result = db.query_raw(
        &top_k_query(2, None, None),
        true,
        true,
        true,
        QueryResultType::QueryPathKeyElementTrioResultType,
        None,
        legacy_version,
    );
    assert!(
        matches!(result.unwrap(), Err(Error::NotSupported(_))),
        "GROVE_V3 must reject per-instance limits, not ignore them"
    );
}

#[test]
fn zero_instance_limits_are_rejected() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate_parents(&db, &[b"p1"], 3, grove_version);

    let result = db.query_raw(
        &top_k_query(0, None, None),
        true,
        true,
        true,
        QueryResultType::QueryPathKeyElementTrioResultType,
        None,
        grove_version,
    );
    assert!(
        matches!(result.unwrap(), Err(Error::InvalidQuery(_))),
        "Query::limit of 0 is malformed"
    );
}

/// Prove `path_query`, verify it, and require the verified result set
/// to equal the trusted read's — the same differential oracle the
/// coverage proof tests use, applied to instance-capped queries.
fn assert_proved_matches_trusted_read(
    db: &TempGroveDb,
    path_query: &PathQuery,
    grove_version: &GroveVersion,
) -> usize {
    let proof = db
        .prove_query(path_query, None, grove_version)
        .unwrap()
        .expect("proving should succeed");
    let (proved_root, proved_result) =
        crate::GroveDb::verify_query(&proof, path_query, grove_version)
            .expect("verifying should succeed");
    assert_eq!(
        proved_root,
        db.root_hash(None, grove_version).unwrap().unwrap(),
        "verified root must be the database root"
    );

    let (trusted, _) = run(db, path_query, grove_version);
    let proved_rows: Vec<(Vec<Vec<u8>>, Vec<u8>)> = proved_result
        .iter()
        .map(|(path, key, _)| (path.clone(), key.clone()))
        .collect();
    assert_eq!(
        proved_rows, trusted,
        "proved result set must match the trusted read"
    );
    assert!(
        proved_result
            .iter()
            .all(|(_, _, element)| element.is_some()),
        "every proved row should carry its element"
    );
    proved_result.len()
}

#[test]
fn proofs_serve_top_k_per_parent() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate_parents(&db, &[b"p1", b"p2", b"p3"], 5, grove_version);

    // Top 2 per parent, unlimited globally: 6 rows.
    let rows = assert_proved_matches_trusted_read(&db, &top_k_query(2, None, None), grove_version);
    assert_eq!(rows, 6);

    // Global cap composes: 5 rows.
    let rows =
        assert_proved_matches_trusted_read(&db, &top_k_query(2, Some(5), None), grove_version);
    assert_eq!(rows, 5);

    // Instance caps wider than the data: everything comes back.
    let rows = assert_proved_matches_trusted_read(&db, &top_k_query(50, None, None), grove_version);
    assert_eq!(rows, 15);
}

#[test]
fn proofs_serve_conditional_branch_caps() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate_parents(&db, &[b"p1", b"p2", b"p3"], 5, grove_version);

    let mut conditional_sub = Query::new_range_full();
    conditional_sub.limit = Some(1);
    let mut default_sub = Query::new_range_full();
    default_sub.limit = Some(2);
    let mut query = Query::new_range_full();
    query.set_subquery(default_sub);
    query.add_conditional_subquery(
        grovedb_query::QueryItem::Key(b"p1".to_vec()),
        None,
        Some(conditional_sub),
    );
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(query, None, None));

    let rows = assert_proved_matches_trusted_read(&db, &path_query, grove_version);
    assert_eq!(rows, 5, "1 from p1 plus 2 each from p2 and p3");
}

#[test]
fn proofs_serve_ancestor_caps_across_levels() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate_mixed_mid_level(&db, false, grove_version);

    // Mid-level instances capped at 1: each mid contributes only the
    // first row of its full child.
    let leaf = Query::new_range_full();
    let mut mid = Query::new_range_full();
    mid.set_subquery(leaf);
    mid.limit = Some(1);
    let mut root = Query::new_range_full();
    root.set_subquery(mid);
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(root, None, None));

    let rows = assert_proved_matches_trusted_read(&db, &path_query, grove_version);
    assert_eq!(rows, 2);
}

#[test]
fn proofs_serve_reverse_direction_instance_caps() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate_parents(&db, &[b"p1", b"p2"], 4, grove_version);

    let mut sub = Query::new_with_direction(false);
    sub.insert_all();
    sub.limit = Some(2);
    let mut query = Query::new_with_direction(false);
    query.insert_all();
    query.set_subquery(sub);
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(query, None, None));

    let rows = assert_proved_matches_trusted_read(&db, &path_query, grove_version);
    assert_eq!(rows, 4, "last 2 of each parent, in reverse order");
}

#[test]
fn verifying_with_a_different_instance_cap_than_proved_is_rejected() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate_parents(&db, &[b"p1", b"p2"], 5, grove_version);

    // A proof built for wider caps carries more rows than the tighter
    // query's budget admits — the per-layer over-delivery check must
    // reject it rather than silently truncate.
    let wide_proof = db
        .prove_query(&top_k_query(4, None, None), None, grove_version)
        .unwrap()
        .expect("proving should succeed");
    let result =
        crate::GroveDb::verify_query(&wide_proof, &top_k_query(2, None, None), grove_version);
    assert!(
        result.is_err(),
        "a wide-cap proof must not verify under a tighter cap"
    );
}

#[test]
fn absence_proof_verification_still_rejects_instance_limits() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate_parents(&db, &[b"p1"], 3, grove_version);

    // Which keys an instance-capped walk returns is data-dependent, so
    // the absence projection would report keys beyond a cap as absent.
    let mut limited = top_k_query(2, None, None);
    limited.query.limit = Some(10); // absence mode requires a global limit
    let proof = db
        .prove_query(&limited, None, grove_version)
        .unwrap()
        .expect("proving should succeed");
    let result = crate::GroveDb::verify_query_with_absence_proof(&proof, &limited, grove_version);
    assert!(
        matches!(result, Err(Error::NotSupported(_))),
        "absence-proof verification must reject per-instance limits"
    );
}

#[test]
fn pre_v4_grove_versions_reject_instance_limit_proofs() {
    let legacy_version = &grovedb_version::version::GROVE_VERSIONS[2];
    assert_eq!(legacy_version.protocol_version, 3);
    let db = make_test_grovedb(legacy_version);
    populate_parents(&db, &[b"p1"], 3, legacy_version);

    let result = db.prove_query(&top_k_query(2, None, None), None, legacy_version);
    assert!(
        matches!(result.unwrap(), Err(Error::NotSupported(_))),
        "GROVE_V3 must reject proving per-instance limits"
    );
}

#[test]
fn merge_lifts_limits_onto_exclusive_branches_and_round_trips() {
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
    populate_parents(&db, &[b"p1", b"p2"], 4, grove_version);
    use crate::tests::common::EMPTY_PATH;
    db.insert(
        EMPTY_PATH,
        b"other",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert other tree");
    db.insert(
        [b"other".as_slice()].as_ref(),
        b"x",
        Element::new_item(vec![9]),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert other item");

    // An instance-capped input and an unrelated plain input merge onto
    // exclusive branches; the merged query proves, verifies, and
    // matches the trusted read.
    let limited = top_k_query(2, None, None);
    let other = PathQuery::new_unsized(
        vec![b"other".to_vec()],
        Query::new_single_key(b"x".to_vec()),
    );
    let merged = PathQuery::merge(vec![&limited, &other], grove_version)
        .expect("exclusive-branch limits merge from v2");
    let rows = assert_proved_matches_trusted_read(&db, &merged, grove_version);
    assert_eq!(rows, 5, "2 per docs parent plus the one other row");

    // A GLOBAL limit on an input is lifted to its branch's instance cap
    // — the merged read returns that input's first 3 rows plus the
    // other input's row.
    let mut globally_limited = top_k_query(50, None, None);
    globally_limited.query.limit = Some(3);
    let merged = PathQuery::merge(vec![&globally_limited, &other], grove_version)
        .expect("global limits lift from v2");
    assert_eq!(merged.query.limit, None, "the merged query stays unsized");
    let rows = assert_proved_matches_trusted_read(&db, &merged, grove_version);
    assert_eq!(rows, 4, "3 lifted rows from docs plus the other row");
}

#[test]
fn merge_still_refuses_colliding_and_root_landing_limits() {
    let grove_version = GroveVersion::latest();

    // Two limited inputs whose branches collide at the same first key
    // (the third, unrelated input keeps the common path above them, so
    // both land as sub-level branches under `docs`).
    let colliding_a = top_k_query(2, None, None);
    let colliding_b = top_k_query(3, None, None);
    let other = PathQuery::new_unsized(
        vec![b"other".to_vec()],
        Query::new_single_key(b"x".to_vec()),
    );
    let result = PathQuery::merge(vec![&colliding_a, &colliding_b, &other], grove_version);
    assert!(
        matches!(&result, Err(Error::NotSupported(message)) if message.contains("collide")),
        "colliding limited branches must be refused, got {result:?}"
    );

    // A limited input whose whole path is the common path lands at the
    // merged root, where budgets cannot blend.
    let at_root = PathQuery::new(
        vec![DOCS.to_vec()],
        SizedQuery::new(Query::new_range_full(), Some(2), None),
    );
    let deeper =
        PathQuery::new_unsized(vec![DOCS.to_vec(), b"p1".to_vec()], Query::new_range_full());
    let result = PathQuery::merge(vec![&at_root, &deeper], grove_version);
    assert!(
        matches!(&result, Err(Error::NotSupported(message)) if message.contains("merged root")),
        "root-landing limited inputs must be refused, got {result:?}"
    );
}

#[test]
fn make_empty_grovedb_smoke_for_instance_limit_defaults() {
    // A plain query without instance limits stays served everywhere —
    // the gates must not over-trigger.
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    populate_parents(&db, &[b"p1"], 2, grove_version);
    let sub = Query::new_range_full();
    let mut query = Query::new_range_full();
    query.set_subquery(sub);
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(query, Some(1), None));
    let (rows, _) = run(&db, &path_query, grove_version);
    assert_eq!(rows.len(), 1);
    db.prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("plain queries still prove");
}
