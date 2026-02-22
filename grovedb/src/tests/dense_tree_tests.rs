//! Dense append-only fixed-size tree integration tests
//!
//! Tests for DenseAppendOnlyFixedSizeTree as a GroveDB subtree type, using
//! blake3-based dense Merkle trees with level-order (BFS) filling.

use grovedb_merk::proofs::{
    query::{QueryItem, SubqueryBranch},
    Query,
};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::QualifiedGroveDbOp,
    operations::delete::DeleteOptions,
    tests::{common::EMPTY_PATH, make_empty_grovedb},
    Element, Error, GroveDb, PathQuery, SizedQuery,
};

// ===========================================================================
// Element tests
// ===========================================================================

#[test]
fn test_insert_dense_tree_at_root() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(4),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("successful dense tree insert at root");

    let element = db
        .get(EMPTY_PATH, b"dense", None, grove_version)
        .unwrap()
        .expect("should retrieve dense tree");
    assert!(element.is_dense_tree());
    assert!(element.is_any_tree());
}

#[test]
fn test_dense_tree_under_normal_tree() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"parent",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert parent");

    db.insert(
        [b"parent"].as_ref(),
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree under parent");

    let element = db
        .get([b"parent"].as_ref(), b"dense", None, grove_version)
        .unwrap()
        .expect("should retrieve dense tree");
    assert!(element.is_dense_tree());
}

#[test]
fn test_dense_tree_serialization_roundtrip() {
    let grove_version = GroveVersion::latest();
    let element = Element::empty_dense_tree(5);
    let serialized = element.serialize(grove_version).expect("serialize");
    let deserialized = Element::deserialize(&serialized, grove_version).expect("deserialize");
    assert_eq!(element, deserialized);
}

#[test]
fn test_dense_tree_with_flags() {
    let grove_version = GroveVersion::latest();
    let flags = Some(vec![1, 2, 3]);
    let element = Element::empty_dense_tree_with_flags(4, flags.clone());
    assert_eq!(element.get_flags(), &flags);

    let serialized = element.serialize(grove_version).expect("serialize");
    let deserialized = Element::deserialize(&serialized, grove_version).expect("deserialize");
    assert_eq!(element, deserialized);
    assert_eq!(deserialized.get_flags(), &flags);
}

#[test]
fn test_dense_tree_type_checks() {
    let element = Element::empty_dense_tree(4);
    assert!(element.is_dense_tree());
    assert!(element.is_any_tree());
    assert!(!element.is_any_item());
    assert!(!element.is_reference());
}

// ===========================================================================
// Insert / Get tests
// ===========================================================================

#[test]
fn test_dense_tree_single_insert() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(3), // capacity = 7
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    let (root_hash, position) = db
        .dense_tree_insert(EMPTY_PATH, b"dense", b"hello".to_vec(), None, grove_version)
        .unwrap()
        .expect("dense tree insert");

    assert_eq!(position, 0);
    assert_ne!(root_hash, [0u8; 32]);
}

#[test]
fn test_dense_tree_sequential_inserts() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(3), // capacity = 7
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    let mut last_root = [0u8; 32];
    for i in 0u16..7 {
        let value = format!("value_{}", i).into_bytes();
        let (root_hash, position) = db
            .dense_tree_insert(EMPTY_PATH, b"dense", value, None, grove_version)
            .unwrap()
            .expect("dense tree insert");
        assert_eq!(position, i);
        // Root hash should change with each insert
        assert_ne!(root_hash, last_root);
        last_root = root_hash;
    }
}

