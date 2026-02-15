//! BulkAppendTree integration tests
//!
//! Tests for BulkAppendTree as a GroveDB subtree type: a two-level
//! authenticated append-only structure with dense Merkle buffer and epoch MMR.

use grovedb_version::version::GroveVersion;

use crate::{
    batch::QualifiedGroveDbOp,
    tests::{common::EMPTY_PATH, make_empty_grovedb},
    Element,
};

/// Small epoch size for tests — triggers compaction after 4 appends.
const TEST_EPOCH_SIZE: u32 = 4;

// ===========================================================================
// Element tests
// ===========================================================================

#[test]
fn test_insert_bulk_append_tree_at_root() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree at root");

    let element = db
        .get(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("should retrieve bulk append tree");
    assert!(element.is_bulk_append_tree());
}

#[test]
fn test_bulk_append_tree_under_normal_tree() {
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
        b"notes",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree under parent");

    let element = db
        .get([b"parent"].as_ref(), b"notes", None, grove_version)
        .unwrap()
        .expect("should get notes");
    assert!(element.is_bulk_append_tree());
}

#[test]
fn test_bulk_append_tree_with_flags() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    let flags = Some(vec![1, 2, 3]);
    db.insert(
        EMPTY_PATH,
        b"flagged",
        Element::empty_bulk_append_tree_with_flags(TEST_EPOCH_SIZE, flags.clone()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert with flags");

    let element = db
        .get(EMPTY_PATH, b"flagged", None, grove_version)
        .unwrap()
        .expect("should get flagged");
    assert!(element.is_bulk_append_tree());
    assert_eq!(element.get_flags().as_ref(), flags.as_ref());
}

#[test]
fn test_bulk_append_tree_is_any_tree() {
    let element = Element::empty_bulk_append_tree(TEST_EPOCH_SIZE);
    assert!(element.is_any_tree());
    assert!(element.is_bulk_append_tree());
    assert!(!element.is_any_item());
}

#[test]
fn test_bulk_append_tree_serialization_roundtrip() {
    let grove_version = GroveVersion::latest();
    let original = Element::new_bulk_append_tree([42u8; 32], 100, 8, Some(vec![7, 8, 9]));
    let bytes = original.serialize(grove_version).expect("serialize");
    let deserialized = Element::deserialize(&bytes, grove_version).expect("deserialize");
    assert_eq!(original, deserialized);
}

// ===========================================================================
// Buffer phase tests (no compaction)
// ===========================================================================

#[test]
fn test_bulk_append_single_value() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let (state_root, position) = db
        .bulk_append(EMPTY_PATH, b"bulk", b"hello".to_vec(), None, grove_version)
        .unwrap()
        .expect("append");

    assert_eq!(position, 0);
    assert_ne!(state_root, [0u8; 32]);
}

#[test]
fn test_bulk_append_multiple_in_buffer() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    // Append 3 values (less than epoch_size=4, no compaction)
    for i in 0u8..3 {
        let (_sr, pos) = db
            .bulk_append(EMPTY_PATH, b"bulk", vec![i], None, grove_version)
            .unwrap()
            .expect("append");
        assert_eq!(pos, i as u64);
    }

    // Count should be 3
    let count = db
        .bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("count");
    assert_eq!(count, 3);

    // No completed epochs
    let epoch_count = db
        .bulk_epoch_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("epoch count");
    assert_eq!(epoch_count, 0);
}

#[test]
fn test_bulk_get_value_from_buffer() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let values: Vec<Vec<u8>> = (0u8..3).map(|i| vec![10 + i]).collect();
    for v in &values {
        db.bulk_append(EMPTY_PATH, b"bulk", v.clone(), None, grove_version)
            .unwrap()
            .expect("append");
    }

    // Retrieve each value
    for (i, expected) in values.iter().enumerate() {
        let got = db
            .bulk_get_value(EMPTY_PATH, b"bulk", i as u64, None, grove_version)
            .unwrap()
            .expect("get value");
        assert_eq!(got.as_ref(), Some(expected));
    }

    // Out of range returns None
    let out_of_range = db
        .bulk_get_value(EMPTY_PATH, b"bulk", 10, None, grove_version)
        .unwrap()
        .expect("out of range");
    assert_eq!(out_of_range, None);
}

#[test]
fn test_bulk_get_buffer() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let values: Vec<Vec<u8>> = vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()];
    for v in &values {
        db.bulk_append(EMPTY_PATH, b"bulk", v.clone(), None, grove_version)
            .unwrap()
            .expect("append");
    }

    let buffer = db
        .bulk_get_buffer(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("get buffer");
    assert_eq!(buffer, values);
}

