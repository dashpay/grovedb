//! An empty plain `Tree` that is itself a result row takes a slot of the
//! query's budget whether or not the verifier reports it.
//!
//! The trusted read returns such a tree as a row and counts it against the
//! limit, and the V1 prover charges it the same way. A verifier that
//! excludes empty trees from its results (`include_empty_trees_in_result`
//! false, the default of `verify_query` and `verify_subset_query`) used to
//! bind the row and charge nothing, so its budget ran one slot ahead of the
//! prover's for every such row. Once the prover's budget was spent, the
//! verifier went on expecting what the prover had rightly left out: an
//! honest proof was refused, or a row past the page was reported.
//!
//! From `GROVE_V5` (`operations.proof.unreported_empty_tree_row_charge` 1)
//! the verifier charges the row it does not report. `GROVE_V4` keeps the
//! released count, which these tests pin.

use grovedb_merk::proofs::query::{QueryItem, VerifyOptions};
use grovedb_version::version::{v4::GROVE_V4, v5::GROVE_V5, GroveVersion};

use crate::{
    query_result_type::QueryResultType,
    tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
    Element, Error, GroveDb, PathQuery, Query, SizedQuery,
};

const INDEX: &[u8] = b"index";

type PathKey = (Vec<Vec<u8>>, Vec<u8>);

/// What the second child of `index` is.
#[derive(Clone, Copy)]
enum Second {
    /// `index/b/{d: item}`: a tree the subquery descends into.
    PopulatedTree,
    /// `index/b: item`: a row of the `index` layer itself.
    Item,
}

/// `index/a/{d: item, t: empty tree}` and a second child `index/b`.
fn fixture(second: Second, grove_version: &GroveVersion) -> TempGroveDb {
    let db = make_test_grovedb(grove_version);
    let insert = |path: &[&[u8]], key: &[u8], element: Element| {
        db.insert(path, key, element, None, None, grove_version)
            .unwrap()
            .expect("insert");
    };
    insert(&[TEST_LEAF], INDEX, Element::empty_tree());
    insert(&[TEST_LEAF, INDEX], b"a", Element::empty_tree());
    insert(&[TEST_LEAF, INDEX, b"a"], b"d", Element::new_item(vec![1]));
    insert(&[TEST_LEAF, INDEX, b"a"], b"t", Element::empty_tree());
    match second {
        Second::PopulatedTree => {
            insert(&[TEST_LEAF, INDEX], b"b", Element::empty_tree());
            insert(&[TEST_LEAF, INDEX, b"b"], b"d", Element::new_item(vec![2]));
        }
        Second::Item => insert(&[TEST_LEAF, INDEX], b"b", Element::new_item(vec![2])),
    }
    db
}

/// Every child of `index` and everything below it, under a global limit
/// and/or a cap on the `index` query instance.
fn page_query(global: Option<u16>, instance_cap: Option<u16>) -> PathQuery {
    let mut query = Query::new_range_full();
    query.set_subquery(Query::new_range_full());
    query.limit = instance_cap;
    PathQuery::new(
        vec![TEST_LEAF.to_vec(), INDEX.to_vec()],
        SizedQuery::new(query, global, None),
    )
}

fn under(parent: &[u8], key: &[u8]) -> PathKey {
    (
        vec![TEST_LEAF.to_vec(), INDEX.to_vec(), parent.to_vec()],
        key.to_vec(),
    )
}

fn in_index(key: &[u8]) -> PathKey {
    (vec![TEST_LEAF.to_vec(), INDEX.to_vec()], key.to_vec())
}

fn trusted_rows(
    db: &TempGroveDb,
    path_query: &PathQuery,
    grove_version: &GroveVersion,
) -> Vec<PathKey> {
    db.query_raw(
        path_query,
        true,
        true,
        true,
        QueryResultType::QueryPathKeyElementTrioResultType,
        None,
        grove_version,
    )
    .unwrap()
    .expect("trusted read")
    .0
    .to_path_key_elements()
    .into_iter()
    .map(|(path, key, _)| (path, key))
    .collect()
}

fn prove(db: &TempGroveDb, path_query: &PathQuery, grove_version: &GroveVersion) -> Vec<u8> {
    db.prove_query(path_query, None, grove_version)
        .unwrap()
        .expect("prove")
}

