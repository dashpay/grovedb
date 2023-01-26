// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

//! Query tests

use merk::proofs::{query::QueryItem, Query};
use rand::Rng;
use tempfile::TempDir;

use crate::{
    batch::GroveDbOp,
    query_result_type::{PathKeyOptionalElementTrio, QueryResultType},
    reference_path::ReferencePathType,
    tests::{
        common::compare_result_sets, make_deep_tree, make_test_grovedb, TempGroveDb,
        ANOTHER_TEST_LEAF, TEST_LEAF,
    },
    Element, Error, GroveDb, PathQuery, SizedQuery,
};

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 200);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 4);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 4);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 115);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 15);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 400);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 400);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 8);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 250);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 5);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 500);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 10);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 550);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 750);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 11);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 200);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 50);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 100);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 200);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 60);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 5);
    compare_result_sets(&elements, &result_set);
}

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
    let (hash, _result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
}

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 4);
    compare_result_sets(&elements, &result_set);

    let path_query = PathQuery::new(path, SizedQuery::new(query.clone(), Some(10), Some(4)));
    let (elements, _) = db
        .query_item_value(&path_query, true, None)
        .unwrap()
        .expect("successful get_path_query");

    assert_eq!(elements.len(), 1);
    assert_eq!(elements, vec![vec![1]]);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 1);
    compare_result_sets(&elements, &result_set);
}

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

    let path_query = PathQuery::new_unsized(path.clone(), query.clone());

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
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 5);

    // TODO: verify that the result set is exactly the same
    // compare_result_sets(&elements, &result_set);

    let path_query = PathQuery::new(path, SizedQuery::new(query.clone(), Some(1), None));

    let (elements, _) = db
        .query_raw(
            &path_query,
            true,
            QueryResultType::QueryPathKeyElementTrioResultType,
            None,
        )
        .unwrap()
        .expect("expected successful get_path_query");

    assert_eq!(elements.len(), 1);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 1);
    // TODO: verify that the result set is exactly the same
    // compare_result_sets(&elements, &result_set);
}

#[test]
fn test_mixed_level_proofs_with_subquery_paths() {
    let db = make_test_grovedb();
    db.insert([TEST_LEAF], b"a", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    db.insert([TEST_LEAF], b"b", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    db.insert([TEST_LEAF], b"c", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");

    db.insert([TEST_LEAF, b"a"], b"d", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF, b"a"],
        b"e",
        Element::new_item(vec![2]),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF, b"a"],
        b"f",
        Element::new_item(vec![3]),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");

    db.insert(
        [TEST_LEAF, b"a", b"d"],
        b"d",
        Element::new_item(vec![6]),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");

    db.insert(
        [TEST_LEAF, b"b"],
        b"g",
        Element::new_item(vec![4]),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    db.insert([TEST_LEAF, b"b"], b"d", Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");

    db.insert(
        [TEST_LEAF, b"b", b"d"],
        b"i",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF, b"b", b"d"],
        b"j",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF, b"b", b"d"],
        b"k",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");

    // if you don't have an item at the subquery path translation, you shouldn't be
    // added to the result set.
    let mut query = Query::new();
    query.insert_all();
    query.set_subquery_path(vec![b"d".to_vec()]);

    let path = vec![TEST_LEAF.to_vec()];

    let path_query = PathQuery::new_unsized(path, query.clone());

    // TODO: proofs seems to be more expressive than query_raw now
    // let (elements, _) = db
    // .query_raw(
    // &path_query,
    // true,
    // QueryResultType::QueryPathKeyElementTrioResultType,
    // None,
    // )
    // .unwrap()
    // .expect("expected successful get_path_query");
    //
    // assert_eq!(elements.len(), 2);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 2);

    // apply path translation then query
    let mut query = Query::new();
    query.insert_all();
    let mut subquery = Query::new();
    subquery.insert_all();
    query.set_subquery_path(vec![b"d".to_vec()]);
    query.set_subquery(subquery);

    let path = vec![TEST_LEAF.to_vec()];

    let path_query = PathQuery::new_unsized(path, query.clone());

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 4);

    // apply empty path translation
    let mut query = Query::new();
    query.insert_all();
    let mut subquery = Query::new();
    subquery.insert_all();
    query.set_subquery_path(vec![]);
    query.set_subquery(subquery);

    let path = vec![TEST_LEAF.to_vec()];

    let path_query = PathQuery::new_unsized(path, query.clone());

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 5);

    // use conditionals to return from more than 2 depth
    let mut query = Query::new();
    query.insert_all();
    let mut subquery = Query::new();
    subquery.insert_all();
    let mut deeper_subquery = Query::new();
    deeper_subquery.insert_all();
    subquery.add_conditional_subquery(QueryItem::Key(b"d".to_vec()), None, Some(deeper_subquery));
    query.add_conditional_subquery(QueryItem::Key(b"a".to_vec()), None, Some(subquery.clone()));
    query.add_conditional_subquery(QueryItem::Key(b"b".to_vec()), None, Some(subquery.clone()));

    let path = vec![TEST_LEAF.to_vec()];

    let path_query = PathQuery::new_unsized(path, query.clone());

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 8);
}

#[test]
fn test_proof_with_limit_zero() {
    let db = make_deep_tree();
    let mut query = Query::new();
    query.insert_all();
    let path_query = PathQuery::new(
        vec![TEST_LEAF.to_vec()],
        SizedQuery::new(query, Some(0), Some(0)),
    );

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 0);
}

