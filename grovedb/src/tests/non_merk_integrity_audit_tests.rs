//! Integrity audits of the append-only family under GROVE_V4 root
//! maintenance: `verify_grovedb` must derive every non-Merk state root from
//! the stored VALUES, never from the dense buffer's hash records (which a
//! normal root read trusts), and must report records that disagree with the
//! values — otherwise a payload altered behind the records, or records
//! written for other values, would verify clean while every V4 read returned
//! a different root.

use grovedb_dense_fixed_sized_merkle_tree::{position_key, record_key, HashRecord};
use grovedb_storage::{Storage, StorageContext};
use grovedb_version::version::GroveVersion;

use crate::{
    tests::{common::EMPTY_PATH, make_empty_grovedb, TempGroveDb},
    Element,
};

const RECORD_ISSUE_KEY: &[u8] = b"__dense_hash_records__";

/// Overwrite a raw key under `tree_path` behind the tree's back.
fn tamper(db: &TempGroveDb, tree_path: &[&[u8]], key: &[u8], value: &[u8]) {
    let tx = db.start_transaction();
    let ctx = db
        .raw_storage()
        .get_immediate_storage_context(tree_path.into(), &tx)
        .unwrap();
    ctx.put(key, value, None, None).unwrap().expect("raw put");
    drop(ctx);
    db.commit_transaction(tx).unwrap().expect("commit tamper");
}

/// Verify and return the issue paths.
fn issue_paths(db: &TempGroveDb, grove_version: &GroveVersion) -> Vec<Vec<Vec<u8>>> {
    db.verify_grovedb(None, true, false, grove_version)
        .expect("verify should run")
        .into_keys()
        .collect()
}

/// A standalone dense tree: four V4 inserts, then position 3's value is
/// replaced on disk by a same-length value while the records are left
/// intact. A root read that trusted the records would still return the
/// committed root; the audit walks the values and must flag the tree — and
/// flag the now-disagreeing records as their own issue.
#[test]
fn test_verify_grovedb_detects_dense_payload_tampering_behind_records() {
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
    .expect("insert dense tree");
    for i in 0..4u8 {
        db.dense_tree_insert(EMPTY_PATH, b"dense", vec![i; 16], None, grove_version)
            .unwrap()
            .expect("insert value");
    }
    assert!(issue_paths(&db, grove_version).is_empty());

    tamper(&db, &[b"dense"], &position_key(3), &[0xEE; 16]);

    let issues = issue_paths(&db, grove_version);
    assert!(
        issues.contains(&vec![b"dense".to_vec()]),
        "the tampered payload must break the child-hash binding: {issues:?}"
    );
    assert!(
        issues.contains(&vec![b"dense".to_vec(), RECORD_ISSUE_KEY.to_vec()]),
        "the records now disagree with the values and must be reported: {issues:?}"
    );
}

/// The converse: the values are intact but the position-0 record (current
/// generation) claims another root. The child-hash binding still holds —
/// the audit walks the values — so only the record issue must fire, and it
/// must fire: every V4 root read would otherwise return the wrong root.
#[test]
fn test_verify_grovedb_detects_dense_records_disagreeing_with_values() {
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
    .expect("insert dense tree");
    for i in 0..5u8 {
        db.dense_tree_insert(EMPTY_PATH, b"dense", vec![i; 16], None, grove_version)
            .unwrap()
            .expect("insert value");
    }
    let forged = HashRecord {
        generation: 0,
        value_hash: [1u8; 32],
        node_hash: [2u8; 32],
    };
    tamper(&db, &[b"dense"], &record_key(0), &forged.to_bytes());

    let issues = issue_paths(&db, grove_version);
    assert_eq!(
        issues,
        vec![vec![b"dense".to_vec(), RECORD_ISSUE_KEY.to_vec()]],
        "only the record audit should fire: {issues:?}"
    );
}

/// A buffer filled under GROVE_V3 has no records: that is not an issue (the
/// next V4 insert catches it up), and the value walk still verifies it.
#[test]
fn test_verify_grovedb_accepts_a_v3_filled_dense_buffer_under_v4() {
    use grovedb_version::version::v3::GROVE_V3;
    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        b"dense",
        Element::empty_dense_tree(4),
        None,
        None,
        &GROVE_V3,
    )
    .unwrap()
    .expect("insert dense tree");
    for i in 0..6u8 {
        db.dense_tree_insert(EMPTY_PATH, b"dense", vec![i; 16], None, &GROVE_V3)
            .unwrap()
            .expect("insert value");
    }
    assert!(issue_paths(&db, GroveVersion::latest()).is_empty());
    assert!(issue_paths(&db, &GROVE_V3).is_empty());
}

/// The same blind spot reaches the trees built on the bulk buffer: a
/// PrivateDocumentStore whose live-buffer entry is altered on disk behind
/// its records must fail verification on both counts.
#[test]
fn test_verify_grovedb_detects_store_payload_tampering_behind_records() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        b"docs",
        Element::empty_private_document_store(16, 3).expect("valid config"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert store");
    for i in 0..5u8 {
        db.private_document_store_insert(EMPTY_PATH, b"docs", vec![i; 16], None, grove_version)
            .unwrap()
            .expect("append");
    }
    assert!(issue_paths(&db, grove_version).is_empty());

    // Buffer position 2 (same length, so the entry-size audit stays quiet).
    tamper(&db, &[b"docs"], &position_key(2), &[0xEE; 16]);

    let issues = issue_paths(&db, grove_version);
    assert!(issues.contains(&vec![b"docs".to_vec()]), "{issues:?}");
    assert!(
        issues.contains(&vec![b"docs".to_vec(), RECORD_ISSUE_KEY.to_vec()]),
        "{issues:?}"
    );
}

/// And a bulk-append tree, whose buffer sits behind completed chunks.
#[test]
fn test_verify_grovedb_detects_bulk_payload_tampering_behind_records() {
    let grove_version = GroveVersion::latest();
    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        b"bulk",
        Element::empty_bulk_append_tree(2).expect("valid chunk_power"),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert bulk tree");
    // Epoch of 4: one completed chunk, two live buffer positions.
    for i in 0..6u8 {
        db.bulk_append(EMPTY_PATH, b"bulk", vec![i; 12], None, grove_version)
            .unwrap()
            .expect("append");
    }
    assert!(issue_paths(&db, grove_version).is_empty());

    tamper(&db, &[b"bulk"], &position_key(1), &[0xEE; 12]);

    let issues = issue_paths(&db, grove_version);
    assert!(issues.contains(&vec![b"bulk".to_vec()]), "{issues:?}");
    assert!(
        issues.contains(&vec![b"bulk".to_vec(), RECORD_ISSUE_KEY.to_vec()]),
        "{issues:?}"
    );
}
