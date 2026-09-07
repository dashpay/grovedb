//! BulkAppendTree integration tests
//!
//! Tests for BulkAppendTree as a GroveDB subtree type: a two-level
//! authenticated append-only structure with dense Merkle buffer and chunk MMR.

use grovedb_version::version::GroveVersion;

use crate::{
    batch::QualifiedGroveDbOp,
    operations::delete::DeleteOptions,
    tests::{common::EMPTY_PATH, make_empty_grovedb},
    Element, Error,
};

/// Small chunk power for tests — chunk size = 2^2 = 4, triggers compaction
/// after 4 appends.
const TEST_CHUNK_POWER: u8 = 2;
const TEST_CHUNK_SIZE: u32 = 1 << (TEST_CHUNK_POWER as u32);

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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
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
        Element::empty_bulk_append_tree_with_flags(TEST_CHUNK_POWER, flags.clone())
            .expect("valid chunk_power"),
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
    let element = Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power");
    assert!(element.is_any_tree());
    assert!(element.is_bulk_append_tree());
    assert!(!element.is_any_item());
}

#[test]
fn test_bulk_append_tree_serialization_roundtrip() {
    let grove_version = GroveVersion::latest();
    let original = Element::new_bulk_append_tree(100, 3, Some(vec![7, 8, 9]));
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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    // Append 3 values (less than chunk_size=4, no compaction)
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

    // No completed chunks
    let chunk_count = db
        .bulk_chunk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("chunk count");
    assert_eq!(chunk_count, 0);
}

#[test]
fn test_bulk_get_value_from_buffer() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
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
fn test_bulk_compaction_at_chunk_boundary() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    // Append exactly chunk_size (4) values → should trigger compaction
    for i in 0u8..TEST_CHUNK_SIZE as u8 {
        db.bulk_append(EMPTY_PATH, b"bulk", vec![i], None, grove_version)
            .unwrap()
            .expect("append");
    }

    // 1 completed chunk
    let chunk_count = db
        .bulk_chunk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("chunk count");
    assert_eq!(chunk_count, 1);

    // Total count = 4
    let count = db
        .bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("count");
    assert_eq!(count, TEST_CHUNK_SIZE as u64);

    // Buffer should be empty after compaction
    let buffer = db
        .bulk_get_buffer(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("buffer");
    assert!(buffer.is_empty());
}

#[test]
fn test_bulk_chunk_blob_retrievable() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let values: Vec<Vec<u8>> = (0u8..TEST_CHUNK_SIZE as u8).map(|i| vec![i]).collect();
    for v in &values {
        db.bulk_append(EMPTY_PATH, b"bulk", v.clone(), None, grove_version)
            .unwrap()
            .expect("append");
    }

    // Chunk 0 should be retrievable
    let blob = db
        .bulk_get_chunk(EMPTY_PATH, b"bulk", 0, None, grove_version)
        .unwrap()
        .expect("get chunk");
    assert!(blob.is_some(), "chunk 0 should exist");

    // Chunk 1 doesn't exist yet
    let none_blob = db
        .bulk_get_chunk(EMPTY_PATH, b"bulk", 1, None, grove_version)
        .unwrap()
        .expect("get nonexistent chunk");
    assert!(none_blob.is_none());
}

#[test]
fn test_bulk_values_accessible_after_compaction() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let values: Vec<Vec<u8>> = (0u8..TEST_CHUNK_SIZE as u8)
        .map(|i| vec![100 + i])
        .collect();
    for v in &values {
        db.bulk_append(EMPTY_PATH, b"bulk", v.clone(), None, grove_version)
            .unwrap()
            .expect("append");
    }

    // All values should still be accessible via bulk_get_value (reads from chunk
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
            Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert");

        for i in 0u8..TEST_CHUNK_SIZE as u8 {
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
        (Element::BulkAppendTree(tc1, cp1, _), Element::BulkAppendTree(tc2, cp2, _)) => {
            assert_eq!(tc1, tc2, "total counts should be deterministic");
            assert_eq!(cp1, cp2);
        }
        _ => panic!("expected BulkAppendTree elements"),
    }
}

// ===========================================================================
// Multi-chunk tests
// ===========================================================================

#[test]
fn test_bulk_multi_chunk() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    // Append 2 * chunk_size + 2 = 10 values → 2 completed chunks + 2 in buffer
    let total = 2 * TEST_CHUNK_SIZE as u8 + 2;
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
        db.bulk_chunk_count(EMPTY_PATH, b"bulk", None, grove_version)
            .unwrap()
            .expect("chunk count"),
        2
    );

    // Buffer has 2 entries
    let buffer = db
        .bulk_get_buffer(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("buffer");
    assert_eq!(buffer.len(), 2);

    // Access values from chunk 0, chunk 1, and buffer
    for i in 0..total {
        let got = db
            .bulk_get_value(EMPTY_PATH, b"bulk", i as u64, None, grove_version)
            .unwrap()
            .expect("get value");
        assert_eq!(got, Some(vec![i]));
    }
}

