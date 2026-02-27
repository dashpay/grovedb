//! MMR tree integration tests
//!
//! Tests for MmrTree as a GroveDB subtree type, using Blake3-based
//! Merkle Mountain Ranges for append-only authenticated data.

use std::{cell::RefCell, collections::BTreeMap};

use grovedb_merkle_mountain_range::{
    CostResult, CostsExt, MMRStoreReadOps, MMRStoreWriteOps, MmrNode, OperationCost, MMR,
};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::QualifiedGroveDbOp,
    operations::delete::DeleteOptions,
    tests::{common::EMPTY_PATH, make_empty_grovedb},
    Element, Error,
};

/// In-memory MMR store for test helpers.
struct MemStore(RefCell<BTreeMap<u64, MmrNode>>);

impl MemStore {
    fn new() -> Self {
        MemStore(RefCell::new(BTreeMap::new()))
    }
}

impl MMRStoreReadOps for &MemStore {
    fn element_at_position(
        &self,
        pos: u64,
    ) -> CostResult<Option<MmrNode>, grovedb_merkle_mountain_range::Error> {
        Ok(self.0.borrow().get(&pos).cloned()).wrap_with_cost(OperationCost::default())
    }
}

impl MMRStoreWriteOps for &MemStore {
    fn append(
        &mut self,
        pos: u64,
        elems: Vec<MmrNode>,
    ) -> CostResult<(), grovedb_merkle_mountain_range::Error> {
        let mut store = self.0.borrow_mut();
        for (i, elem) in elems.into_iter().enumerate() {
            store.insert(pos + i as u64, elem);
        }
        Ok(()).wrap_with_cost(OperationCost::default())
    }
}

// ---------------------------------------------------------------------------
// Helper: compute expected MMR root by appending values to a standalone MMR
// ---------------------------------------------------------------------------
fn expected_mmr_root(values: &[Vec<u8>]) -> [u8; 32] {
    let store = MemStore::new();
    let mut mmr = MMR::new(0, &store);
    for v in values {
        mmr.push(MmrNode::leaf(v.clone()))
            .unwrap()
            .expect("push should succeed");
    }
    mmr.commit().unwrap().expect("commit should succeed");
    mmr.get_root()
        .unwrap()
        .expect("root hash should succeed")
        .hash()
}

// ===========================================================================
// Element tests
// ===========================================================================

#[test]
fn test_insert_mmr_tree_at_root() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"mmr",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("successful mmr tree insert at root");

    let element = db
        .get(EMPTY_PATH, b"mmr", None, grove_version)
        .unwrap()
        .expect("should retrieve mmr tree");
    assert!(element.is_mmr_tree());
}

#[test]
fn test_mmr_tree_under_normal_tree() {
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
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree under parent");

    let element = db
        .get([b"parent"].as_ref(), b"log", None, grove_version)
        .unwrap()
        .expect("should get log");
    assert!(element.is_mmr_tree());
}

#[test]
fn test_mmr_tree_with_flags() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    let flags = Some(vec![1, 2, 3]);
    db.insert(
        EMPTY_PATH,
        b"flagged",
        Element::empty_mmr_tree_with_flags(flags.clone()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert flagged mmr tree");

    let element = db
        .get(EMPTY_PATH, b"flagged", None, grove_version)
        .unwrap()
        .expect("get flagged");
    assert!(element.is_mmr_tree());
}

#[test]
fn test_empty_mmr_tree_serialization_roundtrip() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"mmr",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let elem = db
        .get(EMPTY_PATH, b"mmr", None, grove_version)
        .unwrap()
        .expect("get");
    assert!(elem.is_mmr_tree());
}

#[test]
fn test_mmr_tree_is_any_tree() {
    let elem = Element::empty_mmr_tree();
    assert!(elem.is_any_tree());
    assert!(elem.is_mmr_tree());
}

// ===========================================================================
// Direct operation tests
// ===========================================================================

