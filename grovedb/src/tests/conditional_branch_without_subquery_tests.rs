//! A key whose conditional branch has neither a subquery nor a subquery
//! path is a result row, whatever sits at it.
//!
//! `add_conditional_subquery(item, None, None)` is how a query exempts keys
//! from its default subquery: the trusted read returns the element at such a
//! key and goes no further. V1 proofs classified every key a conditional
//! branch matches as a descent. An empty tree there was proved and verified
//! as an empty child: never reported, and charged against the global limit
//! only, so under a per-instance cap the verified page ran past the trusted
//! read's. A populated tree there could not be proved at all.
//!
//! From `GROVE_V5` (`operations.proof.subquery_descent_on_key` 1) prover and
//! verifier ask whether the query has anything below the key. `GROVE_V4`
//! keeps the released classification, which these tests pin.

use grovedb_merk::proofs::query::{QueryItem, VerifyOptions};
use grovedb_version::version::{v4::GROVE_V4, v5::GROVE_V5, GroveVersion};

use crate::{
    query_result_type::QueryResultType,
    tests::{make_test_grovedb, TempGroveDb, TEST_LEAF},
    Element, Error, GroveDb, PathQuery, Query, SizedQuery,
};

const INDEX: &[u8] = b"index";

type PathKey = (Vec<Vec<u8>>, Vec<u8>);

/// What sits at `index/a`, the key the conditional branch exempts.
#[derive(Clone, Copy, Debug)]
enum Exempt {
    Empty,
    EmptySum,
    EmptyMmr,
    EmptyCountIndexed,
    /// `index/a/{z: item}`.
    Populated,
    /// `index/a/{z: sum item}`.
    PopulatedSum,
}

const EVERY_EXEMPT: [Exempt; 6] = [
    Exempt::Empty,
    Exempt::EmptySum,
    Exempt::EmptyMmr,
    Exempt::EmptyCountIndexed,
    Exempt::Populated,
    Exempt::PopulatedSum,
];

/// `index/a` as `exempt` says, `index/b/{d, e: items}`, `index/c/{d: item}`.
fn fixture(exempt: Exempt, grove_version: &GroveVersion) -> TempGroveDb {
    let db = make_test_grovedb(grove_version);
    let insert = |path: &[&[u8]], key: &[u8], element: Element| {
        db.insert(path, key, element, None, None, grove_version)
            .unwrap()
            .expect("insert");
    };
    let index: &[&[u8]] = &[TEST_LEAF, INDEX];
    let a: &[&[u8]] = &[TEST_LEAF, INDEX, b"a"];
    insert(&[TEST_LEAF], INDEX, Element::empty_tree());
    match exempt {
        Exempt::Empty => insert(index, b"a", Element::empty_tree()),
        Exempt::EmptySum => insert(index, b"a", Element::empty_sum_tree()),
        Exempt::EmptyMmr => insert(index, b"a", Element::empty_mmr_tree()),
        Exempt::EmptyCountIndexed => {
            insert(index, b"a", Element::empty_provable_count_indexed_tree())
        }
        Exempt::Populated => {
            insert(index, b"a", Element::empty_tree());
            insert(a, b"z", Element::new_item(vec![9]));
        }
        Exempt::PopulatedSum => {
            insert(index, b"a", Element::empty_sum_tree());
            insert(a, b"z", Element::new_sum_item(5));
        }
    }
    insert(index, b"b", Element::empty_tree());
    insert(&[TEST_LEAF, INDEX, b"b"], b"d", Element::new_item(vec![1]));
    insert(&[TEST_LEAF, INDEX, b"b"], b"e", Element::new_item(vec![2]));
    insert(index, b"c", Element::empty_tree());
    insert(&[TEST_LEAF, INDEX, b"c"], b"d", Element::new_item(vec![3]));
    db
}

/// How the query leaves `a` without a subquery.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// Every child is descended into, except `a`.
    DefaultSubqueryWithAnExemption,
    /// Only `b` is descended into; `a` is matched by a branch that says
    /// nothing, and `c` by no branch.
    ConditionalBranchesOnly,
}

fn query(
    shape: Shape,
    left_to_right: bool,
    global: Option<u16>,
    instance_cap: Option<u16>,
) -> PathQuery {
    let mut subquery = Query::new_range_full();
    subquery.left_to_right = left_to_right;
    let mut query = Query::new_range_full();
    query.left_to_right = left_to_right;
    query.limit = instance_cap;
    match shape {
        Shape::DefaultSubqueryWithAnExemption => {
            query.set_subquery(subquery);
            query.add_conditional_subquery(QueryItem::Key(b"a".to_vec()), None, None);
        }
        Shape::ConditionalBranchesOnly => {
            query.add_conditional_subquery(QueryItem::Key(b"a".to_vec()), None, None);
            query.add_conditional_subquery(QueryItem::Key(b"b".to_vec()), None, Some(subquery));
        }
    }
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

/// Verify with the succinctness check on, reporting empty trees or not.
fn verified_rows(
    db: &TempGroveDb,
    proof: &[u8],
    path_query: &PathQuery,
    include_empty_trees_in_result: bool,
    grove_version: &GroveVersion,
) -> Result<Vec<PathKey>, Error> {
    let (root_hash, rows) = GroveDb::verify_query_with_options(
        proof,
        path_query,
        VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: true,
            include_empty_trees_in_result,
        },
        grove_version,
    )?;
    assert_eq!(
        root_hash,
        db.root_hash(None, grove_version).unwrap().expect("root"),
        "the proof must verify against the database's root"
    );
    Ok(rows.into_iter().map(|(path, key, _)| (path, key)).collect())
}