#[test]
fn test_bulk_three_full_chunks() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let total = 3 * TEST_CHUNK_SIZE;
    for i in 0..total {
        db.bulk_append(EMPTY_PATH, b"bulk", vec![i as u8], None, grove_version)
            .unwrap()
            .expect("append");
    }

    assert_eq!(
        db.bulk_chunk_count(EMPTY_PATH, b"bulk", None, grove_version)
            .unwrap()
            .expect("chunk count"),
        3
    );

    // Buffer empty (exactly 3 full chunks)
    let buffer = db
        .bulk_get_buffer(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("buffer");
    assert!(buffer.is_empty());

    // All 3 chunk blobs should exist
    for chunk in 0..3u64 {
        let blob = db
            .bulk_get_chunk(EMPTY_PATH, b"bulk", chunk, None, grove_version)
            .unwrap()
            .expect("get chunk");
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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let ops = vec![QualifiedGroveDbOp::bulk_append_op(
        vec![b"bulk".to_vec()],
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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let ops: Vec<QualifiedGroveDbOp> = (0u8..3)
        .map(|i| QualifiedGroveDbOp::bulk_append_op(vec![b"bulk".to_vec()], vec![i]))
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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    // Batch with 6 appends — should trigger 1 compaction (chunk_size=4)
    // and leave 2 in buffer
    let ops: Vec<QualifiedGroveDbOp> = (0u8..6)
        .map(|i| QualifiedGroveDbOp::bulk_append_op(vec![b"bulk".to_vec()], vec![i]))
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
        db.bulk_chunk_count(EMPTY_PATH, b"bulk", None, grove_version)
            .unwrap()
            .expect("chunk count"),
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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    let ops: Vec<QualifiedGroveDbOp> = (0u8..6)
        .map(|i| QualifiedGroveDbOp::bulk_append_op(vec![b"bulk".to_vec()], vec![i]))
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
        (Element::BulkAppendTree(tc1, cp1, ..), Element::BulkAppendTree(tc2, cp2, ..)) => {
            assert_eq!(
                tc1, tc2,
                "total counts should match between direct and batch"
            );
            assert_eq!(cp1, cp2);
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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
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
            vec![b"parent".to_vec(), b"bulk".to_vec()],
            b"note1".to_vec(),
        ),
        QualifiedGroveDbOp::bulk_append_op(
            vec![b"parent".to_vec(), b"bulk".to_vec()],
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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
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
fn test_bulk_invalid_chunk_power() {
    assert!(
        Element::empty_bulk_append_tree(32).is_err(),
        "chunk_power > 31 should return error"
    );
}

// ===========================================================================
// Delete tests
// ===========================================================================

#[test]
fn test_bulk_delete_empty() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree");

    // Delete with default options (empty tree, should succeed)
    db.delete(EMPTY_PATH, b"bulk", None, None, grove_version)
        .unwrap()
        .expect("should delete empty bulk append tree");

    // Verify tree is gone
    let result = db.get(EMPTY_PATH, b"bulk", None, grove_version).unwrap();
    assert!(result.is_err(), "bulk append tree should no longer exist");
}

#[test]
fn test_bulk_delete_non_empty() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree");

    // Append 2 values (still in buffer, no compaction)
    for i in 0..2u8 {
        db.bulk_append(EMPTY_PATH, b"bulk", vec![i], None, grove_version)
            .unwrap()
            .expect("append value");
    }

    // Delete with allow_deleting_non_empty_trees
    db.delete(
        EMPTY_PATH,
        b"bulk",
        Some(DeleteOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: true,
            ..Default::default()
        }),
        None,
        grove_version,
    )
    .unwrap()
    .expect("should delete non-empty bulk append tree");

    // Verify tree is gone
    let result = db.get(EMPTY_PATH, b"bulk", None, grove_version).unwrap();
    assert!(result.is_err(), "bulk append tree should no longer exist");
}

#[test]
fn test_bulk_delete_non_empty_with_compaction() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree");

    // Append 6 values (chunk_size=4 triggers 1 compaction, 2 remain in buffer)
    for i in 0..6u8 {
        db.bulk_append(EMPTY_PATH, b"bulk", vec![i], None, grove_version)
            .unwrap()
            .expect("append value");
    }

    // Verify compaction happened
    let chunk_count = db
        .bulk_chunk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("chunk count");
    assert_eq!(chunk_count, 1, "should have 1 completed chunk");

    // Delete with allow_deleting_non_empty_trees
    db.delete(
        EMPTY_PATH,
        b"bulk",
        Some(DeleteOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: true,
            ..Default::default()
        }),
        None,
        grove_version,
    )
    .unwrap()
    .expect("should delete bulk tree with compacted chunks");

    // Verify tree is gone
    let result = db.get(EMPTY_PATH, b"bulk", None, grove_version).unwrap();
    assert!(result.is_err(), "bulk append tree should no longer exist");
}

#[test]
fn test_bulk_delete_non_empty_error() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree");

    // Append values to make it non-empty
    for i in 0..2u8 {
        db.bulk_append(EMPTY_PATH, b"bulk", vec![i], None, grove_version)
            .unwrap()
            .expect("append value");
    }

    // Delete without allowing non-empty trees (default options)
    let result = db
        .delete(EMPTY_PATH, b"bulk", None, None, grove_version)
        .unwrap();
    assert!(
        matches!(result, Err(Error::DeletingNonEmptyTree(_))),
        "should return DeletingNonEmptyTree error, got: {:?}",
        result
    );
}

#[test]
fn test_bulk_delete_and_recreate() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree");

    // Append 5 values (1 chunk + 1 in buffer)
    for i in 0..5u8 {
        db.bulk_append(EMPTY_PATH, b"bulk", vec![i], None, grove_version)
            .unwrap()
            .expect("append value");
    }

    // Delete with allow
    db.delete(
        EMPTY_PATH,
        b"bulk",
        Some(DeleteOptions {
            allow_deleting_non_empty_trees: true,
            deleting_non_empty_trees_returns_error: true,
            ..Default::default()
        }),
        None,
        grove_version,
    )
    .unwrap()
    .expect("should delete non-empty bulk append tree");

    // Recreate with same chunk_power
    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("recreate bulk append tree");

    // Append 1 value
    db.bulk_append(EMPTY_PATH, b"bulk", b"fresh".to_vec(), None, grove_version)
        .unwrap()
        .expect("append to recreated tree");

    // Count should be 1 (fresh start)
    let count = db
        .bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("count after recreate");
    assert_eq!(count, 1, "recreated tree should have only 1 entry");
}

// ===========================================================================
// verify_grovedb tests
// ===========================================================================

