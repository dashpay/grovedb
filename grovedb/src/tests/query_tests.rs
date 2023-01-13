#[cfg(feature = "full")]
use merk::proofs::{query::QueryItem, Query};
#[cfg(feature = "full")]
use rand::Rng;
#[cfg(feature = "full")]
use tempfile::TempDir;

use crate::query_result_type::QueryResultType;
#[cfg(feature = "full")]
use crate::{
    batch::GroveDbOp,
    reference_path::ReferencePathType,
    tests::{common::compare_result_sets, make_test_grovedb, TempGroveDb, TEST_LEAF},
    Element, GroveDb, PathQuery, SizedQuery,
};

// TODO: get rid of the cfg attribute from each test, do this at the module
// level

#[cfg(feature = "full")]
fn populate_tree_for_non_unique_range_subquery(db: &TempGroveDb) {
    // Insert a couple of subtrees first
    for i in 1985u32..2000 {
        let i_vec = (i as u32).to_be_bytes().to_vec();
        db.insert([TEST_LEAF], &i_vec, Element::empty_tree(), None, None)
            .unwrap()
            .expect("successful subtree insert");
        // Insert element 0
        // Insert some elements into subtree
        db.insert(
            [TEST_LEAF, i_vec.as_slice()],
            b"\0",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");

        for j in 100u32..150 {
            let mut j_vec = i_vec.clone();
            j_vec.append(&mut (j as u32).to_be_bytes().to_vec());
            db.insert(
                [TEST_LEAF, i_vec.as_slice(), b"\0"],
                &j_vec.clone(),
                Element::new_item(j_vec),
                None,
                None,
            )
            .unwrap()
            .expect("successful value insert");
        }
    }
}

#[cfg(feature = "full")]
fn populate_tree_for_non_unique_double_range_subquery(db: &TempGroveDb) {
    // Insert a couple of subtrees first
    for i in 0u32..10 {
        let i_vec = (i as u32).to_be_bytes().to_vec();
        db.insert([TEST_LEAF], &i_vec, Element::empty_tree(), None, None)
            .unwrap()
            .expect("successful subtree insert");
        // Insert element 0
        // Insert some elements into subtree
        db.insert(
            [TEST_LEAF, i_vec.as_slice()],
            b"a",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");

        for j in 25u32..50 {
            let j_vec = (j as u32).to_be_bytes().to_vec();
            db.insert(
                [TEST_LEAF, i_vec.as_slice(), b"a"],
                &j_vec,
                Element::empty_tree(),
                None,
                None,
            )
            .unwrap()
            .expect("successful value insert");

            // Insert element 0
            // Insert some elements into subtree
            db.insert(
                [TEST_LEAF, i_vec.as_slice(), b"a", j_vec.as_slice()],
                b"\0",
                Element::empty_tree(),
                None,
                None,
            )
            .unwrap()
            .expect("successful subtree insert");

            for k in 100u32..110 {
                let k_vec = (k as u32).to_be_bytes().to_vec();
                db.insert(
                    [TEST_LEAF, i_vec.as_slice(), b"a", &j_vec, b"\0"],
                    &k_vec.clone(),
                    Element::new_item(k_vec),
                    None,
                    None,
                )
                .unwrap()
                .expect("successful value insert");
            }
        }
    }
}

#[cfg(feature = "full")]
fn populate_tree_by_reference_for_non_unique_range_subquery(db: &TempGroveDb) {
    // This subtree will be holding values
    db.insert([TEST_LEAF], b"\0", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");

    // This subtree will be holding references
    db.insert([TEST_LEAF], b"1", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    // Insert a couple of subtrees first
    for i in 1985u32..2000 {
        let i_vec = (i as u32).to_be_bytes().to_vec();
        db.insert([TEST_LEAF, b"1"], &i_vec, Element::empty_tree(), None, None)
            .unwrap()
            .expect("successful subtree insert");
        // Insert element 0
        // Insert some elements into subtree
        db.insert(
            [TEST_LEAF, b"1", i_vec.as_slice()],
            b"\0",
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");

        for j in 100u32..150 {
            let random_key = rand::thread_rng().gen::<[u8; 32]>();
            let mut j_vec = i_vec.clone();
            j_vec.append(&mut (j as u32).to_be_bytes().to_vec());

            // We should insert every item to the tree holding items
            db.insert(
                [TEST_LEAF, b"\0"],
                &random_key,
                Element::new_item(j_vec.clone()),
                None,
                None,
            )
            .unwrap()
            .expect("successful value insert");

            db.insert(
                [TEST_LEAF, b"1", i_vec.clone().as_slice(), b"\0"],
                &random_key,
                Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                    TEST_LEAF.to_vec(),
                    b"\0".to_vec(),
                    random_key.to_vec(),
                ])),
                None,
                None,
            )
            .unwrap()
            .expect("successful value insert");
        }
    }
}

#[cfg(feature = "full")]
fn populate_tree_for_unique_range_subquery(db: &TempGroveDb) {
    // Insert a couple of subtrees first
    for i in 1985u32..2000 {
        let i_vec = (i as u32).to_be_bytes().to_vec();
        db.insert([TEST_LEAF], &i_vec, Element::empty_tree(), None, None)
            .unwrap()
            .expect("successful subtree insert");

        db.insert(
            [TEST_LEAF, &i_vec.clone()],
            b"\0",
            Element::new_item(i_vec),
            None,
            None,
        )
        .unwrap()
        .expect("successful value insert");
    }
}

#[cfg(feature = "full")]
fn populate_tree_by_reference_for_unique_range_subquery(db: &TempGroveDb) {
    // This subtree will be holding values
    db.insert([TEST_LEAF], b"\0", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");

    // This subtree will be holding references
    db.insert([TEST_LEAF], b"1", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");

    for i in 1985u32..2000 {
        let i_vec = (i as u32).to_be_bytes().to_vec();
        db.insert([TEST_LEAF, b"1"], &i_vec, Element::empty_tree(), None, None)
            .unwrap()
            .expect("successful subtree insert");

        // We should insert every item to the tree holding items
        db.insert(
            [TEST_LEAF, b"\0"],
            &i_vec,
            Element::new_item(i_vec.clone()),
            None,
            None,
        )
        .unwrap()
        .expect("successful value insert");

        // We should insert a reference to the item
        db.insert(
            [TEST_LEAF, b"1", i_vec.clone().as_slice()],
            b"\0",
            Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
                TEST_LEAF.to_vec(),
                b"\0".to_vec(),
                i_vec.clone(),
            ])),
            None,
            None,
        )
        .unwrap()
        .expect("successful value insert");
    }
}

#[cfg(feature = "full")]
fn populate_tree_for_unique_range_subquery_with_non_unique_null_values(db: &mut TempGroveDb) {
    populate_tree_for_unique_range_subquery(db);
    db.insert([TEST_LEAF], &[], Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    db.insert([TEST_LEAF, &[]], b"\0", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    // Insert a couple of subtrees first
    for i in 100u32..200 {
        let i_vec = (i as u32).to_be_bytes().to_vec();
        db.insert(
            [TEST_LEAF, &[], b"\0"],
            &i_vec,
            Element::new_item(i_vec.clone()),
            None,
            None,
        )
        .unwrap()
        .expect("successful value insert");
    }
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_query_with_non_unique_subquery() {
    let db = make_test_grovedb();
    populate_tree_for_non_unique_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new();
    query.insert_range(1988_u32.to_be_bytes().to_vec()..1992_u32.to_be_bytes().to_vec());

    let subquery_key: Vec<u8> = b"\0".to_vec();
    let mut subquery = Query::new();
    subquery.insert_all();

    query.set_subquery_key(subquery_key);
    query.set_subquery(subquery);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 200);

    let mut first_value = 1988_u32.to_be_bytes().to_vec();
    first_value.append(&mut 100_u32.to_be_bytes().to_vec());
    assert_eq!(elements[0], first_value);

    let mut last_value = 1991_u32.to_be_bytes().to_vec();
    last_value.append(&mut 149_u32.to_be_bytes().to_vec());
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 200);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_query_with_unique_subquery() {
    let mut db = make_test_grovedb();
    populate_tree_for_unique_range_subquery(&mut db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new();
    query.insert_range(1988_u32.to_be_bytes().to_vec()..1992_u32.to_be_bytes().to_vec());

    let subquery_key: Vec<u8> = b"\0".to_vec();

    query.set_subquery_key(subquery_key);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 4);

    let first_value = 1988_u32.to_be_bytes().to_vec();
    assert_eq!(elements[0], first_value);

    let last_value = 1991_u32.to_be_bytes().to_vec();
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 4);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_query_with_unique_subquery_on_references() {
    let db = make_test_grovedb();
    populate_tree_by_reference_for_unique_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec(), b"1".to_vec()];
    let mut query = Query::new();
    query.insert_range(1988_u32.to_be_bytes().to_vec()..1992_u32.to_be_bytes().to_vec());

    let subquery_key: Vec<u8> = b"\0".to_vec();

    query.set_subquery_key(subquery_key);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 4);

    let first_value = 1988_u32.to_be_bytes().to_vec();
    assert_eq!(elements[0], first_value);

    let last_value = 1991_u32.to_be_bytes().to_vec();
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 4);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_query_with_unique_subquery_with_non_unique_null_values() {
    let mut db = make_test_grovedb();
    populate_tree_for_unique_range_subquery_with_non_unique_null_values(&mut db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new();
    query.insert_all();

    let subquery_key: Vec<u8> = b"\0".to_vec();

    query.set_subquery_key(subquery_key);

    let mut subquery = Query::new();
    subquery.insert_all();

    query.add_conditional_subquery(
        QueryItem::Key(b"".to_vec()),
        Some(vec![b"\0".to_vec()]),
        Some(subquery),
    );

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 115);

    let first_value = 100_u32.to_be_bytes().to_vec();
    assert_eq!(elements[0], first_value);

    let last_value = 1999_u32.to_be_bytes().to_vec();
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 115);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_query_with_unique_subquery_ignore_non_unique_null_values() {
    let mut db = make_test_grovedb();
    populate_tree_for_unique_range_subquery_with_non_unique_null_values(&mut db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new();
    query.insert_all();

    let subquery_key: Vec<u8> = b"\0".to_vec();

    query.set_subquery_key(subquery_key);

    let subquery = Query::new();

    query.add_conditional_subquery(
        QueryItem::Key(b"".to_vec()),
        Some(vec![b"\0".to_vec()]),
        Some(subquery),
    );

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 15);

    let first_value = 1985_u32.to_be_bytes().to_vec();
    assert_eq!(elements[0], first_value);

    let last_value = 1999_u32.to_be_bytes().to_vec();
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 15);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_inclusive_query_with_non_unique_subquery() {
    let db = make_test_grovedb();
    populate_tree_for_non_unique_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new();
    query.insert_range_inclusive(1988_u32.to_be_bytes().to_vec()..=1995_u32.to_be_bytes().to_vec());

    let subquery_key: Vec<u8> = b"\0".to_vec();
    let mut subquery = Query::new();
    subquery.insert_all();

    query.set_subquery_key(subquery_key);
    query.set_subquery(subquery);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 400);

    let mut first_value = 1988_u32.to_be_bytes().to_vec();
    first_value.append(&mut 100_u32.to_be_bytes().to_vec());
    assert_eq!(elements[0], first_value);

    let mut last_value = 1995_u32.to_be_bytes().to_vec();
    last_value.append(&mut 149_u32.to_be_bytes().to_vec());
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 400);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_inclusive_query_with_non_unique_subquery_on_references() {
    let db = make_test_grovedb();
    populate_tree_by_reference_for_non_unique_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec(), b"1".to_vec()];
    let mut query = Query::new();
    query.insert_range_inclusive(1988_u32.to_be_bytes().to_vec()..=1995_u32.to_be_bytes().to_vec());

    let subquery_key: Vec<u8> = b"\0".to_vec();
    let mut subquery = Query::new();
    subquery.insert_all();

    query.set_subquery_key(subquery_key);
    query.set_subquery(subquery);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 400);

    let mut first_value = 1988_u32.to_be_bytes().to_vec();
    first_value.append(&mut 100_u32.to_be_bytes().to_vec());
    // using contains as the elements get stored at random key locations
    // hence impossible to predict the final location
    // but must exist
    assert!(elements.contains(&first_value));

    let mut last_value = 1995_u32.to_be_bytes().to_vec();
    last_value.append(&mut 149_u32.to_be_bytes().to_vec());
    assert!(elements.contains(&last_value));

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 400);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_inclusive_query_with_unique_subquery() {
    let db = make_test_grovedb();
    populate_tree_for_unique_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new();
    query.insert_range_inclusive(1988_u32.to_be_bytes().to_vec()..=1995_u32.to_be_bytes().to_vec());

    let subquery_key: Vec<u8> = b"\0".to_vec();

    query.set_subquery_key(subquery_key);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 8);

    let first_value = 1988_u32.to_be_bytes().to_vec();
    assert_eq!(elements[0], first_value);

    let last_value = 1995_u32.to_be_bytes().to_vec();
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 8);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_from_query_with_non_unique_subquery() {
    let db = make_test_grovedb();
    populate_tree_for_non_unique_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new();
    query.insert_range_from(1995_u32.to_be_bytes().to_vec()..);

    let subquery_key: Vec<u8> = b"\0".to_vec();
    let mut subquery = Query::new();
    subquery.insert_all();

    query.set_subquery_key(subquery_key);
    query.set_subquery(subquery);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 250);

    let mut first_value = 1995_u32.to_be_bytes().to_vec();
    first_value.append(&mut 100_u32.to_be_bytes().to_vec());
    assert_eq!(elements[0], first_value);

    let mut last_value = 1999_u32.to_be_bytes().to_vec();
    last_value.append(&mut 149_u32.to_be_bytes().to_vec());
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 250);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_from_query_with_unique_subquery() {
    let db = make_test_grovedb();
    populate_tree_for_unique_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new();
    query.insert_range_from(1995_u32.to_be_bytes().to_vec()..);

    let subquery_key: Vec<u8> = b"\0".to_vec();

    query.set_subquery_key(subquery_key);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 5);

    let first_value = 1995_u32.to_be_bytes().to_vec();
    assert_eq!(elements[0], first_value);

    let last_value = 1999_u32.to_be_bytes().to_vec();
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 5);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_to_query_with_non_unique_subquery() {
    let db = make_test_grovedb();
    populate_tree_for_non_unique_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new();
    query.insert_range_to(..1995_u32.to_be_bytes().to_vec());

    let subquery_key: Vec<u8> = b"\0".to_vec();
    let mut subquery = Query::new();
    subquery.insert_all();

    query.set_subquery_key(subquery_key);
    query.set_subquery(subquery);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 500);

    let mut first_value = 1985_u32.to_be_bytes().to_vec();
    first_value.append(&mut 100_u32.to_be_bytes().to_vec());
    assert_eq!(elements[0], first_value);

    let mut last_value = 1994_u32.to_be_bytes().to_vec();
    last_value.append(&mut 149_u32.to_be_bytes().to_vec());
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 500);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_to_query_with_unique_subquery() {
    let db = make_test_grovedb();
    populate_tree_for_unique_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new();
    query.insert_range_to(..1995_u32.to_be_bytes().to_vec());

    let subquery_key: Vec<u8> = b"\0".to_vec();

    query.set_subquery_key(subquery_key);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 10);

    let first_value = 1985_u32.to_be_bytes().to_vec();
    assert_eq!(elements[0], first_value);

    let last_value = 1994_u32.to_be_bytes().to_vec();
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 10);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_to_inclusive_query_with_non_unique_subquery() {
    let db = make_test_grovedb();
    populate_tree_for_non_unique_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new();
    query.insert_range_to_inclusive(..=1995_u32.to_be_bytes().to_vec());

    let subquery_key: Vec<u8> = b"\0".to_vec();
    let mut subquery = Query::new();
    subquery.insert_all();

    query.set_subquery_key(subquery_key);
    query.set_subquery(subquery);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 550);

    let mut first_value = 1985_u32.to_be_bytes().to_vec();
    first_value.append(&mut 100_u32.to_be_bytes().to_vec());
    assert_eq!(elements[0], first_value);

    let mut last_value = 1995_u32.to_be_bytes().to_vec();
    last_value.append(&mut 149_u32.to_be_bytes().to_vec());
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 550);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_to_inclusive_query_with_non_unique_subquery_and_key_out_of_bounds() {
    let db = make_test_grovedb();
    populate_tree_for_non_unique_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new_with_direction(false);
    query.insert_range_to_inclusive(..=5000_u32.to_be_bytes().to_vec());

    let subquery_key: Vec<u8> = b"\0".to_vec();
    let mut subquery = Query::new_with_direction(false);
    subquery.insert_all();

    query.set_subquery_key(subquery_key);
    query.set_subquery(subquery);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 750);

    let mut first_value = 1999_u32.to_be_bytes().to_vec();
    first_value.append(&mut 149_u32.to_be_bytes().to_vec());
    assert_eq!(elements[0], first_value);

    let mut last_value = 1985_u32.to_be_bytes().to_vec();
    last_value.append(&mut 100_u32.to_be_bytes().to_vec());
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 750);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_to_inclusive_query_with_unique_subquery() {
    let db = make_test_grovedb();
    populate_tree_for_unique_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new();
    query.insert_range_to_inclusive(..=1995_u32.to_be_bytes().to_vec());

    let subquery_key: Vec<u8> = b"\0".to_vec();

    query.set_subquery_key(subquery_key);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 11);

    let first_value = 1985_u32.to_be_bytes().to_vec();
    assert_eq!(elements[0], first_value);

    let last_value = 1995_u32.to_be_bytes().to_vec();
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 11);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_after_query_with_non_unique_subquery() {
    let db = make_test_grovedb();
    populate_tree_for_non_unique_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new();
    query.insert_range_after(1995_u32.to_be_bytes().to_vec()..);

    let subquery_key: Vec<u8> = b"\0".to_vec();
    let mut subquery = Query::new();
    subquery.insert_all();

    query.set_subquery_key(subquery_key);
    query.set_subquery(subquery);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 200);

    let mut first_value = 1996_u32.to_be_bytes().to_vec();
    first_value.append(&mut 100_u32.to_be_bytes().to_vec());
    assert_eq!(elements[0], first_value);

    let mut last_value = 1999_u32.to_be_bytes().to_vec();
    last_value.append(&mut 149_u32.to_be_bytes().to_vec());
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 200);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_after_to_query_with_non_unique_subquery() {
    let db = make_test_grovedb();
    populate_tree_for_non_unique_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new();
    query.insert_range_after_to(1995_u32.to_be_bytes().to_vec()..1997_u32.to_be_bytes().to_vec());

    let subquery_key: Vec<u8> = b"\0".to_vec();
    let mut subquery = Query::new();
    subquery.insert_all();

    query.set_subquery_key(subquery_key);
    query.set_subquery(subquery);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 50);

    let mut first_value = 1996_u32.to_be_bytes().to_vec();
    first_value.append(&mut 100_u32.to_be_bytes().to_vec());
    assert_eq!(elements[0], first_value);

    let mut last_value = 1996_u32.to_be_bytes().to_vec();
    last_value.append(&mut 149_u32.to_be_bytes().to_vec());
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 50);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_after_to_inclusive_query_with_non_unique_subquery() {
    let db = make_test_grovedb();
    populate_tree_for_non_unique_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new();
    query.insert_range_after_to_inclusive(
        1995_u32.to_be_bytes().to_vec()..=1997_u32.to_be_bytes().to_vec(),
    );

    let subquery_key: Vec<u8> = b"\0".to_vec();
    let mut subquery = Query::new();
    subquery.insert_all();

    query.set_subquery_key(subquery_key);
    query.set_subquery(subquery);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 100);

    let mut first_value = 1996_u32.to_be_bytes().to_vec();
    first_value.append(&mut 100_u32.to_be_bytes().to_vec());
    assert_eq!(elements[0], first_value);

    let mut last_value = 1997_u32.to_be_bytes().to_vec();
    last_value.append(&mut 149_u32.to_be_bytes().to_vec());
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 100);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_after_to_inclusive_query_with_non_unique_subquery_and_key_out_of_bounds() {
    let db = make_test_grovedb();
    populate_tree_for_non_unique_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new_with_direction(false);
    query.insert_range_after_to_inclusive(
        1995_u32.to_be_bytes().to_vec()..=5000_u32.to_be_bytes().to_vec(),
    );

    let subquery_key: Vec<u8> = b"\0".to_vec();
    let mut subquery = Query::new_with_direction(false);
    subquery.insert_all();

    query.set_subquery_key(subquery_key);
    query.set_subquery(subquery);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 200);

    let mut first_value = 1999_u32.to_be_bytes().to_vec();
    first_value.append(&mut 149_u32.to_be_bytes().to_vec());
    assert_eq!(elements[0], first_value);

    let mut last_value = 1996_u32.to_be_bytes().to_vec();
    last_value.append(&mut 100_u32.to_be_bytes().to_vec());
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 200);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_inclusive_query_with_double_non_unique_subquery() {
    let db = make_test_grovedb();
    populate_tree_for_non_unique_double_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new();
    query.insert_range_inclusive((3u32).to_be_bytes().to_vec()..=(4u32).to_be_bytes().to_vec());

    query.set_subquery_key(b"a".to_vec());

    let mut subquery = Query::new();
    subquery
        .insert_range_inclusive((29u32).to_be_bytes().to_vec()..=(31u32).to_be_bytes().to_vec());

    subquery.set_subquery_key(b"\0".to_vec());

    let mut subsubquery = Query::new();
    subsubquery.insert_all();

    subquery.set_subquery(subsubquery);

    query.set_subquery(subquery);

    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 60);

    let first_value = 100_u32.to_be_bytes().to_vec();
    assert_eq!(elements[0], first_value);

    let last_value = 109_u32.to_be_bytes().to_vec();
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 60);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_get_range_query_with_limit_and_offset() {
    let db = make_test_grovedb();
    populate_tree_for_non_unique_range_subquery(&db);

    let path = vec![TEST_LEAF.to_vec()];
    let mut query = Query::new_with_direction(true);
    query.insert_range(1990_u32.to_be_bytes().to_vec()..1995_u32.to_be_bytes().to_vec());

    let subquery_key: Vec<u8> = b"\0".to_vec();
    let mut subquery = Query::new();
    subquery.insert_all();

    query.set_subquery_key(subquery_key.clone());
    query.set_subquery(subquery.clone());

    // Baseline query: no offset or limit + left to right
    let path_query = PathQuery::new(path.clone(), SizedQuery::new(query.clone(), None, None));

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 250);

    let mut first_value = 1990_u32.to_be_bytes().to_vec();
    first_value.append(&mut 100_u32.to_be_bytes().to_vec());
    assert_eq!(elements[0], first_value);

    let mut last_value = 1994_u32.to_be_bytes().to_vec();
    last_value.append(&mut 149_u32.to_be_bytes().to_vec());
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 250);
    compare_result_sets(&elements, &result_set);

    subquery.left_to_right = false;

    query.set_subquery_key(subquery_key.clone());
    query.set_subquery(subquery.clone());

    query.left_to_right = false;

    // Baseline query: no offset or limit + right to left
    let path_query = PathQuery::new(path.clone(), SizedQuery::new(query.clone(), None, None));

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 250);

    let mut first_value = 1994_u32.to_be_bytes().to_vec();
    first_value.append(&mut 149_u32.to_be_bytes().to_vec());
    assert_eq!(elements[0], first_value);

    let mut last_value = 1990_u32.to_be_bytes().to_vec();
    last_value.append(&mut 100_u32.to_be_bytes().to_vec());
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 250);
    compare_result_sets(&elements, &result_set);

    subquery.left_to_right = true;

    query.set_subquery_key(subquery_key.clone());
    query.set_subquery(subquery.clone());

    query.left_to_right = true;

    // Limit the result to just 55 elements
    let path_query = PathQuery::new(path.clone(), SizedQuery::new(query.clone(), Some(55), None));

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 55);

    let mut first_value = 1990_u32.to_be_bytes().to_vec();
    first_value.append(&mut 100_u32.to_be_bytes().to_vec());
    assert_eq!(elements[0], first_value);

    // Second tree 5 element [100, 101, 102, 103, 104]
    let mut last_value = 1991_u32.to_be_bytes().to_vec();
    last_value.append(&mut 104_u32.to_be_bytes().to_vec());
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 55);
    compare_result_sets(&elements, &result_set);

    query.set_subquery_key(subquery_key.clone());
    query.set_subquery(subquery.clone());

    // Limit the result set to 60 elements but skip the first 14 elements
    let path_query = PathQuery::new(
        path.clone(),
        SizedQuery::new(query.clone(), Some(60), Some(14)),
    );

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 60);

    // Skips the first 14 elements, starts from the 15th
    // i.e skips [100 - 113] starts from 114
    let mut first_value = 1990_u32.to_be_bytes().to_vec();
    first_value.append(&mut 114_u32.to_be_bytes().to_vec());
    assert_eq!(elements[0], first_value);

    // Continues for 60 iterations
    // Takes 36 elements from the first tree (50 - 14)
    // takes the remaining 24 from the second three (60 - 36)
    let mut last_value = 1991_u32.to_be_bytes().to_vec();
    last_value.append(&mut 123_u32.to_be_bytes().to_vec());
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 60);
    compare_result_sets(&elements, &result_set);

    query.set_subquery_key(subquery_key.clone());
    query.set_subquery(subquery.clone());

    query.left_to_right = false;

    // Limit the result set to 60 element but skip first 10 elements (this time
    // right to left)
    let path_query = PathQuery::new(
        path.clone(),
        SizedQuery::new(query.clone(), Some(60), Some(10)),
    );

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 60);

    // Skips the first 10 elements from the back
    // last tree and starts from the 11th before the end
    // Underlying subquery is ascending
    let mut first_value = 1994_u32.to_be_bytes().to_vec();
    first_value.append(&mut 110_u32.to_be_bytes().to_vec());
    assert_eq!(elements[0], first_value);

    let mut last_value = 1993_u32.to_be_bytes().to_vec();
    last_value.append(&mut 119_u32.to_be_bytes().to_vec());
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 60);
    compare_result_sets(&elements, &result_set);

    query.set_subquery_key(subquery_key.clone());
    query.set_subquery(subquery.clone());

    query.left_to_right = true;

    // Offset bigger than elements in range
    let path_query = PathQuery::new(
        path.clone(),
        SizedQuery::new(query.clone(), None, Some(5000)),
    );

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 0);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 0);

    query.set_subquery_key(subquery_key.clone());
    query.set_subquery(subquery);

    // Limit bigger than elements in range
    let path_query = PathQuery::new(
        path.clone(),
        SizedQuery::new(query.clone(), Some(5000), None),
    );

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 250);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 250);

    // Test on unique subtree build
    let db = make_test_grovedb();
    populate_tree_for_unique_range_subquery(&db);

    let mut query = Query::new_with_direction(true);
    query.insert_range(1990_u32.to_be_bytes().to_vec()..2000_u32.to_be_bytes().to_vec());

    query.set_subquery_key(subquery_key);

    let path_query = PathQuery::new(path, SizedQuery::new(query.clone(), Some(5), Some(2)));

    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 5);

    let first_value = 1992_u32.to_be_bytes().to_vec();
    assert_eq!(elements[0], first_value);

    let last_value = 1996_u32.to_be_bytes().to_vec();
    assert_eq!(elements[elements.len() - 1], last_value);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 5);
    compare_result_sets(&elements, &result_set);
}