#[test]
fn test_result_set_path_after_verification() {
    let db = make_deep_tree();
    let mut query = Query::new();
    query.insert_all();
    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 3);

    // assert the result set path
    assert_eq!(
        result_set[0].path,
        vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
    );
    assert_eq!(
        result_set[1].path,
        vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
    );
    assert_eq!(
        result_set[2].path,
        vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
    );

    assert_eq!(result_set[0].key, b"key1".to_vec());
    assert_eq!(result_set[1].key, b"key2".to_vec());
    assert_eq!(result_set[2].key, b"key3".to_vec());

    // Test path tracking with subquery
    let mut query = Query::new();
    query.insert_all();
    let mut subq = Query::new();
    subq.insert_all();
    query.set_subquery(subq);
    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 5);

    assert_eq!(
        result_set[0].path,
        vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
    );
    assert_eq!(
        result_set[1].path,
        vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
    );
    assert_eq!(
        result_set[2].path,
        vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
    );
    assert_eq!(
        result_set[3].path,
        vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()]
    );
    assert_eq!(
        result_set[4].path,
        vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()]
    );

    // Test path tracking with subquery path
    // perform a query, do a translation, perform another query
    let mut query = Query::new();
    query.insert_key(b"deep_leaf".to_vec());
    query.set_subquery_path(vec![b"deep_node_1".to_vec(), b"deeper_1".to_vec()]);
    let mut subq = Query::new();
    subq.insert_all();
    query.set_subquery(subq);
    let path_query = PathQuery::new_unsized(vec![], query);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 3);

    assert_eq!(
        result_set[0].path,
        vec![
            b"deep_leaf".to_vec(),
            b"deep_node_1".to_vec(),
            b"deeper_1".to_vec()
        ]
    );
    assert_eq!(
        result_set[1].path,
        vec![
            b"deep_leaf".to_vec(),
            b"deep_node_1".to_vec(),
            b"deeper_1".to_vec()
        ]
    );
    assert_eq!(
        result_set[2].path,
        vec![
            b"deep_leaf".to_vec(),
            b"deep_node_1".to_vec(),
            b"deeper_1".to_vec()
        ]
    );

    assert_eq!(result_set[0].key, b"key1".to_vec());
    assert_eq!(result_set[1].key, b"key2".to_vec());
    assert_eq!(result_set[2].key, b"key3".to_vec());

    // Test path tracking for mixed level result set
    let mut query = Query::new();
    query.insert_all();
    let mut subq = Query::new();
    subq.insert_all();
    query.add_conditional_subquery(QueryItem::Key(b"innertree".to_vec()), None, Some(subq));

    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query_raw(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 4);

    assert_eq!(
        result_set[0].path,
        vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
    );
    assert_eq!(
        result_set[1].path,
        vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
    );
    assert_eq!(
        result_set[2].path,
        vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
    );
    assert_eq!(result_set[3].path, vec![TEST_LEAF.to_vec()]);

    assert_eq!(result_set[0].key, b"key1".to_vec());
    assert_eq!(result_set[1].key, b"key2".to_vec());
    assert_eq!(result_set[2].key, b"key3".to_vec());
    assert_eq!(result_set[3].key, b"innertree4".to_vec());
}