#[test]
fn test_verify_grovedb_bulk_tree_valid() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree");

    // Append some values (including enough for at least one compaction)
    for i in 0..6u8 {
        db.bulk_append(EMPTY_PATH, b"bulk", vec![i], None, grove_version)
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
// Persistence-across-reopen tests
// ===========================================================================

#[test]
fn test_bulk_persistence_across_reopen() {
    let grove_version = GroveVersion::latest();
    let tmp_dir = tempfile::TempDir::new().expect("should create temp dir");

    // Open, insert BulkAppendTree (epoch_size=4), append 6 values (triggers
    // compaction)
    {
        let db = crate::GroveDb::open(tmp_dir.path()).expect("should open grovedb");
        db.insert(
            EMPTY_PATH,
            b"bulk",
            Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert bulk append tree");

        // Append 6 values (chunk_size=4 triggers 1 compaction, 2 remain in buffer)
        for i in 0..6u8 {
            db.bulk_append(EMPTY_PATH, b"bulk", vec![i], None, grove_version)
                .unwrap()
                .expect("append value");
        }

        // Verify compaction happened before closing
        let chunk_count = db
            .bulk_chunk_count(EMPTY_PATH, b"bulk", None, grove_version)
            .unwrap()
            .expect("chunk count before close");
        assert_eq!(chunk_count, 1, "should have 1 completed chunk before close");
    }
    // db is dropped here

    // Reopen and verify state
    {
        let db = crate::GroveDb::open(tmp_dir.path()).expect("should reopen grovedb");

        // Verify count == 6
        let count = db
            .bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
            .unwrap()
            .expect("count after reopen");
        assert_eq!(count, 6, "count should be 6 after reopen");

        // Verify all values retrievable (from chunk blob and buffer)
        for i in 0..6u8 {
            let val = db
                .bulk_get_value(EMPTY_PATH, b"bulk", i as u64, None, grove_version)
                .unwrap()
                .expect("get value after reopen");
            assert_eq!(
                val,
                Some(vec![i]),
                "value at position {} should match after reopen",
                i
            );
        }

        // Verify chunk count is stable
        let chunk_count = db
            .bulk_chunk_count(EMPTY_PATH, b"bulk", None, grove_version)
            .unwrap()
            .expect("chunk count after reopen");
        assert_eq!(chunk_count, 1, "chunk count should be stable across reopen");

        // Verify root hash is stable by appending one more value and confirming it
        // doesn't panic (ensures internal state was properly restored)
        let (state_root, position) = db
            .bulk_append(
                EMPTY_PATH,
                b"bulk",
                b"after_reopen".to_vec(),
                None,
                grove_version,
            )
            .unwrap()
            .expect("append after reopen should succeed");
        assert_eq!(position, 6, "next position should be 6");
        assert_ne!(state_root, [0u8; 32], "state root should not be zero");

        let count_after = db
            .bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
            .unwrap()
            .expect("count after additional append");
        assert_eq!(count_after, 7, "count should be 7 after additional append");
    }
}

// ===========================================================================
// Batch-with-transaction tests
// ===========================================================================

#[test]
fn test_bulk_batch_with_transaction() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Tree must exist BEFORE the batch (preprocessing requires it)
    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree");

    let tx = db.start_transaction();

    let ops = vec![
        QualifiedGroveDbOp::bulk_append_op(vec![b"bulk".to_vec()], b"tx_val_1".to_vec()),
        QualifiedGroveDbOp::bulk_append_op(vec![b"bulk".to_vec()], b"tx_val_2".to_vec()),
    ];

    db.apply_batch(ops, None, Some(&tx), grove_version)
        .unwrap()
        .expect("batch apply in transaction");

    // Not visible outside the transaction
    let count_outside = db
        .bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("count outside tx");
    assert_eq!(count_outside, 0, "data should not be visible outside tx");

    // Verify data is visible inside the transaction
    let count_inside = db
        .bulk_count(EMPTY_PATH, b"bulk", Some(&tx), grove_version)
        .unwrap()
        .expect("count inside tx");
    assert_eq!(count_inside, 2, "data should be visible inside tx");

    // Commit the transaction
    db.commit_transaction(tx).unwrap().expect("commit tx");

    // After commit, data should be visible
    let count_after = db
        .bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("count after commit");
    assert_eq!(count_after, 2, "data should be visible after commit");

    // Verify values are correct
    let val0 = db
        .bulk_get_value(EMPTY_PATH, b"bulk", 0, None, grove_version)
        .unwrap()
        .expect("get value 0");
    assert_eq!(val0, Some(b"tx_val_1".to_vec()), "first value should match");

    let val1 = db
        .bulk_get_value(EMPTY_PATH, b"bulk", 1, None, grove_version)
        .unwrap()
        .expect("get value 1");
    assert_eq!(
        val1,
        Some(b"tx_val_2".to_vec()),
        "second value should match"
    );
}

#[test]
fn test_bulk_batch_transaction_rollback() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Tree must exist BEFORE the batch
    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree");

    let tx = db.start_transaction();

    let ops = vec![QualifiedGroveDbOp::bulk_append_op(
        vec![b"bulk".to_vec()],
        b"rollback_val".to_vec(),
    )];

    db.apply_batch(ops, None, Some(&tx), grove_version)
        .unwrap()
        .expect("batch apply in transaction");

    // Verify data is visible inside tx before rollback
    let count_inside = db
        .bulk_count(EMPTY_PATH, b"bulk", Some(&tx), grove_version)
        .unwrap()
        .expect("count inside tx");
    assert_eq!(count_inside, 1, "data should be visible inside tx");

    // Drop tx (rollback)
    drop(tx);

    // Data should NOT be visible after rollback
    let count_after = db
        .bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("count after rollback");
    assert_eq!(count_after, 0, "data should not be visible after rollback");
}

// ===========================================================================
// verify_grovedb empty tree test
// ===========================================================================

#[test]
fn test_verify_grovedb_bulk_tree_empty() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert an empty BulkAppendTree (no appends)
    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert empty bulk append tree");

    // verify_grovedb should report no issues on an empty BulkAppendTree
    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify should not fail");
    assert!(
        issues.is_empty(),
        "expected no issues for empty bulk append tree, got: {:?}",
        issues
    );
}

// ===========================================================================
// Additional coverage tests
// ===========================================================================

