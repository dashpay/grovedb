//! Per-instance query limits (`Query::limit`) — trusted-read engine
//! semantics, V1 proof round-trips, merge lifting, and the fail-closed
//! gates.
//!
//! A per-instance limit gives each execution instance of a query node a
//! fresh result budget for everything originating in that instance's
//! subtree ("top k per parent"), composing with the global
//! `SizedQuery::limit` by `min`. Served under `GROVE_V4`
//! (`path_query_methods.per_instance_query_limits`) on trusted reads
//! (`element.path_query_push` v1), V1 proofs, and merges
//! (`path_query_methods.merge` v1 lifts a merged input's global limit
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
    // The v1 engine reconciles descents by consumed budget, so an
    // empty-subtree charge two levels down reaches the global counter
    // — matching the
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
        .expect("exclusive-branch limits merge under GROVE_V4");
    let rows = assert_proved_matches_trusted_read(&db, &merged, grove_version);
    assert_eq!(rows, 5, "2 per docs parent plus the one other row");

    // A GLOBAL limit on an input is lifted to its branch's instance cap
    // — the merged read returns that input's first 3 rows plus the
    // other input's row.
    let mut globally_limited = top_k_query(50, None, None);
    globally_limited.query.limit = Some(3);
    let merged = PathQuery::merge(vec![&globally_limited, &other], grove_version)
        .expect("global limits lift under GROVE_V4");
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

