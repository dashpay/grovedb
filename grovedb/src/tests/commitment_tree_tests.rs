//! Commitment tree integration tests
//!
//! Tests for CommitmentTree (CountTree + Sinsemilla Frontier) as a GroveDB
//! subtree type.

use grovedb_commitment_tree::{Anchor, CommitmentFrontier};
use grovedb_version::version::GroveVersion;

use crate::{
    batch::QualifiedGroveDbOp,
    operations::delete::DeleteOptions,
    tests::{common::EMPTY_PATH, make_empty_grovedb},
    Element,
};

// ---------------------------------------------------------------------------
// Helper: generate a deterministic 32-byte cmx from an index
// ---------------------------------------------------------------------------
fn test_cmx(index: u8) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[0] = index;
    // Ensure the bytes represent a valid Pallas field element by clearing the
    // top bit (Pallas modulus < 2^255).
    bytes[31] &= 0x7f;
    bytes
}

/// Build the expected sinsemilla root after appending `leaves` in order.
fn expected_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    let mut frontier = CommitmentFrontier::new();
    for leaf in leaves {
        frontier.append(*leaf).expect("valid leaf");
    }
    frontier.root_hash()
}

// ===========================================================================
// Element tests
// ===========================================================================

#[test]
fn test_insert_commitment_tree_at_root() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"commitments",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("successful commitment tree insert at root");

    let element = db
        .get(EMPTY_PATH, b"commitments", None, grove_version)
        .unwrap()
        .expect("should retrieve commitment tree");
    assert!(element.is_commitment_tree());
}

#[test]
fn test_commitment_tree_under_normal_tree() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert a normal tree as parent
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

    // Insert commitment tree under it
    db.insert(
        [b"parent"].as_ref(),
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree under parent");

    let element = db
        .get([b"parent"].as_ref(), b"pool", None, grove_version)
        .unwrap()
        .expect("should get pool");
    assert!(element.is_commitment_tree());
}

#[test]
fn test_commitment_tree_with_flags() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    let flags = Some(vec![1, 2, 3]);
    db.insert(
        EMPTY_PATH,
        b"flagged",
        Element::empty_commitment_tree_with_flags(flags.clone()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert flagged commitment tree");

    let element = db
        .get(EMPTY_PATH, b"flagged", None, grove_version)
        .unwrap()
        .expect("get flagged");
    assert!(element.is_commitment_tree());
}

#[test]
fn test_empty_commitment_tree_serialization_roundtrip() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"ct",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    // Re-fetch and verify
    let elem = db
        .get(EMPTY_PATH, b"ct", None, grove_version)
        .unwrap()
        .expect("get");
    assert!(elem.is_commitment_tree());
}

// ===========================================================================
// Insert tests
// ===========================================================================

#[test]
fn test_commitment_tree_insert_single() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    let cmx = test_cmx(1);
    let payload = b"encrypted_note_data".to_vec();

    let (root, position) = db
        .commitment_tree_insert(
            EMPTY_PATH,
            b"pool",
            cmx,
            payload.clone(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");

    assert_eq!(position, 0);

    // Root should match expected sinsemilla computation
    let exp_root = expected_root(&[cmx]);
    assert_eq!(root, exp_root);

    // Verify the item was stored in the subtree — key = 0u64 BE
    let item_key = 0u64.to_be_bytes();
    let stored = db
        .get([b"pool"].as_ref(), &item_key, None, grove_version)
        .unwrap()
        .expect("get stored item");

    // Item value is cmx || payload
    if let Element::Item(value, _) = stored {
        assert_eq!(&value[..32], &cmx);
        assert_eq!(&value[32..], &payload[..]);
    } else {
        panic!("expected Item element");
    }
}

#[test]
fn test_commitment_tree_insert_multiple() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    let cmx0 = test_cmx(1);
    let cmx1 = test_cmx(2);
    let cmx2 = test_cmx(3);

    let (_, pos0) = db
        .commitment_tree_insert(EMPTY_PATH, b"pool", cmx0, vec![10], None, grove_version)
        .unwrap()
        .expect("insert 0");
    let (_, pos1) = db
        .commitment_tree_insert(EMPTY_PATH, b"pool", cmx1, vec![20], None, grove_version)
        .unwrap()
        .expect("insert 1");
    let (root2, pos2) = db
        .commitment_tree_insert(EMPTY_PATH, b"pool", cmx2, vec![30], None, grove_version)
        .unwrap()
        .expect("insert 2");

    assert_eq!(pos0, 0);
    assert_eq!(pos1, 1);
    assert_eq!(pos2, 2);

    // Final root should match appending all three leaves
    let exp = expected_root(&[cmx0, cmx1, cmx2]);
    assert_eq!(root2, exp);

    // Verify sequential keys
    for i in 0u64..3 {
        let key = i.to_be_bytes();
        let elem = db
            .get([b"pool"].as_ref(), &key, None, grove_version)
            .unwrap()
            .expect("get item");
        assert!(matches!(elem, Element::Item(..)));
    }
}