/// Compare the verified page with the trusted read's, for every element at
/// the exempt key, both query shapes, both directions, no budget and every
/// global limit and per-instance cap up to 6. A verifier that leaves empty
/// trees out returns the trusted rows without the empty plain tree; one that
/// reports them returns all of them. Returns the first case that differs.
fn first_page_differing_from_the_trusted_read(grove_version: &GroveVersion) -> Option<String> {
    for exempt in EVERY_EXEMPT {
        let db = fixture(exempt, grove_version);
        for shape in [
            Shape::DefaultSubqueryWithAnExemption,
            Shape::ConditionalBranchesOnly,
        ] {
            for left_to_right in [true, false] {
                let budgets = std::iter::once((None, None))
                    .chain((1..=6u16).map(|budget| (Some(budget), None)))
                    .chain((1..=6u16).map(|budget| (None, Some(budget))));
                for (global, instance_cap) in budgets {
                    let path_query = query(shape, left_to_right, global, instance_cap);
                    let case = format!(
                        "{exempt:?}, {shape:?}, left_to_right {left_to_right}, global \
                         {global:?}, cap {instance_cap:?}"
                    );
                    let trusted = trusted_rows(&db, &path_query, grove_version);
                    let without_empty_trees: Vec<PathKey> = trusted
                        .iter()
                        .filter(|row| !(matches!(exempt, Exempt::Empty) && **row == in_index(b"a")))
                        .cloned()
                        .collect();
                    let proof = match db.prove_query(&path_query, None, grove_version).unwrap() {
                        Ok(proof) => proof,
                        Err(e) => return Some(format!("{case}: prove: {e:?}")),
                    };
                    match verified_rows(&db, &proof, &path_query, true, grove_version) {
                        Ok(rows) if rows == trusted => {}
                        other => return Some(format!("{case}: with empty trees: {other:?}")),
                    }
                    match verified_rows(&db, &proof, &path_query, false, grove_version) {
                        Ok(rows) if rows == without_empty_trees => {}
                        other => return Some(format!("{case}: without empty trees: {other:?}")),
                    }
                }
            }
        }
    }
    None
}

#[test]
fn every_page_matches_the_trusted_read_under_v5() {
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
        .subquery_descent_on_key = 0;
    assert!(first_page_differing_from_the_trusted_read(&v5_without_the_slot).is_some());
    assert!(first_page_differing_from_the_trusted_read(&GROVE_V4).is_some());
}

/// The rows of one page under one version, as `verify_query` and as a
/// verifier reporting empty trees return them.
fn pages(
    exempt: Exempt,
    global: Option<u16>,
    instance_cap: Option<u16>,
    grove_version: &GroveVersion,
) -> (Vec<PathKey>, Vec<PathKey>, Vec<PathKey>) {
    let db = fixture(exempt, grove_version);
    let path_query = query(
        Shape::DefaultSubqueryWithAnExemption,
        true,
        global,
        instance_cap,
    );
    let trusted = trusted_rows(&db, &path_query, grove_version);
    let proof = db
        .prove_query(&path_query, None, grove_version)
        .unwrap()
        .expect("prove");
    let without = verified_rows(&db, &proof, &path_query, false, grove_version).expect("verifies");
    let with = verified_rows(&db, &proof, &path_query, true, grove_version).expect("verifies");
    (trusted, without, with)
}

#[test]
fn an_exempt_empty_tree_is_a_row_under_v5() {
    let everything = vec![
        in_index(b"a"),
        under(b"b", b"d"),
        under(b"b", b"e"),
        under(b"c", b"d"),
    ];

    // A plain tree is reported when the verifier reports empty trees.
    let (trusted, without, with) = pages(Exempt::Empty, None, None, &GROVE_V5);
    assert_eq!(trusted, everything);
    assert_eq!(without, everything[1..]);
    assert_eq!(with, everything);

    // Any other empty tree is always reported.
    let (trusted, without, with) = pages(Exempt::EmptySum, None, None, &GROVE_V5);
    assert_eq!(trusted, everything);
    assert_eq!(without, everything);
    assert_eq!(with, everything);
}