#[test]
fn instance_cap_does_not_truncate_the_parent_walk_in_proofs() {
    // A parent layer's merk walk must not be truncated by an instance
    // cap: the cap budgets descendant ROWS, and an empty first child
    // consumes none of it, so a later populated child still owes rows.
    // With the cap wrongly applied to the parent walk, the proof
    // carried only `a_empty` and verified to zero rows while the
    // trusted read returned `b_full/k0`.
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
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
    db.insert(
        [DOCS].as_ref(),
        b"a_empty",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert empty child");
    db.insert(
        [DOCS].as_ref(),
        b"b_full",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert full child");
    db.insert(
        [DOCS, b"b_full".as_slice()].as_ref(),
        b"k0",
        Element::new_item(vec![0]),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert item");

    let mut root = Query::new_range_full();
    root.set_subquery(Query::new_range_full());
    root.limit = Some(1);
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(root, None, None));

    let rows = assert_proved_matches_trusted_read(&db, &path_query, grove_version);
    assert_eq!(rows, 1, "the populated later sibling still owes its row");
}

/// Builds `PathQuery` selecting `tree_key` under `path` and descending
/// with `inner` — the shape the non-Merk (MMR / BulkAppend / Dense)
/// proof layers are reached through.
fn non_merk_child_query(tree_key: &[u8], inner: Query) -> PathQuery {
    let mut root = Query::new_single_key(tree_key.to_vec());
    root.set_subquery(inner);
    PathQuery::new(vec![], SizedQuery::new(root, None, None))
}

#[test]
fn non_merk_children_honor_their_own_instance_caps_in_proofs() {
    // The non-Merk proof adapters bypass the recursive frame creation,
    // so the lower query's own `Query::limit` must be min-composed on
    // both sides; without it the verifier returned every selected row.
    let grove_version = GroveVersion::latest();
    use crate::tests::common::EMPTY_PATH;

    // Dense tree: 10 entries, child cap 3.
    let db = make_test_grovedb(grove_version);
    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(4),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");
    for i in 0..10u16 {
        db.dense_tree_insert(
            EMPTY_PATH,
            b"dense",
            format!("v_{i}").into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("dense insert");
    }
    let mut inner = Query::new();
    inner.insert_range_inclusive(0u16.to_be_bytes().to_vec()..=9u16.to_be_bytes().to_vec());
    inner.limit = Some(3);
    let path_query = non_merk_child_query(b"dense", inner);
    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("prove dense with child cap");
    let (_, result_set) =
        crate::GroveDb::verify_query(&proof, &path_query, grove_version).expect("verify");
    assert_eq!(result_set.len(), 3, "dense child cap must bound rows");

    // BulkAppendTree: 3 entries, child cap 1 (the reported repro).
    let db = make_test_grovedb(grove_version);
    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(2).expect("valid chunk power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk tree");
    for i in 0..3u8 {
        db.bulk_append(EMPTY_PATH, b"bulk", vec![i], None, grove_version)
            .unwrap()
            .expect("bulk append");
    }
    let mut inner = Query::new();
    inner.insert_range(0u64.to_be_bytes().to_vec()..3u64.to_be_bytes().to_vec());
    inner.limit = Some(1);
    let path_query = non_merk_child_query(b"bulk", inner);
    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("prove bulk with child cap");
    let (_, result_set) =
        crate::GroveDb::verify_query(&proof, &path_query, grove_version).expect("verify");
    assert_eq!(result_set.len(), 1, "bulk child cap must bound rows");

    // MmrTree: 3 leaves, child cap 1.
    let db = make_test_grovedb(grove_version);
    db.insert(
        EMPTY_PATH,
        b"mmr",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");
    for i in 0..3u8 {
        db.mmr_tree_append(EMPTY_PATH, b"mmr", vec![i], None, grove_version)
            .unwrap()
            .expect("mmr append");
    }
    let mut inner = Query::new();
    inner.insert_range_inclusive(0u64.to_be_bytes().to_vec()..=2u64.to_be_bytes().to_vec());
    inner.limit = Some(1);
    let path_query = non_merk_child_query(b"mmr", inner);
    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("prove mmr with child cap");
    let (_, result_set) =
        crate::GroveDb::verify_query(&proof, &path_query, grove_version).expect("verify");
    assert_eq!(result_set.len(), 1, "mmr child cap must bound rows");
}

#[test]
fn empty_bulk_child_range_with_active_cap_does_not_underflow() {
    // A bulk child query whose positions all sit at or past the stored
    // count clamps its range empty; with an active cap the layer's
    // accounting subtracted `0 - start` and panicked in debug builds.
    let grove_version = GroveVersion::latest();
    use crate::tests::common::EMPTY_PATH;
    let db = make_test_grovedb(grove_version);
    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(2).expect("valid chunk power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk tree");
    for i in 0..3u8 {
        db.bulk_append(EMPTY_PATH, b"bulk", vec![i], None, grove_version)
            .unwrap()
            .expect("bulk append");
    }
    let mut inner = Query::new();
    inner.insert_range(5u64.to_be_bytes().to_vec()..8u64.to_be_bytes().to_vec());
    let mut path_query = non_merk_child_query(b"bulk", inner);
    path_query.query.limit = Some(2);
    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("an empty child range must prove, not underflow");
    let (_, result_set) =
        crate::GroveDb::verify_query(&proof, &path_query, grove_version).expect("verify");
    assert!(result_set.is_empty(), "nothing stored in the range");
}

#[test]
fn merge_refuses_limited_branch_overlapping_the_root_selection() {
    // A grafted conditional overrides the merged root's default/
    // terminal semantics for its key — if a root-landing input already
    // selects that key, proceeding would silently drop its
    // contribution.
    let grove_version = GroveVersion::latest();

    let mut root_query = Query::new_single_key(DOCS.to_vec());
    root_query.set_subquery(Query::new_single_key(b"root-only".to_vec()));
    let at_root = PathQuery::new_unsized(vec![], root_query);

    let limited = PathQuery::new(
        vec![DOCS.to_vec()],
        SizedQuery::new(Query::new_single_key(b"limited".to_vec()), Some(1), None),
    );

    let result = PathQuery::merge(vec![&at_root, &limited], grove_version);
    assert!(
        matches!(&result, Err(Error::NotSupported(message)) if message.contains("collide")),
        "root-selection overlap must be a collision, got {result:?}"
    );
}

#[test]
fn merge_acceptance_is_independent_of_input_order() {
    // A disjoint limited branch must not change whether an unrelated
    // limit-free overlap merges: limit-free branches merge first (full
    // machinery), limited grafts come last, so every permutation
    // derives the same query.
    let grove_version = GroveVersion::latest();

    let mut root_query = Query::new_single_key(DOCS.to_vec());
    root_query.set_subquery(Query::new_single_key(b"root-only".to_vec()));
    let at_root = PathQuery::new_unsized(vec![], root_query);

    let overlapping = PathQuery::new_unsized(
        vec![DOCS.to_vec()],
        Query::new_single_key(b"also-docs".to_vec()),
    );

    let disjoint_limited = PathQuery::new(
        vec![b"other".to_vec()],
        SizedQuery::new(Query::new_single_key(b"x".to_vec()), Some(1), None),
    );

    let merged_a = PathQuery::merge(
        vec![&at_root, &overlapping, &disjoint_limited],
        grove_version,
    )
    .expect("limit-free overlap must merge regardless of the disjoint limited branch");
    let merged_b = PathQuery::merge(
        vec![&at_root, &disjoint_limited, &overlapping],
        grove_version,
    )
    .expect("the permutation must merge identically");
    assert_eq!(merged_a, merged_b, "permutations must derive one query");
}

#[test]
fn descending_child_caps_keep_the_last_positions() {
    // The specialized adapters must order rows by the query direction
    // before the cap applies: a descending cap keeps the LAST
    // positions, not the first ascending ones.
    let grove_version = GroveVersion::latest();
    use crate::tests::common::EMPTY_PATH;

    let make_desc_inner = |upper: u64| {
        let mut inner = Query::new_with_direction(false);
        inner.insert_range_inclusive(0u64.to_be_bytes().to_vec()..=upper.to_be_bytes().to_vec());
        inner.limit = Some(1);
        inner
    };

    // BulkAppendTree.
    let db = make_test_grovedb(grove_version);
    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(2).expect("valid chunk power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk tree");
    for i in 0..3u8 {
        db.bulk_append(EMPTY_PATH, b"bulk", vec![i], None, grove_version)
            .unwrap()
            .expect("bulk append");
    }
    let path_query = non_merk_child_query(b"bulk", make_desc_inner(2));
    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("prove");
    let (_, result_set) =
        crate::GroveDb::verify_query(&proof, &path_query, grove_version).expect("verify");
    assert_eq!(result_set.len(), 1);
    assert_eq!(
        result_set[0].1,
        2u64.to_be_bytes().to_vec(),
        "descending bulk cap must keep the last position"
    );

    // MmrTree.
    let db = make_test_grovedb(grove_version);
    db.insert(
        EMPTY_PATH,
        b"mmr",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");
    for i in 0..3u8 {
        db.mmr_tree_append(EMPTY_PATH, b"mmr", vec![i], None, grove_version)
            .unwrap()
            .expect("mmr append");
    }
    let path_query = non_merk_child_query(b"mmr", make_desc_inner(2));
    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("prove");
    let (_, result_set) =
        crate::GroveDb::verify_query(&proof, &path_query, grove_version).expect("verify");
    assert_eq!(result_set.len(), 1);
    assert_eq!(
        result_set[0].1,
        2u64.to_be_bytes().to_vec(),
        "descending mmr cap must keep the last leaf"
    );

    // DenseAppendOnlyFixedSizeTree (u16 position keys).
    let db = make_test_grovedb(grove_version);
    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(4),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");
    for i in 0..3u16 {
        db.dense_tree_insert(
            EMPTY_PATH,
            b"dense",
            format!("v_{i}").into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("dense insert");
    }
    let mut inner = Query::new_with_direction(false);
    inner.insert_range_inclusive(0u16.to_be_bytes().to_vec()..=2u16.to_be_bytes().to_vec());
    inner.limit = Some(1);
    let path_query = non_merk_child_query(b"dense", inner);
    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("prove");
    let (_, result_set) =
        crate::GroveDb::verify_query(&proof, &path_query, grove_version).expect("verify");
    assert_eq!(result_set.len(), 1);
    assert_eq!(
        result_set[0].1,
        2u16.to_be_bytes().to_vec(),
        "descending dense cap must keep the last position"
    );

    // CommitmentTree (delegates to the BulkAppend machinery).
    let db = make_test_grovedb(grove_version);
    db.insert(
        EMPTY_PATH,
        b"ct",
        Element::empty_commitment_tree(11).expect("valid chunk power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");
    for i in 0..3u8 {
        let mut cmx = [0u8; 32];
        cmx[0] = i;
        cmx[31] &= 0x7f;
        let mut rho = [0u8; 32];
        rho[0] = i;
        rho[1] = 0xB0;
        let mut cv_net = [0u8; 32];
        cv_net[0] = i;
        cv_net[1] = 0xCC;
        let mut enc_data = [0u8; 104];
        enc_data[0] = i;
        let mut epk = [0u8; 32];
        epk[0] = i;
        let mut out_ct = [0u8; 80];
        out_ct[0] = i;
        let ciphertext = grovedb_commitment_tree::TransmittedNoteCiphertext::<
            grovedb_commitment_tree::DashMemo,
        >::from_parts(
            epk,
            grovedb_commitment_tree::NoteBytesData(enc_data),
            out_ct,
        );
        db.commitment_tree_insert(
            EMPTY_PATH,
            b"ct",
            cmx,
            rho,
            cv_net,
            ciphertext,
            None,
            grove_version,
        )
        .unwrap()
        .expect("commitment tree insert");
    }
    let path_query = non_merk_child_query(b"ct", make_desc_inner(2));
    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("prove");
    let (_, result_set) =
        crate::GroveDb::verify_query(&proof, &path_query, grove_version).expect("verify");
    assert_eq!(result_set.len(), 1);
    assert_eq!(
        result_set[0].1,
        2u64.to_be_bytes().to_vec(),
        "descending commitment-tree cap must keep the last position"
    );
}

#[test]
fn sparse_bulk_queries_charge_matched_rows_not_the_span() {
    // Stored/requested positions 0 and 100 under a global limit of 3:
    // charging the bounding span exhausted the prover's budget and
    // omitted the later sibling's layer, which the verifier — charging
    // per matching row — then rejected as missing.
    let grove_version = GroveVersion::latest();
    use crate::tests::common::EMPTY_PATH;
    let db = make_test_grovedb(grove_version);
    db.insert(
        EMPTY_PATH,
        b"a_bulk",
        Element::empty_bulk_append_tree(2).expect("valid chunk power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk tree");
    for i in 0..101u8 {
        db.bulk_append(EMPTY_PATH, b"a_bulk", vec![i], None, grove_version)
            .unwrap()
            .expect("bulk append");
    }
    db.insert(
        EMPTY_PATH,
        b"z_tree",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert sibling tree");
    db.insert(
        [b"z_tree".as_slice()].as_ref(),
        b"row",
        Element::new_item(vec![9]),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert sibling row");

    let mut sparse = Query::new();
    sparse.insert_key(0u64.to_be_bytes().to_vec());
    sparse.insert_key(100u64.to_be_bytes().to_vec());
    // Conditional routing: the bulk child gets the sparse position
    // query; the sibling tree descends into its rows.
    let mut root = Query::new();
    root.insert_key(b"a_bulk".to_vec());
    root.insert_key(b"z_tree".to_vec());
    root.add_conditional_subquery(
        grovedb_query::QueryItem::Key(b"a_bulk".to_vec()),
        None,
        Some(sparse),
    );
    root.add_conditional_subquery(
        grovedb_query::QueryItem::Key(b"z_tree".to_vec()),
        None,
        Some(Query::new_range_full()),
    );
    let path_query = PathQuery::new(vec![], SizedQuery::new(root, Some(3), None));

    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("prove");
    let (_, result_set) = crate::GroveDb::verify_query(&proof, &path_query, grove_version)
        .expect("the honest proof must verify — budgets stay in sync");
    assert_eq!(
        result_set.len(),
        3,
        "two sparse bulk rows plus the sibling row"
    );
}

#[test]
fn empty_parents_are_reported_when_parent_inclusion_is_requested() {
    // With add_parent_tree_on_subquery, empty and non-empty matched
    // parents must behave alike: both parent rows are reported (parent
    // rows stay uncharged — the documented limitation on the flag).
    let grove_version = GroveVersion::latest();
    let db = make_test_grovedb(grove_version);
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
    db.insert(
        [DOCS].as_ref(),
        b"e_empty",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert empty child");
    db.insert(
        [DOCS].as_ref(),
        b"full",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert full child");
    db.insert(
        [DOCS, b"full".as_slice()].as_ref(),
        b"k0",
        Element::new_item(vec![0]),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert item");

    let mut root = Query::new_range_full();
    root.set_subquery(Query::new_range_full());
    root.add_parent_tree_on_subquery = true;
    let path_query = PathQuery::new(vec![DOCS.to_vec()], SizedQuery::new(root, None, None));

    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("prove");
    let (_, result_set) =
        crate::GroveDb::verify_query(&proof, &path_query, grove_version).expect("verify");
    let keys: Vec<&[u8]> = result_set
        .iter()
        .map(|(_, key, _)| key.as_slice())
        .collect();
    assert!(
        keys.contains(&b"e_empty".as_slice()),
        "the empty matched parent must be reported, got keys {keys:?}"
    );
    assert!(keys.contains(&b"full".as_slice()));
    assert!(keys.contains(&b"k0".as_slice()));
}

/// A limited input whose first key is already owned by a limit-free
/// graft descends into it and grafts where the two diverge: `docs/p1`
/// (every row) and `docs/p2` (two rows) share the `docs` key once an
/// unrelated input lifts the common path above it, and still prove,
/// verify and match the trusted read as one query.
#[test]
fn merge_grafts_a_limited_branch_below_a_key_a_limit_free_branch_owns() {
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

    let every_p1 =
        PathQuery::new_unsized(vec![DOCS.to_vec(), b"p1".to_vec()], Query::new_range_full());
    let two_of_p2 = PathQuery::new(
        vec![DOCS.to_vec(), b"p2".to_vec()],
        SizedQuery::new(Query::new_range_full(), Some(2), None),
    );
    let other = PathQuery::new_unsized(
        vec![b"other".to_vec()],
        Query::new_single_key(b"x".to_vec()),
    );

    let merged = PathQuery::merge(vec![&every_p1, &two_of_p2, &other], grove_version)
        .expect("a limited branch grafts below the key the limit-free branch owns");
    assert_eq!(merged.query.limit, None, "the merged query stays unsized");
    let rows = assert_proved_matches_trusted_read(&db, &merged, grove_version);
    assert_eq!(rows, 4 + 2 + 1, "all of p1, two of p2, the other row");

    // Input order does not decide acceptance: limit-free branches merge
    // first, so the descent sees the same owning branch either way.
    let permuted = PathQuery::merge(vec![&two_of_p2, &other, &every_p1], grove_version)
        .expect("the permutation merges identically");
    assert_eq!(merged, permuted);
}

/// Two limited inputs colliding on a key graft below it when their
/// paths diverge there — each keeps its own instance budget.
#[test]
fn merge_grafts_two_limited_branches_that_diverge_below_a_shared_key() {
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

    let one_of_p1 = PathQuery::new(
        vec![DOCS.to_vec(), b"p1".to_vec()],
        SizedQuery::new(Query::new_range_full(), Some(1), None),
    );
    let two_of_p2 = PathQuery::new(
        vec![DOCS.to_vec(), b"p2".to_vec()],
        SizedQuery::new(Query::new_range_full(), Some(2), None),
    );
    let other = PathQuery::new_unsized(
        vec![b"other".to_vec()],
        Query::new_single_key(b"x".to_vec()),
    );

    let merged = PathQuery::merge(vec![&one_of_p1, &two_of_p2, &other], grove_version)
        .expect("limited branches diverging below a shared key merge");
    let rows = assert_proved_matches_trusted_read(&db, &merged, grove_version);
    assert_eq!(rows, 1 + 2 + 1, "one of p1, two of p2, the other row");

    let permuted = PathQuery::merge(vec![&other, &two_of_p2, &one_of_p1], grove_version)
        .expect("the permutation merges identically");
    assert_eq!(merged, permuted);
}

/// The descent is not a licence to blend: two limited inputs on the
/// SAME full path meet at the descended root and are still refused,
/// and so is a limited input whose key the merged root selects by a
/// range.
#[test]
fn merge_still_refuses_limited_branches_that_never_diverge() {
    let grove_version = GroveVersion::latest();

    let one_of_p1 = PathQuery::new(
        vec![DOCS.to_vec(), b"p1".to_vec()],
        SizedQuery::new(Query::new_range_full(), Some(1), None),
    );
    let two_of_p1 = PathQuery::new(
        vec![DOCS.to_vec(), b"p1".to_vec()],
        SizedQuery::new(Query::new_range_full(), Some(2), None),
    );
    let other = PathQuery::new_unsized(
        vec![b"other".to_vec()],
        Query::new_single_key(b"x".to_vec()),
    );
    let result = PathQuery::merge(vec![&one_of_p1, &two_of_p1, &other], grove_version);
    assert!(
        matches!(&result, Err(Error::NotSupported(message)) if message.contains("collide")),
        "limited branches meeting at the same leaf must be refused, got {result:?}"
    );

    // A limit-free branch and a limited one meeting at the same leaf are
    // refused too: the limited body would have to merge with the other.
    let every_p1 =
        PathQuery::new_unsized(vec![DOCS.to_vec(), b"p1".to_vec()], Query::new_range_full());
    let result = PathQuery::merge(vec![&every_p1, &two_of_p1, &other], grove_version);
    assert!(
        matches!(&result, Err(Error::NotSupported(message)) if message.contains("collide")),
        "a limited body meeting a limit-free body must be refused, got {result:?}"
    );

    // The merged root selecting `docs` by a RANGE owns every key under
    // it at once; nothing below can be told apart, so no descent.
    let mut ranged_root = Query::new_range_full();
    ranged_root.set_subquery(Query::new_range_full());
    let at_root = PathQuery::new_unsized(vec![], ranged_root);
    let result = PathQuery::merge(vec![&at_root, &two_of_p1], grove_version);
    assert!(
        matches!(&result, Err(Error::NotSupported(message)) if message.contains("collide")),
        "a range-selected key cannot host a limited graft, got {result:?}"
    );
}