#[test]
fn test_bulk_state_root_changes_each_append() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let (sr1, _) = db
        .bulk_append(EMPTY_PATH, b"bulk", b"first".to_vec(), None, grove_version)
        .unwrap()
        .expect("append 1");

    let (sr2, _) = db
        .bulk_append(EMPTY_PATH, b"bulk", b"second".to_vec(), None, grove_version)
        .unwrap()
        .expect("append 2");

    assert_ne!(sr1, sr2, "state root should change on each append");
}

// ===========================================================================
// Compaction tests
// ===========================================================================

#[test]
fn test_bulk_compaction_at_epoch_boundary() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    // Append exactly epoch_size (4) values → should trigger compaction
    for i in 0u8..TEST_EPOCH_SIZE as u8 {
        db.bulk_append(EMPTY_PATH, b"bulk", vec![i], None, grove_version)
            .unwrap()
            .expect("append");
    }

    // 1 completed epoch
    let epoch_count = db
        .bulk_epoch_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("epoch count");
    assert_eq!(epoch_count, 1);

    // Total count = 4
    let count = db
        .bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("count");
    assert_eq!(count, TEST_EPOCH_SIZE as u64);

    // Buffer should be empty after compaction
    let buffer = db
        .bulk_get_buffer(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("buffer");
    assert!(buffer.is_empty());
}

#[test]
fn test_bulk_epoch_blob_retrievable() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let values: Vec<Vec<u8>> = (0u8..TEST_EPOCH_SIZE as u8).map(|i| vec![i]).collect();
    for v in &values {
        db.bulk_append(EMPTY_PATH, b"bulk", v.clone(), None, grove_version)
            .unwrap()
            .expect("append");
    }

    // Epoch 0 should be retrievable
    let blob = db
        .bulk_get_epoch(EMPTY_PATH, b"bulk", 0, None, grove_version)
        .unwrap()
        .expect("get epoch");
    assert!(blob.is_some(), "epoch 0 should exist");

    // Epoch 1 doesn't exist yet
    let none_blob = db
        .bulk_get_epoch(EMPTY_PATH, b"bulk", 1, None, grove_version)
        .unwrap()
        .expect("get nonexistent epoch");
    assert!(none_blob.is_none());
}

#[test]
fn test_bulk_values_accessible_after_compaction() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let values: Vec<Vec<u8>> = (0u8..TEST_EPOCH_SIZE as u8)
        .map(|i| vec![100 + i])
        .collect();
    for v in &values {
        db.bulk_append(EMPTY_PATH, b"bulk", v.clone(), None, grove_version)
            .unwrap()
            .expect("append");
    }

    // All values should still be accessible via bulk_get_value (reads from epoch
    // blob)
    for (i, expected) in values.iter().enumerate() {
        let got = db
            .bulk_get_value(EMPTY_PATH, b"bulk", i as u64, None, grove_version)
            .unwrap()
            .expect("get value after compaction");
        assert_eq!(got.as_ref(), Some(expected));
    }
}