#[test]
fn test_verification_with_path_key_optional_element_trio() {
    let db = make_deep_tree();
    let mut query = Query::new();
    query.insert_all();
    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"innertree".to_vec()], query);

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 3);

    assert_eq!(
        result_set[0],
        (
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
            b"key1".to_vec(),
            Some(Element::new_item(b"value1".to_vec()))
        )
    );
    assert_eq!(
        result_set[1],
        (
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
            b"key2".to_vec(),
            Some(Element::new_item(b"value2".to_vec()))
        )
    );
    assert_eq!(
        result_set[2],
        (
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
            b"key3".to_vec(),
            Some(Element::new_item(b"value3".to_vec()))
        )
    );
}

#[test]
fn test_absence_proof() {
    let db = make_deep_tree();

    // simple case, request for items k2..=k5 under inner tree
    // we pass them as keys as terminal keys does not handle ranges with start or
    // end len greater than 1 k2, k3 should be Some, k4, k5 should be None, k1,
    // k6.. should not be in map
    let mut query = Query::new();
    query.insert_key(b"key2".to_vec());
    query.insert_key(b"key3".to_vec());
    query.insert_key(b"key4".to_vec());
    query.insert_key(b"key5".to_vec());
    let path_query = PathQuery::new(
        vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
        SizedQuery::new(query, Some(4), None),
    );

    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query_with_absence_proof(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 4);

    assert_eq!(
        result_set[0].0,
        vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
    );
    assert_eq!(
        result_set[1].0,
        vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
    );
    assert_eq!(
        result_set[2].0,
        vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
    );
    assert_eq!(
        result_set[3].0,
        vec![TEST_LEAF.to_vec(), b"innertree".to_vec()]
    );

    assert_eq!(result_set[0].1, b"key2".to_vec());
    assert_eq!(result_set[1].1, b"key3".to_vec());
    assert_eq!(result_set[2].1, b"key4".to_vec());
    assert_eq!(result_set[3].1, b"key5".to_vec());

    assert_eq!(result_set[0].2, Some(Element::new_item(b"value2".to_vec())));
    assert_eq!(result_set[1].2, Some(Element::new_item(b"value3".to_vec())));
    assert_eq!(result_set[2].2, None);
    assert_eq!(result_set[3].2, None);
}

#[test]
fn test_subset_proof_verification() {
    let db = make_deep_tree();

    // original path query
    let mut query = Query::new();
    query.insert_all();
    let mut subq = Query::new();
    subq.insert_all();
    query.set_subquery(subq);

    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

    // first we prove non-verbose
    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 5);
    assert_eq!(
        result_set[0],
        (
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
            b"key1".to_vec(),
            Some(Element::new_item(b"value1".to_vec()))
        )
    );
    assert_eq!(
        result_set[1],
        (
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
            b"key2".to_vec(),
            Some(Element::new_item(b"value2".to_vec()))
        )
    );
    assert_eq!(
        result_set[2],
        (
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
            b"key3".to_vec(),
            Some(Element::new_item(b"value3".to_vec()))
        )
    );
    assert_eq!(
        result_set[3],
        (
            vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()],
            b"key4".to_vec(),
            Some(Element::new_item(b"value4".to_vec()))
        )
    );
    assert_eq!(
        result_set[4],
        (
            vec![TEST_LEAF.to_vec(), b"innertree4".to_vec()],
            b"key5".to_vec(),
            Some(Element::new_item(b"value5".to_vec()))
        )
    );

    // prove verbose
    let verbose_proof = db.prove_verbose(&path_query).unwrap().unwrap();
    assert!(verbose_proof.len() > proof.len());

    // subset path query
    let mut query = Query::new();
    query.insert_key(b"innertree".to_vec());
    let mut subq = Query::new();
    subq.insert_key(b"key1".to_vec());
    query.set_subquery(subq);
    let subset_path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

    let (hash, result_set) =
        GroveDb::verify_subset_query(&verbose_proof, &subset_path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 1);
    assert_eq!(
        result_set[0],
        (
            vec![TEST_LEAF.to_vec(), b"innertree".to_vec()],
            b"key1".to_vec(),
            Some(Element::new_item(b"value1".to_vec()))
        )
    );

    // should not allow verbose proof generation if limit is set
    let path_query_with_limit = {
        let mut cloned_path_query = path_query.clone();
        cloned_path_query.query.limit = Some(10);
        cloned_path_query
    };
    let verbose_proof_result = db.prove_verbose(&path_query_with_limit).unwrap();
    assert!(matches!(
        verbose_proof_result,
        Err(Error::InvalidInput(
            "cannot generate verbose proof for path-query with a limit or offset value"
        ))
    ));

    // should not allow verbose proof generation if offset is set
    let path_query_with_offset = {
        let mut cloned_path_query = path_query.clone();
        cloned_path_query.query.offset = Some(10);
        cloned_path_query
    };
    let verbose_proof_result = db.prove_verbose(&path_query_with_offset).unwrap();
    assert!(matches!(
        verbose_proof_result,
        Err(Error::InvalidInput(
            "cannot generate verbose proof for path-query with a limit or offset value"
        ))
    ));
}

