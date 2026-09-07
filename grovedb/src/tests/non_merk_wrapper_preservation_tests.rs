//! Wrapper preservation on typed appends to non-Merk data trees.
//!
//! A typed append (direct or batch) rewrites the tree's parent-Merk element
//! from the new entry count and the stored flags. The rebuild constructors
//! produce a BARE element, so a stored `NonCounted(tree)` needs its wrapper
//! restored or the append strips it — flipping the tree's contribution to a
//! `CountTree` parent from 0 to 1, and with it the parent aggregate and the
//! root hash.
//!
//! `GROVE_V4`+ restores the wrapper for every family
//! (`operations.non_merk_tree.parent_element_rewrap: 1` — see
//! `rewrap_non_merk_tree_parent_element`); `GROVE_V1`..`GROVE_V3` keep the
//! released wrapper drop, pinned here so the legacy replay path cannot
//! silently change. `PrivateDocumentStore`, whose wrapper is preserved at
//! every version, is covered in `private_document_store_tests`.

use grovedb_commitment_tree::{DashMemo, NoteBytesData, TransmittedNoteCiphertext};
use grovedb_version::version::{v3::GROVE_V3, GroveVersion};

use crate::{
    batch::QualifiedGroveDbOp,
    tests::{common::EMPTY_PATH, make_empty_grovedb, TempGroveDb},
    Element,
};

/// Chunk power for the bulk-append and commitment trees (buffer of 2^4
/// entries — large enough that no test triggers compaction).
const TEST_CHUNK_POWER: u8 = 4;

/// Read the root `CountTree`'s recorded aggregate count.
fn count_tree_value(db: &TempGroveDb, grove_version: &GroveVersion) -> u64 {
    match db
        .get(EMPTY_PATH, b"counted", None, grove_version)
        .unwrap()
        .expect("get count tree")
    {
        Element::CountTree(_, count, _) => count,
        other => panic!("expected CountTree, got {}", other.type_str()),
    }
}

/// Build a `CountTree` at `counted` holding the given tree element wrapped in
/// `NonCounted` at `counted/tree`, and assert the parent count starts at 0.
fn make_non_counted_tree_under_count_tree(
    inner: Element,
    grove_version: &GroveVersion,
) -> TempGroveDb {
    let db = make_empty_grovedb();
    db.insert(
        EMPTY_PATH,
        b"counted",
        Element::empty_count_tree(),
        None,
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert count tree");

    let wrapped = Element::new_non_counted(inner).expect("wrap in NonCounted");
    db.insert(&[b"counted"], b"tree", wrapped, None, None, grove_version)
        .unwrap()
        .expect("insert NonCounted tree");

    assert_eq!(
        count_tree_value(&db, grove_version),
        0,
        "a NonCounted child must not be counted"
    );
    db
}

/// Read the RAW stored element at `counted/tree` — `get` resolves through
/// wrappers by design, so only the raw read shows whether one survived.
fn stored_tree(db: &TempGroveDb, grove_version: &GroveVersion) -> Element {
    db.get_raw(
        [b"counted".as_slice()].as_ref().into(),
        b"tree",
        None,
        grove_version,
    )
    .unwrap()
    .expect("get raw tree")
}

/// Assert the wrapper survived the append, the entry landed, the parent
/// count is untouched, and the grove verifies clean.
fn assert_wrapper_preserved(db: &TempGroveDb, entry_count: u64, grove_version: &GroveVersion) {
    let stored = stored_tree(db, grove_version);
    assert!(
        stored.is_non_counted(),
        "the append must not strip the NonCounted wrapper, got {}",
        stored.type_str()
    );
    assert_eq!(stored.non_merk_entry_count(), Some(entry_count));
    assert_eq!(
        count_tree_value(db, grove_version),
        0,
        "appending to a NonCounted tree must not change the parent's count"
    );

    let issues = db
        .verify_grovedb(None, true, false, grove_version)
        .expect("verify_grovedb");
    assert!(issues.is_empty(), "issues: {:?}", issues);
}

/// Assert the released `GROVE_V3` behaviour: the append strips the wrapper,
/// the tree becomes counted, and the grove still verifies clean.
fn assert_wrapper_dropped_on_v3(db: &TempGroveDb, entry_count: u64) {
    let stored = stored_tree(db, &GROVE_V3);
    assert!(
        !stored.is_non_counted(),
        "GROVE_V3 must keep the released wrapper drop, got {}",
        stored.type_str()
    );
    assert_eq!(stored.non_merk_entry_count(), Some(entry_count));
    assert_eq!(
        count_tree_value(db, &GROVE_V3),
        1,
        "the bare tree must count toward its parent under GROVE_V3"
    );

    let issues = db
        .verify_grovedb(None, true, false, &GROVE_V3)
        .expect("verify_grovedb");
    assert!(issues.is_empty(), "issues: {:?}", issues);
}

/// A deterministic test ciphertext for the commitment tree appends. Layout:
/// `epk_bytes (32) || enc_ciphertext (104) || out_ciphertext (80)`.
fn test_ciphertext(index: u8) -> TransmittedNoteCiphertext<DashMemo> {
    let mut epk_bytes = [0u8; 32];
    epk_bytes[0] = index;
    let mut enc_data = [0u8; 104];
    enc_data[0] = index;
    let mut out_ciphertext = [0u8; 80];
    out_ciphertext[0] = index;
    TransmittedNoteCiphertext::from_parts(epk_bytes, NoteBytesData(enc_data), out_ciphertext)
}

/// A deterministic 32-byte note commitment, top bit cleared so the bytes are
/// a valid Pallas field element.
fn test_cmx(index: u8) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[0] = index;
    bytes[31] &= 0x7f;
    bytes
}