/// `verify_query`: empty trees excluded, succinctness checked.
fn verified_rows(
    db: &TempGroveDb,
    proof: &[u8],
    path_query: &PathQuery,
    grove_version: &GroveVersion,
) -> Result<Vec<PathKey>, Error> {
    let (root_hash, rows) = GroveDb::verify_query(proof, path_query, grove_version)?;
    assert_eq!(
        root_hash,
        db.root_hash(None, grove_version).unwrap().expect("root"),
        "the proof must verify against the database's root"
    );
    Ok(rows.into_iter().map(|(path, key, _)| (path, key)).collect())
}

/// The same proof read by a verifier that reports empty trees.
fn verified_rows_with_empty_trees(
    proof: &[u8],
    path_query: &PathQuery,
    grove_version: &GroveVersion,
) -> Vec<PathKey> {
    GroveDb::verify_query_with_options(
        proof,
        path_query,
        VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: true,
            include_empty_trees_in_result: true,
        },
        grove_version,
    )
    .expect("a verifier that reports empty trees counts as the prover does")
    .1
    .into_iter()
    .map(|(path, key, _)| (path, key))
    .collect()
}

fn assert_missing_lower_layer_for_b(result: Result<Vec<PathKey>, Error>) {
    match result {
        Err(Error::InvalidProof(_, message)) => assert_eq!(
            message, "V1 proof is missing lower layer for non-empty tree at key 62",
            "GROVE_V4 must refuse the page exactly as GroveDB 6.1.1 does"
        ),
        other => panic!("expected the released GROVE_V4 refusal, got {other:?}"),
    }
}

/// The page both budgets describe: `a/d` and the empty tree `a/t`.
fn first_page() -> Vec<PathKey> {
    vec![under(b"a", b"d"), under(b"a", b"t")]
}

#[test]
fn a_global_limit_spent_on_an_empty_tree_row_verifies_under_v5() {
    let grove_version = &GROVE_V5;
    let db = fixture(Second::PopulatedTree, grove_version);
    let path_query = page_query(Some(2), None);

    assert_eq!(trusted_rows(&db, &path_query, grove_version), first_page());

    let proof = prove(&db, &path_query, grove_version);
    assert_eq!(
        verified_rows(&db, &proof, &path_query, grove_version)
            .expect("the honest proof of the page must verify"),
        vec![under(b"a", b"d")],
        "the empty tree takes its slot and is left out of the rows"
    );
    assert_eq!(
        verified_rows_with_empty_trees(&proof, &path_query, grove_version),
        first_page()
    );
}

#[test]
fn a_global_limit_spent_on_an_empty_tree_row_is_refused_under_v4_as_released() {
    let grove_version = &GROVE_V4;
    let db = fixture(Second::PopulatedTree, grove_version);
    let path_query = page_query(Some(2), None);

    assert_eq!(trusted_rows(&db, &path_query, grove_version), first_page());

    let proof = prove(&db, &path_query, grove_version);
    assert_missing_lower_layer_for_b(verified_rows(&db, &proof, &path_query, grove_version));
    assert_eq!(
        verified_rows_with_empty_trees(&proof, &path_query, grove_version),
        first_page()
    );
}

#[test]
fn an_instance_cap_spent_on_an_empty_tree_row_verifies_under_v5() {
    let grove_version = &GROVE_V5;
    let db = fixture(Second::PopulatedTree, grove_version);
    let path_query = page_query(None, Some(2));

    assert_eq!(trusted_rows(&db, &path_query, grove_version), first_page());

    let proof = prove(&db, &path_query, grove_version);
    assert_eq!(
        verified_rows(&db, &proof, &path_query, grove_version)
            .expect("the honest proof of the capped page must verify"),
        vec![under(b"a", b"d")]
    );
    assert_eq!(
        verified_rows_with_empty_trees(&proof, &path_query, grove_version),
        first_page()
    );
}

#[test]
fn an_instance_cap_spent_on_an_empty_tree_row_is_refused_under_v4_as_released() {
    let grove_version = &GROVE_V4;
    let db = fixture(Second::PopulatedTree, grove_version);
    let path_query = page_query(None, Some(2));

    assert_eq!(trusted_rows(&db, &path_query, grove_version), first_page());

    let proof = prove(&db, &path_query, grove_version);
    assert_missing_lower_layer_for_b(verified_rows(&db, &proof, &path_query, grove_version));
    assert_eq!(
        verified_rows_with_empty_trees(&proof, &path_query, grove_version),
        first_page()
    );
}

