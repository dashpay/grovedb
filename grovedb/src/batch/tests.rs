use std::collections::BTreeMap;

use visualize::visualize_stdout;

use super::*;
use crate::{
    tests::{make_grovedb, ANOTHER_TEST_LEAF, TEST_LEAF},
    Element,
};

#[test]
fn test_batch_validation_ok() {
    let db = make_grovedb();
    let element = Element::Item(b"ayy".to_vec());
    let element2 = Element::Item(b"ayy2".to_vec());
    let ops = vec![
        GroveDbOp::insert(vec![], b"key1".to_vec(), Element::empty_tree()),
        GroveDbOp::insert(
            vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
            b"key4".to_vec(),
            element.clone(),
        ),
        GroveDbOp::insert(
            vec![b"key1".to_vec(), b"key2".to_vec()],
            b"key3".to_vec(),
            Element::empty_tree(),
        ),
        GroveDbOp::insert(
            vec![b"key1".to_vec()],
            b"key2".to_vec(),
            Element::empty_tree(),
        ),
        GroveDbOp::insert(
            vec![TEST_LEAF.to_vec()],
            b"key1".to_vec(),
            Element::empty_tree(),
        ),
        GroveDbOp::insert(
            vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
            b"key2".to_vec(),
            element2.clone(),
        ),
    ];
    db.apply_batch(ops, None).expect("cannot apply batch");

    db.get([], b"key1", None).expect("cannot get element");
    db.get([b"key1".as_ref()], b"key2", None)
        .expect("cannot get element");
    db.get([b"key1".as_ref(), b"key2"], b"key3", None)
        .expect("cannot get element");
    db.get([b"key1".as_ref(), b"key2", b"key3"], b"key4", None)
        .expect("cannot get element");

    assert_eq!(
        db.get([b"key1".as_ref(), b"key2", b"key3"], b"key4", None)
            .expect("cannot get element"),
        element
    );
    assert_eq!(
        db.get([TEST_LEAF, b"key1"], b"key2", None)
            .expect("cannot get element"),
        element2
    );
}

#[test]
fn test_batch_validation_broken_chain() {
    let db = make_grovedb();
    let element = Element::Item(b"ayy".to_vec());
    let ops = vec![
        GroveDbOp::insert(vec![], b"key1".to_vec(), Element::empty_tree()),
        GroveDbOp::insert(
            vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
            b"key4".to_vec(),
            element.clone(),
        ),
        GroveDbOp::insert(
            vec![b"key1".to_vec()],
            b"key2".to_vec(),
            Element::empty_tree(),
        ),
    ];
    assert!(db.apply_batch(ops, None).is_err());
    assert!(db.get([b"key1".as_ref()], b"key2", None).is_err());
}

#[test]
fn test_batch_validation_broken_chain_aborts_whole_batch() {
    let db = make_grovedb();
    let element = Element::Item(b"ayy".to_vec());
    let ops = vec![
        GroveDbOp::insert(
            vec![TEST_LEAF.to_vec()],
            b"key1".to_vec(),
            Element::empty_tree(),
        ),
        GroveDbOp::insert(
            vec![TEST_LEAF.to_vec(), b"key1".to_vec()],
            b"key2".to_vec(),
            element.clone(),
        ),
        GroveDbOp::insert(vec![], b"key1".to_vec(), Element::empty_tree()),
        GroveDbOp::insert(
            vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
            b"key4".to_vec(),
            element.clone(),
        ),
        GroveDbOp::insert(
            vec![b"key1".to_vec()],
            b"key2".to_vec(),
            Element::empty_tree(),
        ),
    ];
    assert!(db.apply_batch(ops, None).is_err());
    assert!(db.get([b"key1".as_ref()], b"key2", None).is_err());
    assert!(db.get([TEST_LEAF, b"key1"], b"key2", None).is_err(),);
}

#[test]
fn test_batch_validation_deletion_brokes_chain() {
    let db = make_grovedb();
    let element = Element::Item(b"ayy".to_vec());

    db.insert([], b"key1", Element::empty_tree(), None)
        .expect("cannot insert a subtree");
    db.insert([], b"key2", Element::empty_tree(), None)
        .expect("cannot insert a subtree");

    let ops = vec![
        GroveDbOp::insert(
            vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
            b"key4".to_vec(),
            element.clone(),
        ),
        GroveDbOp::insert(
            vec![b"key1".to_vec(), b"key2".to_vec()],
            b"key3".to_vec(),
            Element::empty_tree(),
        ),
        GroveDbOp::delete(vec![b"key1".to_vec()], b"key2".to_vec()),
    ];
    assert!(db.apply_batch(ops, None).is_err());
}