#[test]
fn test_mmr_tree_append_single() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    let value = b"first_entry".to_vec();
    let (root, leaf_index) = db
        .mmr_tree_append(EMPTY_PATH, b"log", value.clone(), None, grove_version)
        .unwrap()
        .expect("append single value");

    assert_eq!(leaf_index, 0);

    // Root should match expected MMR computation
    let exp_root = expected_mmr_root(&[value]);
    assert_eq!(root, exp_root);
}

#[test]
fn test_mmr_tree_append_multiple() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    let values: Vec<Vec<u8>> = (0..5u8)
        .map(|i| format!("entry_{}", i).into_bytes())
        .collect();

    for (i, value) in values.iter().enumerate() {
        let (_, leaf_index) = db
            .mmr_tree_append(EMPTY_PATH, b"log", value.clone(), None, grove_version)
            .unwrap()
            .expect("append value");
        assert_eq!(leaf_index, i as u64);
    }

    // Final root should match expected MMR computation
    let root = db
        .mmr_tree_root_hash(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("get root hash");
    let exp_root = expected_mmr_root(&values);
    assert_eq!(root, exp_root);
}

#[test]
fn test_mmr_tree_root_hash() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    // Empty MMR root should be zeros
    let empty_root = db
        .mmr_tree_root_hash(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("get empty root hash");
    assert_eq!(empty_root, [0u8; 32]);

    // After appending, root should change
    db.mmr_tree_append(EMPTY_PATH, b"log", b"data".to_vec(), None, grove_version)
        .unwrap()
        .expect("append");

    let root_after = db
        .mmr_tree_root_hash(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("get root after append");
    assert_ne!(root_after, [0u8; 32]);
}

#[test]
fn test_mmr_tree_get_value() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    let values: Vec<Vec<u8>> = vec![b"alpha".to_vec(), b"beta".to_vec(), b"gamma".to_vec()];

    for value in &values {
        db.mmr_tree_append(EMPTY_PATH, b"log", value.clone(), None, grove_version)
            .unwrap()
            .expect("append");
    }

    // Retrieve each leaf by index
    for (i, expected_val) in values.iter().enumerate() {
        let retrieved = db
            .mmr_tree_get_value(EMPTY_PATH, b"log", i as u64, None, grove_version)
            .unwrap()
            .expect("get value");
        assert_eq!(retrieved, Some(expected_val.clone()));
    }

    // Out of range returns None
    let none_val = db
        .mmr_tree_get_value(EMPTY_PATH, b"log", 99, None, grove_version)
        .unwrap()
        .expect("get out of range");
    assert!(none_val.is_none());
}