/// When the row after the spent budget is an item of the layer itself, the
/// released verifier does not refuse the proof: it reports the item, a row
/// the trusted read leaves for the next page.
#[test]
fn a_row_past_the_spent_budget_is_not_reported_under_v5() {
    let grove_version = &GROVE_V5;
    let db = fixture(Second::Item, grove_version);
    let path_query = page_query(Some(2), None);

    assert_eq!(trusted_rows(&db, &path_query, grove_version), first_page());

    let proof = prove(&db, &path_query, grove_version);
    assert_eq!(
        verified_rows(&db, &proof, &path_query, grove_version).expect("verifies"),
        vec![under(b"a", b"d")],
        "`b` is past the page the trusted read returns"
    );
}

#[test]
fn a_row_past_the_spent_budget_is_reported_under_v4_as_released() {
    let grove_version = &GROVE_V4;
    let db = fixture(Second::Item, grove_version);
    let path_query = page_query(Some(2), None);

    assert_eq!(trusted_rows(&db, &path_query, grove_version), first_page());

    let proof = prove(&db, &path_query, grove_version);
    assert_eq!(
        verified_rows(&db, &proof, &path_query, grove_version).expect("verifies"),
        vec![under(b"a", b"d"), in_index(b"b")],
        "GROVE_V4 keeps reporting the row past the page, as GroveDB 6.1.1 does"
    );
}

/// A budget the empty tree does not exhaust changes nothing: both versions
/// return every other row.
#[test]
fn a_budget_with_room_left_returns_the_same_rows_under_both_versions() {
    for grove_version in [&GROVE_V4, &GROVE_V5] {
        let db = fixture(Second::PopulatedTree, grove_version);
        for path_query in [
            page_query(None, None),
            page_query(Some(3), None),
            page_query(None, Some(3)),
        ] {
            assert_eq!(
                trusted_rows(&db, &path_query, grove_version),
                vec![under(b"a", b"d"), under(b"a", b"t"), under(b"b", b"d")]
            );
            let proof = prove(&db, &path_query, grove_version);
            assert_eq!(
                verified_rows(&db, &proof, &path_query, grove_version).expect("verifies"),
                vec![under(b"a", b"d"), under(b"b", b"d")]
            );
        }
    }
}

/// The subset verifier excludes empty trees too and counts the same way.
#[test]
fn the_subset_verifier_counts_the_unreported_row_under_v5() {
    let path_query = page_query(Some(2), None);

    let db = fixture(Second::PopulatedTree, &GROVE_V5);
    let proof = prove(&db, &path_query, &GROVE_V5);
    let (_, rows) = GroveDb::verify_subset_query(&proof, &path_query, &GROVE_V5)
        .expect("the honest proof of the page must verify as a subset");
    assert_eq!(
        rows.into_iter()
            .map(|(path, key, _)| (path, key))
            .collect::<Vec<_>>(),
        vec![under(b"a", b"d")]
    );

    let db = fixture(Second::PopulatedTree, &GROVE_V4);
    let proof = prove(&db, &path_query, &GROVE_V4);
    assert!(
        matches!(
            GroveDb::verify_subset_query(&proof, &path_query, &GROVE_V4),
            Err(Error::InvalidProof(..))
        ),
        "GROVE_V4 keeps refusing the page"
    );
}

/// A layer without subqueries already counted the row in every version: the
/// Merk proof of the layer is cut at the limit, and the empty tree is one of
/// its rows. The page holds fewer rows than its limit while more follow.
#[test]
fn a_single_layer_page_counts_the_empty_tree_under_both_versions() {
    for grove_version in [&GROVE_V4, &GROVE_V5] {
        let db = make_test_grovedb(grove_version);
        for (key, element) in [
            (b"c", Element::empty_tree()),
            (b"d", Element::new_item(vec![1])),
            (b"e", Element::new_item(vec![2])),
        ] {
            db.insert(&[TEST_LEAF], key, element, None, None, grove_version)
                .unwrap()
                .expect("insert");
        }
        let path_query = PathQuery::new(
            vec![TEST_LEAF.to_vec()],
            SizedQuery::new(Query::new_range_full(), Some(2), None),
        );
        let leaf_row = |key: &[u8]| (vec![TEST_LEAF.to_vec()], key.to_vec());

        assert_eq!(
            trusted_rows(&db, &path_query, grove_version),
            vec![leaf_row(b"c"), leaf_row(b"d")]
        );
        let proof = prove(&db, &path_query, grove_version);
        assert_eq!(
            verified_rows(&db, &proof, &path_query, grove_version).expect("verifies"),
            vec![leaf_row(b"d")]
        );
    }
}