#[test]
fn test_commitment_tree_insert_with_transaction() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    let tx = db.start_transaction();
    let cmx = test_cmx(42);

    let (_root, pos) = db
        .commitment_tree_insert(
            EMPTY_PATH,
            b"pool",
            cmx,
            b"payload".to_vec(),
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert in tx");

    assert_eq!(pos, 0);

    // Not visible outside tx
    let key = 0u64.to_be_bytes();
    let outside = db
        .get([b"pool"].as_ref(), &key, None, grove_version)
        .unwrap();
    assert!(outside.is_err());

    // Visible inside tx
    let inside = db
        .get([b"pool"].as_ref(), &key, Some(&tx), grove_version)
        .unwrap()
        .expect("get in tx");
    assert!(matches!(inside, Element::Item(..)));

    // Commit and verify visible
    db.commit_transaction(tx).unwrap().expect("commit");

    let after_commit = db
        .get([b"pool"].as_ref(), &key, None, grove_version)
        .unwrap()
        .expect("get after commit");
    assert!(matches!(after_commit, Element::Item(..)));
}

#[test]
fn test_commitment_tree_insert_transaction_rollback() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    let tx = db.start_transaction();

    db.commitment_tree_insert(
        EMPTY_PATH,
        b"pool",
        test_cmx(1),
        vec![],
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .expect("insert in tx");

    // Rollback by dropping the transaction
    db.rollback_transaction(&tx).expect("rollback");
    drop(tx);

    // Item should not be visible
    let key = 0u64.to_be_bytes();
    let result = db
        .get([b"pool"].as_ref(), &key, None, grove_version)
        .unwrap();
    assert!(result.is_err());
}

// ===========================================================================
// Anchor / Frontier tests
// ===========================================================================

#[test]
fn test_commitment_tree_anchor_empty() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    let anchor = db
        .commitment_tree_anchor(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("get anchor");

    // Empty frontier has a well-defined empty tree root (not all zeros)
    let empty_root = expected_root(&[]);
    assert_eq!(anchor, Anchor::from_bytes(empty_root).unwrap());
}