#[test]
fn an_exempt_empty_tree_is_never_reported_under_v4_as_released() {
    let everything = vec![
        in_index(b"a"),
        under(b"b", b"d"),
        under(b"b", b"e"),
        under(b"c", b"d"),
    ];
    for exempt in [Exempt::Empty, Exempt::EmptySum] {
        let (trusted, without, with) = pages(exempt, None, None, &GROVE_V4);
        assert_eq!(trusted, everything);
        assert_eq!(without, everything[1..], "{exempt:?}");
        assert_eq!(with, everything[1..], "{exempt:?}");
    }
}

#[test]
fn an_exempt_empty_tree_takes_a_slot_of_the_instance_cap_under_v5() {
    let (trusted, without, with) = pages(Exempt::Empty, None, Some(1), &GROVE_V5);
    assert_eq!(trusted, vec![in_index(b"a")]);
    assert_eq!(without, vec![]);
    assert_eq!(with, vec![in_index(b"a")]);

    let (trusted, without, with) = pages(Exempt::EmptySum, None, Some(2), &GROVE_V5);
    assert_eq!(trusted, vec![in_index(b"a"), under(b"b", b"d")]);
    assert_eq!(without, trusted);
    assert_eq!(with, trusted);
}

#[test]
fn an_exempt_empty_tree_takes_no_slot_of_the_instance_cap_under_v4_as_released() {
    // The verified page starts after the row the trusted read returns first.
    let (trusted, without, with) = pages(Exempt::Empty, None, Some(1), &GROVE_V4);
    assert_eq!(trusted, vec![in_index(b"a")]);
    assert_eq!(without, vec![under(b"b", b"d")]);
    assert_eq!(with, vec![under(b"b", b"d")]);

    let (trusted, without, with) = pages(Exempt::EmptySum, None, Some(2), &GROVE_V4);
    assert_eq!(trusted, vec![in_index(b"a"), under(b"b", b"d")]);
    assert_eq!(without, vec![under(b"b", b"d"), under(b"b", b"e")]);
    assert_eq!(with, vec![under(b"b", b"d"), under(b"b", b"e")]);
}

#[test]
fn an_exempt_populated_tree_is_proved_as_a_row_under_v5() {
    for exempt in [Exempt::Populated, Exempt::PopulatedSum] {
        let (trusted, without, with) = pages(exempt, None, None, &GROVE_V5);
        assert_eq!(
            trusted,
            vec![
                in_index(b"a"),
                under(b"b", b"d"),
                under(b"b", b"e"),
                under(b"c", b"d"),
            ],
            "nothing below the exempt tree is a row"
        );
        assert_eq!(without, trusted, "{exempt:?}");
        assert_eq!(with, trusted, "{exempt:?}");
    }
}

#[test]
fn an_exempt_populated_tree_cannot_be_proved_under_v4_as_released() {
    for exempt in [Exempt::Populated, Exempt::PopulatedSum] {
        let db = fixture(exempt, &GROVE_V4);
        let path_query = query(Shape::DefaultSubqueryWithAnExemption, true, None, None);
        assert_eq!(trusted_rows(&db, &path_query, &GROVE_V4).len(), 4);
        assert!(
            matches!(
                db.prove_query(&path_query, None, &GROVE_V4).unwrap(),
                Err(Error::CorruptedPath(_))
            ),
            "{exempt:?}: GROVE_V4 keeps refusing to prove the query"
        );
    }
}

/// A proof cannot leave the exempt row out: the page with the row and the
/// page without it are different proofs.
#[test]
fn a_released_proof_without_the_row_is_refused_under_v5() {
    // Under `GROVE_V4` the prover does not charge the row to the cap, so
    // its page of one is `b/d`. `GROVE_V5` owes `a` first.
    let path_query = query(Shape::DefaultSubqueryWithAnExemption, true, None, Some(1));
    let db = fixture(Exempt::EmptySum, &GROVE_V4);
    let proof = db
        .prove_query(&path_query, None, &GROVE_V4)
        .unwrap()
        .expect("prove");
    assert!(matches!(
        GroveDb::verify_query(&proof, &path_query, &GROVE_V5),
        Err(Error::InvalidProof(..))
    ));
}

#[test]
fn an_unknown_slot_version_is_refused_by_prover_and_verifier() {
    let db = fixture(Exempt::Empty, &GROVE_V5);
    let path_query = query(Shape::DefaultSubqueryWithAnExemption, true, None, None);
    let proof = db
        .prove_query(&path_query, None, &GROVE_V5)
        .unwrap()
        .expect("prove");

    let mut future = GROVE_V5.clone();
    future
        .grovedb_versions
        .operations
        .proof
        .subquery_descent_on_key = 2;
    assert!(matches!(
        db.prove_query(&path_query, None, &future).unwrap(),
        Err(Error::VersionError(_))
    ));
    assert!(matches!(
        GroveDb::verify_query(&proof, &path_query, &future),
        Err(Error::VersionError(_))
    ));
}