/// Tests error when calling `bulk_count` on a non-BulkAppendTree element.
#[test]
fn test_bulk_count_on_non_bulk_element() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"item",
        Element::new_item(b"hello".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert item");

    let result = db
        .bulk_count(EMPTY_PATH, b"item", None, grove_version)
        .unwrap();
    assert!(
        matches!(result, Err(Error::InvalidInput(msg)) if msg.contains("not a BulkAppendTree")),
        "expected InvalidInput error for non-bulk element, got: {:?}",
        result
    );
}

/// Tests error when calling `bulk_chunk_count` on a non-BulkAppendTree element.
#[test]
fn test_bulk_chunk_count_on_non_bulk_element() {
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
        .bulk_chunk_count(EMPTY_PATH, b"item", None, grove_version)
        .unwrap();
    assert!(
        matches!(result, Err(Error::InvalidInput(msg)) if msg.contains("not a BulkAppendTree")),
        "expected InvalidInput error for non-bulk element, got: {:?}",
        result
    );
}

/// Tests error when calling `bulk_get_buffer` on a non-BulkAppendTree element.
#[test]
fn test_bulk_get_buffer_on_non_bulk_element() {
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
        .bulk_get_buffer(EMPTY_PATH, b"item", None, grove_version)
        .unwrap();
    assert!(
        matches!(result, Err(Error::InvalidInput(msg)) if msg.contains("not a BulkAppendTree")),
        "expected InvalidInput error for non-bulk element, got: {:?}",
        result
    );
}

/// Tests error when calling `bulk_get_chunk` on a non-BulkAppendTree element.
#[test]
fn test_bulk_get_chunk_on_non_bulk_element() {
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
        .bulk_get_chunk(EMPTY_PATH, b"item", 0, None, grove_version)
        .unwrap();
    assert!(
        matches!(result, Err(Error::InvalidInput(msg)) if msg.contains("not a BulkAppendTree")),
        "expected InvalidInput error for non-bulk element, got: {:?}",
        result
    );
}

/// Tests error when calling `bulk_get_value` on a non-BulkAppendTree element.
#[test]
fn test_bulk_get_value_on_non_bulk_element() {
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
        .bulk_get_value(EMPTY_PATH, b"item", 0, None, grove_version)
        .unwrap();
    assert!(
        matches!(result, Err(Error::InvalidInput(msg)) if msg.contains("not a BulkAppendTree")),
        "expected InvalidInput error for non-bulk element, got: {:?}",
        result
    );
}