#[test]
fn test_dense_tree_get_by_position() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    // Insert some values
    for i in 0..3 {
        let value = format!("val_{}", i).into_bytes();
        db.dense_tree_insert(EMPTY_PATH, b"dense", value, None, grove_version)
            .unwrap()
            .expect("insert value");
    }

    // Retrieve by position
    let val0 = db
        .dense_tree_get(EMPTY_PATH, b"dense", 0, None, grove_version)
        .unwrap()
        .expect("get position 0");
    assert_eq!(val0, Some(b"val_0".to_vec()));

    let val1 = db
        .dense_tree_get(EMPTY_PATH, b"dense", 1, None, grove_version)
        .unwrap()
        .expect("get position 1");
    assert_eq!(val1, Some(b"val_1".to_vec()));

    let val2 = db
        .dense_tree_get(EMPTY_PATH, b"dense", 2, None, grove_version)
        .unwrap()
        .expect("get position 2");
    assert_eq!(val2, Some(b"val_2".to_vec()));
}

#[test]
fn test_dense_tree_get_out_of_bounds() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    db.dense_tree_insert(EMPTY_PATH, b"dense", b"hello".to_vec(), None, grove_version)
        .unwrap()
        .expect("insert one value");

    // Position 1 is beyond count (1), should return None
    let result = db
        .dense_tree_get(EMPTY_PATH, b"dense", 1, None, grove_version)
        .unwrap()
        .expect("get out of bounds");
    assert_eq!(result, None);
}

#[test]
fn test_dense_tree_fill_capacity() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Height 2 => capacity = 3 (root + 2 children)
    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(2),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    // Fill all 3 positions
    for i in 0..3 {
        db.dense_tree_insert(
            EMPTY_PATH,
            b"dense",
            format!("v{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert value");
    }

    // Next insert should fail — tree is full
    let result = db
        .dense_tree_insert(
            EMPTY_PATH,
            b"dense",
            b"overflow".to_vec(),
            None,
            grove_version,
        )
        .unwrap();
    assert!(result.is_err(), "expected error when tree is full");
}

// ===========================================================================
// Root hash tests
// ===========================================================================

#[test]
fn test_dense_tree_root_hash_empty() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    let root_hash = db
        .dense_tree_root_hash(EMPTY_PATH, b"dense", None, grove_version)
        .unwrap()
        .expect("get root hash");
    assert_eq!(
        root_hash, [0u8; 32],
        "empty tree should have zero root hash"
    );
}

#[test]
fn test_dense_tree_root_hash_changes() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    let hash_before = db
        .dense_tree_root_hash(EMPTY_PATH, b"dense", None, grove_version)
        .unwrap()
        .expect("get root hash before");

    db.dense_tree_insert(EMPTY_PATH, b"dense", b"hello".to_vec(), None, grove_version)
        .unwrap()
        .expect("insert value");

    let hash_after = db
        .dense_tree_root_hash(EMPTY_PATH, b"dense", None, grove_version)
        .unwrap()
        .expect("get root hash after");

    assert_ne!(
        hash_before, hash_after,
        "root hash should change after insert"
    );
}

