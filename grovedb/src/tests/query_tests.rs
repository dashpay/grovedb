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

use grovedb_merk::{
    execute_proof,
    proofs::{query::QueryItem, Query},
};
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
    Element, GroveDb, PathQuery, SizedQuery,
};

fn populate_tree_for_non_unique_range_subquery(db: &TempGroveDb) {
    // Insert a couple of subtrees first
    for i in 1985u32..2000 {
        let i_vec = (i as u32).to_be_bytes().to_vec();
        db.insert(
            [TEST_LEAF].as_ref(),
            &i_vec,
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
        // Insert element 0
        // Insert some elements into subtree
        db.insert(
            [TEST_LEAF, i_vec.as_slice()].as_ref(),
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
                [TEST_LEAF, i_vec.as_slice(), b"\0"].as_ref(),
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
        db.insert(
            [TEST_LEAF].as_ref(),
            &i_vec,
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
        // Insert element 0
        // Insert some elements into subtree
        db.insert(
            [TEST_LEAF, i_vec.as_slice()].as_ref(),
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
                [TEST_LEAF, i_vec.as_slice(), b"a"].as_ref(),
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
                [TEST_LEAF, i_vec.as_slice(), b"a", j_vec.as_slice()].as_ref(),
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
                    [TEST_LEAF, i_vec.as_slice(), b"a", &j_vec, b"\0"].as_ref(),
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
    db.insert(
        [TEST_LEAF].as_ref(),
        b"\0",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");

    // This subtree will be holding references
    db.insert(
        [TEST_LEAF].as_ref(),
        b"1",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    // Insert a couple of subtrees first
    for i in 1985u32..2000 {
        let i_vec = (i as u32).to_be_bytes().to_vec();
        db.insert(
            [TEST_LEAF, b"1"].as_ref(),
            &i_vec,
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");
        // Insert element 0
        // Insert some elements into subtree
        db.insert(
            [TEST_LEAF, b"1", i_vec.as_slice()].as_ref(),
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
                [TEST_LEAF, b"\0"].as_ref(),
                &random_key,
                Element::new_item(j_vec.clone()),
                None,
                None,
            )
            .unwrap()
            .expect("successful value insert");

            db.insert(
                [TEST_LEAF, b"1", i_vec.clone().as_slice(), b"\0"].as_ref(),
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
        db.insert(
            [TEST_LEAF].as_ref(),
            &i_vec,
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");

        db.insert(
            [TEST_LEAF, &i_vec.clone()].as_ref(),
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
    db.insert(
        [TEST_LEAF].as_ref(),
        b"\0",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");

    // This subtree will be holding references
    db.insert(
        [TEST_LEAF].as_ref(),
        b"1",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");

    for i in 1985u32..2000 {
        let i_vec = (i as u32).to_be_bytes().to_vec();
        db.insert(
            [TEST_LEAF, b"1"].as_ref(),
            &i_vec,
            Element::empty_tree(),
            None,
            None,
        )
        .unwrap()
        .expect("successful subtree insert");

        // We should insert every item to the tree holding items
        db.insert(
            [TEST_LEAF, b"\0"].as_ref(),
            &i_vec,
            Element::new_item(i_vec.clone()),
            None,
            None,
        )
        .unwrap()
        .expect("successful value insert");

        // We should insert a reference to the item
        db.insert(
            [TEST_LEAF, b"1", i_vec.clone().as_slice()].as_ref(),
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
    db.insert([TEST_LEAF].as_ref(), &[], Element::empty_tree(), None, None)
        .unwrap()
        .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF, &[]].as_ref(),
        b"\0",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    // Insert a couple of subtrees first
    for i in 100u32..200 {
        let i_vec = (i as u32).to_be_bytes().to_vec();
        db.insert(
            [TEST_LEAF, &[], b"\0"].as_ref(),
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
    db.insert(
        [TEST_LEAF].as_ref(),
        b"key1",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"key2",
        Element::new_item(vec![1]),
        None,
        None,
    )
    .unwrap()
    .expect("successful item insert");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"key3",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"key4",
        Element::new_reference(ReferencePathType::SiblingReference(b"key2".to_vec())),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");

    db.insert(
        [TEST_LEAF, b"key1"].as_ref(),
        b"k1",
        Element::new_item(vec![2]),
        None,
        None,
    )
    .unwrap()
    .expect("successful item insert");
    db.insert(
        [TEST_LEAF, b"key1"].as_ref(),
        b"k2",
        Element::new_item(vec![3]),
        None,
        None,
    )
    .unwrap()
    .expect("successful item insert");
    db.insert(
        [TEST_LEAF, b"key1"].as_ref(),
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
    db.insert(
        [TEST_LEAF].as_ref(),
        b"key1",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"key2",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"key3",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");

    db.insert(
        [TEST_LEAF, b"key1"].as_ref(),
        b"k1",
        Element::new_item(vec![2]),
        None,
        None,
    )
    .unwrap()
    .expect("successful item insert");
    db.insert(
        [TEST_LEAF, b"key1"].as_ref(),
        b"k2",
        Element::new_item(vec![3]),
        None,
        None,
    )
    .unwrap()
    .expect("successful item insert");
    db.insert(
        [TEST_LEAF, b"key1"].as_ref(),
        b"k3",
        Element::new_item(vec![4]),
        None,
        None,
    )
    .unwrap()
    .expect("successful item insert");
    db.insert(
        [TEST_LEAF, b"key2"].as_ref(),
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
    db.insert(
        [TEST_LEAF].as_ref(),
        b"a",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"b",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF].as_ref(),
        b"c",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");

    db.insert(
        [TEST_LEAF, b"a"].as_ref(),
        b"d",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF, b"a"].as_ref(),
        b"e",
        Element::new_item(vec![2]),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF, b"a"].as_ref(),
        b"f",
        Element::new_item(vec![3]),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");

    db.insert(
        [TEST_LEAF, b"a", b"d"].as_ref(),
        b"d",
        Element::new_item(vec![6]),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");

    db.insert(
        [TEST_LEAF, b"b"].as_ref(),
        b"g",
        Element::new_item(vec![4]),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF, b"b"].as_ref(),
        b"d",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");

    db.insert(
        [TEST_LEAF, b"b", b"d"].as_ref(),
        b"i",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF, b"b", b"d"].as_ref(),
        b"j",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("successful subtree insert");
    db.insert(
        [TEST_LEAF, b"b", b"d"].as_ref(),
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
    chained_path_queries.push(|_elements: Vec<PathKeyOptionalElementTrio>| {
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
        Some(deeper_2_path_query)
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

#[test]
fn test_query_b_depends_on_query_a() {
    // we have two trees
    // one with a mapping of id to name
    // another with a mapping of name to age
    // we want to get the age of every one after a certain id ordered by name
    let db = make_test_grovedb();

    // TEST_LEAF contains the id to name mapping
    db.insert(
        [TEST_LEAF].as_ref(),
        &[1],
        Element::new_item(b"d".to_vec()),
        None,
        None,
    )
    .unwrap()
    .expect("successful root tree leaf insert");
    db.insert(
        [TEST_LEAF].as_ref(),
        &[2],
        Element::new_item(b"b".to_vec()),
        None,
        None,
    )
    .unwrap()
    .expect("successful root tree leaf insert");
    db.insert(
        [TEST_LEAF].as_ref(),
        &[3],
        Element::new_item(b"c".to_vec()),
        None,
        None,
    )
    .unwrap()
    .expect("successful root tree leaf insert");
    db.insert(
        [TEST_LEAF].as_ref(),
        &[4],
        Element::new_item(b"a".to_vec()),
        None,
        None,
    )
    .unwrap()
    .expect("successful root tree leaf insert");

    // ANOTHER_TEST_LEAF contains the name to age mapping
    db.insert(
        [ANOTHER_TEST_LEAF].as_ref(),
        b"a",
        Element::new_item(vec![10]),
        None,
        None,
    )
    .unwrap()
    .expect("successful root tree leaf insert");
    db.insert(
        [ANOTHER_TEST_LEAF].as_ref(),
        b"b",
        Element::new_item(vec![30]),
        None,
        None,
    )
    .unwrap()
    .expect("successful root tree leaf insert");
    db.insert(
        [ANOTHER_TEST_LEAF].as_ref(),
        b"c",
        Element::new_item(vec![12]),
        None,
        None,
    )
    .unwrap()
    .expect("successful root tree leaf insert");
    db.insert(
        [ANOTHER_TEST_LEAF].as_ref(),
        b"d",
        Element::new_item(vec![46]),
        None,
        None,
    )
    .unwrap()
    .expect("successful root tree leaf insert");

    // Query: return the age of everyone greater than id 2 ordered by name
    // id 2 - b
    // so we want to return the age for c and d = 12, 46 respectively
    // the proof generator knows that id 2 = b, but the verifier doesn't
    // hence we need to generate two proofs
    // prove that 2 - b then prove age after b
    // the verifier has to use the result of the first proof 2 - b
    // to generate the path query for the verification of the second proof

    // query name associated with id 2
    let mut query = Query::new();
    query.insert_key(vec![2]);
    let mut path_query_one = PathQuery::new_unsized(vec![TEST_LEAF.to_vec()], query);

    // first we show that this returns the correct output
    let proof = db.prove_query(&path_query_one).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query_one).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 1);
    assert_eq!(result_set[0].2, Some(Element::new_item(b"b".to_vec())));

    // next query should return the age for elements above b
    let mut query = Query::new();
    query.insert_range_after(b"b".to_vec()..);
    let path_query_two = PathQuery::new_unsized(vec![ANOTHER_TEST_LEAF.to_vec()], query);

    // show that we get the correct output
    let proof = db.prove_query(&path_query_two).unwrap().unwrap();
    let (hash, result_set) = GroveDb::verify_query(&proof, &path_query_two).unwrap();
    assert_eq!(hash, db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 2);
    assert_eq!(result_set[0].2, Some(Element::new_item(vec![12])));
    assert_eq!(result_set[1].2, Some(Element::new_item(vec![46])));

    // now we merge the path queries
    let mut merged_path_queries = PathQuery::merge(vec![&path_query_one, &path_query_two]).unwrap();
    merged_path_queries.query.limit = Some(3);
    let proof = db.prove_verbose(&merged_path_queries).unwrap().unwrap();

    // verifier only has access to the statement age > 2
    // need to first get the name associated with 2 from the proof
    // then use that to construct the next path query
    let mut chained_path_queries = vec![];
    chained_path_queries.push(|prev_elements: Vec<PathKeyOptionalElementTrio>| {
        let mut query = Query::new();
        let name_element = prev_elements[0].2.as_ref().unwrap();
        if let Element::Item(name, ..) = name_element {
            query.insert_range_after(name.to_owned()..);
            Some(PathQuery::new(
                vec![ANOTHER_TEST_LEAF.to_vec()],
                SizedQuery::new(query, Some(2), None),
            ))
        } else {
            None
        }
    });

    // add limit to path query one
    path_query_one.query.limit = Some(1);

    let (_, result_set) = GroveDb::verify_query_with_chained_path_queries(
        proof.as_slice(),
        &path_query_one,
        chained_path_queries,
    )
    .unwrap();
    assert_eq!(result_set.len(), 2);
    assert_eq!(result_set[0].len(), 1);
    assert_eq!(result_set[1].len(), 2);

    let age_result = result_set[1].clone();
    assert_eq!(age_result[0].2, Some(Element::new_item(vec![12])));
    assert_eq!(age_result[1].2, Some(Element::new_item(vec![46])));
}

#[test]
fn test_bad_path_proof_bug() {
    // let proof_str =
    // "02c007032000950b5d6a55e9bdec22c758c964fb47900b0f396b7ba53108ed7788\
    // dce2e689015a00fb31010000950b5d6a55e9bdec22c758c964fb47900b0f396b7ba53108ed7788dce2e6891f1\
    // 8de83f3a647d49a6913fa90dfcec5513e27de08a7de55e7d4b3b3b6272dc10300000187c5bb01e00100000187\
    // d007b9e0e46e8c0e985601140144784f09e501d4596026ccd3879ba000e549831a28bb815f75a3879fe6a03b5\
    // 69ff0d9e65c01d7070b78ab825a3fad93745b496710f7864dec9c2c6ec268b3ed6af52bbb845cef420f25e546\
    // 72bee812b7e69085cc5e812ef2e7006344380e2244e6e042a9b8f973f71f1b3c347136182f1e829a3f195f779\
    // 70e96c0438373685822e719788b179235cd976854369cce40b70aa1a3a62312b6b6e3fcd624932caa50726b82\
    // 3fcb7ee49b6a6521fd5a47c6f88e97e5c5b2049e183418fbf000d1237cd8a0a075a11a1c452aa6ddaa544d176\
    // f6e49280123021f18de83f3a647d49a6913fa90dfcec5513e27de08a7de55e7d4b3b3b6272dc100000147fc79\
    // b91b37fc867c781b349177879916bdbf67a78e8b85da25e1f134ccbfd11102bef6e5877084784ac67862cf425\
    // 5d45630514270dfe6c0a52db33a37b6d6d1e61001c2f372374eb9a11852e173f1cf6fdc28fc8d5eb77b449838\
    // f4b59572dc77b3011102c8da15dd3153f89e793f03d4666d0fdbb9479da3813e2f38c3c1f0f5a602249510013\
    // c56f79eb5a2c7b8c448d6cae38d1573c378b2a4e0943ddf2eb0546cd7e662b71102b458d209beec86dcf7ec93\
    // a7e9c7fcc573ca39cd20522efcb6fc5605b60db2b810013674b0dac7d276761b21a5258d1e16e72315d19459b\
    // b9edf0412f7db1e3962f5110243af1ce722cb6a5487d91bcf2c3abf2d8edcbc64ea3ac272da522aa64df2c11d\
    // 1001aba2c1df40ac35f24f4569aebb978ed6ff46415615d6e04dad02c3e29263b9031102600f491e1fb37be0e\
    // 5e13ca7ec71b7370ff664a86d864b83278d85c1d95dec7b1001519ea0e5704987965293dec9d2b1761e2a0306\
    // 594fcae98f02fc74fd4c853e6c1102e10c0fe164f228677474fea91c776698c56fe476e91ed0cc0ad35c39c90\
    // b0a9410010b98380790c3fb9566d011493c31aad3c3efcfc08559a1eeb039f4d65c39a5c0110247a9a209dc9b\
    // 0239871f048008363b7db37797c6cf7df5d8953785ff551b9dff10015c13458e8391595f061f37ffb8e8226ff\
    // d085515cd57590edfc64be94749b4c01102f04d587a340f1bb9e1557e44c20540a2c356824a13eb9e101ec5be\
    // 38caefce111001d99e85f9eda7051e69bfc0ab30680f649e295decf278672188a5350b101ee89311018d01040\
    // 10000240201209a3a6a49bf7cb5da00f9de8121de2dce1b80440d74c7248bbcd4eef485a2d5af006c58f37166\
    // 4183c13b16e9c22bbf1742ae40ae4540f15a85f4c1d1e9e26fde0f0218ae49cb7c974a2875076c57f7949ae59\
    // e43b9702443750b38c9ed47ba3d0e291001a1eb79c8414ed260509ae17d05c700b15bc8741e7ddd82a624724b\
    // 758c2fcea31101820101ec994bfe357cd190394abbe6902dc1e02f06ef5f18da23d30f998584a8ca2f81040e6\
    // 36f6e7461637452657175657374000c020108246f776e6572496400a29d8de42c8a728d0ec1efb0d8bcf965c0\
    // e202fa2aa1d19e7f97dff566ececea100158b67b6e8b463f9708562719d93291d402a2839bbc9387b1c49bfb1\
    // 04391bc5511015901ed330a0a547934ccc68285410c83ce43f63703dbf5d582527338aaa7b3cb935504010100\
    // 1202010e636f6e7461637452657175657374000914897045efb638270025542aa4c8194894cf5b961b8e39f20\
    // 1daa0a57d95111001d101014dc5830cd415eb9f58a34f8533b52e3bed1c79964c32a0c2a7b8586eb17898e502\
    // 7a04d752f9be265fad97be31d25a400a9ce4f79465b5a7107d1e5dbed9815ae41004204f9c5807f97951d06b2\
    // daa6a625e060f4f3fb1f6badde15ef8705955cb1f0b41000502010101000b3e06f37ffdebeea2ab51c5c43f44\
    // 08d79c480fe50070250acd840bee02604c1102a2a0da80d93d0fa7e9bab7d9d9bfcebc7a6fa7e506a59e076cb\
    // f9adc5eb46752100107441373b76b283c66aeb2f1e2f0f86aab28a68cf7851b5779a4b59a677c0dd011018d01\
    // 01aa81b5bb2f73c3f9f9cbb36cb663b8e5dbcd224da5cc48f7030b47935d069acf0401400024020120a2a1b4a\
    // c6fef22ea2a1a68e8123644b357875f6b412c18109281c146e7b271bc00ce56d57f1547408426db61f3323a15\
    // 2829b2a11a5e9176e60f9c3cbffeb0964d10012c53c18e8905f139ab3d3ef09729f9b54debb7c2caccf73bbea\
    // 588cd702e746f11";

    // let proof_str =
    // "02c007032000950b5d6a55e9bdec22c758c964fb47900b0f396b7ba53108ed7788dce2e689015a00fb31010000950b5d6a55e9bdec22c758c964fb47900b0f396b7ba53108ed7788dce2e6891f18de83f3a647d49a6913fa90dfcec5513e27de08a7de55e7d4b3b3b6272dc10300000187c5bb01e00100000187d007b9e0e46e8c0e985601140144784f09e501d4596026ccd3879ba000e549831a28bb815f75a3879fe6a03b569ff0d9e65c01d7070b78ab825a3fad93745b496710f7864dec9c2c6ec268b3ed6af52bbb845cef420f25e54672bee812b7e69085cc5e812ef2e7006344380e2244e6e042a9b8f973f71f1b3c347136182f1e829a3f195f77970e96c0438373685822e719788b179235cd976854369cce40b70aa1a3a62312b6b6e3fcd624932caa50726b823fcb7ee49b6a6521fd5a47c6f88e97e5c5b2049e183418fbf000d1237cd8a0a075a11a1c452aa6ddaa544d176f6e49280123021f18de83f3a647d49a6913fa90dfcec5513e27de08a7de55e7d4b3b3b6272dc100000147fc79b91b37fc867c781b349177879916bdbf67a78e8b85da25e1f134ccbfd11102bef6e5877084784ac67862cf4255d45630514270dfe6c0a52db33a37b6d6d1e61001c2f372374eb9a11852e173f1cf6fdc28fc8d5eb77b449838f4b59572dc77b3011102c8da15dd3153f89e793f03d4666d0fdbb9479da3813e2f38c3c1f0f5a602249510013c56f79eb5a2c7b8c448d6cae38d1573c378b2a4e0943ddf2eb0546cd7e662b71102b458d209beec86dcf7ec93a7e9c7fcc573ca39cd20522efcb6fc5605b60db2b810013674b0dac7d276761b21a5258d1e16e72315d19459bb9edf0412f7db1e3962f5110243af1ce722cb6a5487d91bcf2c3abf2d8edcbc64ea3ac272da522aa64df2c11d1001aba2c1df40ac35f24f4569aebb978ed6ff46415615d6e04dad02c3e29263b9031102600f491e1fb37be0e5e13ca7ec71b7370ff664a86d864b83278d85c1d95dec7b1001519ea0e5704987965293dec9d2b1761e2a0306594fcae98f02fc74fd4c853e6c1102e10c0fe164f228677474fea91c776698c56fe476e91ed0cc0ad35c39c90b0a9410010b98380790c3fb9566d011493c31aad3c3efcfc08559a1eeb039f4d65c39a5c0110247a9a209dc9b0239871f048008363b7db37797c6cf7df5d8953785ff551b9dff10015c13458e8391595f061f37ffb8e8226ffd085515cd57590edfc64be94749b4c01102f04d587a340f1bb9e1557e44c20540a2c356824a13eb9e101ec5be38caefce111001d99e85f9eda7051e69bfc0ab30680f649e295decf278672188a5350b101ee89311018d0104010000240201209a3a6a49bf7cb5da00f9de8121de2dce1b80440d74c7248bbcd4eef485a2d5af006c58f371664183c13b16e9c22bbf1742ae40ae4540f15a85f4c1d1e9e26fde0f0218ae49cb7c974a2875076c57f7949ae59e43b9702443750b38c9ed47ba3d0e291001a1eb79c8414ed260509ae17d05c700b15bc8741e7ddd82a624724b758c2fcea31101820101ec994bfe357cd190394abbe6902dc1e02f06ef5f18da23d30f998584a8ca2f81040e636f6e7461637452657175657374000c020108246f776e6572496400a29d8de42c8a728d0ec1efb0d8bcf965c0e202fa2aa1d19e7f97dff566ececea100158b67b6e8b463f9708562719d93291d402a2839bbc9387b1c49bfb104391bc5511015901ed330a0a547934ccc68285410c83ce43f63703dbf5d582527338aaa7b3cb9355040101001202010e636f6e7461637452657175657374000914897045efb638270025542aa4c8194894cf5b961b8e39f201daa0a57d95111001d101014dc5830cd415eb9f58a34f8533b52e3bed1c79964c32a0c2a7b8586eb17898e5027a04d752f9be265fad97be31d25a400a9ce4f79465b5a7107d1e5dbed9815ae41004204f9c5807f97951d06b2daa6a625e060f4f3fb1f6badde15ef8705955cb1f0b41000502010101000b3e06f37ffdebeea2ab51c5c43f4408d79c480fe50070250acd840bee02604c1102a2a0da80d93d0fa7e9bab7d9d9bfcebc7a6fa7e506a59e076cbf9adc5eb46752100107441373b76b283c66aeb2f1e2f0f86aab28a68cf7851b5779a4b59a677c0dd011018d0101aa81b5bb2f73c3f9f9cbb36cb663b8e5dbcd224da5cc48f7030b47935d069acf0401400024020120a2a1b4ac6fef22ea2a1a68e8123644b357875f6b412c18109281c146e7b271bc00ce56d57f1547408426db61f3323a152829b2a11a5e9176e60f9c3cbffeb0964d10012c53c18e8905f139ab3d3ef09729f9b54debb7c2caccf73bbea588cd702e746f11"
    // ;

    // let proof_str =
    // "02cf0601738d92261a22ea1b3644faadbc9b7ba643b266fda3dc207b344e583328e1d5530247a9a209dc9b0239871f048008363b7db37797c6cf7df5d8953785ff551b9dff10018269516f2583fb3fd8f0d52a42dcde655e8a97e66514def8d18d97a39ff352a80229117f4f0184ee7293eb83388861737d692fbebb53a6ee565af5339c9c20d6a2100156c3a0e1fc3aa510101439f1ba52797f1665ccd9f694f4888d540c2ca776c7ef02b06c2338509cae8a126baf35896f1cc1ced199cd8c913b23d131c60cf3b7c0e0100320794eef73c3ad765a0d8195d4e0e9489193e515b996b9b1b162376669982d24cf014f00fb260100794eef73c3ad765a0d8195d4e0e9489193e515b996b9b1b162376669982d24cfb6f4889272e651ea8b741822dc5e5b9a931c3ee69e7d076b927d2cacaeaec8700100000187d007b9e0003f36660327fcaf45014127aaa00294c311d9ee7d85dc21127ab3d02d28bfdbb588bea8eec860df07cd55c96adac7c787f6d9c4a7b77d4744e8f07c3266e04d40d0428e468a0c257be99b635e10ba064ebb6e0c2e753ffa289cb34cc4707bedbf080d510ade035a1ef1b439dcb9c8e7317cffd89ccde5ba4d11ebf53ab4f86032ad8e03bd20dbd48c1c9cab07a860bd16c5af05ba00614920e1e39fc50c62949bbdba924a8ec8ba1d2447d1d1cf6277060aedfbd6ed88ed6200caa7efe129b518cd0d95b8807d158af4d2d61ef838d2e7b7e87198a0e2d36e1f0289012302b6f4889272e651ea8b741822dc5e5b9a931c3ee69e7d076b927d2cacaeaec87000011102ba9008e2c9276664f852c90aed49064c98097f5eeecdbbd02225f0dadd78346f1001022b734078d681ba110a290a3c9c08d2b253a8f9ee4d78ce5f87f02b613e9b131102db426ce6c6b20fd6c4ed4eeb03e58708959ef0a3bd2014867b0641f673f92f3a100148f579406ab870d1b86dc5aad9f69daa79446ecb932ae61da68f82d77ef9424c111102e55ce602107d46937ae88ca72075d202a44523b67a2d85bc9b7dce1b36e4aa7510016593a9d796ac363aea6a34791c4b6024f8d643708ae79d0b46115283ec89f41a111102f04d587a340f1bb9e1557e44c20540a2c356824a13eb9e101ec5be38caefce111001d99e85f9eda7051e69bfc0ab30680f649e295decf278672188a5350b101ee89311018d0104010000240201209a3a6a49bf7cb5da00f9de8121de2dce1b80440d74c7248bbcd4eef485a2d5af006c58f371664183c13b16e9c22bbf1742ae40ae4540f15a85f4c1d1e9e26fde0f0218ae49cb7c974a2875076c57f7949ae59e43b9702443750b38c9ed47ba3d0e291001a1eb79c8414ed260509ae17d05c700b15bc8741e7ddd82a624724b758c2fcea31101820101ec994bfe357cd190394abbe6902dc1e02f06ef5f18da23d30f998584a8ca2f81040e636f6e7461637452657175657374000c020108246f776e6572496400a29d8de42c8a728d0ec1efb0d8bcf965c0e202fa2aa1d19e7f97dff566ececea100158b67b6e8b463f9708562719d93291d402a2839bbc9387b1c49bfb104391bc5511015901ed330a0a547934ccc68285410c83ce43f63703dbf5d582527338aaa7b3cb9355040101001202010e636f6e7461637452657175657374000914897045efb638270025542aa4c8194894cf5b961b8e39f201daa0a57d95111001d101014dc5830cd415eb9f58a34f8533b52e3bed1c79964c32a0c2a7b8586eb17898e5027a04d752f9be265fad97be31d25a400a9ce4f79465b5a7107d1e5dbed9815ae41004204f9c5807f97951d06b2daa6a625e060f4f3fb1f6badde15ef8705955cb1f0b41000502010101000b3e06f37ffdebeea2ab51c5c43f4408d79c480fe50070250acd840bee02604c1102a2a0da80d93d0fa7e9bab7d9d9bfcebc7a6fa7e506a59e076cbf9adc5eb46752100107441373b76b283c66aeb2f1e2f0f86aab28a68cf7851b5779a4b59a677c0dd011018d0101aa81b5bb2f73c3f9f9cbb36cb663b8e5dbcd224da5cc48f7030b47935d069acf0401400024020120a2a1b4ac6fef22ea2a1a68e8123644b357875f6b412c18109281c146e7b271bc00ce56d57f1547408426db61f3323a152829b2a11a5e9176e60f9c3cbffeb0964d10012c53c18e8905f139ab3d3ef09729f9b54debb7c2caccf73bbea588cd702e746f11"
    // ;

    // let proof = hex::decode(proof_str).expect("should decode proof");

    let proof = vec![
        0, 2, 192, 7, 3, 32, 0, 149, 11, 93, 106, 85, 233, 189, 236, 34, 199, 88, 201, 100, 251,
        71, 144, 11, 15, 57, 107, 123, 165, 49, 8, 237, 119, 136, 220, 226, 230, 137, 1, 90, 0,
        251, 49, 1, 0, 0, 149, 11, 93, 106, 85, 233, 189, 236, 34, 199, 88, 201, 100, 251, 71, 144,
        11, 15, 57, 107, 123, 165, 49, 8, 237, 119, 136, 220, 226, 230, 137, 31, 24, 222, 131, 243,
        166, 71, 212, 154, 105, 19, 250, 144, 223, 206, 197, 81, 62, 39, 222, 8, 167, 222, 85, 231,
        212, 179, 179, 182, 39, 45, 193, 3, 0, 0, 1, 135, 197, 187, 1, 224, 1, 0, 0, 1, 135, 208,
        7, 185, 224, 228, 110, 140, 14, 152, 86, 1, 20, 1, 68, 120, 79, 9, 229, 1, 212, 89, 96, 38,
        204, 211, 135, 155, 160, 0, 229, 73, 131, 26, 40, 187, 129, 95, 117, 163, 135, 159, 230,
        160, 59, 86, 159, 240, 217, 230, 92, 1, 215, 7, 11, 120, 171, 130, 90, 63, 173, 147, 116,
        91, 73, 103, 16, 247, 134, 77, 236, 156, 44, 110, 194, 104, 179, 237, 106, 245, 43, 187,
        132, 92, 239, 66, 15, 37, 229, 70, 114, 190, 232, 18, 183, 230, 144, 133, 204, 94, 129, 46,
        242, 231, 0, 99, 68, 56, 14, 34, 68, 230, 224, 66, 169, 184, 249, 115, 247, 31, 27, 60, 52,
        113, 54, 24, 47, 30, 130, 154, 63, 25, 95, 119, 151, 14, 150, 192, 67, 131, 115, 104, 88,
        34, 231, 25, 120, 139, 23, 146, 53, 205, 151, 104, 84, 54, 156, 206, 64, 183, 10, 161, 163,
        166, 35, 18, 182, 182, 227, 252, 214, 36, 147, 44, 170, 80, 114, 107, 130, 63, 203, 126,
        228, 155, 106, 101, 33, 253, 90, 71, 198, 248, 142, 151, 229, 197, 178, 4, 158, 24, 52, 24,
        251, 240, 0, 209, 35, 124, 216, 160, 160, 117, 161, 26, 28, 69, 42, 166, 221, 170, 84, 77,
        23, 111, 110, 73, 40, 1, 35, 2, 31, 24, 222, 131, 243, 166, 71, 212, 154, 105, 19, 250,
        144, 223, 206, 197, 81, 62, 39, 222, 8, 167, 222, 85, 231, 212, 179, 179, 182, 39, 45, 193,
        0, 0, 1, 71, 252, 121, 185, 27, 55, 252, 134, 124, 120, 27, 52, 145, 119, 135, 153, 22,
        189, 191, 103, 167, 142, 139, 133, 218, 37, 225, 241, 52, 204, 191, 209, 17, 2, 190, 246,
        229, 135, 112, 132, 120, 74, 198, 120, 98, 207, 66, 85, 212, 86, 48, 81, 66, 112, 223, 230,
        192, 165, 45, 179, 58, 55, 182, 214, 209, 230, 16, 1, 194, 243, 114, 55, 78, 185, 161, 24,
        82, 225, 115, 241, 207, 111, 220, 40, 252, 141, 94, 183, 123, 68, 152, 56, 244, 181, 149,
        114, 220, 119, 179, 1, 17, 2, 200, 218, 21, 221, 49, 83, 248, 158, 121, 63, 3, 212, 102,
        109, 15, 219, 185, 71, 157, 163, 129, 62, 47, 56, 195, 193, 240, 245, 166, 2, 36, 149, 16,
        1, 60, 86, 247, 158, 181, 162, 199, 184, 196, 72, 214, 202, 227, 141, 21, 115, 195, 120,
        178, 164, 224, 148, 61, 223, 46, 176, 84, 108, 215, 230, 98, 183, 17, 2, 180, 88, 210, 9,
        190, 236, 134, 220, 247, 236, 147, 167, 233, 199, 252, 197, 115, 202, 57, 205, 32, 82, 46,
        252, 182, 252, 86, 5, 182, 13, 178, 184, 16, 1, 54, 116, 176, 218, 199, 210, 118, 118, 27,
        33, 165, 37, 141, 30, 22, 231, 35, 21, 209, 148, 89, 187, 158, 223, 4, 18, 247, 219, 30,
        57, 98, 245, 17, 2, 67, 175, 28, 231, 34, 203, 106, 84, 135, 217, 27, 207, 44, 58, 191, 45,
        142, 220, 188, 100, 234, 58, 194, 114, 218, 82, 42, 166, 77, 242, 193, 29, 16, 1, 171, 162,
        193, 223, 64, 172, 53, 242, 79, 69, 105, 174, 187, 151, 142, 214, 255, 70, 65, 86, 21, 214,
        224, 77, 173, 2, 195, 226, 146, 99, 185, 3, 17, 2, 96, 15, 73, 30, 31, 179, 123, 224, 229,
        225, 60, 167, 236, 113, 183, 55, 15, 246, 100, 168, 109, 134, 75, 131, 39, 141, 133, 193,
        217, 93, 236, 123, 16, 1, 81, 158, 160, 229, 112, 73, 135, 150, 82, 147, 222, 201, 210,
        177, 118, 30, 42, 3, 6, 89, 79, 202, 233, 143, 2, 252, 116, 253, 76, 133, 62, 108, 17, 2,
        225, 12, 15, 225, 100, 242, 40, 103, 116, 116, 254, 169, 28, 119, 102, 152, 197, 111, 228,
        118, 233, 30, 208, 204, 10, 211, 92, 57, 201, 11, 10, 148, 16, 1, 11, 152, 56, 7, 144, 195,
        251, 149, 102, 208, 17, 73, 60, 49, 170, 211, 195, 239, 207, 192, 133, 89, 161, 238, 176,
        57, 244, 214, 92, 57, 165, 192, 17, 2, 71, 169, 162, 9, 220, 155, 2, 57, 135, 31, 4, 128,
        8, 54, 59, 125, 179, 119, 151, 198, 207, 125, 245, 216, 149, 55, 133, 255, 85, 27, 157,
        255, 16, 1, 92, 19, 69, 142, 131, 145, 89, 95, 6, 31, 55, 255, 184, 232, 34, 111, 253, 8,
        85, 21, 205, 87, 89, 14, 223, 198, 75, 233, 71, 73, 180, 192, 17, 2, 240, 77, 88, 122, 52,
        15, 27, 185, 225, 85, 126, 68, 194, 5, 64, 162, 195, 86, 130, 74, 19, 235, 158, 16, 30,
        197, 190, 56, 202, 239, 206, 17, 16, 1, 217, 158, 133, 249, 237, 167, 5, 30, 105, 191, 192,
        171, 48, 104, 15, 100, 158, 41, 93, 236, 242, 120, 103, 33, 136, 165, 53, 11, 16, 30, 232,
        147, 17, 1, 141, 1, 4, 1, 0, 0, 36, 2, 1, 32, 154, 58, 106, 73, 191, 124, 181, 218, 0, 249,
        222, 129, 33, 222, 45, 206, 27, 128, 68, 13, 116, 199, 36, 139, 188, 212, 238, 244, 133,
        162, 213, 175, 0, 108, 88, 243, 113, 102, 65, 131, 193, 59, 22, 233, 194, 43, 191, 23, 66,
        174, 64, 174, 69, 64, 241, 90, 133, 244, 193, 209, 233, 226, 111, 222, 15, 2, 24, 174, 73,
        203, 124, 151, 74, 40, 117, 7, 108, 87, 247, 148, 154, 229, 158, 67, 185, 112, 36, 67, 117,
        11, 56, 201, 237, 71, 186, 61, 14, 41, 16, 1, 161, 235, 121, 200, 65, 78, 210, 96, 80, 154,
        225, 125, 5, 199, 0, 177, 91, 200, 116, 30, 125, 221, 130, 166, 36, 114, 75, 117, 140, 47,
        206, 163, 17, 1, 130, 1, 1, 236, 153, 75, 254, 53, 124, 209, 144, 57, 74, 187, 230, 144,
        45, 193, 224, 47, 6, 239, 95, 24, 218, 35, 211, 15, 153, 133, 132, 168, 202, 47, 129, 4,
        14, 99, 111, 110, 116, 97, 99, 116, 82, 101, 113, 117, 101, 115, 116, 0, 12, 2, 1, 8, 36,
        111, 119, 110, 101, 114, 73, 100, 0, 162, 157, 141, 228, 44, 138, 114, 141, 14, 193, 239,
        176, 216, 188, 249, 101, 192, 226, 2, 250, 42, 161, 209, 158, 127, 151, 223, 245, 102, 236,
        236, 234, 16, 1, 88, 182, 123, 110, 139, 70, 63, 151, 8, 86, 39, 25, 217, 50, 145, 212, 2,
        162, 131, 155, 188, 147, 135, 177, 196, 155, 251, 16, 67, 145, 188, 85, 17, 1, 89, 1, 237,
        51, 10, 10, 84, 121, 52, 204, 198, 130, 133, 65, 12, 131, 206, 67, 246, 55, 3, 219, 245,
        213, 130, 82, 115, 56, 170, 167, 179, 203, 147, 85, 4, 1, 1, 0, 18, 2, 1, 14, 99, 111, 110,
        116, 97, 99, 116, 82, 101, 113, 117, 101, 115, 116, 0, 9, 20, 137, 112, 69, 239, 182, 56,
        39, 0, 37, 84, 42, 164, 200, 25, 72, 148, 207, 91, 150, 27, 142, 57, 242, 1, 218, 160, 165,
        125, 149, 17, 16, 1, 209, 1, 1, 77, 197, 131, 12, 212, 21, 235, 159, 88, 163, 79, 133, 51,
        181, 46, 59, 237, 28, 121, 150, 76, 50, 160, 194, 167, 184, 88, 110, 177, 120, 152, 229, 2,
        122, 4, 215, 82, 249, 190, 38, 95, 173, 151, 190, 49, 210, 90, 64, 10, 156, 228, 247, 148,
        101, 181, 167, 16, 125, 30, 93, 190, 217, 129, 90, 228, 16, 4, 32, 79, 156, 88, 7, 249,
        121, 81, 208, 107, 45, 170, 106, 98, 94, 6, 15, 79, 63, 177, 246, 186, 221, 225, 94, 248,
        112, 89, 85, 203, 31, 11, 65, 0, 5, 2, 1, 1, 1, 0, 11, 62, 6, 243, 127, 253, 235, 238, 162,
        171, 81, 197, 196, 63, 68, 8, 215, 156, 72, 15, 229, 0, 112, 37, 10, 205, 132, 11, 238, 2,
        96, 76, 17, 2, 162, 160, 218, 128, 217, 61, 15, 167, 233, 186, 183, 217, 217, 191, 206,
        188, 122, 111, 167, 229, 6, 165, 158, 7, 108, 191, 154, 220, 94, 180, 103, 82, 16, 1, 7,
        68, 19, 115, 183, 107, 40, 60, 102, 174, 178, 241, 226, 240, 248, 106, 171, 40, 166, 140,
        247, 133, 27, 87, 121, 164, 181, 154, 103, 124, 13, 208, 17, 1, 141, 1, 1, 170, 129, 181,
        187, 47, 115, 195, 249, 249, 203, 179, 108, 182, 99, 184, 229, 219, 205, 34, 77, 165, 204,
        72, 247, 3, 11, 71, 147, 93, 6, 154, 207, 4, 1, 64, 0, 36, 2, 1, 32, 162, 161, 180, 172,
        111, 239, 34, 234, 42, 26, 104, 232, 18, 54, 68, 179, 87, 135, 95, 107, 65, 44, 24, 16,
        146, 129, 193, 70, 231, 178, 113, 188, 0, 206, 86, 213, 127, 21, 71, 64, 132, 38, 219, 97,
        243, 50, 58, 21, 40, 41, 178, 161, 26, 94, 145, 118, 230, 15, 156, 60, 191, 254, 176, 150,
        77, 16, 1, 44, 83, 193, 142, 137, 5, 241, 57, 171, 61, 62, 240, 151, 41, 249, 181, 77, 235,
        183, 194, 202, 204, 247, 59, 190, 165, 136, 205, 112, 46, 116, 111, 17,
    ];

    let mut query = Query::new();
    query.insert_key(vec![
        0, 149, 11, 93, 106, 85, 233, 189, 236, 34, 199, 88, 201, 100, 251, 71, 144, 11, 15, 57,
        107, 123, 165, 49, 8, 237, 119, 136, 220, 226, 230, 137,
    ]);

    let path_query = PathQuery::new(
        vec![
            vec![64],
            vec![
                79, 156, 88, 7, 249, 121, 81, 208, 107, 45, 170, 106, 98, 94, 6, 15, 79, 63, 177,
                246, 186, 221, 225, 94, 248, 112, 89, 85, 203, 31, 11, 65,
            ],
            vec![1],
            vec![
                99, 111, 110, 116, 97, 99, 116, 82, 101, 113, 117, 101, 115, 116,
            ],
            vec![0],
        ],
        SizedQuery::new(query, Some(1), None),
    );

    dbg!(GroveDb::verify_query_with_absence_proof(
        &proof,
        &path_query
    ));
}

#[test]
fn test_root_hash() {
    let proof = vec![
        4, 32, 0, 149, 11, 93, 106, 85, 233, 189, 236, 34, 199, 88, 201, 100, 251, 71, 144, 11, 15,
        57, 107, 123, 165, 49, 8, 237, 119, 136, 220, 226, 230, 137, 1, 90, 0, 251, 49, 1, 0, 0,
        149, 11, 93, 106, 85, 233, 189, 236, 34, 199, 88, 201, 100, 251, 71, 144, 11, 15, 57, 107,
        123, 165, 49, 8, 237, 119, 136, 220, 226, 230, 137, 31, 24, 222, 131, 243, 166, 71, 212,
        154, 105, 19, 250, 144, 223, 206, 197, 81, 62, 39, 222, 8, 167, 222, 85, 231, 212, 179,
        179, 182, 39, 45, 193, 3, 0, 0, 1, 135, 197, 187, 1, 224, 1, 0, 0, 1, 135, 208, 7, 185,
        224, 228, 110, 140, 14, 152, 86, 1, 20, 1, 68, 120, 79, 9, 229, 1, 212, 89, 96, 38, 204,
        211, 135, 155, 160, 0, 229, 73, 131, 26, 40, 187, 129, 95, 117, 163, 135, 159, 230, 160,
        59, 86, 159, 240, 217, 230, 92, 1, 215, 7, 11, 120, 171, 130, 90, 63, 173, 147, 116, 91,
        73, 103, 16, 247, 134, 77, 236, 156, 44, 110, 194, 104, 179, 237, 106, 245, 43, 187, 132,
        92, 239, 66, 15, 37, 229, 70, 114, 190, 232, 18, 183, 230, 144, 133, 204, 94, 129, 46, 242,
        231, 0, 99, 68, 56, 14, 34, 68, 230, 224, 66, 169, 184, 249, 115, 247, 31, 27, 60, 52, 113,
        54, 24, 47, 30, 130, 154, 63, 25, 95, 119, 151, 14, 150, 192, 67, 131, 115, 104, 88, 34,
        231, 25, 120, 139, 23, 146, 53, 205, 151, 104, 84, 54, 156, 206, 64, 183, 10, 161, 163,
        166, 35, 18, 182, 182, 227, 252, 214, 36, 147, 44, 170, 80, 114, 107, 130, 63, 203, 126,
        228, 155, 106, 101, 33, 253, 90, 71, 198, 248, 142, 151, 229, 197, 178, 4, 158, 24, 52, 24,
        251, 240, 0, 209, 35, 124, 216, 160, 160, 117, 161, 26, 28, 69, 42, 166, 221, 170, 84, 77,
        23, 111, 110, 73, 40, 1, 35, 2, 31, 24, 222, 131, 243, 166, 71, 212, 154, 105, 19, 250,
        144, 223, 206, 197, 81, 62, 39, 222, 8, 167, 222, 85, 231, 212, 179, 179, 182, 39, 45, 193,
        0, 0, 129, 251, 239, 96, 116, 74, 81, 68, 100, 117, 209, 68, 246, 220, 207, 13, 158, 188,
        124, 168, 175, 63, 194, 64, 217, 222, 241, 179, 59, 149, 185, 43, 1, 71, 252, 121, 185, 27,
        55, 252, 134, 124, 120, 27, 52, 145, 119, 135, 153, 22, 189, 191, 103, 167, 142, 139, 133,
        218, 37, 225, 241, 52, 204, 191, 209, 17, 2, 190, 246, 229, 135, 112, 132, 120, 74, 198,
        120, 98, 207, 66, 85, 212, 86, 48, 81, 66, 112, 223, 230, 192, 165, 45, 179, 58, 55, 182,
        214, 209, 230, 16, 1, 194, 243, 114, 55, 78, 185, 161, 24, 82, 225, 115, 241, 207, 111,
        220, 40, 252, 141, 94, 183, 123, 68, 152, 56, 244, 181, 149, 114, 220, 119, 179, 1, 17, 2,
        200, 218, 21, 221, 49, 83, 248, 158, 121, 63, 3, 212, 102, 109, 15, 219, 185, 71, 157, 163,
        129, 62, 47, 56, 195, 193, 240, 245, 166, 2, 36, 149, 16, 1, 60, 86, 247, 158, 181, 162,
        199, 184, 196, 72, 214, 202, 227, 141, 21, 115, 195, 120, 178, 164, 224, 148, 61, 223, 46,
        176, 84, 108, 215, 230, 98, 183, 17, 2, 180, 88, 210, 9, 190, 236, 134, 220, 247, 236, 147,
        167, 233, 199, 252, 197, 115, 202, 57, 205, 32, 82, 46, 252, 182, 252, 86, 5, 182, 13, 178,
        184, 16, 1, 54, 116, 176, 218, 199, 210, 118, 118, 27, 33, 165, 37, 141, 30, 22, 231, 35,
        21, 209, 148, 89, 187, 158, 223, 4, 18, 247, 219, 30, 57, 98, 245, 17, 2, 67, 175, 28, 231,
        34, 203, 106, 84, 135, 217, 27, 207, 44, 58, 191, 45, 142, 220, 188, 100, 234, 58, 194,
        114, 218, 82, 42, 166, 77, 242, 193, 29, 16, 1, 171, 162, 193, 223, 64, 172, 53, 242, 79,
        69, 105, 174, 187, 151, 142, 214, 255, 70, 65, 86, 21, 214, 224, 77, 173, 2, 195, 226, 146,
        99, 185, 3, 17, 2, 96, 15, 73, 30, 31, 179, 123, 224, 229, 225, 60, 167, 236, 113, 183, 55,
        15, 246, 100, 168, 109, 134, 75, 131, 39, 141, 133, 193, 217, 93, 236, 123, 16, 1, 81, 158,
        160, 229, 112, 73, 135, 150, 82, 147, 222, 201, 210, 177, 118, 30, 42, 3, 6, 89, 79, 202,
        233, 143, 2, 252, 116, 253, 76, 133, 62, 108, 17, 2, 225, 12, 15, 225, 100, 242, 40, 103,
        116, 116, 254, 169, 28, 119, 102, 152, 197, 111, 228, 118, 233, 30, 208, 204, 10, 211, 92,
        57, 201, 11, 10, 148, 16, 1, 11, 152, 56, 7, 144, 195, 251, 149, 102, 208, 17, 73, 60, 49,
        170, 211, 195, 239, 207, 192, 133, 89, 161, 238, 176, 57, 244, 214, 92, 57, 165, 192, 17,
        2, 71, 169, 162, 9, 220, 155, 2, 57, 135, 31, 4, 128, 8, 54, 59, 125, 179, 119, 151, 198,
        207, 125, 245, 216, 149, 55, 133, 255, 85, 27, 157, 255, 16, 1, 92, 19, 69, 142, 131, 145,
        89, 95, 6, 31, 55, 255, 184, 232, 34, 111, 253, 8, 85, 21, 205, 87, 89, 14, 223, 198, 75,
        233, 71, 73, 180, 192, 17, 2, 240, 77, 88, 122, 52, 15, 27, 185, 225, 85, 126, 68, 194, 5,
        64, 162, 195, 86, 130, 74, 19, 235, 158, 16, 30, 197, 190, 56, 202, 239, 206, 17, 16, 1,
        217, 158, 133, 249, 237, 167, 5, 30, 105, 191, 192, 171, 48, 104, 15, 100, 158, 41, 93,
        236, 242, 120, 103, 33, 136, 165, 53, 11, 16, 30, 232, 147, 17,
    ];
    let mut query = Query::new();
    query.insert_key(vec![
        0, 149, 11, 93, 106, 85, 233, 189, 236, 34, 199, 88, 201, 100, 251, 71, 144, 11, 15, 57,
        107, 123, 165, 49, 8, 237, 119, 136, 220, 226, 230, 137,
    ]);
    let (root_hash, _) = execute_proof(&proof, &query, Some(1), None, true)
        .unwrap()
        .unwrap();
    dbg!(root_hash);

    let p2 = vec![
        4, 32, 0, 149, 11, 93, 106, 85, 233, 189, 236, 34, 199, 88, 201, 100, 251, 71, 144, 11, 15,
        57, 107, 123, 165, 49, 8, 237, 119, 136, 220, 226, 230, 137, 1, 90, 0, 251, 49, 1, 0, 0,
        149, 11, 93, 106, 85, 233, 189, 236, 34, 199, 88, 201, 100, 251, 71, 144, 11, 15, 57, 107,
        123, 165, 49, 8, 237, 119, 136, 220, 226, 230, 137, 31, 24, 222, 131, 243, 166, 71, 212,
        154, 105, 19, 250, 144, 223, 206, 197, 81, 62, 39, 222, 8, 167, 222, 85, 231, 212, 179,
        179, 182, 39, 45, 193, 3, 0, 0, 1, 135, 197, 187, 1, 224, 1, 0, 0, 1, 135, 208, 7, 185,
        224, 228, 110, 140, 14, 152, 86, 1, 20, 1, 68, 120, 79, 9, 229, 1, 212, 89, 96, 38, 204,
        211, 135, 155, 160, 0, 229, 73, 131, 26, 40, 187, 129, 95, 117, 163, 135, 159, 230, 160,
        59, 86, 159, 240, 217, 230, 92, 1, 215, 7, 11, 120, 171, 130, 90, 63, 173, 147, 116, 91,
        73, 103, 16, 247, 134, 77, 236, 156, 44, 110, 194, 104, 179, 237, 106, 245, 43, 187, 132,
        92, 239, 66, 15, 37, 229, 70, 114, 190, 232, 18, 183, 230, 144, 133, 204, 94, 129, 46, 242,
        231, 0, 99, 68, 56, 14, 34, 68, 230, 224, 66, 169, 184, 249, 115, 247, 31, 27, 60, 52, 113,
        54, 24, 47, 30, 130, 154, 63, 25, 95, 119, 151, 14, 150, 192, 67, 131, 115, 104, 88, 34,
        231, 25, 120, 139, 23, 146, 53, 205, 151, 104, 84, 54, 156, 206, 64, 183, 10, 161, 163,
        166, 35, 18, 182, 182, 227, 252, 214, 36, 147, 44, 170, 80, 114, 107, 130, 63, 203, 126,
        228, 155, 106, 101, 33, 253, 90, 71, 198, 248, 142, 151, 229, 197, 178, 4, 158, 24, 52, 24,
        251, 240, 0, 209, 35, 124, 216, 160, 160, 117, 161, 26, 28, 69, 42, 166, 221, 170, 84, 77,
        23, 111, 110, 73, 40, 1, 35, 2, 31, 24, 222, 131, 243, 166, 71, 212, 154, 105, 19, 250,
        144, 223, 206, 197, 81, 62, 39, 222, 8, 167, 222, 85, 231, 212, 179, 179, 182, 39, 45, 193,
        0, 0, 129, 251, 239, 96, 116, 74, 81, 68, 100, 117, 209, 68, 246, 220, 207, 13, 158, 188,
        124, 168, 175, 63, 194, 64, 217, 222, 241, 179, 59, 149, 185, 43, 1, 71, 252, 121, 185, 27,
        55, 252, 134, 124, 120, 27, 52, 145, 119, 135, 153, 22, 189, 191, 103, 167, 142, 139, 133,
        218, 37, 225, 241, 52, 204, 191, 209, 17, 2, 190, 246, 229, 135, 112, 132, 120, 74, 198,
        120, 98, 207, 66, 85, 212, 86, 48, 81, 66, 112, 223, 230, 192, 165, 45, 179, 58, 55, 182,
        214, 209, 230, 16, 1, 194, 243, 114, 55, 78, 185, 161, 24, 82, 225, 115, 241, 207, 111,
        220, 40, 252, 141, 94, 183, 123, 68, 152, 56, 244, 181, 149, 114, 220, 119, 179, 1, 17, 2,
        200, 218, 21, 221, 49, 83, 248, 158, 121, 63, 3, 212, 102, 109, 15, 219, 185, 71, 157, 163,
        129, 62, 47, 56, 195, 193, 240, 245, 166, 2, 36, 149, 16, 1, 60, 86, 247, 158, 181, 162,
        199, 184, 196, 72, 214, 202, 227, 141, 21, 115, 195, 120, 178, 164, 224, 148, 61, 223, 46,
        176, 84, 108, 215, 230, 98, 183, 17, 2, 180, 88, 210, 9, 190, 236, 134, 220, 247, 236, 147,
        167, 233, 199, 252, 197, 115, 202, 57, 205, 32, 82, 46, 252, 182, 252, 86, 5, 182, 13, 178,
        184, 16, 1, 54, 116, 176, 218, 199, 210, 118, 118, 27, 33, 165, 37, 141, 30, 22, 231, 35,
        21, 209, 148, 89, 187, 158, 223, 4, 18, 247, 219, 30, 57, 98, 245, 17, 2, 67, 175, 28, 231,
        34, 203, 106, 84, 135, 217, 27, 207, 44, 58, 191, 45, 142, 220, 188, 100, 234, 58, 194,
        114, 218, 82, 42, 166, 77, 242, 193, 29, 16, 1, 171, 162, 193, 223, 64, 172, 53, 242, 79,
        69, 105, 174, 187, 151, 142, 214, 255, 70, 65, 86, 21, 214, 224, 77, 173, 2, 195, 226, 146,
        99, 185, 3, 17, 2, 96, 15, 73, 30, 31, 179, 123, 224, 229, 225, 60, 167, 236, 113, 183, 55,
        15, 246, 100, 168, 109, 134, 75, 131, 39, 141, 133, 193, 217, 93, 236, 123, 16, 1, 81, 158,
        160, 229, 112, 73, 135, 150, 82, 147, 222, 201, 210, 177, 118, 30, 42, 3, 6, 89, 79, 202,
        233, 143, 2, 252, 116, 253, 76, 133, 62, 108, 17, 2, 225, 12, 15, 225, 100, 242, 40, 103,
        116, 116, 254, 169, 28, 119, 102, 152, 197, 111, 228, 118, 233, 30, 208, 204, 10, 211, 92,
        57, 201, 11, 10, 148, 16, 1, 11, 152, 56, 7, 144, 195, 251, 149, 102, 208, 17, 73, 60, 49,
        170, 211, 195, 239, 207, 192, 133, 89, 161, 238, 176, 57, 244, 214, 92, 57, 165, 192, 17,
        2, 71, 169, 162, 9, 220, 155, 2, 57, 135, 31, 4, 128, 8, 54, 59, 125, 179, 119, 151, 198,
        207, 125, 245, 216, 149, 55, 133, 255, 85, 27, 157, 255, 16, 1, 92, 19, 69, 142, 131, 145,
        89, 95, 6, 31, 55, 255, 184, 232, 34, 111, 253, 8, 85, 21, 205, 87, 89, 14, 223, 198, 75,
        233, 71, 73, 180, 192, 17, 2, 240, 77, 88, 122, 52, 15, 27, 185, 225, 85, 126, 68, 194, 5,
        64, 162, 195, 86, 130, 74, 19, 235, 158, 16, 30, 197, 190, 56, 202, 239, 206, 17, 16, 1,
        217, 158, 133, 249, 237, 167, 5, 30, 105, 191, 192, 171, 48, 104, 15, 100, 158, 41, 93,
        236, 242, 120, 103, 33, 136, 165, 53, 11, 16, 30, 232, 147, 17,
    ];

    let mut query = Query::new();
    query.insert_key(vec![
        0, 149, 11, 93, 106, 85, 233, 189, 236, 34, 199, 88, 201, 100, 251, 71, 144, 11, 15, 57,
        107, 123, 165, 49, 8, 237, 119, 136, 220, 226, 230, 137,
    ]);
    let (root_hash, _) = execute_proof(&p2, &query, Some(1), None, true)
        .unwrap()
        .unwrap();
    dbg!(root_hash);

    let bad_proof = vec![
        3, 32, 0, 149, 11, 93, 106, 85, 233, 189, 236, 34, 199, 88, 201, 100, 251, 71, 144, 11, 15,
        57, 107, 123, 165, 49, 8, 237, 119, 136, 220, 226, 230, 137, 1, 90, 0, 251, 49, 1, 0, 0,
        149, 11, 93, 106, 85, 233, 189, 236, 34, 199, 88, 201, 100, 251, 71, 144, 11, 15, 57, 107,
        123, 165, 49, 8, 237, 119, 136, 220, 226, 230, 137, 31, 24, 222, 131, 243, 166, 71, 212,
        154, 105, 19, 250, 144, 223, 206, 197, 81, 62, 39, 222, 8, 167, 222, 85, 231, 212, 179,
        179, 182, 39, 45, 193, 3, 0, 0, 1, 135, 197, 187, 1, 224, 1, 0, 0, 1, 135, 208, 7, 185,
        224, 228, 110, 140, 14, 152, 86, 1, 20, 1, 68, 120, 79, 9, 229, 1, 212, 89, 96, 38, 204,
        211, 135, 155, 160, 0, 229, 73, 131, 26, 40, 187, 129, 95, 117, 163, 135, 159, 230, 160,
        59, 86, 159, 240, 217, 230, 92, 1, 215, 7, 11, 120, 171, 130, 90, 63, 173, 147, 116, 91,
        73, 103, 16, 247, 134, 77, 236, 156, 44, 110, 194, 104, 179, 237, 106, 245, 43, 187, 132,
        92, 239, 66, 15, 37, 229, 70, 114, 190, 232, 18, 183, 230, 144, 133, 204, 94, 129, 46, 242,
        231, 0, 99, 68, 56, 14, 34, 68, 230, 224, 66, 169, 184, 249, 115, 247, 31, 27, 60, 52, 113,
        54, 24, 47, 30, 130, 154, 63, 25, 95, 119, 151, 14, 150, 192, 67, 131, 115, 104, 88, 34,
        231, 25, 120, 139, 23, 146, 53, 205, 151, 104, 84, 54, 156, 206, 64, 183, 10, 161, 163,
        166, 35, 18, 182, 182, 227, 252, 214, 36, 147, 44, 170, 80, 114, 107, 130, 63, 203, 126,
        228, 155, 106, 101, 33, 253, 90, 71, 198, 248, 142, 151, 229, 197, 178, 4, 158, 24, 52, 24,
        251, 240, 0, 209, 35, 124, 216, 160, 160, 117, 161, 26, 28, 69, 42, 166, 221, 170, 84, 77,
        23, 111, 110, 73, 40, 1, 35, 2, 31, 24, 222, 131, 243, 166, 71, 212, 154, 105, 19, 250,
        144, 223, 206, 197, 81, 62, 39, 222, 8, 167, 222, 85, 231, 212, 179, 179, 182, 39, 45, 193,
        0, 0, 1, 71, 252, 121, 185, 27, 55, 252, 134, 124, 120, 27, 52, 145, 119, 135, 153, 22,
        189, 191, 103, 167, 142, 139, 133, 218, 37, 225, 241, 52, 204, 191, 209, 17, 2, 190, 246,
        229, 135, 112, 132, 120, 74, 198, 120, 98, 207, 66, 85, 212, 86, 48, 81, 66, 112, 223, 230,
        192, 165, 45, 179, 58, 55, 182, 214, 209, 230, 16, 1, 194, 243, 114, 55, 78, 185, 161, 24,
        82, 225, 115, 241, 207, 111, 220, 40, 252, 141, 94, 183, 123, 68, 152, 56, 244, 181, 149,
        114, 220, 119, 179, 1, 17, 2, 200, 218, 21, 221, 49, 83, 248, 158, 121, 63, 3, 212, 102,
        109, 15, 219, 185, 71, 157, 163, 129, 62, 47, 56, 195, 193, 240, 245, 166, 2, 36, 149, 16,
        1, 60, 86, 247, 158, 181, 162, 199, 184, 196, 72, 214, 202, 227, 141, 21, 115, 195, 120,
        178, 164, 224, 148, 61, 223, 46, 176, 84, 108, 215, 230, 98, 183, 17, 2, 180, 88, 210, 9,
        190, 236, 134, 220, 247, 236, 147, 167, 233, 199, 252, 197, 115, 202, 57, 205, 32, 82, 46,
        252, 182, 252, 86, 5, 182, 13, 178, 184, 16, 1, 54, 116, 176, 218, 199, 210, 118, 118, 27,
        33, 165, 37, 141, 30, 22, 231, 35, 21, 209, 148, 89, 187, 158, 223, 4, 18, 247, 219, 30,
        57, 98, 245, 17, 2, 67, 175, 28, 231, 34, 203, 106, 84, 135, 217, 27, 207, 44, 58, 191, 45,
        142, 220, 188, 100, 234, 58, 194, 114, 218, 82, 42, 166, 77, 242, 193, 29, 16, 1, 171, 162,
        193, 223, 64, 172, 53, 242, 79, 69, 105, 174, 187, 151, 142, 214, 255, 70, 65, 86, 21, 214,
        224, 77, 173, 2, 195, 226, 146, 99, 185, 3, 17, 2, 96, 15, 73, 30, 31, 179, 123, 224, 229,
        225, 60, 167, 236, 113, 183, 55, 15, 246, 100, 168, 109, 134, 75, 131, 39, 141, 133, 193,
        217, 93, 236, 123, 16, 1, 81, 158, 160, 229, 112, 73, 135, 150, 82, 147, 222, 201, 210,
        177, 118, 30, 42, 3, 6, 89, 79, 202, 233, 143, 2, 252, 116, 253, 76, 133, 62, 108, 17, 2,
        225, 12, 15, 225, 100, 242, 40, 103, 116, 116, 254, 169, 28, 119, 102, 152, 197, 111, 228,
        118, 233, 30, 208, 204, 10, 211, 92, 57, 201, 11, 10, 148, 16, 1, 11, 152, 56, 7, 144, 195,
        251, 149, 102, 208, 17, 73, 60, 49, 170, 211, 195, 239, 207, 192, 133, 89, 161, 238, 176,
        57, 244, 214, 92, 57, 165, 192, 17, 2, 71, 169, 162, 9, 220, 155, 2, 57, 135, 31, 4, 128,
        8, 54, 59, 125, 179, 119, 151, 198, 207, 125, 245, 216, 149, 55, 133, 255, 85, 27, 157,
        255, 16, 1, 92, 19, 69, 142, 131, 145, 89, 95, 6, 31, 55, 255, 184, 232, 34, 111, 253, 8,
        85, 21, 205, 87, 89, 14, 223, 198, 75, 233, 71, 73, 180, 192, 17, 2, 240, 77, 88, 122, 52,
        15, 27, 185, 225, 85, 126, 68, 194, 5, 64, 162, 195, 86, 130, 74, 19, 235, 158, 16, 30,
        197, 190, 56, 202, 239, 206, 17, 16, 1, 217, 158, 133, 249, 237, 167, 5, 30, 105, 191, 192,
        171, 48, 104, 15, 100, 158, 41, 93, 236, 242, 120, 103, 33, 136, 165, 53, 11, 16, 30, 232,
        147, 17,
    ];

    let mut query = Query::new();
    query.insert_key(vec![
        0, 149, 11, 93, 106, 85, 233, 189, 236, 34, 199, 88, 201, 100, 251, 71, 144, 11, 15, 57,
        107, 123, 165, 49, 8, 237, 119, 136, 220, 226, 230, 137,
    ]);
    let (root_hash, _) = execute_proof(&bad_proof, &query, Some(1), None, true)
        .unwrap()
        .unwrap();
    dbg!(root_hash);
}