/// `index/{a0, a1: empty trees, b/{d, e: items}}` where only `b` has a
/// subquery: the empty trees are rows of a layer that also descends.
fn mixed_layer(grove_version: &GroveVersion) -> (TempGroveDb, PathQuery) {
    let db = make_test_grovedb(grove_version);
    let insert = |path: &[&[u8]], key: &[u8], element: Element| {
        db.insert(path, key, element, None, None, grove_version)
            .unwrap()
            .expect("insert");
    };
    insert(&[TEST_LEAF], INDEX, Element::empty_tree());
    insert(&[TEST_LEAF, INDEX], b"a0", Element::empty_tree());
    insert(&[TEST_LEAF, INDEX], b"a1", Element::empty_tree());
    insert(&[TEST_LEAF, INDEX], b"b", Element::empty_tree());
    insert(&[TEST_LEAF, INDEX, b"b"], b"d", Element::new_item(vec![1]));
    insert(&[TEST_LEAF, INDEX, b"b"], b"e", Element::new_item(vec![2]));

    let mut query = Query::new_range_full();
    query.add_conditional_subquery(
        QueryItem::Key(b"b".to_vec()),
        None,
        Some(Query::new_range_full()),
    );
    let path_query = PathQuery::new(
        vec![TEST_LEAF.to_vec(), INDEX.to_vec()],
        SizedQuery::new(query, Some(3), None),
    );
    (db, path_query)
}

#[test]
fn empty_tree_rows_beside_a_subquery_branch_are_counted_under_v5() {
    let grove_version = &GROVE_V5;
    let (db, path_query) = mixed_layer(grove_version);

    assert_eq!(
        trusted_rows(&db, &path_query, grove_version),
        vec![in_index(b"a0"), in_index(b"a1"), under(b"b", b"d")]
    );

    let proof = prove(&db, &path_query, grove_version);
    assert_eq!(
        verified_rows(&db, &proof, &path_query, grove_version)
            .expect("the honest proof of the page must verify"),
        vec![under(b"b", b"d")],
        "two slots go to the empty trees, one row of `b` fills the page"
    );
}

#[test]
fn empty_tree_rows_beside_a_subquery_branch_are_refused_under_v4_as_released() {
    let grove_version = &GROVE_V4;
    let (db, path_query) = mixed_layer(grove_version);

    assert_eq!(
        trusted_rows(&db, &path_query, grove_version),
        vec![in_index(b"a0"), in_index(b"a1"), under(b"b", b"d")]
    );

    // The verifier reaches `b` with all three slots and reads its layer,
    // proved for the one slot the prover had left, as cut short.
    let proof = prove(&db, &path_query, grove_version);
    assert!(
        matches!(
            verified_rows(&db, &proof, &path_query, grove_version),
            Err(Error::InvalidProof(..))
        ),
        "GROVE_V4 keeps refusing the page"
    );
}

/// Charging the row must not let a proof stop early: the page of two proves
/// nothing about a page of three, which owes `b/d`.
#[test]
fn a_proof_one_row_short_of_the_limit_is_refused_under_v5() {
    let grove_version = &GROVE_V5;
    let db = fixture(Second::PopulatedTree, grove_version);

    let proof = prove(&db, &page_query(Some(2), None), grove_version);
    assert!(matches!(
        GroveDb::verify_query(&proof, &page_query(Some(3), None), grove_version),
        Err(Error::InvalidProof(..))
    ));
    assert!(matches!(
        GroveDb::verify_subset_query(&proof, &page_query(Some(3), None), grove_version),
        Err(Error::InvalidProof(..))
    ));
    assert!(matches!(
        GroveDb::verify_query(&proof, &page_query(None, Some(3)), grove_version),
        Err(Error::InvalidProof(..))
    ));
}