#[test]
fn test_commitment_tree_anchor_changes_after_insert() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    let anchor_before = db
        .commitment_tree_anchor(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("get anchor before");

    db.commitment_tree_insert(
        EMPTY_PATH,
        b"pool",
        test_cmx(1),
        vec![],
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let anchor_after = db
        .commitment_tree_anchor(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("get anchor after");

    assert_ne!(anchor_before, anchor_after);
}

#[test]
fn test_commitment_tree_anchor_deterministic() {
    let grove_version = GroveVersion::latest();

    // Two independent databases with the same inserts should produce same anchor
    let db1 = make_empty_grovedb();
    let db2 = make_empty_grovedb();

    for db in [&db1, &db2] {
        db.insert(
            EMPTY_PATH,
            b"pool",
            Element::empty_commitment_tree(),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert ct");

        db.commitment_tree_insert(
            EMPTY_PATH,
            b"pool",
            test_cmx(10),
            vec![1, 2, 3],
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert 1");

        db.commitment_tree_insert(
            EMPTY_PATH,
            b"pool",
            test_cmx(20),
            vec![4, 5, 6],
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert 2");
    }

    let anchor1 = db1
        .commitment_tree_anchor(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("anchor1");
    let anchor2 = db2
        .commitment_tree_anchor(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("anchor2");

    assert_eq!(anchor1, anchor2);
}

// ===========================================================================
// Root hash propagation tests
// ===========================================================================

#[test]
fn test_commitment_tree_insert_propagates_root_hash() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    let root_hash_before = db.root_hash(None, grove_version).unwrap().unwrap();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    let root_hash_after_create = db.root_hash(None, grove_version).unwrap().unwrap();
    assert_ne!(root_hash_before, root_hash_after_create);

    db.commitment_tree_insert(
        EMPTY_PATH,
        b"pool",
        test_cmx(1),
        b"data".to_vec(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert item");

    let root_hash_after_insert = db.root_hash(None, grove_version).unwrap().unwrap();
    assert_ne!(root_hash_after_create, root_hash_after_insert);
}

#[test]
fn test_commitment_tree_nested_propagation() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // parent (Tree) -> pool (CommitmentTree) -> items
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
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool");

    let root_before = db.root_hash(None, grove_version).unwrap().unwrap();

    db.commitment_tree_insert(
        [b"parent"].as_ref(),
        b"pool",
        test_cmx(1),
        vec![],
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert into nested pool");

    let root_after = db.root_hash(None, grove_version).unwrap().unwrap();
    assert_ne!(root_before, root_after);
}

// ===========================================================================
// Count aggregation tests
// ===========================================================================

#[test]
fn test_commitment_tree_count_aggregation() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    // Check count is 0 initially
    let elem = db
        .get(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("get pool");
    assert_eq!(elem.count_value_or_default(), 0);

    // Insert 3 items
    for i in 0..3u8 {
        db.commitment_tree_insert(
            EMPTY_PATH,
            b"pool",
            test_cmx(i + 1),
            vec![i],
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
    }

    let elem = db
        .get(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("get pool after inserts");
    assert_eq!(elem.count_value_or_default(), 3);
}

// ===========================================================================
// Batch operation tests
// ===========================================================================

#[test]
fn test_commitment_tree_batch_insert() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // First insert the commitment tree
    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    let ops = vec![
        QualifiedGroveDbOp::commitment_tree_insert_op(
            vec![],
            b"pool".to_vec(),
            test_cmx(1),
            b"payload1".to_vec(),
        ),
        QualifiedGroveDbOp::commitment_tree_insert_op(
            vec![],
            b"pool".to_vec(),
            test_cmx(2),
            b"payload2".to_vec(),
        ),
        QualifiedGroveDbOp::commitment_tree_insert_op(
            vec![],
            b"pool".to_vec(),
            test_cmx(3),
            b"payload3".to_vec(),
        ),
    ];

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch apply");

    // Verify items were stored
    for i in 0u64..3 {
        let key = i.to_be_bytes();
        let elem = db
            .get([b"pool"].as_ref(), &key, None, grove_version)
            .unwrap()
            .expect("get item");
        assert!(matches!(elem, Element::Item(..)));
    }

    // Verify count
    let elem = db
        .get(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("get pool");
    assert_eq!(elem.count_value_or_default(), 3);

    // Verify anchor matches expected
    let anchor = db
        .commitment_tree_anchor(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("anchor");
    let exp = expected_root(&[test_cmx(1), test_cmx(2), test_cmx(3)]);
    assert_eq!(anchor, Anchor::from_bytes(exp).unwrap());
}

#[test]
fn test_commitment_tree_batch_with_transaction() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    let tx = db.start_transaction();

    let ops = vec![QualifiedGroveDbOp::commitment_tree_insert_op(
        vec![],
        b"pool".to_vec(),
        test_cmx(1),
        vec![],
    )];

    db.apply_batch(ops, None, Some(&tx), grove_version)
        .unwrap()
        .expect("batch in tx");

    // Not visible outside transaction
    let key = 0u64.to_be_bytes();
    let outside = db
        .get([b"pool"].as_ref(), &key, None, grove_version)
        .unwrap();
    assert!(outside.is_err());

    // Commit and verify
    db.commit_transaction(tx).unwrap().expect("commit");

    let elem = db
        .get([b"pool"].as_ref(), &key, None, grove_version)
        .unwrap()
        .expect("get after commit");
    assert!(matches!(elem, Element::Item(..)));
}

// ===========================================================================
// Batch + non-batch consistency test
// ===========================================================================

#[test]
fn test_commitment_tree_batch_and_nonbatch_same_result() {
    let grove_version = GroveVersion::latest();

    // Database A: use non-batch API
    let db_a = make_empty_grovedb();
    db_a.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    for i in 1..=3u8 {
        db_a.commitment_tree_insert(
            EMPTY_PATH,
            b"pool",
            test_cmx(i),
            vec![i],
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
    }

    // Database B: use batch API
    let db_b = make_empty_grovedb();
    db_b.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    let ops = (1..=3u8)
        .map(|i| {
            QualifiedGroveDbOp::commitment_tree_insert_op(
                vec![],
                b"pool".to_vec(),
                test_cmx(i),
                vec![i],
            )
        })
        .collect();

    db_b.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch");

    // Both should have same GroveDB root hash
    let root_a = db_a.root_hash(None, grove_version).unwrap().unwrap();
    let root_b = db_b.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(root_a, root_b);

    // Both should have same anchor
    let anchor_a = db_a
        .commitment_tree_anchor(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("anchor a");
    let anchor_b = db_b
        .commitment_tree_anchor(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("anchor b");
    assert_eq!(anchor_a, anchor_b);
}

// ===========================================================================
// Delete tests
// ===========================================================================

#[test]
fn test_commitment_tree_delete() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    // Insert an item
    db.commitment_tree_insert(
        EMPTY_PATH,
        b"pool",
        test_cmx(1),
        vec![],
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    // Delete the entire commitment tree (non-empty, so must allow)
    let delete_opts = Some(DeleteOptions {
        allow_deleting_non_empty_trees: true,
        deleting_non_empty_trees_returns_error: false,
        ..Default::default()
    });
    db.delete(EMPTY_PATH, b"pool", delete_opts, None, grove_version)
        .unwrap()
        .expect("delete");

    let result = db.get(EMPTY_PATH, b"pool", None, grove_version).unwrap();
    assert!(result.is_err());
}

#[test]
fn test_commitment_tree_delete_and_recreate() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create, insert, delete
    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("create");

    db.commitment_tree_insert(
        EMPTY_PATH,
        b"pool",
        test_cmx(1),
        vec![],
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let delete_opts = Some(DeleteOptions {
        allow_deleting_non_empty_trees: true,
        deleting_non_empty_trees_returns_error: false,
        ..Default::default()
    });
    db.delete(EMPTY_PATH, b"pool", delete_opts, None, grove_version)
        .unwrap()
        .expect("delete");

    // Recreate
    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("recreate");

    // Fresh commitment tree should have empty anchor
    let anchor = db
        .commitment_tree_anchor(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("anchor after recreate");
    let empty_root = expected_root(&[]);
    assert_eq!(anchor, Anchor::from_bytes(empty_root).unwrap());

    // Should be able to insert again at position 0
    let (_, pos) = db
        .commitment_tree_insert(
            EMPTY_PATH,
            b"pool",
            test_cmx(99),
            vec![],
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert after recreate");
    assert_eq!(pos, 0);
}

// ===========================================================================
// Error handling tests
// ===========================================================================

#[test]
fn test_commitment_tree_insert_on_non_commitment_tree_fails() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert a normal tree
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
        .commitment_tree_insert(
            EMPTY_PATH,
            b"normal",
            test_cmx(1),
            vec![],
            None,
            grove_version,
        )
        .unwrap();
    assert!(result.is_err());
}

#[test]
fn test_commitment_tree_anchor_on_non_commitment_tree_fails() {
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
        .commitment_tree_anchor(EMPTY_PATH, b"normal", None, grove_version)
        .unwrap();
    assert!(result.is_err());
}

// ===========================================================================
// Multi-pool architecture test
// ===========================================================================

#[test]
fn test_multiple_commitment_trees_independent() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Create two independent pools
    db.insert(
        EMPTY_PATH,
        b"pool_a",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool_a");

    db.insert(
        EMPTY_PATH,
        b"pool_b",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool_b");

    // Insert different data into each
    db.commitment_tree_insert(
        EMPTY_PATH,
        b"pool_a",
        test_cmx(1),
        b"note_a".to_vec(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert into pool_a");

    db.commitment_tree_insert(
        EMPTY_PATH,
        b"pool_b",
        test_cmx(2),
        b"note_b".to_vec(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert into pool_b");

    // Anchors should differ
    let anchor_a = db
        .commitment_tree_anchor(EMPTY_PATH, b"pool_a", None, grove_version)
        .unwrap()
        .expect("anchor_a");
    let anchor_b = db
        .commitment_tree_anchor(EMPTY_PATH, b"pool_b", None, grove_version)
        .unwrap()
        .expect("anchor_b");
    assert_ne!(anchor_a, anchor_b);

    // Each has count 1
    let elem_a = db
        .get(EMPTY_PATH, b"pool_a", None, grove_version)
        .unwrap()
        .expect("get pool_a");
    let elem_b = db
        .get(EMPTY_PATH, b"pool_b", None, grove_version)
        .unwrap()
        .expect("get pool_b");
    assert_eq!(elem_a.count_value_or_default(), 1);
    assert_eq!(elem_b.count_value_or_default(), 1);
}

// ---------------------------------------------------------------------------
// verify_grovedb tests
// ---------------------------------------------------------------------------

#[test]
fn test_verify_grovedb_commitment_tree_valid() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert a commitment tree and add some notes
    db.insert(
        EMPTY_PATH,
        b"ct",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    db.commitment_tree_insert(EMPTY_PATH, b"ct", test_cmx(1), vec![], None, grove_version)
        .unwrap()
        .expect("insert 1");

    db.commitment_tree_insert(EMPTY_PATH, b"ct", test_cmx(2), vec![], None, grove_version)
        .unwrap()
        .expect("insert 2");

    // verify_grovedb should report no issues
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify");
    assert!(issues.is_empty(), "expected no issues, got: {:?}", issues);
}

#[test]
fn test_verify_grovedb_commitment_tree_detects_corrupted_frontier() {
    use grovedb_storage::{Storage, StorageContext};

    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert a commitment tree with one note
    db.insert(
        EMPTY_PATH,
        b"ct",
        Element::empty_commitment_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    db.commitment_tree_insert(EMPTY_PATH, b"ct", test_cmx(1), vec![], None, grove_version)
        .unwrap()
        .expect("insert 1");

    // Build frontiers with different numbers of notes and confirm roots differ.
    // NOTE: test_cmx(2) == MerkleHashOrchard::empty_leaf() (pallas::Base(2)),
    // so we use test_cmx(3) to avoid the degenerate case where appending the
    // empty leaf doesn't change the root.
    let mut f1 = CommitmentFrontier::new();
    let root_after_1 = f1.append(test_cmx(1)).unwrap();

    let mut f2 = CommitmentFrontier::new();
    f2.append(test_cmx(1)).unwrap();
    let root_after_2 = f2.append(test_cmx(3)).unwrap();
    // Sanity: roots must differ for test to be meaningful
    assert_ne!(root_after_1, root_after_2);

    // Corrupt the frontier in aux storage by writing the 2-note frontier
    let tx = db.start_transaction();
    let ct_path: &[&[u8]] = &[b"ct"];
    let storage_ctx = db
        .db
        .get_immediate_storage_context(ct_path.into(), &tx)
        .unwrap();
    storage_ctx
        .put_aux(
            crate::operations::commitment_tree::COMMITMENT_TREE_DATA_KEY,
            &f2.serialize(),
            None,
        )
        .unwrap()
        .expect("put_aux");
    drop(storage_ctx);
    tx.commit().expect("tx commit");

    // verify_grovedb should detect the corruption
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify");
    assert!(
        !issues.is_empty(),
        "expected issues from corrupted frontier"
    );
}
