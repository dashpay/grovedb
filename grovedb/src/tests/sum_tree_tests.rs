use merk::proofs::Query;

use crate::{
    tests::{make_test_grovedb, TEST_LEAF},
    Element, Error, GroveDb, PathQuery,
};

#[test]
fn test_sum_tree_behaves_like_regular_tree() {
    let db = make_test_grovedb();
    db.insert([TEST_LEAF], b"key", Element::empty_sum_tree(), None, None)
        .unwrap()
        .expect("should insert tree");

    // Can fetch sum tree
    let sum_tree = db
        .get([TEST_LEAF], b"key", None)
        .unwrap()
        .expect("should get tree");
    assert!(matches!(sum_tree, Element::SumTree(..)));

    db.insert(
        [TEST_LEAF, b"key"],
        b"innerkey",
        Element::new_item(vec![1]),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");
    db.insert(
        [TEST_LEAF, b"key"],
        b"innerkey2",
        Element::new_item(vec![3]),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");
    db.insert(
        [TEST_LEAF, b"key"],
        b"innerkey3",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");

    // Test proper item retrieval
    let item = db
        .get([TEST_LEAF, b"key"], b"innerkey", None)
        .unwrap()
        .expect("should get item");
    assert_eq!(item, Element::new_item(vec![1]));

    // Test proof generation
    let mut query = Query::new();
    query.insert_key(b"innerkey2".to_vec());

    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"key".to_vec()], query);
    let proof = db
        .prove_query(&path_query)
        .unwrap()
        .expect("should generate proof");
    let (root_hash, result_set) =
        GroveDb::verify_query(&proof, &path_query).expect("should verify proof");
    assert_eq!(root_hash, db.grove_db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 1);
    assert_eq!(
        Element::deserialize(&result_set[0].1).expect("should deserialize element"),
        Element::new_item(vec![3])
    );
}

#[test]
fn test_sum_item_behaves_like_regular_item() {
    let db = make_test_grovedb();
    db.insert(
        [TEST_LEAF],
        b"sumkey",
        Element::empty_sum_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("should insert tree");
    db.insert(
        [TEST_LEAF, b"sumkey"],
        b"k1",
        Element::new_item(vec![1]),
        None,
        None,
    )
    .unwrap()
    .expect("should insert tree");
    db.insert(
        [TEST_LEAF, b"sumkey"],
        b"k2",
        Element::new_sum_item(5),
        None,
        None,
    )
    .unwrap()
    .expect("should insert tree");
    db.insert(
        [TEST_LEAF, b"sumkey"],
        b"k3",
        Element::empty_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("should insert tree");

    // Test proper item retrieval
    let item = db
        .get([TEST_LEAF, b"sumkey"], b"k2", None)
        .unwrap()
        .expect("should get item");
    assert_eq!(item, Element::new_sum_item(5));

    // Test proof generation
    let mut query = Query::new();
    query.insert_key(b"k2".to_vec());

    let path_query = PathQuery::new_unsized(vec![TEST_LEAF.to_vec(), b"sumkey".to_vec()], query);
    let proof = db
        .prove_query(&path_query)
        .unwrap()
        .expect("should generate proof");
    let (root_hash, result_set) =
        GroveDb::verify_query(&proof, &path_query).expect("should verify proof");
    assert_eq!(root_hash, db.grove_db.root_hash(None).unwrap().unwrap());
    assert_eq!(result_set.len(), 1);
    let element_from_proof =
        Element::deserialize(&result_set[0].1).expect("should deserialize element");
    assert_eq!(element_from_proof, Element::new_sum_item(5));
    assert_eq!(element_from_proof.sum_value(), Some(5));
}

#[test]
fn test_cannot_insert_sum_item_in_regular_tree() {
    let db = make_test_grovedb();
    db.insert([TEST_LEAF], b"sumkey", Element::empty_tree(), None, None)
        .unwrap()
        .expect("should insert tree");
    assert!(matches!(
        db.insert(
            [TEST_LEAF, b"sumkey"],
            b"k1",
            Element::new_sum_item(5),
            None,
            None,
        )
        .unwrap(),
        Err(Error::InvalidInput("cannot add sum item to non sum tree"))
    ));
}