#[test]
fn test_batch_validation_deletion_and_insertion_restore_chain() {
    let db = make_grovedb();
    let element = Element::Item(b"ayy".to_vec());
    let ops = vec![
        GroveDbOp::insert(vec![], b"key1".to_vec(), Element::empty_tree()),
        GroveDbOp::insert(
            vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
            b"key4".to_vec(),
            element.clone(),
        ),
        GroveDbOp::insert(
            vec![b"key1".to_vec(), b"key2".to_vec()],
            b"key3".to_vec(),
            Element::empty_tree(),
        ),
        GroveDbOp::insert(
            vec![b"key1".to_vec()],
            b"key2".to_vec(),
            Element::empty_tree(),
        ),
        GroveDbOp::delete(vec![b"key1".to_vec()], b"key2".to_vec()),
    ];
    db.apply_batch(ops, None).expect("cannot apply batch");
    assert_eq!(
        db.get([b"key1".as_ref(), b"key2", b"key3"], b"key4", None)
            .expect("cannot get element"),
        element
    );
}

#[test]
fn test_batch_validation_insert_into_existing_tree() {
    let db = make_grovedb();
    let element = Element::Item(b"ayy".to_vec());

    db.insert([TEST_LEAF], b"invalid", element.clone(), None)
        .expect("cannot insert value");
    db.insert([TEST_LEAF], b"valid", Element::empty_tree(), None)
        .expect("cannot insert value");

    // Insertion into scalar is invalid
    let ops = vec![GroveDbOp::insert(
        vec![TEST_LEAF.to_vec(), b"invalid".to_vec()],
        b"key1".to_vec(),
        element.clone(),
    )];
    assert!(db.apply_batch(ops, None).is_err());

    // Insertion into a tree is correct
    let ops = vec![GroveDbOp::insert(
        vec![TEST_LEAF.to_vec(), b"valid".to_vec()],
        b"key1".to_vec(),
        element.clone(),
    )];
    db.apply_batch(ops, None).expect("cannot apply batch");
    assert_eq!(
        db.get([TEST_LEAF, b"valid"], b"key1", None)
            .expect("cannot get element"),
        element
    );
}

#[test]
fn test_batch_validation_nested_subtree_overwrite() {
    let db = make_grovedb();
    let element = Element::Item(b"ayy".to_vec());
    let element2 = Element::Item(b"ayy2".to_vec());
    db.insert([TEST_LEAF], b"key_subtree", Element::empty_tree(), None)
        .expect("cannot insert a subtree");
    db.insert([TEST_LEAF, b"key_subtree"], b"key2", element, None)
        .expect("cannot insert an item");

    // TEST_LEAF will be overwritten thus nested subtrees will be deleted and it is
    // invalid to insert into them
    let ops = vec![
        GroveDbOp::insert(vec![], TEST_LEAF.to_vec(), element2.clone()),
        GroveDbOp::insert(
            vec![TEST_LEAF.to_vec(), b"key_subtree".to_vec()],
            b"key1".to_vec(),
            Element::empty_tree(),
        ),
    ];
    assert!(db.apply_batch(ops, None).is_err());

    // TEST_LEAF will became a scalar, insertion into scalar is also invalid
    let ops = vec![
        GroveDbOp::insert(vec![], TEST_LEAF.to_vec(), element2.clone()),
        GroveDbOp::insert(
            vec![TEST_LEAF.to_vec()],
            b"key1".to_vec(),
            Element::empty_tree(),
        ),
    ];
    assert!(db.apply_batch(ops, None).is_err());

    // Here TEST_LEAF is overwritten and new data should be available why older data
    // shouldn't
    let ops = vec![
        GroveDbOp::insert(vec![], TEST_LEAF.to_vec(), Element::empty_tree()),
        GroveDbOp::insert(vec![TEST_LEAF.to_vec()], b"key1".to_vec(), element2.clone()),
    ];
    assert!(db.apply_batch(ops, None).is_ok());

    assert_eq!(
        db.get([TEST_LEAF], b"key1", None).expect("cannot get data"),
        element2
    );
    assert!(db.get([TEST_LEAF, b"key_subtree"], b"key1", None).is_err());
}

#[test]
fn test_batch_validation_root_leaf_removal() {
    let db = make_grovedb();
    let ops = vec![
        GroveDbOp::insert(vec![], TEST_LEAF.to_vec(), Element::Item(b"ayy".to_vec())),
        GroveDbOp::insert(
            vec![TEST_LEAF.to_vec()],
            b"key1".to_vec(),
            Element::empty_tree(),
        ),
    ];
    assert!(db.apply_batch(ops, None).is_err());
}