/// Tests batch preprocessing error when a BulkAppend op targets a
/// non-BulkAppendTree element.
#[test]
fn test_bulk_batch_with_non_bulk_element() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    // Insert a normal tree, not a BulkAppendTree
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

    // Insert an item inside the tree so the path resolves
    db.insert(
        [b"tree"].as_ref(),
        b"item",
        Element::new_item(b"hello".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert item");

    // Batch with BulkAppend targeting the item (not a BulkAppendTree)
    let ops = vec![QualifiedGroveDbOp::bulk_append_op(
        vec![b"tree".to_vec(), b"item".to_vec()],
        b"should_fail".to_vec(),
    )];

    let result = db.apply_batch(ops, None, None, grove_version).unwrap();
    assert!(
        matches!(result, Err(Error::InvalidInput(msg)) if msg.contains("not a BulkAppendTree")),
        "expected InvalidInput error when batch targets non-bulk element, got: {:?}",
        result
    );
}

// ===========================================================================
// Batch discard / transaction rollback tests
// ===========================================================================

/// A batch with bulk appends succeeds, then a later op in the batch fails.
/// The entire batch must be discarded — the bulk count should remain at its
/// pre-batch value.
#[test]
fn test_bulk_batch_discarded_on_later_op_failure() {
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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk tree");

    // Pre-insert an item so insert_if_not_exists will fail
    db.insert(
        [b"parent"].as_ref(),
        b"existing",
        Element::new_item(b"old".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert existing item");

    // Batch: bulk append (should succeed internally) + conflicting insert
    let ops = vec![
        QualifiedGroveDbOp::bulk_append_op(
            vec![b"parent".to_vec(), b"bulk".to_vec()],
            b"note_data".to_vec(),
        ),
        QualifiedGroveDbOp::insert_if_not_exists_op(
            vec![b"parent".to_vec()],
            b"existing".to_vec(),
            Element::new_item(b"conflict".to_vec()),
        ),
    ];

    // Use an external (borrowed) transaction to verify no preprocessing data
    // leaks into it when the batch fails.
    let tx = db.start_transaction();
    let result = db.apply_batch(ops, None, Some(&tx), grove_version).unwrap();
    assert!(result.is_err(), "batch should fail due to duplicate key");

    // Bulk count must remain 0 inside the transaction
    let count_in_tx = db
        .bulk_count([b"parent"].as_ref(), b"bulk", Some(&tx), grove_version)
        .unwrap()
        .expect("count in tx");
    assert_eq!(
        count_in_tx, 0,
        "bulk count should be 0 inside tx after batch discard"
    );

    // Verify no raw subtree data leaked into the transaction. The bulk append
    // tree's dense buffer uses 2-byte position keys; position 0 → [0, 0].
    use grovedb_dense_fixed_sized_merkle_tree::position_key;
    let subtree_path: Vec<Vec<u8>> = vec![b"parent".to_vec(), b"bulk".to_vec()];
    let subtree_refs: Vec<&[u8]> = subtree_path.iter().map(|v| v.as_slice()).collect();
    let raw_pos0 = db
        .raw_subtree_get(subtree_refs.as_slice().into(), &position_key(0), &tx)
        .expect("raw get should not error");
    assert!(
        raw_pos0.is_none(),
        "raw bulk subtree storage at position 0 should be empty inside tx after batch discard"
    );

    // Also verify outside the transaction
    let count = db
        .bulk_count([b"parent"].as_ref(), b"bulk", None, grove_version)
        .unwrap()
        .expect("count");
    assert_eq!(count, 0, "bulk count should be 0 after batch discard");

    db.rollback_transaction(&tx).expect("rollback");
}

/// Bulk appends that trigger compaction, then a later op fails. Verifies
/// both the dense tree buffer and the MMR state are discarded.
#[test]
fn test_bulk_batch_discarded_after_compaction() {
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

    // chunk_power=2 → capacity=3, epoch_size=4 → compaction on 4th append
    db.insert(
        [b"parent"].as_ref(),
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk tree");

    db.insert(
        [b"parent"].as_ref(),
        b"existing",
        Element::new_item(b"old".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert existing item");

    // Build a batch with enough appends to trigger compaction (4 appends for
    // chunk_power=2), followed by a conflicting insert to fail the batch
    let mut ops: Vec<QualifiedGroveDbOp> = (0..TEST_CHUNK_SIZE as u8)
        .map(|i| {
            QualifiedGroveDbOp::bulk_append_op(vec![b"parent".to_vec(), b"bulk".to_vec()], vec![i])
        })
        .collect();
    ops.push(QualifiedGroveDbOp::insert_if_not_exists_op(
        vec![b"parent".to_vec()],
        b"existing".to_vec(),
        Element::new_item(b"conflict".to_vec()),
    ));

    // Use an external (borrowed) transaction to verify no preprocessing data
    // leaks into it when the batch fails — even after compaction.
    let tx = db.start_transaction();
    let result = db.apply_batch(ops, None, Some(&tx), grove_version).unwrap();
    assert!(
        result.is_err(),
        "batch should fail after compaction + conflict"
    );

    // Bulk count must be 0 inside the transaction
    let count_in_tx = db
        .bulk_count([b"parent"].as_ref(), b"bulk", Some(&tx), grove_version)
        .unwrap()
        .expect("count in tx");
    assert_eq!(
        count_in_tx, 0,
        "bulk count should be 0 inside tx after batch discard (even after compaction)"
    );

    // Verify no raw subtree data leaked — check both dense buffer (position 0)
    // and MMR data (MSB-tagged position 0) since compaction was triggered.
    use grovedb_dense_fixed_sized_merkle_tree::position_key;
    let subtree_path: Vec<Vec<u8>> = vec![b"parent".to_vec(), b"bulk".to_vec()];
    let subtree_refs: Vec<&[u8]> = subtree_path.iter().map(|v| v.as_slice()).collect();
    let raw_dense_pos0 = db
        .raw_subtree_get(subtree_refs.as_slice().into(), &position_key(0), &tx)
        .expect("raw dense get should not error");
    assert!(
        raw_dense_pos0.is_none(),
        "raw dense buffer at position 0 should be empty inside tx after compaction discard"
    );
    // BulkAppendTree compaction uses MmrKeySize::U32 (4-byte tagged keys),
    // not U64 (8-byte). Position 0 with U32 tag: 0x8000_0000.
    let mmr_key_pos0 = 0x8000_0000u32.to_be_bytes();
    let raw_mmr_pos0 = db
        .raw_subtree_get(subtree_refs.as_slice().into(), &mmr_key_pos0, &tx)
        .expect("raw mmr get should not error");
    assert!(
        raw_mmr_pos0.is_none(),
        "raw MMR data at position 0 should be empty inside tx after compaction discard"
    );

    // Also verify outside the transaction
    let count = db
        .bulk_count([b"parent"].as_ref(), b"bulk", None, grove_version)
        .unwrap()
        .expect("count");
    assert_eq!(
        count, 0,
        "bulk count should be 0 after batch discard (even after compaction)"
    );

    db.rollback_transaction(&tx).expect("rollback");
}

/// Bulk appends inside a transaction, then the transaction is rolled back.
/// Verifies the count reverts to pre-transaction value.
#[test]
fn test_bulk_transaction_rollback_reverts_appends() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk tree");

    // Insert one value outside the transaction
    db.bulk_append(EMPTY_PATH, b"bulk", b"v0".to_vec(), None, grove_version)
        .unwrap()
        .expect("append v0");

    let count_before = db
        .bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("count before");
    assert_eq!(count_before, 1);

    // Start transaction and append more (enough to trigger compaction)
    let tx = db.start_transaction();

    for i in 1..(TEST_CHUNK_SIZE as u8 + 2) {
        db.bulk_append(EMPTY_PATH, b"bulk", vec![i], Some(&tx), grove_version)
            .unwrap()
            .expect("append in tx");
    }

    // Rollback
    db.rollback_transaction(&tx).expect("rollback");

    // Count should revert to 1
    let count_after = db
        .bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("count after rollback");
    assert_eq!(count_after, 1, "bulk count should revert after rollback");
}

/// After a failed batch, a subsequent successful batch on the same bulk
/// append tree should work correctly — no stale cache/overlay state leaks.
#[test]
fn test_bulk_successful_batch_after_failed_batch() {
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
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk tree");

    db.insert(
        [b"parent"].as_ref(),
        b"existing",
        Element::new_item(b"old".to_vec()),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert existing item");

    // Use an external transaction — the failing batch must not leak
    // preprocessing data into the tx.
    let tx = db.start_transaction();

    // First batch: bulk append + conflicting insert → fails
    let ops_fail = vec![
        QualifiedGroveDbOp::bulk_append_op(
            vec![b"parent".to_vec(), b"bulk".to_vec()],
            b"ghost".to_vec(),
        ),
        QualifiedGroveDbOp::insert_if_not_exists_op(
            vec![b"parent".to_vec()],
            b"existing".to_vec(),
            Element::new_item(b"conflict".to_vec()),
        ),
    ];

    let result = db
        .apply_batch(ops_fail, None, Some(&tx), grove_version)
        .unwrap();
    assert!(result.is_err(), "first batch should fail");

    // Tx should be clean — no preprocessing data leaked
    let count_after_fail = db
        .bulk_count([b"parent"].as_ref(), b"bulk", Some(&tx), grove_version)
        .unwrap()
        .expect("count after fail");
    assert_eq!(
        count_after_fail, 0,
        "bulk count in tx should be 0 after failed batch"
    );

    // Verify no raw subtree data leaked into the transaction
    use grovedb_dense_fixed_sized_merkle_tree::position_key;
    let subtree_path: Vec<Vec<u8>> = vec![b"parent".to_vec(), b"bulk".to_vec()];
    let subtree_refs: Vec<&[u8]> = subtree_path.iter().map(|v| v.as_slice()).collect();
    let raw_pos0 = db
        .raw_subtree_get(subtree_refs.as_slice().into(), &position_key(0), &tx)
        .expect("raw get should not error");
    assert!(
        raw_pos0.is_none(),
        "raw bulk subtree storage at position 0 should be empty inside tx after failed batch"
    );

    // Second batch: bulk append only → should succeed in the same tx
    let ops_ok = vec![QualifiedGroveDbOp::bulk_append_op(
        vec![b"parent".to_vec(), b"bulk".to_vec()],
        b"real".to_vec(),
    )];

    db.apply_batch(ops_ok, None, Some(&tx), grove_version)
        .unwrap()
        .expect("second batch should succeed");

    // Commit the transaction
    db.commit_transaction(tx)
        .unwrap()
        .expect("commit transaction");

    let count = db
        .bulk_count([b"parent"].as_ref(), b"bulk", None, grove_version)
        .unwrap()
        .expect("count");
    assert_eq!(count, 1, "only the second batch's value should be present");
}

/// Multiple compaction cycles via batch + transaction, then rollback.
/// Verifies the tree fully reverts even when multiple chunks were created.
#[test]
fn test_bulk_batch_multi_compaction_transaction_rollback() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk tree");

    let tx = db.start_transaction();

    // Append enough to trigger 2 full compaction cycles + partial buffer
    // chunk_power=2: epoch_size=4, so 9 appends = 2 compactions + 1 buffered
    let total_appends = TEST_CHUNK_SIZE * 2 + 1;
    let ops: Vec<QualifiedGroveDbOp> = (0..total_appends as u8)
        .map(|i| QualifiedGroveDbOp::bulk_append_op(vec![b"bulk".to_vec()], vec![i]))
        .collect();

    db.apply_batch(ops, None, Some(&tx), grove_version)
        .unwrap()
        .expect("batch in tx");

    let count_in_tx = db
        .bulk_count(EMPTY_PATH, b"bulk", Some(&tx), grove_version)
        .unwrap()
        .expect("count in tx");
    assert_eq!(count_in_tx, total_appends as u64);

    // Drop transaction
    drop(tx);

    let count_after = db
        .bulk_count(EMPTY_PATH, b"bulk", None, grove_version)
        .unwrap()
        .expect("count after drop");
    assert_eq!(
        count_after, 0,
        "bulk tree should be empty after multi-compaction tx rollback"
    );
}

// ===========================================================================
// Paginated position-range reads (get_range) and range proofs
// ===========================================================================

/// Helper: create a DB with a BulkAppendTree at root key `b"bulk"` holding
/// `n` values `b"value_0"`, `b"value_1"`, ... (chunk_power = 2 → chunk size
/// 4).
fn make_bulk_db_with_values(n: u32) -> crate::tests::TempGroveDb {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree");

    for i in 0..n {
        db.bulk_append(
            EMPTY_PATH,
            b"bulk",
            format!("value_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("bulk append");
    }

    db
}

/// Helper: assert a page holds exactly positions `start..end` with the
/// values written by [`make_bulk_db_with_values`].
fn assert_bulk_page(page: &crate::RangePage, start: u64, end: u64, total_count: u64) {
    assert_eq!(page.total_count, total_count, "page total_count");
    assert_eq!(page.entries.len(), (end - start) as usize, "page length");
    for (i, (pos, value)) in page.entries.iter().enumerate() {
        assert_eq!(*pos, start + i as u64, "page position");
        assert_eq!(value, format!("value_{}", pos).as_bytes(), "page value");
    }
}

#[test]
fn test_bulk_get_range_matches_get_value() {
    let grove_version = GroveVersion::latest();
    // 11 values = 2 full chunks (8) + 3 buffered
    let db = make_bulk_db_with_values(11);

    // Every possible page of size 4 must match per-position reads
    for start in 0..12u64 {
        let page = db
            .bulk_get_range(EMPTY_PATH, b"bulk", start, 4, None, grove_version)
            .unwrap()
            .expect("bulk get range");
        assert_eq!(page.total_count, 11);

        let end = (start + 4).min(11);
        let expected_len = end.saturating_sub(start.min(end));
        assert_eq!(page.entries.len(), expected_len as usize);

        for (pos, value) in &page.entries {
            let expected = db
                .bulk_get_value(EMPTY_PATH, b"bulk", *pos, None, grove_version)
                .unwrap()
                .expect("bulk get value")
                .expect("value exists");
            assert_eq!(value, &expected);
        }
    }
}

#[test]
fn test_bulk_get_range_empty_and_past_end() {
    let grove_version = GroveVersion::latest();
    let db = make_bulk_db_with_values(10);

    // limit = 0
    let page = db
        .bulk_get_range(EMPTY_PATH, b"bulk", 3, 0, None, grove_version)
        .unwrap()
        .expect("bulk get range");
    assert_bulk_page(&page, 3, 3, 10);

    // start exactly at total_count
    let page = db
        .bulk_get_range(EMPTY_PATH, b"bulk", 10, 5, None, grove_version)
        .unwrap()
        .expect("bulk get range");
    assert_bulk_page(&page, 10, 10, 10);

    // start far past total_count
    let page = db
        .bulk_get_range(EMPTY_PATH, b"bulk", 1000, 5, None, grove_version)
        .unwrap()
        .expect("bulk get range");
    assert!(page.entries.is_empty());
    assert_eq!(page.total_count, 10);

    // clamped at the end
    let page = db
        .bulk_get_range(EMPTY_PATH, b"bulk", 8, 100, None, grove_version)
        .unwrap()
        .expect("bulk get range");
    assert_bulk_page(&page, 8, 10, 10);
}

#[test]
fn test_bulk_get_range_wrong_element_type() {
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
        .bulk_get_range(EMPTY_PATH, b"normal", 0, 4, None, grove_version)
        .unwrap();
    assert!(matches!(result, Err(Error::InvalidInput(_))));
}

/// Helper: prove and verify a position range page, asserting the returned
/// root hash matches the database root hash, and return the page.
fn roundtrip_bulk_range_proof(
    db: &crate::tests::TempGroveDb,
    start: u64,
    limit: u16,
) -> crate::RangePage {
    let grove_version = GroveVersion::latest();

    let proof = db
        .prove_bulk_position_range(vec![], b"bulk", start, limit, None, grove_version)
        .unwrap()
        .expect("prove bulk position range");

    let (root_hash, page) = crate::GroveDb::verify_bulk_position_range_proof(
        &proof,
        vec![],
        b"bulk",
        start,
        limit,
        grove_version,
    )
    .expect("verify bulk position range proof");

    let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(root_hash, expected_root, "proof root must match db root");

    page
}

#[test]
fn test_bulk_position_range_proof_across_chunk_boundary() {
    // 10 values = 2 chunks (8) + 2 buffered, chunk size 4
    let db = make_bulk_db_with_values(10);

    // Page [3, 6) spans the chunk 0 / chunk 1 boundary
    let page = roundtrip_bulk_range_proof(&db, 3, 3);
    assert_bulk_page(&page, 3, 6, 10);

    // Page [6, 10) spans the chunk 1 / buffer boundary
    let page = roundtrip_bulk_range_proof(&db, 6, 4);
    assert_bulk_page(&page, 6, 10, 10);
}

#[test]
fn test_bulk_position_range_proof_empty_range() {
    let db = make_bulk_db_with_values(10);
    let page = roundtrip_bulk_range_proof(&db, 3, 0);
    assert_bulk_page(&page, 3, 3, 10);
}

#[test]
fn test_bulk_position_range_proof_past_end() {
    let db = make_bulk_db_with_values(10);

    // Start exactly at total_count: provably nothing there — the verified
    // total_count is the absence proof.
    let page = roundtrip_bulk_range_proof(&db, 10, 5);
    assert!(page.entries.is_empty());
    assert_eq!(page.total_count, 10);

    // Start far past total_count
    let page = roundtrip_bulk_range_proof(&db, 1000, 5);
    assert!(page.entries.is_empty());
    assert_eq!(page.total_count, 10);

    // Range starting inside but extending past the end is clamped
    let page = roundtrip_bulk_range_proof(&db, 8, 100);
    assert_bulk_page(&page, 8, 10, 10);
}

#[test]
fn test_bulk_position_range_proof_single_entry_pages() {
    let db = make_bulk_db_with_values(10);
    for pos in 0..10u64 {
        let page = roundtrip_bulk_range_proof(&db, pos, 1);
        assert_bulk_page(&page, pos, pos + 1, 10);
    }
}

#[test]
fn test_bulk_position_range_proof_large_multi_chunk_page() {
    // 30 values = 7 full chunks (28) + 2 buffered, chunk size 4.
    let db = make_bulk_db_with_values(30);

    // One page covering everything touches all chunks and the buffer
    let page = roundtrip_bulk_range_proof(&db, 0, 30);
    assert_bulk_page(&page, 0, 30, 30);

    // A large page crossing several chunk boundaries mid-tree
    let page = roundtrip_bulk_range_proof(&db, 5, 20);
    assert_bulk_page(&page, 5, 25, 30);
}

#[test]
fn test_bulk_position_range_proof_empty_tree() {
    let db = make_bulk_db_with_values(0);
    let page = roundtrip_bulk_range_proof(&db, 0, 10);
    assert!(page.entries.is_empty());
    assert_eq!(page.total_count, 0);
}

#[test]
fn test_bulk_position_range_paged_scan() {
    // The scanning pattern: walk the whole tree in proved pages of 7
    // (deliberately not aligned to the chunk size of 4).
    let db = make_bulk_db_with_values(30);

    let mut cursor = 0u64;
    let mut seen = Vec::new();
    loop {
        let page = roundtrip_bulk_range_proof(&db, cursor, 7);
        if page.entries.is_empty() {
            assert!(cursor >= page.total_count, "empty page only at the end");
            break;
        }
        cursor += page.entries.len() as u64;
        seen.extend(page.entries);
    }
    assert_eq!(seen.len(), 30);
    for (i, (pos, value)) in seen.iter().enumerate() {
        assert_eq!(*pos, i as u64);
        assert_eq!(value, format!("value_{}", i).as_bytes());
    }
}

#[test]
fn test_bulk_position_range_prove_wrong_element_type() {
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
        .prove_bulk_position_range(vec![], b"normal", 0, 4, None, grove_version)
        .unwrap();
    assert!(matches!(result, Err(Error::InvalidInput(_))));
}

#[test]
fn test_bulk_position_range_proof_wrong_range_rejected() {
    let grove_version = GroveVersion::latest();
    let db = make_bulk_db_with_values(10);

    // Proof generated for [0, 2) (inside chunk 0) must not verify a request
    // for [0, 6), which also needs chunk 1.
    let narrow_proof = db
        .prove_bulk_position_range(vec![], b"bulk", 0, 2, None, grove_version)
        .unwrap()
        .expect("prove narrow range");

    crate::GroveDb::verify_bulk_position_range_proof(
        &narrow_proof,
        vec![],
        b"bulk",
        0,
        6,
        grove_version,
    )
    .expect_err("proof for a narrower range must be rejected");

    // Intended semantics pin: a DISJOINT range within the SAME chunk
    // verifies. The proof for [0, 2) carries the whole chunk 0 blob
    // (positions 0..4) because proofs are chunk-aligned, and every entry in
    // it is authenticated against the root — so a verifier asking [2, 4)
    // legitimately gets those entries from the same bytes.
    let (root_hash, page) = crate::GroveDb::verify_bulk_position_range_proof(
        &narrow_proof,
        vec![],
        b"bulk",
        2,
        2,
        grove_version,
    )
    .expect("disjoint range within the proved chunk must verify");
    let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(root_hash, expected_root);
    assert_bulk_page(&page, 2, 4, 10);
}

#[test]
fn test_bulk_position_range_proof_deep_path() {
    // Same flow but with the BulkAppendTree nested under a normal tree.
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"deep",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert parent tree");

    db.insert(
        &[b"deep"],
        b"bulk",
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk append tree");

    for i in 0..10u32 {
        db.bulk_append(
            &[b"deep"],
            b"bulk",
            format!("value_{}", i).into_bytes(),
            None,
            grove_version,
        )
        .unwrap()
        .expect("bulk append");
    }

    let proof = db
        .prove_bulk_position_range(vec![b"deep".to_vec()], b"bulk", 2, 5, None, grove_version)
        .unwrap()
        .expect("prove nested bulk position range");

    let (root_hash, page) = crate::GroveDb::verify_bulk_position_range_proof(
        &proof,
        vec![b"deep".to_vec()],
        b"bulk",
        2,
        5,
        grove_version,
    )
    .expect("verify nested bulk position range proof");

    let expected_root = db.root_hash(None, grove_version).unwrap().unwrap();
    assert_eq!(root_hash, expected_root);
    assert_bulk_page(&page, 2, 7, 10);
}

#[test]
fn test_bulk_position_range_version_gates() {
    let grove_version = GroveVersion::latest();
    let db = make_bulk_db_with_values(4);
    let proof = db
        .prove_bulk_position_range(vec![], b"bulk", 0, 4, None, grove_version)
        .unwrap()
        .expect("prove under latest version");

    // Unknown prove version → VersionError before any work happens
    let mut gated = grove_version.clone();
    gated
        .grovedb_versions
        .operations
        .proof
        .prove_bulk_position_range = 99;
    let result = db
        .prove_bulk_position_range(vec![], b"bulk", 0, 4, None, &gated)
        .unwrap();
    assert!(matches!(result, Err(Error::VersionError(_))));

    // Unknown verify version → VersionError before any work happens
    let mut gated = grove_version.clone();
    gated
        .grovedb_versions
        .operations
        .proof
        .verify_bulk_position_range_proof = 99;
    let result =
        crate::GroveDb::verify_bulk_position_range_proof(&proof, vec![], b"bulk", 0, 4, &gated);
    assert!(matches!(result, Err(Error::VersionError(_))));
}

/// A proof over a NORMAL tree holding items at 8-byte keys: the range
/// entries verify as plain Merk rows, but total_count extraction must
/// reject the element type instead of trusting a non-append-only element.
#[test]
fn test_bulk_position_range_verify_rejects_non_append_element() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();

    db.insert(
        EMPTY_PATH,
        b"plain",
        Element::empty_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert normal tree");
    for i in 0..4u64 {
        db.insert(
            &[b"plain"],
            &i.to_be_bytes(),
            Element::new_item(vec![i as u8]),
            None,
            None,
            grove_version,
        )
        .unwrap()
        .expect("insert item");
    }

    // Bypass prove_bulk_position_range's own element check by proving the
    // canonical query directly, as a crafted prover would.
    let query = crate::PathQuery::new_bulk_position_range(vec![], b"plain".to_vec(), 0, 4);
    let proof = db
        .prove_query(&query, None, grove_version)
        .unwrap()
        .expect("prove canonical query over normal tree");

    let err = crate::GroveDb::verify_bulk_position_range_proof(
        &proof,
        vec![],
        b"plain",
        0,
        4,
        grove_version,
    )
    .expect_err("normal tree element must be rejected");
    assert!(matches!(err, Error::InvalidProof(..)));
}

/// A proof for a key that does not exist: the range query verifies (as an
/// absence), but the element subset query binds nothing, so the verifier
/// must refuse to invent a total_count.
#[test]
fn test_bulk_position_range_verify_rejects_unbound_element() {
    let grove_version = GroveVersion::latest();
    let db = make_bulk_db_with_values(4);

    let query = crate::PathQuery::new_bulk_position_range(vec![], b"ghost".to_vec(), 0, 4);
    let proof = db
        .prove_query(&query, None, grove_version)
        .unwrap()
        .expect("prove canonical query for missing key");

    let err = crate::GroveDb::verify_bulk_position_range_proof(
        &proof,
        vec![],
        b"ghost",
        0,
        4,
        grove_version,
    )
    .expect_err("proof that binds no element must be rejected");
    assert!(matches!(err, Error::InvalidProof(..)));
}

/// Range reads must charge their storage work: the page's chunk-MMR seeks
/// and buffer reads all show up in the returned cost, so cost limits
/// reflect the actual work instead of treating a 65k-entry page as free.
#[test]
fn test_bulk_get_range_reports_storage_costs() {
    let grove_version = GroveVersion::latest();
    // 11 values = 2 full chunks + 3 buffered (chunk size 4)
    let db = make_bulk_db_with_values(11);

    // A page spanning both chunks and the buffer
    let result = db.bulk_get_range(EMPTY_PATH, b"bulk", 0, 11, None, grove_version);
    let cost = result.cost.clone();
    let page = result.unwrap().expect("bulk get range");
    assert_eq!(page.entries.len(), 11);
    assert!(
        cost.storage_loaded_bytes > 0,
        "range read must charge loaded bytes, got {:?}",
        cost
    );
    assert!(
        cost.seek_count > 0,
        "range read must charge seeks, got {:?}",
        cost
    );

    // A wider page must not cost less than a narrower one
    let narrow_cost = db
        .bulk_get_range(EMPTY_PATH, b"bulk", 8, 1, None, grove_version)
        .cost;
    assert!(
        cost.storage_loaded_bytes > narrow_cost.storage_loaded_bytes,
        "an 11-entry page must load more bytes than a 1-entry page ({} vs {})",
        cost.storage_loaded_bytes,
        narrow_cost.storage_loaded_bytes
    );
}