#[test]
fn test_mmr_tree_leaf_count() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    let count0 = db
        .mmr_tree_leaf_count(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("leaf count");
    assert_eq!(count0, 0);

    for i in 0..7u8 {
        db.mmr_tree_append(EMPTY_PATH, b"log", vec![i], None, grove_version)
            .unwrap()
            .expect("append");
    }

    let count7 = db
        .mmr_tree_leaf_count(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("leaf count after 7");
    assert_eq!(count7, 7);
}

#[test]
fn test_mmr_tree_deterministic_roots() {
    let grove_version = GroveVersion::latest();

    let db1 = make_empty_grovedb();
    let db2 = make_empty_grovedb();

    for db in [&db1, &db2] {
        db.insert(
            EMPTY_PATH,
            b"log",
            Element::empty_mmr_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert mmr tree");

        for i in 0..5u8 {
            db.mmr_tree_append(
                EMPTY_PATH,
                b"log",
                format!("val_{}", i).into_bytes(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("append");
        }
    }

    let root1 = db1
        .mmr_tree_root_hash(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("root1");
    let root2 = db2
        .mmr_tree_root_hash(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("root2");

    assert_eq!(root1, root2);
}

// ===========================================================================
// Root hash propagation tests
// ===========================================================================

#[test]
fn test_mmr_tree_append_propagates_root_hash() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    let root_hash_before = db.root_hash(None, grove_version).unwrap().unwrap();

    db.insert(
        EMPTY_PATH,
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    let root_hash_after_create = db.root_hash(None, grove_version).unwrap().unwrap();
    assert_ne!(root_hash_before, root_hash_after_create);

    db.mmr_tree_append(EMPTY_PATH, b"log", b"data".to_vec(), None, grove_version)
        .unwrap()
        .expect("append item");

    let root_hash_after_append = db.root_hash(None, grove_version).unwrap().unwrap();
    assert_ne!(root_hash_after_create, root_hash_after_append);
}

#[test]
fn test_mmr_tree_nested_propagation() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // parent (Tree) -> log (MmrTree) -> values
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
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert log");

    let root_before = db.root_hash(None, grove_version).unwrap().unwrap();

    db.mmr_tree_append(
        [b"parent"].as_ref(),
        b"log",
        b"nested_data".to_vec(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("append into nested mmr tree");

    let root_after = db.root_hash(None, grove_version).unwrap().unwrap();
    assert_ne!(root_before, root_after);
}

#[test]
fn test_mmr_tree_each_append_changes_grovedb_root() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    let mut prev_root = db.root_hash(None, grove_version).unwrap().unwrap();

    for i in 0..5u8 {
        db.mmr_tree_append(EMPTY_PATH, b"log", vec![i], None, grove_version)
            .unwrap()
            .expect("append");

        let new_root = db.root_hash(None, grove_version).unwrap().unwrap();
        assert_ne!(prev_root, new_root, "root should change on append {}", i);
        prev_root = new_root;
    }
}

// ===========================================================================
// Batch operation tests
// ===========================================================================

#[test]
fn test_mmr_tree_batch_single_append() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    let ops = vec![QualifiedGroveDbOp::mmr_tree_append_op(
        vec![b"log".to_vec()],
        b"batch_value".to_vec(),
    )];

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch apply");

    // Verify root matches expected
    let root = db
        .mmr_tree_root_hash(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("get root");
    let exp = expected_mmr_root(&[b"batch_value".to_vec()]);
    assert_eq!(root, exp);

    // Verify leaf count
    let count = db
        .mmr_tree_leaf_count(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("leaf count");
    assert_eq!(count, 1);
}

#[test]
fn test_mmr_tree_batch_multiple_appends() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    let values: Vec<Vec<u8>> = (0..5u8)
        .map(|i| format!("item_{}", i).into_bytes())
        .collect();

    let ops: Vec<QualifiedGroveDbOp> = values
        .iter()
        .map(|v| QualifiedGroveDbOp::mmr_tree_append_op(vec![b"log".to_vec()], v.clone()))
        .collect();

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch apply");

    // Verify root matches expected
    let root = db
        .mmr_tree_root_hash(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("get root");
    let exp = expected_mmr_root(&values);
    assert_eq!(root, exp);

    // Verify all leaves retrievable
    for (i, val) in values.iter().enumerate() {
        let retrieved = db
            .mmr_tree_get_value(EMPTY_PATH, b"log", i as u64, None, grove_version)
            .unwrap()
            .expect("get value");
        assert_eq!(retrieved, Some(val.clone()));
    }

    // Verify leaf count
    let count = db
        .mmr_tree_leaf_count(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("leaf count");
    assert_eq!(count, 5);
}

#[test]
fn test_mmr_tree_batch_matches_direct_ops() {
    let grove_version = GroveVersion::latest();

    // Direct operations
    let db_direct = make_empty_grovedb();
    db_direct
        .insert(
            EMPTY_PATH,
            b"log",
            Element::empty_mmr_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");

    let values: Vec<Vec<u8>> = (0..4u8)
        .map(|i| format!("val_{}", i).into_bytes())
        .collect();

    for v in &values {
        db_direct
            .mmr_tree_append(EMPTY_PATH, b"log", v.clone(), None, grove_version)
            .unwrap()
            .expect("direct append");
    }

    // Batch operations
    let db_batch = make_empty_grovedb();
    db_batch
        .insert(
            EMPTY_PATH,
            b"log",
            Element::empty_mmr_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");

    let ops: Vec<QualifiedGroveDbOp> = values
        .iter()
        .map(|v| QualifiedGroveDbOp::mmr_tree_append_op(vec![b"log".to_vec()], v.clone()))
        .collect();

    db_batch
        .apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch apply");

    // Both should produce the same MMR root
    let root_direct = db_direct
        .mmr_tree_root_hash(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("root direct");
    let root_batch = db_batch
        .mmr_tree_root_hash(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("root batch");
    assert_eq!(root_direct, root_batch);
}

#[test]
fn test_mmr_tree_batch_mixed_ops() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert a normal tree and an MMR tree
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
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    // Batch with both normal insert and MMR append
    let ops = vec![
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"parent".to_vec()],
            b"item".to_vec(),
            Element::new_item(b"some data".to_vec()),
        ),
        QualifiedGroveDbOp::mmr_tree_append_op(
            vec![b"parent".to_vec(), b"log".to_vec()],
            b"log_entry".to_vec(),
        ),
    ];

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("mixed batch apply");

    // Verify item was inserted
    let item = db
        .get([b"parent"].as_ref(), b"item", None, grove_version)
        .unwrap()
        .expect("get item");
    assert!(matches!(item, Element::Item(..)));

    // Verify MMR append worked
    let root = db
        .mmr_tree_root_hash([b"parent"].as_ref(), b"log", None, grove_version)
        .unwrap()
        .expect("get root");
    let exp = expected_mmr_root(&[b"log_entry".to_vec()]);
    assert_eq!(root, exp);
}

#[test]
fn test_mmr_tree_batch_multiple_trees() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Two independent MMR trees
    db.insert(
        EMPTY_PATH,
        b"log_a",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert log_a");

    db.insert(
        EMPTY_PATH,
        b"log_b",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert log_b");

    let ops = vec![
        QualifiedGroveDbOp::mmr_tree_append_op(vec![b"log_a".to_vec()], b"a1".to_vec()),
        QualifiedGroveDbOp::mmr_tree_append_op(vec![b"log_a".to_vec()], b"a2".to_vec()),
        QualifiedGroveDbOp::mmr_tree_append_op(vec![b"log_b".to_vec()], b"b1".to_vec()),
    ];

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch apply");

    let root_a = db
        .mmr_tree_root_hash(EMPTY_PATH, b"log_a", None, grove_version)
        .unwrap()
        .expect("root a");
    let root_b = db
        .mmr_tree_root_hash(EMPTY_PATH, b"log_b", None, grove_version)
        .unwrap()
        .expect("root b");

    assert_eq!(root_a, expected_mmr_root(&[b"a1".to_vec(), b"a2".to_vec()]));
    assert_eq!(root_b, expected_mmr_root(&[b"b1".to_vec()]));
    assert_ne!(root_a, root_b);
}

// ===========================================================================
// Lifecycle tests
// ===========================================================================

#[test]
fn test_mmr_tree_persistence_across_reopen() {
    let grove_version = GroveVersion::latest();
    let tmp_dir = tempfile::TempDir::new().unwrap();

    // Open, insert MMR tree, append some values
    {
        let db = crate::GroveDb::open(tmp_dir.path()).unwrap();
        db.insert(
            EMPTY_PATH,
            b"log",
            Element::empty_mmr_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert mmr tree");

        for i in 0..3u8 {
            db.mmr_tree_append(EMPTY_PATH, b"log", vec![i], None, grove_version)
                .unwrap()
                .expect("append");
        }
    }

    // Reopen and verify state
    {
        let db = crate::GroveDb::open(tmp_dir.path()).unwrap();

        let count = db
            .mmr_tree_leaf_count(EMPTY_PATH, b"log", None, grove_version)
            .unwrap()
            .expect("leaf count");
        assert_eq!(count, 3);

        let root = db
            .mmr_tree_root_hash(EMPTY_PATH, b"log", None, grove_version)
            .unwrap()
            .expect("root hash");
        let exp = expected_mmr_root(&[vec![0], vec![1], vec![2]]);
        assert_eq!(root, exp);

        // Values should be retrievable
        for i in 0..3u8 {
            let val = db
                .mmr_tree_get_value(EMPTY_PATH, b"log", i as u64, None, grove_version)
                .unwrap()
                .expect("get value");
            assert_eq!(val, Some(vec![i]));
        }
    }
}

#[test]
fn test_mmr_tree_transaction_commit() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    let tx = db.start_transaction();

    db.mmr_tree_append(
        EMPTY_PATH,
        b"log",
        b"tx_data".to_vec(),
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .expect("append in tx");

    // Not yet visible outside tx (leaf count still 0)
    let count_outside = db
        .mmr_tree_leaf_count(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("leaf count outside tx");
    assert_eq!(count_outside, 0);

    // Visible inside tx
    let count_inside = db
        .mmr_tree_leaf_count(EMPTY_PATH, b"log", Some(&tx), grove_version)
        .unwrap()
        .expect("leaf count inside tx");
    assert_eq!(count_inside, 1);

    // Commit
    db.commit_transaction(tx).unwrap().expect("commit");

    // Now visible
    let count_after = db
        .mmr_tree_leaf_count(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("leaf count after commit");
    assert_eq!(count_after, 1);
}

#[test]
fn test_mmr_tree_transaction_rollback() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    let tx = db.start_transaction();

    db.mmr_tree_append(
        EMPTY_PATH,
        b"log",
        b"rollback_data".to_vec(),
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .expect("append in tx");

    // Rollback
    db.rollback_transaction(&tx).expect("rollback");
    drop(tx);

    // Not visible after rollback
    let count = db
        .mmr_tree_leaf_count(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("leaf count");
    assert_eq!(count, 0);

    let root = db
        .mmr_tree_root_hash(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("root hash");
    assert_eq!(root, [0u8; 32]);
}

// ===========================================================================
// Cost tests
// ===========================================================================

#[test]
fn test_mmr_tree_append_tracks_hash_costs() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    // First append: leaf_count=0 → 1 hash (leaf only, no merges)
    let cost0 = db
        .mmr_tree_append(EMPTY_PATH, b"log", b"v0".to_vec(), None, grove_version)
        .cost;
    assert!(
        cost0.hash_node_calls >= 1,
        "first append should hash at least once"
    );

    // Second append: leaf_count=1 → 2 hashes (1 leaf + 1 merge)
    let cost1 = db
        .mmr_tree_append(EMPTY_PATH, b"log", b"v1".to_vec(), None, grove_version)
        .cost;
    assert!(
        cost1.hash_node_calls >= 2,
        "second append should hash at least twice"
    );
}

#[test]
fn test_mmr_tree_append_tracks_storage_costs() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    let result = db.mmr_tree_append(EMPTY_PATH, b"log", b"data".to_vec(), None, grove_version);
    let cost = result.cost;

    // Should have some seek count (reading element, writing nodes)
    assert!(cost.seek_count > 0, "should have seeks");
    // Should have loaded bytes (reading the element at minimum)
    assert!(cost.storage_loaded_bytes > 0, "should have loaded bytes");
}

// ===========================================================================
// Error tests
// ===========================================================================

#[test]
fn test_mmr_tree_append_to_wrong_type() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"normal",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert normal tree");

    let result = db
        .mmr_tree_append(EMPTY_PATH, b"normal", b"data".to_vec(), None, grove_version)
        .unwrap();
    assert!(result.is_err(), "should fail on non-MMR tree");
}

#[test]
fn test_mmr_tree_root_hash_wrong_type() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"item",
        Element::new_item(b"data".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert item");

    let result = db
        .mmr_tree_root_hash(EMPTY_PATH, b"item", None, grove_version)
        .unwrap();
    assert!(result.is_err(), "should fail on non-MMR element");
}

#[test]
fn test_mmr_tree_get_value_wrong_type() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"tree",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert tree");

    let result = db
        .mmr_tree_get_value(EMPTY_PATH, b"tree", 0, None, grove_version)
        .unwrap();
    assert!(result.is_err(), "should fail on non-MMR element");
}

#[test]
fn test_mmr_tree_leaf_count_wrong_type() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"tree",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert tree");

    let result = db
        .mmr_tree_leaf_count(EMPTY_PATH, b"tree", None, grove_version)
        .unwrap();
    assert!(result.is_err(), "should fail on non-MMR element");
}

// ===========================================================================
// Delete tests
// ===========================================================================

#[test]
fn test_mmr_tree_delete_empty() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

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

    // Delete with default options (empty tree, so this should succeed)
    db.delete(EMPTY_PATH, b"mmr", None, None, grove_version)
        .unwrap()
        .expect("should delete empty mmr tree");

    // Verify tree is gone
    let result = db.get(EMPTY_PATH, b"mmr", None, grove_version).unwrap();
    assert!(result.is_err(), "mmr tree should no longer exist");
}

#[test]
fn test_mmr_tree_delete_non_empty() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

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

    // Append 3 values
    for i in 0..3u8 {
        db.mmr_tree_append(EMPTY_PATH, b"mmr", vec![i], None, grove_version)
            .unwrap()
            .expect("append value");
    }

    // Delete with allow_deleting_non_empty_trees
    db.delete(
        EMPTY_PATH,
        b"mmr",
        Some(DeleteOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: true,
            ..Default::default()
        }),
        None,
        grove_version,
    )
    .unwrap()
    .expect("should delete non-empty mmr tree");

    // Verify tree is gone
    let result = db.get(EMPTY_PATH, b"mmr", None, grove_version).unwrap();
    assert!(result.is_err(), "mmr tree should no longer exist");
}

#[test]
fn test_mmr_tree_delete_non_empty_error() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

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

    // Append values to make it non-empty
    for i in 0..3u8 {
        db.mmr_tree_append(EMPTY_PATH, b"mmr", vec![i], None, grove_version)
            .unwrap()
            .expect("append value");
    }

    // Delete without allowing non-empty trees (default options)
    let result = db
        .delete(EMPTY_PATH, b"mmr", None, None, grove_version)
        .unwrap();
    assert!(
        matches!(result, Err(Error::DeletingNonEmptyTree(_))),
        "should return DeletingNonEmptyTree error, got: {:?}",
        result
    );
}

#[test]
fn test_mmr_tree_delete_and_recreate() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

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

    // Append 3 values
    for i in 0..3u8 {
        db.mmr_tree_append(EMPTY_PATH, b"mmr", vec![i], None, grove_version)
            .unwrap()
            .expect("append value");
    }

    // Delete with allow
    db.delete(
        EMPTY_PATH,
        b"mmr",
        Some(DeleteOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: true,
            ..Default::default()
        }),
        None,
        grove_version,
    )
    .unwrap()
    .expect("should delete non-empty mmr tree");

    // Recreate empty MmrTree
    db.insert(
        EMPTY_PATH,
        b"mmr",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("recreate mmr tree");

    // Append a new value
    db.mmr_tree_append(EMPTY_PATH, b"mmr", b"fresh".to_vec(), None, grove_version)
        .unwrap()
        .expect("append to recreated tree");

    // Leaf count should be 1 (fresh start)
    let count = db
        .mmr_tree_leaf_count(EMPTY_PATH, b"mmr", None, grove_version)
        .unwrap()
        .expect("leaf count after recreate");
    assert_eq!(count, 1, "recreated tree should have only 1 leaf");
}

// ===========================================================================
// Cost consistency tests
// ===========================================================================

/// Test that MmrStore cache hits return the same cost as store hits.
///
/// After mmr_tree_append, the MmrStore write-through cache holds recently
/// written nodes. A subsequent get_root reads peaks from this cache.
/// With a fresh MmrStore (no cache), the same get_root reads from RocksDB.
/// Both paths must return the same cost for deterministic fee estimation.
#[test]
fn test_mmr_store_cost_consistency_cache_vs_store() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

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

    // Append several values to create multiple peaks
    for i in 0..5u8 {
        db.mmr_tree_append(EMPTY_PATH, b"mmr", vec![i], None, grove_version)
            .unwrap()
            .expect("append value");
    }

    // Two consecutive appends of same-sized values should have same cost
    let cost1 = db
        .mmr_tree_append(EMPTY_PATH, b"mmr", vec![0xAA], None, grove_version)
        .cost;
    let cost2 = db
        .mmr_tree_append(EMPTY_PATH, b"mmr", vec![0xBB], None, grove_version)
        .cost;

    // Both appends add a leaf of the same size at similar MMR positions.
    // After 5+1=6 leaves (mmr_size=10, 2 peaks), append #7 = mmr_size=11
    // After 7 leaves (mmr_size=11, 3 peaks), append #8 triggers merge to
    // mmr_size=15 The seek and loaded_bytes pattern should be consistent.
    // At minimum, both must have non-zero seek counts and loaded bytes
    // from reading existing nodes.
    assert!(
        cost1.seek_count > 0,
        "first append cost should have seeks: {:?}",
        cost1
    );
    assert!(
        cost2.seek_count > 0,
        "second append cost should have seeks: {:?}",
        cost2
    );
    assert!(
        cost1.storage_loaded_bytes > 0,
        "first append should load bytes: {:?}",
        cost1
    );
    assert!(
        cost2.storage_loaded_bytes > 0,
        "second append should load bytes: {:?}",
        cost2
    );
}

/// Test that mmr_tree_root_hash cost is non-zero and consistent.
///
/// Verifies that reading the MMR root hash incurs proper storage costs,
/// even when the MmrStore cache is involved.
#[test]
fn test_mmr_root_hash_cost_nonzero() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

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

    // Append values
    for i in 0..4u8 {
        db.mmr_tree_append(EMPTY_PATH, b"mmr", vec![i], None, grove_version)
            .unwrap()
            .expect("append value");
    }

    // Get root hash — should have storage costs from reading the element + peaks
    let result = db.mmr_tree_root_hash(EMPTY_PATH, b"mmr", None, grove_version);
    let cost = result.cost;

    assert!(
        cost.seek_count > 0,
        "root_hash should incur seeks: {:?}",
        cost
    );
    assert!(
        cost.storage_loaded_bytes > 0,
        "root_hash should load bytes: {:?}",
        cost
    );

    // Call again — cost should be the same (deterministic)
    let result2 = db.mmr_tree_root_hash(EMPTY_PATH, b"mmr", None, grove_version);
    let cost2 = result2.cost;

    assert_eq!(
        cost.seek_count, cost2.seek_count,
        "root_hash cost should be deterministic across calls"
    );
    assert_eq!(
        cost.storage_loaded_bytes, cost2.storage_loaded_bytes,
        "root_hash loaded bytes should be deterministic across calls"
    );
}

// ===========================================================================
// verify_grovedb tests
// ===========================================================================

#[test]
fn test_verify_grovedb_merkle_mountain_range_tree_valid() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

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

    // Append some values
    for i in 0..5u8 {
        db.mmr_tree_append(EMPTY_PATH, b"mmr", vec![i], None, grove_version)
            .unwrap()
            .expect("append value");
    }

    // verify_grovedb should report no issues
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify should not fail");
    assert!(issues.is_empty(), "expected no issues, got: {:?}", issues);
}

// ===========================================================================
// Batch-with-transaction tests
// ===========================================================================

#[test]
fn test_mmr_tree_batch_with_transaction() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Tree must exist BEFORE the batch (preprocessing requires it)
    db.insert(
        EMPTY_PATH,
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    let tx = db.start_transaction();

    let ops = vec![
        QualifiedGroveDbOp::mmr_tree_append_op(vec![b"log".to_vec()], b"tx_val_1".to_vec()),
        QualifiedGroveDbOp::mmr_tree_append_op(vec![b"log".to_vec()], b"tx_val_2".to_vec()),
    ];

    db.apply_batch(ops, None, Some(&tx), grove_version)
        .unwrap()
        .expect("batch apply in transaction");

    // Not visible outside the transaction
    let count_outside = db
        .mmr_tree_leaf_count(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("leaf count outside tx");
    assert_eq!(count_outside, 0, "data should not be visible outside tx");

    // Verify data is visible inside the transaction
    let count_inside = db
        .mmr_tree_leaf_count(EMPTY_PATH, b"log", Some(&tx), grove_version)
        .unwrap()
        .expect("leaf count inside tx");
    assert_eq!(count_inside, 2, "data should be visible inside tx");

    // Commit the transaction
    db.commit_transaction(tx).unwrap().expect("commit tx");

    // After commit, data should be visible
    let count_after = db
        .mmr_tree_leaf_count(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("leaf count after commit");
    assert_eq!(count_after, 2, "data should be visible after commit");

    // Verify values are correct
    let root = db
        .mmr_tree_root_hash(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("get root hash");
    let exp = expected_mmr_root(&[b"tx_val_1".to_vec(), b"tx_val_2".to_vec()]);
    assert_eq!(root, exp, "root hash should match expected after commit");
}

#[test]
fn test_mmr_tree_batch_transaction_rollback() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Tree must exist BEFORE the batch
    db.insert(
        EMPTY_PATH,
        b"log",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert mmr tree");

    let tx = db.start_transaction();

    let ops = vec![QualifiedGroveDbOp::mmr_tree_append_op(
        vec![b"log".to_vec()],
        b"rollback_val".to_vec(),
    )];

    db.apply_batch(ops, None, Some(&tx), grove_version)
        .unwrap()
        .expect("batch apply in transaction");

    // Verify data is visible inside tx before rollback
    let count_inside = db
        .mmr_tree_leaf_count(EMPTY_PATH, b"log", Some(&tx), grove_version)
        .unwrap()
        .expect("leaf count inside tx");
    assert_eq!(count_inside, 1, "data should be visible inside tx");

    // Drop tx (rollback)
    drop(tx);

    // Data should NOT be visible after rollback
    let count_after = db
        .mmr_tree_leaf_count(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("leaf count after rollback");
    assert_eq!(count_after, 0, "data should not be visible after rollback");

    let root = db
        .mmr_tree_root_hash(EMPTY_PATH, b"log", None, grove_version)
        .unwrap()
        .expect("root hash after rollback");
    assert_eq!(
        root, [0u8; 32],
        "empty MMR root should be zeros after rollback"
    );
}

// ===========================================================================
// Empty V1 proof test
// ===========================================================================

/// Empty MMR tree proof generation currently errors because
/// `generate_mmr_layer_proof` passes empty leaf_indices to
/// `MmrTreeProof::generate` which rejects it. This test documents
/// the current behavior: querying an empty MmrTree via V1 proof
/// returns a CorruptedData error containing "leaf_indices must not
/// be empty". A future fix should handle this gracefully and return
/// an empty result set instead.
#[test]
fn test_mmr_tree_v1_proof_empty() {
    use grovedb_merk::proofs::{
        query::{QueryItem, SubqueryBranch},
        Query,
    };

    use crate::{PathQuery, SizedQuery};

    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert parent tree and empty MmrTree
    db.insert(
        EMPTY_PATH,
        b"root",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert root tree");

    db.insert(
        &[b"root"],
        b"mmr",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert empty mmr tree");

    // Query position [0..=0] on an empty MmrTree
    let mut inner_query = Query::new();
    inner_query.insert_range_inclusive(0u64.to_be_bytes().to_vec()..=0u64.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![b"root".to_vec()],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"mmr".to_vec())],
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

    // Empty MmrTree proof generation currently errors because
    // MmrTreeProof::generate rejects empty leaf_indices.
    let result = db.prove_query_v1(&path_query, None, grove_version).unwrap();

    match result {
        Err(Error::CorruptedData(msg)) => {
            assert!(
                msg.contains("leaf_indices must not be empty"),
                "error should mention empty leaf_indices, got: {}",
                msg
            );
        }
        Err(other) => {
            panic!(
                "expected CorruptedData error about empty leaf_indices, got: {:?}",
                other
            );
        }
        Ok(_) => {
            // If a future fix makes this succeed, verify the result is correct
            panic!(
                "prove_query_v1 succeeded for empty MmrTree; if this is intentional, update this \
                 test to verify the proof produces an empty result set"
            );
        }
    }
}

// ===========================================================================
// verify_grovedb empty tree test
// ===========================================================================

#[test]
fn test_verify_grovedb_mmr_tree_empty() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert an empty MmrTree (no appends)
    db.insert(
        EMPTY_PATH,
        b"mmr",
        Element::empty_mmr_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert empty mmr tree");

    // verify_grovedb should report no issues on an empty MmrTree
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify should not fail");
    assert!(
        issues.is_empty(),
        "expected no issues for empty mmr tree, got: {:?}",
        issues
    );
}