#[test]
fn test_bulk_state_root_deterministic() {
    let grove_version = GroveVersion::latest();

    // Create two independent DBs with the same data
    let db1 = make_empty_grovedb();
    let db2 = make_empty_grovedb();

    for db in [&db1, &db2] {
        db.insert(
            EMPTY_PATH,
            b"bulk",
            Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");

        for i in 0u8..TEST_EPOCH_SIZE as u8 {
            db.bulk_append(EMPTY_PATH, b"bulk", vec![i], None, grove_version)
                .unwrap()
                .expect("append");
        }
    }

    // State roots should match
    let elem1 = db1
        .get(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("get1");
    let elem2 = db2
        .get(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("get2");

    match (&elem1, &elem2) {
        (
            Element::BulkAppendTree(_, sr1, tc1, es1, _),
            Element::BulkAppendTree(_, sr2, tc2, es2, _),
        ) => {
            assert_eq!(sr1, sr2, "state roots should be deterministic");
            assert_eq!(tc1, tc2);
            assert_eq!(es1, es2);
        }
        _ => panic!("expected BulkAppendTree elements"),
    }
}

// ===========================================================================
// Multi-epoch tests
// ===========================================================================

#[test]
fn test_bulk_multi_epoch() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    // Append 2 * epoch_size + 2 = 10 values → 2 completed epochs + 2 in buffer
    let total = 2 * TEST_EPOCH_SIZE as u8 + 2;
    for i in 0..total {
        db.bulk_append(EMPTY_PATH, b"bulk", vec![i], None, grove_version)
            .unwrap()
            .expect("append");
    }

    assert_eq!(
        db.bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
            .unwrap()
            .expect("count"),
        total as u64
    );

    assert_eq!(
        db.bulk_epoch_count(EMPTY_PATH, b"bulk", None, grove_version)
            .unwrap()
            .expect("epoch count"),
        2
    );

    // Buffer has 2 entries
    let buffer = db
        .bulk_get_buffer(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("buffer");
    assert_eq!(buffer.len(), 2);

    // Access values from epoch 0, epoch 1, and buffer
    for i in 0..total {
        let got = db
            .bulk_get_value(EMPTY_PATH, b"bulk", i as u64, None, grove_version)
            .unwrap()
            .expect("get value");
        assert_eq!(got, Some(vec![i]));
    }
}

#[test]
fn test_bulk_three_full_epochs() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let total = 3 * TEST_EPOCH_SIZE;
    for i in 0..total {
        db.bulk_append(EMPTY_PATH, b"bulk", vec![i as u8], None, grove_version)
            .unwrap()
            .expect("append");
    }

    assert_eq!(
        db.bulk_epoch_count(EMPTY_PATH, b"bulk", None, grove_version)
            .unwrap()
            .expect("epoch count"),
        3
    );

    // Buffer empty (exactly 3 full epochs)
    let buffer = db
        .bulk_get_buffer(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("buffer");
    assert!(buffer.is_empty());

    // All 3 epoch blobs should exist
    for epoch in 0..3u64 {
        let blob = db
            .bulk_get_epoch(EMPTY_PATH, b"bulk", epoch, None, grove_version)
            .unwrap()
            .expect("get epoch");
        assert!(blob.is_some());
    }
}

// ===========================================================================
// Batch tests
// ===========================================================================

#[test]
fn test_bulk_batch_single_append() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let ops = vec![QualifiedGroveDbOp::bulk_append_op(
        vec![],
        b"bulk".to_vec(),
        b"batch_value".to_vec(),
    )];

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("apply batch");

    let count = db
        .bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("count");
    assert_eq!(count, 1);

    let val = db
        .bulk_get_value(EMPTY_PATH, b"bulk", 0, None, grove_version)
        .unwrap()
        .expect("get value");
    assert_eq!(val, Some(b"batch_value".to_vec()));
}

#[test]
fn test_bulk_batch_multiple_appends() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let ops: Vec<QualifiedGroveDbOp> = (0u8..3)
        .map(|i| QualifiedGroveDbOp::bulk_append_op(vec![], b"bulk".to_vec(), vec![i]))
        .collect();

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("apply batch");

    let count = db
        .bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("count");
    assert_eq!(count, 3);

    for i in 0u8..3 {
        let val = db
            .bulk_get_value(EMPTY_PATH, b"bulk", i as u64, None, grove_version)
            .unwrap()
            .expect("get value");
        assert_eq!(val, Some(vec![i]));
    }
}

#[test]
fn test_bulk_batch_spanning_compaction() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    // Batch with 6 appends — should trigger 1 compaction (epoch_size=4)
    // and leave 2 in buffer
    let ops: Vec<QualifiedGroveDbOp> = (0u8..6)
        .map(|i| QualifiedGroveDbOp::bulk_append_op(vec![], b"bulk".to_vec(), vec![i]))
        .collect();

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("apply batch");

    assert_eq!(
        db.bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
            .unwrap()
            .expect("count"),
        6
    );

    assert_eq!(
        db.bulk_epoch_count(EMPTY_PATH, b"bulk", None, grove_version)
            .unwrap()
            .expect("epoch count"),
        1
    );

    let buffer = db
        .bulk_get_buffer(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("buffer");
    assert_eq!(buffer.len(), 2);

    // All 6 values accessible
    for i in 0u8..6 {
        let val = db
            .bulk_get_value(EMPTY_PATH, b"bulk", i as u64, None, grove_version)
            .unwrap()
            .expect("get value");
        assert_eq!(val, Some(vec![i]));
    }
}

#[test]
fn test_bulk_batch_matches_direct_ops() {
    let grove_version = GroveVersion::latest();

    // Direct ops
    let db1 = make_empty_grovedb();
    db1.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    for i in 0u8..6 {
        db1.bulk_append(EMPTY_PATH, b"bulk", vec![i], None, grove_version)
            .unwrap()
            .expect("append");
    }

    // Batch ops
    let db2 = make_empty_grovedb();
    db2.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let ops: Vec<QualifiedGroveDbOp> = (0u8..6)
        .map(|i| QualifiedGroveDbOp::bulk_append_op(vec![], b"bulk".to_vec(), vec![i]))
        .collect();

    db2.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("apply batch");

    // State roots should match
    let elem1 = db1
        .get(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("get1");
    let elem2 = db2
        .get(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("get2");

    match (&elem1, &elem2) {
        (Element::BulkAppendTree(_, sr1, tc1, ..), Element::BulkAppendTree(_, sr2, tc2, ..)) => {
            assert_eq!(
                sr1, sr2,
                "state roots should match between direct and batch"
            );
            assert_eq!(tc1, tc2);
        }
        _ => panic!("expected BulkAppendTree elements"),
    }
}

#[test]
fn test_bulk_batch_with_mixed_ops() {
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
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk");

    // Mix bulk appends with a normal insert
    let ops = vec![
        QualifiedGroveDbOp::insert_or_replace_op(
            vec![b"parent".to_vec()],
            b"item".to_vec(),
            Element::new_item(b"hello".to_vec()),
        ),
        QualifiedGroveDbOp::bulk_append_op(
            vec![b"parent".to_vec()],
            b"bulk".to_vec(),
            b"note1".to_vec(),
        ),
        QualifiedGroveDbOp::bulk_append_op(
            vec![b"parent".to_vec()],
            b"bulk".to_vec(),
            b"note2".to_vec(),
        ),
    ];

    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("apply mixed batch");

    // Verify both operations applied
    let item = db
        .get([b"parent"].as_ref(), b"item", None, grove_version)
        .unwrap()
        .expect("get item");
    assert_eq!(item, Element::new_item(b"hello".to_vec()));

    let count = db
        .bulk_count([b"parent"].as_ref(), b"bulk", None, grove_version)
        .unwrap()
        .expect("count");
    assert_eq!(count, 2);
}

// ===========================================================================
// Lifecycle tests
// ===========================================================================

#[test]
fn test_bulk_root_hash_propagation() {
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
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk");

    let hash_before = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root hash");

    db.bulk_append(
        [b"parent"].as_ref(),
        b"bulk",
        b"data".to_vec(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("append");

    let hash_after = db
        .root_hash(None, grove_version)
        .unwrap()
        .expect("root hash");
    assert_ne!(
        hash_before, hash_after,
        "root hash should change after append"
    );
}

#[test]
fn test_bulk_transaction_commit() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let tx = db.start_transaction();

    db.bulk_append(
        EMPTY_PATH,
        b"bulk",
        b"in_tx".to_vec(),
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .expect("append in tx");

    // Before commit, non-tx view should see 0
    let count_outside = db
        .bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("count outside tx");
    assert_eq!(count_outside, 0);

    db.commit_transaction(tx).unwrap().expect("commit");

    // After commit, should see 1
    let count_after = db
        .bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("count after commit");
    assert_eq!(count_after, 1);
}

#[test]
fn test_bulk_transaction_rollback() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let tx = db.start_transaction();

    db.bulk_append(
        EMPTY_PATH,
        b"bulk",
        b"should_rollback".to_vec(),
        Some(&tx),
        grove_version,
    )
    .unwrap()
    .expect("append in tx");

    db.rollback_transaction(&tx).expect("rollback");

    let count = db
        .bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("count");
    assert_eq!(count, 0);
}

// ===========================================================================
// Error tests
// ===========================================================================

#[test]
fn test_bulk_append_to_wrong_element_type() {
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
        .bulk_append(EMPTY_PATH, b"tree", b"data".to_vec(), None, grove_version)
        .unwrap();

    assert!(result.is_err(), "should error on wrong element type");
}

#[test]
fn test_bulk_get_value_out_of_range() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_EPOCH_SIZE),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    db.bulk_append(EMPTY_PATH, b"bulk", b"one".to_vec(), None, grove_version)
        .unwrap()
        .expect("append");

    let result = db
        .bulk_get_value(EMPTY_PATH, b"bulk", 100, None, grove_version)
        .unwrap()
        .expect("out of range");
    assert_eq!(result, None);
}

#[test]
#[should_panic(expected = "power of 2")]
fn test_bulk_invalid_epoch_size() {
    let _element = Element::empty_bulk_append_tree(3); // Not a power of 2
}