#[test]
fn test_dense_tree_root_hash_determinism() {
    let grove_version = GroveVersion::latest();

    let db1 = make_empty_grovedb();
    let db2 = make_empty_grovedb();

    for db in [&db1, &db2] {
        db.insert(
            EMPTY_PATH,
            b"dense",
            Element::empty_dense_tree(3),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert dense tree");

        db.dense_tree_insert(EMPTY_PATH, b"dense", b"hello".to_vec(), None, grove_version)
            .unwrap()
            .expect("insert value");

        db.dense_tree_insert(EMPTY_PATH, b"dense", b"world".to_vec(), None, grove_version)
            .unwrap()
            .expect("insert value 2");
    }

    let hash1 = db1
        .dense_tree_root_hash(EMPTY_PATH, b"dense", None, grove_version)
        .unwrap()
        .expect("root hash db1");
    let hash2 = db2
        .dense_tree_root_hash(EMPTY_PATH, b"dense", None, grove_version)
        .unwrap()
        .expect("root hash db2");

    assert_eq!(hash1, hash2, "same inserts should produce same root hash");
}

// ===========================================================================
// Count tests
// ===========================================================================

#[test]
fn test_dense_tree_count() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    let count = db
        .dense_tree_count(EMPTY_PATH, b"dense", None, grove_version)
        .unwrap()
        .expect("get count");
    assert_eq!(count, 0);

    for i in 0..4 {
        db.dense_tree_insert(
            EMPTY_PATH,
            b"dense",
            format!("v{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert value");
    }

    let count = db
        .dense_tree_count(EMPTY_PATH, b"dense", None, grove_version)
        .unwrap()
        .expect("get count after inserts");
    assert_eq!(count, 4);
}

// ===========================================================================
// Propagation tests
// ===========================================================================

#[test]
fn test_dense_tree_root_propagates_to_grove() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"parent",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert parent");

    db.insert(
        [b"parent"].as_ref(),
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    let root_hash_before = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root hash before");

    db.dense_tree_insert(
        [b"parent"].as_ref(),
        b"dense",
        b"hello".to_vec(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert value");

    let root_hash_after = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root hash after");

    assert_ne!(
        root_hash_before, root_hash_after,
        "GroveDB root hash should change after dense tree insert"
    );
}

#[test]
fn test_dense_tree_nested_propagation() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create: root -> parent -> child -> dense
    db.insert(
        EMPTY_PATH,
        b"parent",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert parent");
    db.insert(
        [b"parent"].as_ref(),
        b"child",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert child");
    let path_parent_child: Vec<&[u8]> = vec![b"parent", b"child"];
    db.insert(
        path_parent_child.as_slice(),
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    let root_before = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root before");

    db.dense_tree_insert(
        path_parent_child.as_slice(),
        b"dense",
        b"nested".to_vec(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert into nested dense tree");

    let root_after = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root after");
    assert_ne!(
        root_before, root_after,
        "root hash should propagate through nested trees"
    );
}

// ===========================================================================
// Batch tests
// ===========================================================================

#[test]
fn test_dense_tree_batch_single_insert() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    let ops = vec![QualifiedGroveDbOp::dense_tree_insert_op(
        vec![],
        b"dense".to_vec(),
        b"batch_val".to_vec(),
    )];

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch apply");

    let val = db
        .dense_tree_get(EMPTY_PATH, b"dense", 0, None, grove_version)
        .unwrap()
        .expect("get after batch");
    assert_eq!(val, Some(b"batch_val".to_vec()));
}

#[test]
fn test_dense_tree_batch_multiple_inserts() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    let ops = vec![
        QualifiedGroveDbOp::dense_tree_insert_op(vec![], b"dense".to_vec(), b"first".to_vec()),
        QualifiedGroveDbOp::dense_tree_insert_op(vec![], b"dense".to_vec(), b"second".to_vec()),
        QualifiedGroveDbOp::dense_tree_insert_op(vec![], b"dense".to_vec(), b"third".to_vec()),
    ];

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch apply multiple");

    let count = db
        .dense_tree_count(EMPTY_PATH, b"dense", None, grove_version)
        .unwrap()
        .expect("count after batch");
    assert_eq!(count, 3);

    for (i, expected) in ["first", "second", "third"].iter().enumerate() {
        let val = db
            .dense_tree_get(EMPTY_PATH, b"dense", i as u16, None, grove_version)
            .unwrap()
            .expect("get value");
        assert_eq!(val, Some(expected.as_bytes().to_vec()));
    }
}

#[test]
fn test_dense_tree_batch_mixed_with_items() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"parent",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert parent");

    db.insert(
        [b"parent"].as_ref(),
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    let ops = vec![
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"parent".to_vec()],
            b"item1".to_vec(),
            Element::new_item(b"hello".to_vec()),
        ),
        QualifiedGroveDbOp::dense_tree_insert_op(
            vec![b"parent".to_vec()],
            b"dense".to_vec(),
            b"dense_val".to_vec(),
        ),
    ];

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch apply mixed ops");

    let item = db
        .get([b"parent"].as_ref(), b"item1", None, grove_version)
        .unwrap()
        .expect("get item");
    assert_eq!(item, Element::new_item(b"hello".to_vec()));

    let val = db
        .dense_tree_get([b"parent"].as_ref(), b"dense", 0, None, grove_version)
        .unwrap()
        .expect("get dense tree value");
    assert_eq!(val, Some(b"dense_val".to_vec()));
}

#[test]
fn test_dense_tree_batch_propagation() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"parent",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert parent");

    db.insert(
        [b"parent"].as_ref(),
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    let root_before = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root before");

    let ops = vec![QualifiedGroveDbOp::dense_tree_insert_op(
        vec![b"parent".to_vec()],
        b"dense".to_vec(),
        b"batch_propagate".to_vec(),
    )];

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch apply");

    let root_after = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root after");
    assert_ne!(
        root_before, root_after,
        "batch insert should propagate root hash change"
    );
}

// ===========================================================================
// Lifecycle / edge-case tests
// ===========================================================================

#[test]
fn test_dense_tree_height_immutability() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(4), // height 4 => capacity 15
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    // Insert some values
    for i in 0..5 {
        db.dense_tree_insert(
            EMPTY_PATH,
            b"dense",
            format!("v{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
    }

    // Retrieve the element and verify height is unchanged
    let element = db
        .get(EMPTY_PATH, b"dense", None, grove_version)
        .unwrap()
        .expect("get element");

    match element {
        Element::DenseAppendOnlyFixedSizeTree(count, height, _) => {
            assert_eq!(height, 4, "height should remain 4");
            assert_eq!(count, 5, "count should be 5");
        }
        _ => panic!("expected DenseAppendOnlyFixedSizeTree"),
    }
}

#[test]
fn test_dense_tree_multiple_trees_independent() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense_a",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense_a");

    db.insert(
        EMPTY_PATH,
        b"dense_b",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense_b");

    db.dense_tree_insert(
        EMPTY_PATH,
        b"dense_a",
        b"a_val".to_vec(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert into dense_a");

    db.dense_tree_insert(
        EMPTY_PATH,
        b"dense_b",
        b"b_val".to_vec(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert into dense_b");

    let a_count = db
        .dense_tree_count(EMPTY_PATH, b"dense_a", None, grove_version)
        .unwrap()
        .expect("count dense_a");
    let b_count = db
        .dense_tree_count(EMPTY_PATH, b"dense_b", None, grove_version)
        .unwrap()
        .expect("count dense_b");

    assert_eq!(a_count, 1);
    assert_eq!(b_count, 1);

    let a_val = db
        .dense_tree_get(EMPTY_PATH, b"dense_a", 0, None, grove_version)
        .unwrap()
        .expect("get from dense_a");
    let b_val = db
        .dense_tree_get(EMPTY_PATH, b"dense_b", 0, None, grove_version)
        .unwrap()
        .expect("get from dense_b");

    assert_eq!(a_val, Some(b"a_val".to_vec()));
    assert_eq!(b_val, Some(b"b_val".to_vec()));

    // Root hashes should differ since they have different values
    let a_hash = db
        .dense_tree_root_hash(EMPTY_PATH, b"dense_a", None, grove_version)
        .unwrap()
        .expect("hash dense_a");
    let b_hash = db
        .dense_tree_root_hash(EMPTY_PATH, b"dense_b", None, grove_version)
        .unwrap()
        .expect("hash dense_b");

    assert_ne!(a_hash, b_hash);
}

// ===========================================================================
// Delete tests
// ===========================================================================

#[test]
fn test_dense_tree_delete_empty() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    // Delete with default options (empty tree, should succeed)
    db.delete(EMPTY_PATH, b"dense", None, None, grove_version)
        .unwrap()
        .expect("should delete empty dense tree");

    // Verify tree is gone
    let result = db.get(EMPTY_PATH, b"dense", None, grove_version).unwrap();
    assert!(result.is_err(), "dense tree should no longer exist");
}

#[test]
fn test_dense_tree_delete_non_empty() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    // Insert 3 values
    for i in 0..3u8 {
        db.dense_tree_insert(
            EMPTY_PATH,
            b"dense",
            format!("val_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert value");
    }

    // Delete with allow_deleting_non_empty_trees
    db.delete(
        EMPTY_PATH,
        b"dense",
        Some(DeleteOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: true,
            ..Default::default()
        }),
        None,
        grove_version,
    )
    .unwrap()
    .expect("should delete non-empty dense tree");

    // Verify tree is gone
    let result = db.get(EMPTY_PATH, b"dense", None, grove_version).unwrap();
    assert!(result.is_err(), "dense tree should no longer exist");
}

#[test]
fn test_dense_tree_delete_non_empty_error() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    // Insert values to make it non-empty
    for i in 0..3u8 {
        db.dense_tree_insert(
            EMPTY_PATH,
            b"dense",
            format!("val_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert value");
    }

    // Delete without allowing non-empty trees (default options)
    let result = db
        .delete(EMPTY_PATH, b"dense", None, None, grove_version)
        .unwrap();
    assert!(
        matches!(result, Err(Error::DeletingNonEmptyTree(_))),
        "should return DeletingNonEmptyTree error, got: {:?}",
        result
    );
}

#[test]
fn test_dense_tree_delete_and_recreate() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    // Insert values
    for i in 0..3u8 {
        db.dense_tree_insert(
            EMPTY_PATH,
            b"dense",
            format!("val_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert value");
    }

    // Delete with allow
    db.delete(
        EMPTY_PATH,
        b"dense",
        Some(DeleteOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: true,
            ..Default::default()
        }),
        None,
        grove_version,
    )
    .unwrap()
    .expect("should delete non-empty dense tree");

    // Recreate
    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("recreate dense tree");

    // Insert a single value
    db.dense_tree_insert(EMPTY_PATH, b"dense", b"fresh".to_vec(), None, grove_version)
        .unwrap()
        .expect("insert into recreated tree");

    // Count should be 1 (fresh start)
    let count = db
        .dense_tree_count(EMPTY_PATH, b"dense", None, grove_version)
        .unwrap()
        .expect("count after recreate");
    assert_eq!(count, 1, "recreated tree should have only 1 entry");
}

// ===========================================================================
// verify_grovedb tests
// ===========================================================================

#[test]
fn test_verify_grovedb_dense_tree_valid() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(3),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    // Insert some values
    for i in 0..4u8 {
        db.dense_tree_insert(
            EMPTY_PATH,
            b"dense",
            format!("val_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert value");
    }

    // verify_grovedb should report no issues
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify should not fail");
    assert!(issues.is_empty(), "expected no issues, got: {:?}", issues);
}

// ===========================================================================
// V1 proof tests
// ===========================================================================

#[test]
fn test_dense_tree_v1_proof_range_query() {
    // Insert 10 values into a height-4 dense tree (capacity 15).
    // Query range [4..=8] and verify that the proof returns exactly
    // positions 4, 5, 6, 7, 8 — and nothing else.
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(4), // capacity = 15
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    // Insert 10 values at positions 0..9
    for i in 0..10u16 {
        db.dense_tree_insert(
            EMPTY_PATH,
            b"dense",
            format!("val_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("dense tree insert");
    }

    // Build a query for range [4..=8]
    let mut inner_query = Query::new();
    inner_query.insert_range_inclusive(4u16.to_be_bytes().to_vec()..=8u16.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"dense".to_vec())],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };

    // Generate V1 proof
    let proof_bytes = db
        .prove_query_v1(&path_query, None, grove_version)
        .unwrap()
        .expect("generate V1 proof for dense tree range");

    // Verify the proof
    let (root_hash, result_set) = GroveDb::verify_query_with_options(
        &proof_bytes,
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: false,
        },
        grove_version,
    )
    .expect("verify V1 proof for dense tree range");

    // Root hash must match
    let expected_root = db
        .grove_db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root hash");
    assert_eq!(root_hash, expected_root, "root hash should match");

    // Exactly 5 results: positions 4, 5, 6, 7, 8
    assert_eq!(result_set.len(), 5, "should have exactly 5 results");

    for (i, expected_pos) in (4u16..=8).enumerate() {
        let (_, key, element) = &result_set[i];
        assert_eq!(
            key,
            &expected_pos.to_be_bytes().to_vec(),
            "key at index {} should be position {}",
            i,
            expected_pos
        );
        let element = element.as_ref().expect("element should be Some");
        match element {
            Element::Item(data, _) => {
                assert_eq!(
                    data,
                    &format!("val_{}", expected_pos).into_bytes(),
                    "value at position {} should match",
                    expected_pos
                );
            }
            other => panic!(
                "expected Item at position {}, got {:?}",
                expected_pos, other
            ),
        }
    }
}

#[test]
fn test_dense_tree_v1_proof_single_position() {
    // Prove a single position and verify only that position is returned.
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(3), // capacity = 7
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree");

    for i in 0..7u16 {
        db.dense_tree_insert(
            EMPTY_PATH,
            b"dense",
            format!("item_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("dense tree insert");
    }

    // Query only position 3
    let mut inner_query = Query::new();
    inner_query.insert_key(3u16.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"dense".to_vec())],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };

    let proof_bytes = db
        .prove_query_v1(&path_query, None, grove_version)
        .unwrap()
        .expect("generate V1 proof");

    let (root_hash, result_set) = GroveDb::verify_query_with_options(
        &proof_bytes,
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: false,
        },
        grove_version,
    )
    .expect("verify V1 proof");

    let expected_root = db
        .grove_db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root hash");
    assert_eq!(root_hash, expected_root, "root hash should match");

    assert_eq!(result_set.len(), 1, "should have exactly 1 result");
    let (_, key, element) = &result_set[0];
    assert_eq!(key, &3u16.to_be_bytes().to_vec());
    match element.as_ref().expect("element should be Some") {
        Element::Item(data, _) => assert_eq!(data, b"item_3"),
        other => panic!("expected Item, got {:?}", other),
    }
}

#[test]
fn test_dense_tree_v1_proof_multiple_disjoint_positions() {
    // Query positions 1, 5, 9 from a tree with 10 values.
    // Verifies that non-contiguous positions are returned correctly
    // and that intermediate positions (2, 3, 4, 6, 7, 8) are NOT included.
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(4), // capacity = 15
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
            format!("d_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("dense tree insert");
    }

    // Query specific positions: 1, 5, 9
    let mut inner_query = Query::new();
    inner_query.insert_key(1u16.to_be_bytes().to_vec());
    inner_query.insert_key(5u16.to_be_bytes().to_vec());
    inner_query.insert_key(9u16.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"dense".to_vec())],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };

    let proof_bytes = db
        .prove_query_v1(&path_query, None, grove_version)
        .unwrap()
        .expect("generate V1 proof");

    let (root_hash, result_set) = GroveDb::verify_query_with_options(
        &proof_bytes,
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: false,
        },
        grove_version,
    )
    .expect("verify V1 proof");

    let expected_root = db
        .grove_db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root hash");
    assert_eq!(root_hash, expected_root, "root hash should match");

    assert_eq!(result_set.len(), 3, "should have exactly 3 results");

    let expected = vec![
        (1u16, b"d_1".to_vec()),
        (5u16, b"d_5".to_vec()),
        (9u16, b"d_9".to_vec()),
    ];
    for (i, (pos, val)) in expected.iter().enumerate() {
        let (_, key, element) = &result_set[i];
        assert_eq!(
            key,
            &pos.to_be_bytes().to_vec(),
            "key at index {} should be position {}",
            i,
            pos
        );
        match element.as_ref().expect("element should be Some") {
            Element::Item(data, _) => assert_eq!(data, val),
            other => panic!("expected Item at position {}, got {:?}", pos, other),
        }
    }
}

