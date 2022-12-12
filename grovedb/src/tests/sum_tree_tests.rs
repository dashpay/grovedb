use merk::{
    proofs::Query,
    TreeFeatureType::{BasicMerk, SummedMerk},
};

use crate::{
    batch::GroveDbOp,
    reference_path::ReferencePathType,
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

#[test]
fn test_homogenous_node_type_in_sum_trees_and_regular_trees() {
    // All elements in a sum tree must have a summed feature type
    let db = make_test_grovedb();
    db.insert([TEST_LEAF], b"key", Element::empty_sum_tree(), None, None)
        .unwrap()
        .expect("should insert tree");
    // Add sum items
    db.insert(
        [TEST_LEAF, b"key"],
        b"item1",
        Element::new_sum_item(30),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");
    db.insert(
        [TEST_LEAF, b"key"],
        b"item2",
        Element::new_sum_item(10),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");
    // Add regular items
    db.insert(
        [TEST_LEAF, b"key"],
        b"item3",
        Element::new_item(vec![10]),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");
    db.insert(
        [TEST_LEAF, b"key"],
        b"item4",
        Element::new_item(vec![15]),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");

    // Open merk and check all elements in it
    let merk = db
        .open_non_transactional_merk_at_path([TEST_LEAF, b"key"])
        .unwrap()
        .expect("should open tree");
    assert!(matches!(
        merk.get_feature_type(b"item1")
            .unwrap()
            .expect("node should exist"),
        Some(SummedMerk(30))
    ));
    assert!(matches!(
        merk.get_feature_type(b"item2")
            .unwrap()
            .expect("node should exist"),
        Some(SummedMerk(10))
    ));
    assert!(matches!(
        merk.get_feature_type(b"item3")
            .unwrap()
            .expect("node should exist"),
        Some(SummedMerk(0))
    ));
    assert!(matches!(
        merk.get_feature_type(b"item4")
            .unwrap()
            .expect("node should exist"),
        Some(SummedMerk(0))
    ));
    assert_eq!(merk.sum(), Some(40));

    // Perform the same test on regular trees
    let db = make_test_grovedb();
    db.insert([TEST_LEAF], b"key", Element::empty_tree(), None, None)
        .unwrap()
        .expect("should insert tree");
    db.insert(
        [TEST_LEAF, b"key"],
        b"item1",
        Element::new_item(vec![30]),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");
    db.insert(
        [TEST_LEAF, b"key"],
        b"item2",
        Element::new_item(vec![10]),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");

    let merk = db
        .open_non_transactional_merk_at_path([TEST_LEAF, b"key"])
        .unwrap()
        .expect("should open tree");
    assert!(matches!(
        merk.get_feature_type(b"item1")
            .unwrap()
            .expect("node should exist"),
        Some(BasicMerk)
    ));
    assert!(matches!(
        merk.get_feature_type(b"item2")
            .unwrap()
            .expect("node should exist"),
        Some(BasicMerk)
    ));
    assert_eq!(merk.sum(), None);
}

#[test]
fn test_sum_tree_feature() {
    let db = make_test_grovedb();
    db.insert([TEST_LEAF], b"key", Element::empty_tree(), None, None)
        .unwrap()
        .expect("should insert tree");

    // Sum should be non for non sum tree
    // TODO: change interface to retrieve element directly
    let merk = db
        .open_non_transactional_merk_at_path([TEST_LEAF, b"key"])
        .unwrap()
        .expect("should open tree");
    assert_eq!(merk.sum(), None);

    // Add sum tree
    db.insert([TEST_LEAF], b"key2", Element::empty_sum_tree(), None, None)
        .unwrap()
        .expect("should insert sum tree");
    let sum_tree = db
        .get([TEST_LEAF], b"key2", None)
        .unwrap()
        .expect("should retrieve tree");
    assert_eq!(sum_tree.sum_value(), Some(0));

    // Add sum items to the sum tree
    db.insert(
        [TEST_LEAF, b"key2"],
        b"item1",
        Element::new_sum_item(30),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");
    // TODO: change interface to retrieve element directly
    let merk = db
        .open_non_transactional_merk_at_path([TEST_LEAF, b"key2"])
        .unwrap()
        .expect("should open tree");
    assert_eq!(merk.sum(), Some(30));

    // Add more sum items
    db.insert(
        [TEST_LEAF, b"key2"],
        b"item2",
        Element::new_sum_item(-10),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");
    db.insert(
        [TEST_LEAF, b"key2"],
        b"item3",
        Element::new_sum_item(50),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");
    let merk = db
        .open_non_transactional_merk_at_path([TEST_LEAF, b"key2"])
        .unwrap()
        .expect("should open tree");
    assert_eq!(merk.sum(), Some(70)); // 30 - 10 + 50 = 70

    // Add non sum items, result should remain the same
    db.insert(
        [TEST_LEAF, b"key2"],
        b"item4",
        Element::new_item(vec![29]),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");
    let merk = db
        .open_non_transactional_merk_at_path([TEST_LEAF, b"key2"])
        .unwrap()
        .expect("should open tree");
    assert_eq!(merk.sum(), Some(70));

    // Update existing sum items
    db.insert(
        [TEST_LEAF, b"key2"],
        b"item2",
        Element::new_sum_item(10),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");
    db.insert(
        [TEST_LEAF, b"key2"],
        b"item3",
        Element::new_sum_item(-100),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");
    let merk = db
        .open_non_transactional_merk_at_path([TEST_LEAF, b"key2"])
        .unwrap()
        .expect("should open tree");
    assert_eq!(merk.sum(), Some(-60)); // 30 + 10 - 100 = -60

    // Use a large value
    db.insert(
        [TEST_LEAF, b"key2"],
        b"item4",
        Element::new_sum_item(10000000),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");
    let merk = db
        .open_non_transactional_merk_at_path([TEST_LEAF, b"key2"])
        .unwrap()
        .expect("should open tree");
    assert_eq!(merk.sum(), Some(9999940)); // 30 + 10 - 100 + 10000000

    // TODO: Test out overflows
}

#[test]
fn test_sum_tree_propagation() {
    let db = make_test_grovedb();
    // Tree
    //   SumTree
    //      SumTree
    //        Item1
    //        SumItem1
    //        SumItem2
    //      SumItem3
    db.insert([TEST_LEAF], b"key", Element::empty_sum_tree(), None, None)
        .unwrap()
        .expect("should insert tree");
    db.insert(
        [TEST_LEAF, b"key"],
        b"tree2",
        Element::empty_sum_tree(),
        None,
        None,
    )
    .unwrap()
    .expect("should insert tree");
    db.insert(
        [TEST_LEAF, b"key"],
        b"sumitem3",
        Element::new_sum_item(20),
        None,
        None,
    )
    .unwrap()
    .expect("should insert tree");
    db.insert(
        [TEST_LEAF, b"key", b"tree2"],
        b"item1",
        Element::new_item(vec![2]),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");
    db.insert(
        [TEST_LEAF, b"key", b"tree2"],
        b"sumitem1",
        Element::new_sum_item(5),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");
    db.insert(
        [TEST_LEAF, b"key", b"tree2"],
        b"sumitem2",
        Element::new_sum_item(10),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");
    db.insert(
        [TEST_LEAF, b"key", b"tree2"],
        b"item2",
        Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            TEST_LEAF.to_vec(),
            b"key".to_vec(),
            b"tree2".to_vec(),
            b"sumitem1".to_vec(),
        ])),
        None,
        None,
    )
    .unwrap()
    .expect("should insert item");

    let sum_tree = db
        .get([TEST_LEAF], b"key", None)
        .unwrap()
        .expect("should fetch tree");
    assert_eq!(sum_tree.sum_value(), Some(35));

    // Assert node feature types
    let test_leaf_merk = db
        .open_non_transactional_merk_at_path([TEST_LEAF])
        .unwrap()
        .expect("should open tree");
    assert!(matches!(
        test_leaf_merk
            .get_feature_type(b"key")
            .unwrap()
            .expect("node should exist"),
        Some(BasicMerk)
    ));

    let parent_sum_tree = db
        .open_non_transactional_merk_at_path([TEST_LEAF, b"key"])
        .unwrap()
        .expect("should open tree");
    assert!(matches!(
        parent_sum_tree
            .get_feature_type(b"tree2")
            .unwrap()
            .expect("node should exist"),
        Some(SummedMerk(15)) /* 15 because the child sum tree has one sum item of
                              * value 5 and
                              * another of value 10 */
    ));

    let child_sum_tree = db
        .open_non_transactional_merk_at_path([TEST_LEAF, b"key", b"tree2"])
        .unwrap()
        .expect("should open tree");
    assert!(matches!(
        child_sum_tree
            .get_feature_type(b"item1")
            .unwrap()
            .expect("node should exist"),
        Some(SummedMerk(0))
    ));
    assert!(matches!(
        child_sum_tree
            .get_feature_type(b"sumitem1")
            .unwrap()
            .expect("node should exist"),
        Some(SummedMerk(5))
    ));
    assert!(matches!(
        child_sum_tree
            .get_feature_type(b"sumitem2")
            .unwrap()
            .expect("node should exist"),
        Some(SummedMerk(10))
    ));

    // TODO: should references take the sum of the referenced element??
    assert!(matches!(
        child_sum_tree
            .get_feature_type(b"item2")
            .unwrap()
            .expect("node should exist"),
        Some(SummedMerk(0))
    ));
}

#[test]
fn test_sum_tree_with_batches() {
    let db = make_test_grovedb();
    let ops = vec![
        GroveDbOp::insert_op(
            vec![TEST_LEAF.to_vec()],
            b"key1".to_vec(),
            Element::empty_sum_tree(),
        ),
        GroveDbOp::insert_op(
            vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
            b"a".to_vec(),
            Element::new_item(vec![214]),
        ),
        GroveDbOp::insert_op(
            vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
            b"b".to_vec(),
            Element::new_sum_item(10),
        ),
    ];
    db.apply_batch(ops, None, None)
        .unwrap()
        .expect("should apply batch");

    let sum_tree = db
        .open_non_transactional_merk_at_path([TEST_LEAF, b"key1"])
        .unwrap()
        .expect("should open tree");

    assert!(matches!(
        sum_tree
            .get_feature_type(b"a")
            .unwrap()
            .expect("node should exist"),
        Some(SummedMerk(0))
    ));
    assert!(matches!(
        sum_tree
            .get_feature_type(b"b")
            .unwrap()
            .expect("node should exist"),
        Some(SummedMerk(10))
    ));

    // Create new batch to use existing tree
    let ops = vec![GroveDbOp::insert_op(
        vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
        b"c".to_vec(),
        Element::new_sum_item(10),
    )];
    db.apply_batch(ops, None, None)
        .unwrap()
        .expect("should apply batch");
    let sum_tree = db
        .open_non_transactional_merk_at_path([TEST_LEAF, b"key1"])
        .unwrap()
        .expect("should open tree");
    assert!(matches!(
        sum_tree
            .get_feature_type(b"c")
            .unwrap()
            .expect("node should exist"),
        Some(SummedMerk(10))
    ));
    assert_eq!(sum_tree.sum(), Some(20));

    // Test propagation
    // Add a new sum tree with it's own sum items, should affect sum of original
    // tree
    let ops = vec![
        GroveDbOp::insert_op(
            vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
            b"d".to_vec(),
            Element::empty_sum_tree(),
        ),
        GroveDbOp::insert_op(
            vec![TEST_LEAF.to_vec(), b"key1".to_vec(), b"d".to_vec()],
            b"first".to_vec(),
            Element::new_sum_item(4),
        ),
        GroveDbOp::insert_op(
            vec![TEST_LEAF.to_vec(), b"key1".to_vec(), b"d".to_vec()],
            b"second".to_vec(),
            Element::new_item(vec![4]),
        ),
        GroveDbOp::insert_op(
            vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
            b"e".to_vec(),
            Element::empty_sum_tree(),
        ),
        GroveDbOp::insert_op(
            vec![TEST_LEAF.to_vec(), b"key1".to_vec(), b"e".to_vec()],
            b"first".to_vec(),
            Element::new_sum_item(12),
        ),
        GroveDbOp::insert_op(
            vec![TEST_LEAF.to_vec(), b"key1".to_vec(), b"e".to_vec()],
            b"second".to_vec(),
            Element::new_item(vec![4]),
        ),
        GroveDbOp::insert_op(
            vec![TEST_LEAF.to_vec(), b"key1".to_vec(), b"e".to_vec()],
            b"third".to_vec(),
            Element::empty_sum_tree(),
        ),
        GroveDbOp::insert_op(
            vec![
                TEST_LEAF.to_vec(),
                b"key1".to_vec(),
                b"e".to_vec(),
                b"third".to_vec(),
            ],
            b"a".to_vec(),
            Element::new_sum_item(5),
        ),
        GroveDbOp::insert_op(
            vec![
                TEST_LEAF.to_vec(),
                b"key1".to_vec(),
                b"e".to_vec(),
                b"third".to_vec(),
            ],
            b"b".to_vec(),
            Element::new_item(vec![5]),
        ),
    ];
    db.apply_batch(ops, None, None)
        .unwrap()
        .expect("should apply batch");
    let sum_tree = db
        .open_non_transactional_merk_at_path([TEST_LEAF, b"key1"])
        .unwrap()
        .expect("should open tree");
    assert_eq!(sum_tree.sum(), Some(41));
}