#[test]
fn an_unknown_slot_version_is_refused() {
    let db = fixture(Second::PopulatedTree, &GROVE_V5);
    let path_query = page_query(Some(2), None);
    let proof = prove(&db, &path_query, &GROVE_V5);

    let mut future = GROVE_V5.clone();
    future
        .grovedb_versions
        .operations
        .proof
        .unreported_empty_tree_row_charge = 2;
    assert!(matches!(
        GroveDb::verify_query(&proof, &path_query, &future),
        Err(Error::VersionError(_))
    ));
}

/// Compare, at every budget and in both directions, the verifier's page
/// with the trusted read's page without its empty plain trees, over a layer
/// mixing populated children, an item, an empty child the subquery descends
/// into and children holding empty trees. Returns the first case that
/// differs.
fn first_page_differing_from_the_trusted_read(grove_version: &GroveVersion) -> Option<String> {
    let db = make_test_grovedb(grove_version);
    let insert = |path: &[&[u8]], key: &[u8], element: Element| {
        db.insert(path, key, element, None, None, grove_version)
            .unwrap()
            .expect("insert");
    };
    let index: &[&[u8]] = &[TEST_LEAF, INDEX];
    insert(&[TEST_LEAF], INDEX, Element::empty_tree());
    insert(index, b"a", Element::empty_tree());
    insert(&[TEST_LEAF, INDEX, b"a"], b"d", Element::new_item(vec![1]));
    insert(&[TEST_LEAF, INDEX, b"a"], b"t", Element::empty_tree());
    insert(index, b"b", Element::new_item(vec![2]));
    insert(index, b"c", Element::empty_tree());
    insert(&[TEST_LEAF, INDEX, b"c"], b"t0", Element::empty_tree());
    insert(&[TEST_LEAF, INDEX, b"c"], b"t1", Element::empty_tree());
    insert(&[TEST_LEAF, INDEX, b"c"], b"x", Element::new_item(vec![3]));
    insert(index, b"e", Element::empty_tree());
    insert(index, b"f", Element::empty_tree());
    insert(&[TEST_LEAF, INDEX, b"f"], b"d", Element::new_item(vec![4]));
    insert(&[TEST_LEAF, INDEX, b"f"], b"s", Element::empty_sum_tree());

    let empty_plain_trees = [under(b"a", b"t"), under(b"c", b"t0"), under(b"c", b"t1")];

    for left_to_right in [true, false] {
        for budget in 1..=10u16 {
            for (global, instance_cap) in [(Some(budget), None), (None, Some(budget))] {
                let mut subquery = Query::new_range_full();
                subquery.left_to_right = left_to_right;
                let mut query = Query::new_range_full();
                query.left_to_right = left_to_right;
                query.set_subquery(subquery);
                query.limit = instance_cap;
                let path_query = PathQuery::new(
                    vec![TEST_LEAF.to_vec(), INDEX.to_vec()],
                    SizedQuery::new(query, global, None),
                );

                let trusted = trusted_rows(&db, &path_query, grove_version);
                let expected: Vec<PathKey> = trusted
                    .iter()
                    .filter(|row| !empty_plain_trees.contains(row))
                    .cloned()
                    .collect();
                let proof = prove(&db, &path_query, grove_version);
                let case = format!(
                    "left_to_right {left_to_right}, global {global:?}, cap {instance_cap:?}"
                );
                assert_eq!(
                    verified_rows_with_empty_trees(&proof, &path_query, grove_version),
                    trusted,
                    "{case}"
                );
                match verified_rows(&db, &proof, &path_query, grove_version) {
                    Ok(rows) if rows == expected => {}
                    other => return Some(format!("{case}: {other:?}")),
                }
            }
        }
    }
    None
}

#[test]
fn every_page_matches_the_trusted_read_without_its_empty_trees_under_v5() {
    assert_eq!(first_page_differing_from_the_trusted_read(&GROVE_V5), None);
}

/// The slot alone makes the difference: `GROVE_V5` with it switched back
/// disagrees with the trusted read, as `GROVE_V4` does.
#[test]
fn the_pages_differ_from_the_trusted_read_without_the_slot() {
    let mut v5_without_the_slot = GROVE_V5.clone();
    v5_without_the_slot
        .grovedb_versions
        .operations
        .proof
        .unreported_empty_tree_row_charge = 0;
    assert!(first_page_differing_from_the_trusted_read(&v5_without_the_slot).is_some());
    assert!(first_page_differing_from_the_trusted_read(&GROVE_V4).is_some());
}