#[test]
fn test_dense_tree_v1_proof_nested_in_tree() {
    // Dense tree nested under a normal tree — proves the full path works.
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"parent",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert parent");

    let parent_path: &[&[u8]] = &[b"parent"];
    db.insert(
        parent_path,
        b"dense",
        Element::empty_dense_tree(3), // capacity = 7
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert dense tree under parent");

    for i in 0..7u16 {
        db.dense_tree_insert(
            parent_path,
            b"dense",
            format!("nested_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("dense tree insert");
    }

    // Query range [2..=5] inside the nested dense tree
    let mut inner_query = Query::new();
    inner_query.insert_range_inclusive(2u16.to_be_bytes().to_vec()..=5u16.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![b"parent".to_vec()],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"dense".to_vec())],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: None,
            offset: None,
        },
    };

    let proof_bytes = db
        .prove_query_v1(&path_query, None, grove_version)
        .unwrap()
        .expect("generate V1 proof for nested dense tree");

    let (root_hash, result_set) = GroveDb::verify_query_with_options(
        &proof_bytes,
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: false,
        },
        grove_version,
    )
    .expect("verify V1 proof for nested dense tree");

    let expected_root = db
        .grove_db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root hash");
    assert_eq!(root_hash, expected_root, "root hash should match");

    assert_eq!(result_set.len(), 4, "should have 4 results (positions 2-5)");

    for (i, expected_pos) in (2u16..=5).enumerate() {
        let (_, key, element) = &result_set[i];
        assert_eq!(key, &expected_pos.to_be_bytes().to_vec());
        match element.as_ref().expect("element should be Some") {
            Element::Item(data, _) => {
                assert_eq!(data, &format!("nested_{}", expected_pos).into_bytes());
            }
            other => panic!("expected Item, got {:?}", other),
        }
    }
}