#[cfg(feature = "full")]
#[test]
fn test_correct_child_root_hash_propagation_for_parent_in_same_batch() {
    let tmp_dir = TempDir::new().unwrap();
    let db = GroveDb::open(tmp_dir.path()).unwrap();
    let tree_name_slice: &[u8] = &[
        2, 17, 40, 46, 227, 17, 179, 211, 98, 50, 130, 107, 246, 26, 147, 45, 234, 189, 245, 77,
        252, 86, 99, 107, 197, 226, 188, 54, 239, 64, 17, 37,
    ];

    let batch = vec![GroveDbOp::insert_op(vec![], vec![1], Element::empty_tree())];
    db.apply_batch(batch, None, None)
        .unwrap()
        .expect("should apply batch");

    let batch = vec![
        GroveDbOp::insert_op(
            vec![vec![1]],
            tree_name_slice.to_vec(),
            Element::empty_tree(),
        ),
        GroveDbOp::insert_op(
            vec![vec![1], tree_name_slice.to_vec()],
            b"\0".to_vec(),
            Element::empty_tree(),
        ),
        GroveDbOp::insert_op(
            vec![vec![1], tree_name_slice.to_vec()],
            vec![1],
            Element::empty_tree(),
        ),
        GroveDbOp::insert_op(
            vec![vec![1], tree_name_slice.to_vec(), vec![1]],
            b"person".to_vec(),
            Element::empty_tree(),
        ),
        GroveDbOp::insert_op(
            vec![
                vec![1],
                tree_name_slice.to_vec(),
                vec![1],
                b"person".to_vec(),
            ],
            b"\0".to_vec(),
            Element::empty_tree(),
        ),
        GroveDbOp::insert_op(
            vec![
                vec![1],
                tree_name_slice.to_vec(),
                vec![1],
                b"person".to_vec(),
            ],
            b"firstName".to_vec(),
            Element::empty_tree(),
        ),
    ];
    db.apply_batch(batch, None, None)
        .unwrap()
        .expect("should apply batch");

    let batch = vec![
        GroveDbOp::insert_op(
            vec![
                vec![1],
                tree_name_slice.to_vec(),
                vec![1],
                b"person".to_vec(),
                b"\0".to_vec(),
            ],
            b"person_id_1".to_vec(),
            Element::new_item(vec![50]),
        ),
        GroveDbOp::insert_op(
            vec![
                vec![1],
                tree_name_slice.to_vec(),
                vec![1],
                b"person".to_vec(),
                b"firstName".to_vec(),
            ],
            b"cammi".to_vec(),
            Element::empty_tree(),
        ),
        GroveDbOp::insert_op(
            vec![
                vec![1],
                tree_name_slice.to_vec(),
                vec![1],
                b"person".to_vec(),
                b"firstName".to_vec(),
                b"cammi".to_vec(),
            ],
            b"\0".to_vec(),
            Element::empty_tree(),
        ),
        GroveDbOp::insert_op(
            vec![
                vec![1],
                tree_name_slice.to_vec(),
                vec![1],
                b"person".to_vec(),
                b"firstName".to_vec(),
                b"cammi".to_vec(),
                b"\0".to_vec(),
            ],
            b"person_ref_id".to_vec(),
            Element::new_reference(ReferencePathType::UpstreamRootHeightReference(
                4,
                vec![b"\0".to_vec(), b"person_id_1".to_vec()],
            )),
        ),
    ];
    db.apply_batch(batch, None, None)
        .unwrap()
        .expect("should apply batch");

    let path = vec![
        vec![1],
        tree_name_slice.to_vec(),
        vec![1],
        b"person".to_vec(),
        b"firstName".to_vec(),
    ];
    let mut query = Query::new();
    query.insert_all();
    query.set_subquery_key(b"\0".to_vec());
    let mut subquery = Query::new();
    subquery.insert_all();
    query.set_subquery(subquery);
    let path_query = PathQuery::new(
        path,
        SizedQuery {
            query: query.clone(),
            limit: Some(100),
            offset: Some(0),
        },
    );

    let proof = db
        .prove_query(&path_query)
        .unwrap()
        .expect("expected successful proving");
    let (hash, _result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
}

#[cfg(feature = "full")]
#[test]
fn test_mixed_level_proofs() {
    let db = make_test_grovedb();
    db.insert([TEST_LEAF], b"key1", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    db.insert([TEST_LEAF], b"key2", Element::new_item(vec![1]), None, None)
        .unwrap()
        .expect("successful item insert");
    db.insert([TEST_LEAF], b"key3", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF],
        b"key4",
        Element::new_reference(ReferencePathType::SiblingReference(b"key2".to_vec())),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");

    db.insert(
        [TEST_LEAF, b"key1"],
        b"k1",
        Element::new_item(vec![2]),
        None,
        None,
    )
    .unwrap()
    .expect("successful item insert");
    db.insert(
        [TEST_LEAF, b"key1"],
        b"k2",
        Element::new_item(vec![3]),
        None,
        None,
    )
    .unwrap()
    .expect("successful item insert");
    db.insert(
        [TEST_LEAF, b"key1"],
        b"k3",
        Element::new_item(vec![4]),
        None,
        None,
    )
    .unwrap()
    .expect("successful item insert");

    let mut query = Query::new();
    query.insert_all();
    let mut subquery = Query::new();
    subquery.insert_all();
    query.set_subquery(subquery);

    let path = vec![TEST_LEAF.to_vec()];

    let path_query = PathQuery::new_unsized(path.clone(), query.clone());
    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("successful get_path_query");

    assert_eq!(elements.len(), 5);
    assert_eq!(elements, vec![vec![2], vec![3], vec![4], vec![1], vec![1]]);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 5);
    compare_result_sets(&elements, &result_set);

    // Test mixed element proofs with limit and offset
    let path_query = PathQuery::new_unsized(path.clone(), query.clone());
    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("successful get_path_query");

    assert_eq!(elements.len(), 5);
    assert_eq!(elements, vec![vec![2], vec![3], vec![4], vec![1], vec![1]]);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 5);
    compare_result_sets(&elements, &result_set);

    // TODO: Fix noticed bug when limit and offset are both set to Some(0)

    let path_query = PathQuery::new(path.clone(), SizedQuery::new(query.clone(), Some(1), None));
    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("successful get_path_query");

    assert_eq!(elements.len(), 1);
    assert_eq!(elements, vec![vec![2]]);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 1);
    compare_result_sets(&elements, &result_set);

    let path_query = PathQuery::new(
        path.clone(),
        SizedQuery::new(query.clone(), Some(3), Some(0)),
    );
    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("successful get_path_query");

    assert_eq!(elements.len(), 3);
    assert_eq!(elements, vec![vec![2], vec![3], vec![4]]);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 3);
    compare_result_sets(&elements, &result_set);

    let path_query = PathQuery::new(
        path.clone(),
        SizedQuery::new(query.clone(), Some(4), Some(0)),
    );
    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("successful get_path_query");

    assert_eq!(elements.len(), 4);
    assert_eq!(elements, vec![vec![2], vec![3], vec![4], vec![1]]);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 4);
    compare_result_sets(&elements, &result_set);

    let path_query = PathQuery::new(
        path.clone(),
        SizedQuery::new(query.clone(), Some(10), Some(4)),
    );
    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("successful get_path_query");

    assert_eq!(elements.len(), 1);
    assert_eq!(elements, vec![vec![1]]);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 1);
    compare_result_sets(&elements, &result_set);

    // TODO: test return of tree in mixed element query
}