// ===========================================================================
// MmrTree
// ===========================================================================

#[test]
fn test_mmr_direct_append_preserves_non_counted_wrapper() {
    let grove_version = GroveVersion::latest();
    let db = make_non_counted_tree_under_count_tree(Element::empty_mmr_tree(), grove_version);

    db.mmr_tree_append(
        &[b"counted"],
        b"tree",
        b"leaf".to_vec(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("append");

    // mmr_size of a single-leaf MMR is 1.
    assert_wrapper_preserved(&db, 1, grove_version);
}

#[test]
fn test_mmr_batch_append_preserves_non_counted_wrapper() {
    let grove_version = GroveVersion::latest();
    let db = make_non_counted_tree_under_count_tree(Element::empty_mmr_tree(), grove_version);

    let ops = vec![QualifiedGroveDbOp::mmr_tree_append_op(
        vec![b"counted".to_vec(), b"tree".to_vec()],
        b"leaf".to_vec(),
    )];
    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch append");

    assert_wrapper_preserved(&db, 1, grove_version);
}

#[test]
fn test_mmr_append_drops_wrapper_on_grove_v3() {
    // Direct path.
    let db = make_non_counted_tree_under_count_tree(Element::empty_mmr_tree(), &GROVE_V3);
    db.mmr_tree_append(&[b"counted"], b"tree", b"leaf".to_vec(), None, &GROVE_V3)
        .unwrap()
        .expect("append");
    assert_wrapper_dropped_on_v3(&db, 1);

    // Batch path.
    let db = make_non_counted_tree_under_count_tree(Element::empty_mmr_tree(), &GROVE_V3);
    let ops = vec![QualifiedGroveDbOp::mmr_tree_append_op(
        vec![b"counted".to_vec(), b"tree".to_vec()],
        b"leaf".to_vec(),
    )];
    db.apply_batch(ops, None, None, &GROVE_V3)
        .unwrap()
        .expect("batch append");
    assert_wrapper_dropped_on_v3(&db, 1);
}

// ===========================================================================
// DenseAppendOnlyFixedSizeTree
// ===========================================================================

#[test]
fn test_dense_direct_insert_preserves_non_counted_wrapper() {
    let grove_version = GroveVersion::latest();
    let db = make_non_counted_tree_under_count_tree(Element::empty_dense_tree(4), grove_version);

    db.dense_tree_insert(
        &[b"counted"],
        b"tree",
        b"value".to_vec(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    assert_wrapper_preserved(&db, 1, grove_version);
}

#[test]
fn test_dense_batch_insert_preserves_non_counted_wrapper() {
    let grove_version = GroveVersion::latest();
    let db = make_non_counted_tree_under_count_tree(Element::empty_dense_tree(4), grove_version);

    let ops = vec![QualifiedGroveDbOp::dense_tree_insert_op(
        vec![b"counted".to_vec(), b"tree".to_vec()],
        b"value".to_vec(),
    )];
    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch insert");

    assert_wrapper_preserved(&db, 1, grove_version);
}

#[test]
fn test_dense_insert_drops_wrapper_on_grove_v3() {
    // Direct path.
    let db = make_non_counted_tree_under_count_tree(Element::empty_dense_tree(4), &GROVE_V3);
    db.dense_tree_insert(&[b"counted"], b"tree", b"value".to_vec(), None, &GROVE_V3)
        .unwrap()
        .expect("insert");
    assert_wrapper_dropped_on_v3(&db, 1);

    // Batch path.
    let db = make_non_counted_tree_under_count_tree(Element::empty_dense_tree(4), &GROVE_V3);
    let ops = vec![QualifiedGroveDbOp::dense_tree_insert_op(
        vec![b"counted".to_vec(), b"tree".to_vec()],
        b"value".to_vec(),
    )];
    db.apply_batch(ops, None, None, &GROVE_V3)
        .unwrap()
        .expect("batch insert");
    assert_wrapper_dropped_on_v3(&db, 1);
}

// ===========================================================================
// BulkAppendTree
// ===========================================================================

#[test]
fn test_bulk_direct_append_preserves_non_counted_wrapper() {
    let grove_version = GroveVersion::latest();
    let db = make_non_counted_tree_under_count_tree(
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        grove_version,
    );

    db.bulk_append(
        &[b"counted"],
        b"tree",
        b"value".to_vec(),
        None,
        grove_version,
    )
    .unwrap()
    .expect("append");

    assert_wrapper_preserved(&db, 1, grove_version);
}

#[test]
fn test_bulk_batch_append_preserves_non_counted_wrapper() {
    let grove_version = GroveVersion::latest();
    let db = make_non_counted_tree_under_count_tree(
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        grove_version,
    );

    let ops = vec![QualifiedGroveDbOp::bulk_append_op(
        vec![b"counted".to_vec(), b"tree".to_vec()],
        b"value".to_vec(),
    )];
    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch append");

    assert_wrapper_preserved(&db, 1, grove_version);
}

#[test]
fn test_bulk_append_drops_wrapper_on_grove_v3() {
    // Direct path.
    let db = make_non_counted_tree_under_count_tree(
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        &GROVE_V3,
    );
    db.bulk_append(&[b"counted"], b"tree", b"value".to_vec(), None, &GROVE_V3)
        .unwrap()
        .expect("append");
    assert_wrapper_dropped_on_v3(&db, 1);

    // Batch path.
    let db = make_non_counted_tree_under_count_tree(
        Element::empty_bulk_append_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        &GROVE_V3,
    );
    let ops = vec![QualifiedGroveDbOp::bulk_append_op(
        vec![b"counted".to_vec(), b"tree".to_vec()],
        b"value".to_vec(),
    )];
    db.apply_batch(ops, None, None, &GROVE_V3)
        .unwrap()
        .expect("batch append");
    assert_wrapper_dropped_on_v3(&db, 1);
}

// ===========================================================================
// CommitmentTree
// ===========================================================================

#[test]
fn test_commitment_direct_insert_preserves_non_counted_wrapper() {
    let grove_version = GroveVersion::latest();
    let db = make_non_counted_tree_under_count_tree(
        Element::empty_commitment_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        grove_version,
    );

    db.commitment_tree_insert(
        &[b"counted"],
        b"tree",
        test_cmx(1),
        [1u8; 32],
        [2u8; 32],
        test_ciphertext(1),
        None,
        grove_version,
    )
    .unwrap()
    .expect("insert");

    assert_wrapper_preserved(&db, 1, grove_version);
}

#[test]
fn test_commitment_batch_insert_preserves_non_counted_wrapper() {
    let grove_version = GroveVersion::latest();
    let db = make_non_counted_tree_under_count_tree(
        Element::empty_commitment_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        grove_version,
    );

    let ops = vec![QualifiedGroveDbOp::commitment_tree_insert_op_typed(
        vec![b"counted".to_vec(), b"tree".to_vec()],
        test_cmx(1),
        [1u8; 32],
        [2u8; 32],
        &test_ciphertext(1),
    )];
    db.apply_batch(ops, None, None, grove_version)
        .unwrap()
        .expect("batch insert");

    assert_wrapper_preserved(&db, 1, grove_version);
}

#[test]
fn test_commitment_insert_drops_wrapper_on_grove_v3() {
    // Direct path.
    let db = make_non_counted_tree_under_count_tree(
        Element::empty_commitment_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        &GROVE_V3,
    );
    db.commitment_tree_insert(
        &[b"counted"],
        b"tree",
        test_cmx(1),
        [1u8; 32],
        [2u8; 32],
        test_ciphertext(1),
        None,
        &GROVE_V3,
    )
    .unwrap()
    .expect("insert");
    assert_wrapper_dropped_on_v3(&db, 1);

    // Batch path.
    let db = make_non_counted_tree_under_count_tree(
        Element::empty_commitment_tree(TEST_CHUNK_POWER).expect("valid chunk_power"),
        &GROVE_V3,
    );
    let ops = vec![QualifiedGroveDbOp::commitment_tree_insert_op_typed(
        vec![b"counted".to_vec(), b"tree".to_vec()],
        test_cmx(1),
        [1u8; 32],
        [2u8; 32],
        &test_ciphertext(1),
    )];
    db.apply_batch(ops, None, None, &GROVE_V3)
        .unwrap()
        .expect("batch insert");
    assert_wrapper_dropped_on_v3(&db, 1);
}

// ===========================================================================
// Wrapper survives repeated appends (the second append reads the state the
// first one wrote)
// ===========================================================================

#[test]
fn test_wrapper_survives_repeated_appends() {
    let grove_version = GroveVersion::latest();
    let db = make_non_counted_tree_under_count_tree(Element::empty_mmr_tree(), grove_version);

    for i in 0..4u8 {
        db.mmr_tree_append(&[b"counted"], b"tree", vec![i], None, grove_version)
            .unwrap()
            .expect("append");
    }

    // mmr_size of a 4-leaf MMR is 7 (4 leaves + 3 internal nodes).
    assert_wrapper_preserved(&db, 7, grove_version);
}