#[test]
fn test_dense_tree_v1_proof_with_limit() {
    // Query range [0..=9] but with limit=3 — should return only first 3.
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(4), // capacity = 15
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
            format!("v_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("dense tree insert");
    }

    let mut inner_query = Query::new();
    inner_query.insert_range_inclusive(0u16.to_be_bytes().to_vec()..=9u16.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"dense".to_vec())],
                default_subquery_branch: SubqueryBranch {
                    subquery_path: None,
                    subquery: Some(inner_query.into()),
                },
                left_to_right: true,
                conditional_subquery_branches: None,
                add_parent_tree_on_subquery: false,
            },
            limit: Some(3),
            offset: None,
        },
    };

    let proof_bytes = db
        .prove_query_v1(&path_query, None, grove_version)
        .unwrap()
        .expect("generate V1 proof with limit");

    let (root_hash, result_set) = GroveDb::verify_query_with_options(
        &proof_bytes,
        &path_query,
        grovedb_merk::proofs::query::VerifyOptions {
            absence_proofs_for_non_existing_searched_keys: false,
            verify_proof_succinctness: false,
            include_empty_trees_in_result: false,
        },
        grove_version,
    )
    .expect("verify V1 proof with limit");

    let expected_root = db
        .grove_db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root hash");
    assert_eq!(root_hash, expected_root, "root hash should match");

    assert_eq!(
        result_set.len(),
        3,
        "should have exactly 3 results due to limit"
    );

    for (i, expected_pos) in (0u16..3).enumerate() {
        let (_, key, element) = &result_set[i];
        assert_eq!(key, &expected_pos.to_be_bytes().to_vec());
        match element.as_ref().expect("element should be Some") {
            Element::Item(data, _) => {
                assert_eq!(data, &format!("v_{}", expected_pos).into_bytes());
            }
            other => panic!("expected Item, got {:?}", other),
        }
    }
}
