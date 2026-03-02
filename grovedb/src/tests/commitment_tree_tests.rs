//! Commitment tree integration tests
//!
//! Tests for CommitmentTree (BulkAppendTree + Sinsemilla Frontier) as a GroveDB
//! subtree type.

use grovedb_commitment_tree::{
    serialize_ciphertext, Anchor, CommitmentFrontier, DashMemo, NoteBytesData,
    TransmittedNoteCiphertext,
};
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

/// Default chunk power for tests (2^10 = 1024, large enough that compaction
/// doesn't happen in most tests with only a few items).
const TEST_CHUNK_POWER: u8 = 10;

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

/// Generate a deterministic 32-byte rho (nullifier) from an index.
fn test_rho(index: u8) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[0] = index;
    bytes[1] = 0xAA; // distinguishes rho from cmx/ciphertext
    bytes
}

/// Create a deterministic test ciphertext for DashMemo from an index.
///
/// Layout: `epk_bytes (32) || enc_ciphertext (104) || out_ciphertext (80)` =
/// 216 bytes.
fn test_ciphertext(index: u8) -> TransmittedNoteCiphertext<DashMemo> {
    let mut epk_bytes = [0u8; 32];
    epk_bytes[0] = index;
    epk_bytes[1] = index.wrapping_add(1);

    let mut enc_data = [0u8; 104];
    enc_data[0] = index;
    enc_data[1] = 0xEC;
    let enc_ciphertext = NoteBytesData(enc_data);

    let mut out_ciphertext = [0u8; 80];
    out_ciphertext[0] = index;
    out_ciphertext[1] = 0x0C;

    TransmittedNoteCiphertext::from_parts(epk_bytes, enc_ciphertext, out_ciphertext)
}