#[test]
fn test_chained_path_query_verification() {
    let db = make_deep_tree();

    let mut query = Query::new();
    query.insert_all();
    let mut subq = Query::new();
    subq.insert_all();
    let mut subsubq = Query::new();
    subsubq.insert_all();

    subq.set_subquery(subsubq);
    query.set_subquery(subq);

    let path_query = PathQuery::new_unsized(vec![b"deep_leaf".to_vec()], query);

    // first prove non verbose
    let proof = db.prove_query(&path_query).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 11);

    // prove verbose
    let verbose_proof = db.prove_verbose(&path_query).unwrap().unwrap();
    assert!(verbose_proof.len() > proof.len());

    // init deeper_1 path query
    let mut query = Query::new();
    query.insert_all();

    let deeper_1_path_query = PathQuery::new_unsized(
        vec![
            b"deep_leaf".to_vec(),
            b"deep_node_1".to_vec(),
            b"deeper_1".to_vec(),
        ],
        query,
    );

    // define the path query generators
    let mut chained_path_queries = vec![];
    chained_path_queries.push(|elements: Vec<PathKeyOptionalElementTrio>| {
        let mut query = Query::new();
        query.insert_all();

        let deeper_2_path_query = PathQuery::new_unsized(
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_1".to_vec(),
                b"deeper_2".to_vec(),
            ],
            query,
        );
        return Some(deeper_2_path_query);
    });

    // verify the path query chain
    let (root_hash, results) = GroveDb::verify_query_with_chained_path_queries(
        &verbose_proof,
        &deeper_1_path_query,
        chained_path_queries,
    )
    .unwrap();
    assert_eq!(root_hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].len(), 3);
    assert_eq!(
        results[0][0],
        (
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_1".to_vec(),
                b"deeper_1".to_vec()
            ],
            b"key1".to_vec(),
            Some(Element::new_item(b"value1".to_vec()))
        )
    );
    assert_eq!(
        results[0][1],
        (
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_1".to_vec(),
                b"deeper_1".to_vec()
            ],
            b"key2".to_vec(),
            Some(Element::new_item(b"value2".to_vec()))
        )
    );
    assert_eq!(
        results[0][2],
        (
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_1".to_vec(),
                b"deeper_1".to_vec()
            ],
            b"key3".to_vec(),
            Some(Element::new_item(b"value3".to_vec()))
        )
    );

    assert_eq!(results[1].len(), 3);
    assert_eq!(
        results[1][0],
        (
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_1".to_vec(),
                b"deeper_2".to_vec()
            ],
            b"key4".to_vec(),
            Some(Element::new_item(b"value4".to_vec()))
        )
    );
    assert_eq!(
        results[1][1],
        (
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_1".to_vec(),
                b"deeper_2".to_vec()
            ],
            b"key5".to_vec(),
            Some(Element::new_item(b"value5".to_vec()))
        )
    );
    assert_eq!(
        results[1][2],
        (
            vec![
                b"deep_leaf".to_vec(),
                b"deep_node_1".to_vec(),
                b"deeper_2".to_vec()
            ],
            b"key6".to_vec(),
            Some(Element::new_item(b"value6".to_vec()))
        )
    );
}