#[test]
fn test_merk_data_is_deleted() {
    let db = make_grovedb();
    let element = Element::Item(b"ayy".to_vec());

    db.insert([TEST_LEAF], b"key1", Element::empty_tree(), None)
        .expect("cannot insert a subtree");
    db.insert([TEST_LEAF, b"key1"], b"key2", element.clone(), None)
        .expect("cannot insert an item");
    let ops = vec![GroveDbOp::insert(
        vec![TEST_LEAF.to_vec()],
        b"key1".to_vec(),
        Element::Item(b"ayy2".to_vec()),
    )];

    assert_eq!(
        db.get([TEST_LEAF, b"key1"], b"key2", None)
            .expect("cannot get item"),
        element
    );
    db.apply_batch(ops, None).expect("cannot apply batch");
    assert!(db.get([TEST_LEAF, b"key1"], b"key2", None).is_err());
}

#[test]
fn test_multi_tree_insertion_deletion_with_propagation_no_tx() {
    let db = make_grovedb();
    db.insert([], b"key1", Element::empty_tree(), None)
        .expect("cannot insert root leaf");
    db.insert([], b"key2", Element::empty_tree(), None)
        .expect("cannot insert root leaf");
    db.insert([ANOTHER_TEST_LEAF], b"key1", Element::empty_tree(), None)
        .expect("cannot insert root leaf");

    let hash = db
        .root_hash(None)
        .ok()
        .flatten()
        .expect("cannot get root hash");
    let element = Element::Item(b"ayy".to_vec());
    let element2 = Element::Item(b"ayy2".to_vec());

    let ops = vec![
        GroveDbOp::insert(
            vec![b"key1".to_vec(), b"key2".to_vec(), b"key3".to_vec()],
            b"key4".to_vec(),
            element.clone(),
        ),
        GroveDbOp::insert(
            vec![b"key1".to_vec(), b"key2".to_vec()],
            b"key3".to_vec(),
            Element::empty_tree(),
        ),
        GroveDbOp::insert(
            vec![b"key1".to_vec()],
            b"key2".to_vec(),
            Element::empty_tree(),
        ),
        GroveDbOp::insert(vec![TEST_LEAF.to_vec()], b"key".to_vec(), element2.clone()),
        GroveDbOp::delete(vec![ANOTHER_TEST_LEAF.to_vec()], b"key1".to_vec()),
    ];
    db.apply_batch(ops, None).expect("cannot apply batch");

    assert!(db.get([ANOTHER_TEST_LEAF], b"key1", None).is_err());

    assert_eq!(
        db.get([b"key1".as_ref(), b"key2", b"key3"], b"key4", None)
            .expect("cannot get element"),
        element
    );
    assert_eq!(
        db.get([TEST_LEAF], b"key", None)
            .expect("cannot get element"),
        element2
    );
    assert_ne!(
        db.root_hash(None)
            .ok()
            .flatten()
            .expect("cannot get root hash"),
        hash
    );
    let mut root_leafs = BTreeMap::new();
    root_leafs.insert(TEST_LEAF.to_vec(), 0);
    root_leafs.insert(ANOTHER_TEST_LEAF.to_vec(), 1);
    root_leafs.insert(b"key1".to_vec(), 2);
    root_leafs.insert(b"key2".to_vec(), 3);

    assert_eq!(
        db.get_root_leaf_keys(None).expect("cannot get root leafs"),
        root_leafs
    );
}

#[test]
fn test_nested_batch_insertion_corrupts_state() {
    let db = make_grovedb();
    let full_path = vec![
        b"leaf1".to_vec(),
        b"sub1".to_vec(),
        b"sub2".to_vec(),
        b"sub3".to_vec(),
        b"sub4".to_vec(),
        b"sub5".to_vec(),
    ];
    let mut acc_path: Vec<Vec<u8>> = vec![];
    for p in full_path.into_iter() {
        db.insert(
            acc_path.iter().map(|x| x.as_slice()),
            &p,
            Element::empty_tree(),
            None,
        )
        .unwrap();
        acc_path.push(p);
    }

    let element = Element::Item(b"ayy".to_vec());
    let batch = vec![GroveDbOp::insert(
        acc_path.clone(),
        b"key".to_vec(),
        element.clone(),
    )];
    db.apply_batch(batch, None).expect("cannot apply batch");

    visualize_stdout(&db);

    let batch = vec![GroveDbOp::insert(
        acc_path,
        b"key".to_vec(),
        element.clone(),
    )];
    db.apply_batch(batch, None)
        .expect("cannot apply same batch twice");
}