/// Build the expected sinsemilla root after appending `leaves` in order.
fn expected_root(leaves: &[[u8; 32]]) -> [u8; 32] {
    let mut frontier = CommitmentFrontier::new();
    for leaf in leaves {
        frontier.append(*leaf).value.expect("valid leaf");
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
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
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
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
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
        Element::empty_commitment_tree_with_flags(TEST_CHUNK_POWER, flags.clone()),
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
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
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
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    let cmx = test_cmx(1);
    let ct = test_ciphertext(1);
    let payload = serialize_ciphertext(&ct);

    let (root, position) = db
        .commitment_tree_insert(
            EMPTY_PATH,
            b"pool",
            cmx,
            test_rho(1),
            test_ciphertext(1),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");

    assert_eq!(position, 0);

    // Root should match expected sinsemilla computation
    let exp_root = expected_root(&[cmx]);
    assert_eq!(root, exp_root);

    // Verify the item was stored — use commitment_tree_get_value
    let stored = db
        .commitment_tree_get_value(EMPTY_PATH, b"pool", 0, None, grove_version)
        .unwrap()
        .expect("get stored value");

    // Item value is cmx || rho || payload
    let value = stored.expect("value should exist");
    assert_eq!(&value[..32], &cmx);
    assert_eq!(&value[32..64], &test_rho(1));
    assert_eq!(&value[64..], &payload[..]);
}

#[test]
fn test_commitment_tree_insert_multiple() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
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
        .commitment_tree_insert(
            EMPTY_PATH,
            b"pool",
            cmx0,
            test_rho(1),
            test_ciphertext(1),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert 0");
    let (_, pos1) = db
        .commitment_tree_insert(
            EMPTY_PATH,
            b"pool",
            cmx1,
            test_rho(2),
            test_ciphertext(2),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert 1");
    let (root2, pos2) = db
        .commitment_tree_insert(
            EMPTY_PATH,
            b"pool",
            cmx2,
            test_rho(3),
            test_ciphertext(3),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert 2");

    assert_eq!(pos0, 0);
    assert_eq!(pos1, 1);
    assert_eq!(pos2, 2);

    // Final root should match appending all three leaves
    let exp = expected_root(&[cmx0, cmx1, cmx2]);
    assert_eq!(root2, exp);

    // Verify all items stored via commitment_tree_get_value
    for i in 0u64..3 {
        let value = db
            .commitment_tree_get_value(EMPTY_PATH, b"pool", i, None, grove_version)
            .unwrap()
            .expect("get value");
        assert!(value.is_some(), "value at position {} should exist", i);
    }
}

#[test]
fn test_commitment_tree_insert_with_transaction() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
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
            test_rho(42),
            test_ciphertext(42),
            Some(&tx),
            grove_version,
        )
        .unwrap()
        .expect("insert in tx");

    assert_eq!(pos, 0);

    // Not visible outside tx — count should still be 0
    let count_outside = db
        .commitment_tree_count(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("count outside tx");
    assert_eq!(count_outside, 0);

    // Visible inside tx
    let count_inside = db
        .commitment_tree_count(EMPTY_PATH, b"pool", Some(&tx), grove_version)
        .unwrap()
        .expect("count inside tx");
    assert_eq!(count_inside, 1);

    // Commit and verify visible
    db.commit_transaction(tx).unwrap().expect("commit");

    let count_after = db
        .commitment_tree_count(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("count after commit");
    assert_eq!(count_after, 1);
}

#[test]
fn test_commitment_tree_insert_transaction_rollback() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
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
        test_rho(1),
        test_ciphertext(1),
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .expect("insert in tx");

    // Rollback by dropping the transaction
    db.rollback_transaction(&tx).expect("rollback");
    drop(tx);

    // Count should still be 0
    let count = db
        .commitment_tree_count(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("count after rollback");
    assert_eq!(count, 0);
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
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
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
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
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
        test_rho(1),
        test_ciphertext(1),
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
            Element::empty_commitment_tree(TEST_CHUNK_POWER),
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
            test_rho(10),
            test_ciphertext(10),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert 1");

        db.commitment_tree_insert(
            EMPTY_PATH,
            b"pool",
            test_cmx(20),
            test_rho(20),
            test_ciphertext(20),
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
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
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
        test_rho(1),
        test_ciphertext(1),
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
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
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
        test_rho(1),
        test_ciphertext(1),
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert into nested pool");

    let root_after = db.root_hash(None, grove_version).unwrap().unwrap();
    assert_ne!(root_before, root_after);
}

// ===========================================================================
// Count tests
// ===========================================================================

#[test]
fn test_commitment_tree_count() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    // Check count is 0 initially
    let count = db
        .commitment_tree_count(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("count");
    assert_eq!(count, 0);

    // Insert 3 items
    for i in 0..3u8 {
        db.commitment_tree_insert(
            EMPTY_PATH,
            b"pool",
            test_cmx(i + 1),
            test_rho(i + 1),
            test_ciphertext(i + 1),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
    }

    let count = db
        .commitment_tree_count(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("count after inserts");
    assert_eq!(count, 3);
}

// ===========================================================================
// Get value tests
// ===========================================================================

#[test]
fn test_commitment_tree_get_value() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    let cmx = test_cmx(42);
    let ct = test_ciphertext(42);
    let payload = serialize_ciphertext(&ct);

    db.commitment_tree_insert(
        EMPTY_PATH,
        b"pool",
        cmx,
        test_rho(42),
        test_ciphertext(42),
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    // Get the value at position 0
    let value = db
        .commitment_tree_get_value(EMPTY_PATH, b"pool", 0, None, grove_version)
        .unwrap()
        .expect("get value");
    let value = value.expect("value should exist");
    assert_eq!(&value[..32], &cmx);
    assert_eq!(&value[32..64], &test_rho(42));
    assert_eq!(&value[64..], payload.as_slice());

    // Position 1 should not exist
    let none_value = db
        .commitment_tree_get_value(EMPTY_PATH, b"pool", 1, None, grove_version)
        .unwrap()
        .expect("get value out of range");
    assert!(none_value.is_none());
}

// ===========================================================================
// Compaction test (small chunk_power)
// ===========================================================================

#[test]
fn test_commitment_tree_compaction() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Use chunk_power=2 (chunk_size=4) to trigger compaction after 4 items
    db.insert(
        EMPTY_PATH,
        b"pool",
        Element::empty_commitment_tree(2),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    // Insert 6 items (triggers 1 compaction at item 4, 2 remain in buffer)
    for i in 0..6u8 {
        db.commitment_tree_insert(
            EMPTY_PATH,
            b"pool",
            test_cmx(i + 1),
            test_rho(i + 1),
            test_ciphertext(i + 1),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");
    }

    let count = db
        .commitment_tree_count(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("count");
    assert_eq!(count, 6);

    // All items should be retrievable (from chunk blob or buffer)
    for i in 0..6u64 {
        let value = db
            .commitment_tree_get_value(EMPTY_PATH, b"pool", i, None, grove_version)
            .unwrap()
            .expect("get value");
        assert!(
            value.is_some(),
            "value at position {} should exist after compaction",
            i
        );
    }

    // Sinsemilla root should still be correct
    let leaves: Vec<[u8; 32]> = (0..6u8).map(|i| test_cmx(i + 1)).collect();
    let exp = expected_root(&leaves);
    let anchor = db
        .commitment_tree_anchor(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("anchor");
    assert_eq!(anchor, Anchor::from_bytes(exp).unwrap());
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
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    let ops = vec![
        QualifiedGroveDbOp::commitment_tree_insert_op_typed(
            vec![b"pool".to_vec()],
            test_cmx(1),
            test_rho(1),
            &test_ciphertext(1),
        ),
        QualifiedGroveDbOp::commitment_tree_insert_op_typed(
            vec![b"pool".to_vec()],
            test_cmx(2),
            test_rho(2),
            &test_ciphertext(2),
        ),
        QualifiedGroveDbOp::commitment_tree_insert_op_typed(
            vec![b"pool".to_vec()],
            test_cmx(3),
            test_rho(3),
            &test_ciphertext(3),
        ),
    ];

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch apply");

    // Verify items were stored
    for i in 0u64..3 {
        let value = db
            .commitment_tree_get_value(EMPTY_PATH, b"pool", i, None, grove_version)
            .unwrap()
            .expect("get value");
        assert!(value.is_some(), "value at position {} should exist", i);
    }

    // Verify count
    let count = db
        .commitment_tree_count(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("count");
    assert_eq!(count, 3);

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
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    let tx = db.start_transaction();

    let ops = vec![QualifiedGroveDbOp::commitment_tree_insert_op_typed(
        vec![b"pool".to_vec()],
        test_cmx(1),
        test_rho(1),
        &test_ciphertext(1),
    )];

    db.apply_batch(ops, None, Some(&tx), grove_version)
        .unwrap()
        .expect("batch in tx");

    // Not visible outside transaction
    let count_outside = db
        .commitment_tree_count(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("count outside tx");
    assert_eq!(count_outside, 0);

    // Commit and verify
    db.commit_transaction(tx).unwrap().expect("commit");

    let count_after = db
        .commitment_tree_count(EMPTY_PATH, b"pool", None, grove_version)
        .unwrap()
        .expect("count after commit");
    assert_eq!(count_after, 1);
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
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
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
            test_rho(i),
            test_ciphertext(i),
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
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    let ops = (1..=3u8)
        .map(|i| {
            QualifiedGroveDbOp::commitment_tree_insert_op_typed(
                vec![b"pool".to_vec()],
                test_cmx(i),
                test_rho(i),
                &test_ciphertext(i),
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
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
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
        test_rho(1),
        test_ciphertext(1),
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
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
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
        test_rho(1),
        test_ciphertext(1),
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
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
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
            test_rho(99),
            test_ciphertext(99),
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
            test_rho(1),
            test_ciphertext(1),
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
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert pool_a");

    db.insert(
        EMPTY_PATH,
        b"pool_b",
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
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
        test_rho(1),
        test_ciphertext(1),
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert into pool_a");

    db.commitment_tree_insert(
        EMPTY_PATH,
        b"pool_b",
        test_cmx(2),
        test_rho(2),
        test_ciphertext(2),
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
    let count_a = db
        .commitment_tree_count(EMPTY_PATH, b"pool_a", None, grove_version)
        .unwrap()
        .expect("count_a");
    let count_b = db
        .commitment_tree_count(EMPTY_PATH, b"pool_b", None, grove_version)
        .unwrap()
        .expect("count_b");
    assert_eq!(count_a, 1);
    assert_eq!(count_b, 1);
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
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert ct");

    db.commitment_tree_insert(
        EMPTY_PATH,
        b"ct",
        test_cmx(1),
        test_rho(1),
        test_ciphertext(1),
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert 1");

    db.commitment_tree_insert(
        EMPTY_PATH,
        b"ct",
        test_cmx(2),
        test_rho(2),
        test_ciphertext(2),
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert 2");

    // verify_grovedb should report no issues
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify");
    assert!(issues.is_empty(), "expected no issues, got: {:?}", issues);
}

// ===========================================================================
// Additional delete tests
// ===========================================================================

#[test]
fn test_commitment_tree_delete_empty() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"ct",
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    // Delete with default options (empty tree, should succeed since no notes
    // appended)
    db.delete(EMPTY_PATH, b"ct", None, None, grove_version)
        .unwrap()
        .expect("should delete empty commitment tree");

    // Verify tree is gone
    let result = db.get(EMPTY_PATH, b"ct", None, grove_version).unwrap();
    assert!(result.is_err(), "commitment tree should no longer exist");
}

#[test]
fn test_commitment_tree_delete_non_empty_error() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"ct",
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    // Append notes to make it non-empty
    for i in 0..3u8 {
        db.commitment_tree_insert(
            EMPTY_PATH,
            b"ct",
            test_cmx(i + 1),
            test_rho(i + 1),
            test_ciphertext(i + 1),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert note");
    }

    // Delete without allowing non-empty trees (default options)
    let result = db
        .delete(EMPTY_PATH, b"ct", None, None, grove_version)
        .unwrap();
    assert!(
        matches!(result, Err(Error::DeletingNonEmptyTree(_))),
        "should return DeletingNonEmptyTree error, got: {:?}",
        result
    );
}

#[test]
fn test_verify_grovedb_after_commitment_tree_delete() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert a normal tree and a commitment tree as siblings
    db.insert(
        EMPTY_PATH,
        b"sibling",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert sibling tree");

    db.insert(
        EMPTY_PATH,
        b"ct",
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    // Append notes
    for i in 0..3u8 {
        db.commitment_tree_insert(
            EMPTY_PATH,
            b"ct",
            test_cmx(i + 1),
            test_rho(i + 1),
            test_ciphertext(i + 1),
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert note");
    }

    // Also insert an item into the sibling tree so the DB is not trivially empty
    db.insert(
        [b"sibling"].as_ref(),
        b"item",
        Element::new_item(b"hello".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert item into sibling");

    // Delete the commitment tree (allow non-empty)
    db.delete(
        EMPTY_PATH,
        b"ct",
        Some(DeleteOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: true,
            ..Default::default()
        }),
        None,
        grove_version,
    )
    .unwrap()
    .expect("should delete non-empty commitment tree");

    // verify_grovedb on the remaining database should be clean
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify should not fail");
    assert!(
        issues.is_empty(),
        "expected no issues after delete, got: {:?}",
        issues
    );
}

// ===========================================================================
// V1 proof tests
// ===========================================================================

#[test]
fn test_commitment_tree_prove_query_v1_empty() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert a parent tree and an empty CommitmentTree
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
        b"pool",
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    // Query all positions [0..1) — should return nothing (tree is empty)
    let mut inner_query = Query::new();
    inner_query.insert_range_inclusive(0u64.to_be_bytes().to_vec()..=0u64.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![b"root".to_vec()],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"pool".to_vec())],
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
        .expect("generate V1 proof for empty commitment tree");

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
    .expect("verify V1 proof for empty commitment tree");

    let expected_root = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(root_hash, expected_root, "root hash should match");
    assert_eq!(result_set.len(), 0, "empty tree should return no results");
}

#[test]
fn test_commitment_tree_prove_query_v1_buffer_only() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert a parent tree and CommitmentTree beneath it
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
        b"pool",
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    // Append 5 notes (all in buffer — well below chunk_size=2^TEST_CHUNK_POWER)
    let mut expected_values = Vec::new();
    for i in 0..5u8 {
        let cmx = test_cmx(i);
        let rho = test_rho(i);
        let ct = test_ciphertext(i);
        let payload = serialize_ciphertext(&ct);
        db.commitment_tree_insert(
            &[b"root"],
            b"pool",
            cmx,
            rho,
            test_ciphertext(i),
            None,
            grove_version,
        )
        .unwrap()
        .expect("commitment tree insert");

        let mut expected = Vec::with_capacity(32 + 32 + payload.len());
        expected.extend_from_slice(&cmx);
        expected.extend_from_slice(&rho);
        expected.extend_from_slice(&payload);
        expected_values.push(expected);
    }

    // Build PathQuery: path=[b"root"], key=b"pool", subquery = range [0..5)
    let mut inner_query = Query::new();
    inner_query.insert_range_inclusive(0u64.to_be_bytes().to_vec()..=4u64.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![b"root".to_vec()],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"pool".to_vec())],
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
        .expect("generate V1 proof for commitment tree");

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
    .expect("verify V1 proof for commitment tree");

    // Check root hash matches
    let expected_root = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(root_hash, expected_root, "root hash should match");

    // Check result set
    assert_eq!(result_set.len(), 5, "should have 5 results");
    for i in 0..5u64 {
        let (_, key, element) = &result_set[i as usize];
        assert_eq!(
            key,
            &i.to_be_bytes().to_vec(),
            "key should be position {}",
            i
        );
        match element.as_ref().expect("element should be Some") {
            Element::Item(data, _) => {
                assert_eq!(
                    data, &expected_values[i as usize],
                    "value at position {} should match (cmx || payload)",
                    i
                );
            }
            other => panic!("expected Item, got {:?}", other),
        }
    }
}

#[test]
fn test_commitment_tree_prove_query_v1_with_chunks() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    let chunk_power: u8 = 2;

    // Insert a parent tree and CommitmentTree with small chunk_power
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
        b"pool",
        Element::empty_commitment_tree(chunk_power),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    // Append 6 notes: 1 full chunk (4) + 2 buffer entries
    let mut expected_values = Vec::new();
    for i in 0..6u8 {
        let cmx = test_cmx(i);
        let rho = test_rho(i);
        let ct = test_ciphertext(i);
        let payload = serialize_ciphertext(&ct);
        db.commitment_tree_insert(
            &[b"root"],
            b"pool",
            cmx,
            rho,
            test_ciphertext(i),
            None,
            grove_version,
        )
        .unwrap()
        .expect("commitment tree insert");

        let mut expected = Vec::with_capacity(32 + 32 + payload.len());
        expected.extend_from_slice(&cmx);
        expected.extend_from_slice(&rho);
        expected.extend_from_slice(&payload);
        expected_values.push(expected);
    }

    // Query all 6 positions [0..6)
    let mut inner_query = Query::new();
    inner_query.insert_range_inclusive(0u64.to_be_bytes().to_vec()..=5u64.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![b"root".to_vec()],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"pool".to_vec())],
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
        .expect("generate V1 proof with chunks");

    // Decode and display the proof structure
    let config = bincode::config::standard()
        .with_big_endian()
        .with_no_limit();
    let (_proof, _): (crate::operations::proof::GroveDBProof, _) =
        bincode::decode_from_slice(&proof_bytes, config).expect("decode proof for display");

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
    .expect("verify V1 proof with chunks");

    let expected_root = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(root_hash, expected_root, "root hash should match");
    assert_eq!(result_set.len(), 6, "should have 6 results");

    for i in 0..6u64 {
        let (_, key, element) = &result_set[i as usize];
        assert_eq!(key, &i.to_be_bytes().to_vec());
        match element.as_ref().expect("element should be Some") {
            Element::Item(data, _) => {
                assert_eq!(data, &expected_values[i as usize]);
            }
            other => panic!("expected Item at position {}, got {:?}", i, other),
        }
    }
}

#[test]
fn test_commitment_tree_prove_query_v1_partial_range() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

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
        b"pool",
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert commitment tree");

    // Append 10 notes
    let mut expected_values = Vec::new();
    for i in 0..10u8 {
        let cmx = test_cmx(i);
        let rho = test_rho(i);
        let ct = test_ciphertext(i);
        let payload = serialize_ciphertext(&ct);
        db.commitment_tree_insert(
            &[b"root"],
            b"pool",
            cmx,
            rho,
            test_ciphertext(i),
            None,
            grove_version,
        )
        .unwrap()
        .expect("commitment tree insert");

        let mut expected = Vec::with_capacity(32 + 32 + payload.len());
        expected.extend_from_slice(&cmx);
        expected.extend_from_slice(&rho);
        expected.extend_from_slice(&payload);
        expected_values.push(expected);
    }

    // Query only positions [3..7)
    let mut inner_query = Query::new();
    inner_query.insert_range_inclusive(3u64.to_be_bytes().to_vec()..=6u64.to_be_bytes().to_vec());

    let path_query = PathQuery {
        path: vec![b"root".to_vec()],
        query: SizedQuery {
            query: Query {
                items: vec![QueryItem::Key(b"pool".to_vec())],
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
        .expect("generate V1 proof for partial range");

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
    .expect("verify V1 proof for partial range");

    let expected_root = db.grove_db.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(root_hash, expected_root, "root hash should match");
    assert_eq!(result_set.len(), 4, "should have 4 results (positions 3-6)");

    for (i, pos) in (3u64..=6u64).enumerate() {
        let (_, key, element) = &result_set[i];
        assert_eq!(key, &pos.to_be_bytes().to_vec());
        match element.as_ref().expect("element should be Some") {
            Element::Item(data, _) => {
                assert_eq!(data, &expected_values[pos as usize]);
            }
            other => panic!("expected Item at position {}, got {:?}", pos, other),
        }
    }
}

// ===========================================================================
// Error-path tests: wrong element type
// ===========================================================================

#[test]
fn test_commitment_tree_get_value_on_wrong_element_type() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert a normal tree (not a CommitmentTree)
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
        .commitment_tree_get_value(EMPTY_PATH, b"normal", 0, None, grove_version)
        .unwrap();
    assert!(
        result.is_err(),
        "commitment_tree_get_value on a normal tree should fail"
    );
    match result {
        Err(Error::InvalidInput(msg)) => {
            assert_eq!(msg, "element is not a commitment tree");
        }
        other => panic!(
            "expected InvalidInput('element is not a commitment tree'), got: {:?}",
            other
        ),
    }
}

#[test]
fn test_commitment_tree_count_on_wrong_element_type() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert a normal tree (not a CommitmentTree)
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
        .commitment_tree_count(EMPTY_PATH, b"normal", None, grove_version)
        .unwrap();
    assert!(
        result.is_err(),
        "commitment_tree_count on a normal tree should fail"
    );
    match result {
        Err(Error::InvalidInput(msg)) => {
            assert_eq!(msg, "element is not a commitment tree");
        }
        other => panic!(
            "expected InvalidInput('element is not a commitment tree'), got: {:?}",
            other
        ),
    }
}

// ===========================================================================
// Persistence-across-reopen tests
// ===========================================================================

#[test]
fn test_commitment_tree_persistence_across_reopen() {
    let grove_version = GroveVersion::latest();
    let tmp_dir = tempfile::TempDir::new().expect("should create temp dir");

    let anchor_before_close;

    // Open, insert CommitmentTree, append 3 notes
    {
        let db = crate::GroveDb::open(tmp_dir.path()).expect("should open grovedb");
        db.insert(
            EMPTY_PATH,
            b"pool",
            Element::empty_commitment_tree(TEST_CHUNK_POWER),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert commitment tree");

        for i in 1..=3u8 {
            db.commitment_tree_insert(
                EMPTY_PATH,
                b"pool",
                test_cmx(i),
                test_rho(i),
                test_ciphertext(i),
                None,
                grove_version,
            )
            .unwrap()
            .expect("insert note");
        }

        anchor_before_close = db
            .commitment_tree_anchor(EMPTY_PATH, b"pool", None, grove_version)
            .unwrap()
            .expect("anchor before close");
    }
    // db is dropped here

    // Reopen and verify state
    {
        let db = crate::GroveDb::open(tmp_dir.path()).expect("should reopen grovedb");

        // Verify count == 3
        let count = db
            .commitment_tree_count(EMPTY_PATH, b"pool", None, grove_version)
            .unwrap()
            .expect("count after reopen");
        assert_eq!(count, 3, "count should be 3 after reopen");

        // Verify anchor (root hash) is unchanged
        let anchor_after_reopen = db
            .commitment_tree_anchor(EMPTY_PATH, b"pool", None, grove_version)
            .unwrap()
            .expect("anchor after reopen");
        assert_eq!(
            anchor_before_close, anchor_after_reopen,
            "anchor (sinsemilla root) should be stable across reopen"
        );

        // Verify the anchor matches expected computation
        let leaves: Vec<[u8; 32]> = (1..=3u8).map(test_cmx).collect();
        let exp = expected_root(&leaves);
        assert_eq!(
            anchor_after_reopen,
            Anchor::from_bytes(exp).expect("valid anchor"),
            "anchor should match expected sinsemilla root"
        );
    }
}

// ===========================================================================
// verify_grovedb empty tree test
// ===========================================================================

/// An empty CommitmentTree (no inserts) currently has a hash mismatch in
/// verify_grovedb: the initial insert stores NULL_HASH ([0; 32]) as the
/// child hash in Merk, but verify_grovedb computes the actual state root
/// (which includes the non-zero sinsemilla empty tree root). This test
/// documents that known behavior. After the first insert, verify_grovedb
/// passes cleanly (see `test_verify_grovedb_commitment_tree_valid`).
#[test]
fn test_verify_grovedb_commitment_tree_empty() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert an empty CommitmentTree (no inserts)
    db.insert(
        EMPTY_PATH,
        b"ct",
        Element::empty_commitment_tree(TEST_CHUNK_POWER),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert empty commitment tree");

    // An empty CommitmentTree currently reports a hash mismatch because the
    // initial child hash is NULL_HASH but the computed state root includes
    // the non-zero sinsemilla empty tree root.
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify should not fail");
    assert!(
        !issues.is_empty(),
        "empty commitment tree should report hash mismatch (NULL_HASH vs sinsemilla empty root)"
    );
    // Exactly one issue for the path [b"ct"]
    assert_eq!(
        issues.len(),
        1,
        "should have exactly one issue for the empty commitment tree path"
    );
}