#[cfg(feature = "full")]
#[test]
fn test_mixed_level_proofs_with_tree() {
    let db = make_test_grovedb();
    db.insert([TEST_LEAF], b"key1", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    db.insert([TEST_LEAF], b"key2", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    db.insert([TEST_LEAF], b"key3", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");

    db.insert(
        [TEST_LEAF, b"key1"],
        b"k1",
        Element::new_item(vec![2]),
        None,
        None,
    )
    .unwrap()
    .expect("successful item insert");
    db.insert(
        [TEST_LEAF, b"key1"],
        b"k2",
        Element::new_item(vec![3]),
        None,
        None,
    )
    .unwrap()
    .expect("successful item insert");
    db.insert(
        [TEST_LEAF, b"key1"],
        b"k3",
        Element::new_item(vec![4]),
        None,
        None,
    )
    .unwrap()
    .expect("successful item insert");
    db.insert(
        [TEST_LEAF, b"key2"],
        b"k1",
        Element::new_item(vec![5]),
        None,
        None,
    )
    .unwrap()
    .expect("successful item insert");

    let mut query = Query::new();
    query.insert_all();
    let mut subquery = Query::new();
    subquery.insert_all();
    query.add_conditional_subquery(QueryItem::Key(b"key1".to_vec()), None, Some(subquery));

    let path = vec![TEST_LEAF.to_vec()];
    let path_query = PathQuery::new_unsized(path, query.clone());

    let (elements, _) = db
        .query_raw(
            &path_query,
            true,
            QueryResultType::QueryPathKeyElementTrioResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 5);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 5);

    // TODO: verify that the result set is exactly the same
    // compare_result_sets(&elements, &result_set);

    // TODO: test with subquery paths
    // TODO: test with limit and offset
    // TODO: add test for when the tree is empty
    // TODO: add test for subquery paths not none but empty
}
